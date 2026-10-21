use crate::config::{PhotoEffectConfig, PhotoEffectOptions};
use crate::events::PhotoLoaded;
use anyhow::Result;
use image::RgbaImage;
use rand::{SeedableRng, rngs::StdRng};
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Applies optional photo effects to decoded images before they reach the viewer.
pub async fn run(
    from_loader: Receiver<PhotoLoaded>,
    to_viewer: Sender<PhotoLoaded>,
    cancel: CancellationToken,
    config: PhotoEffectConfig,
) -> Result<()> {
    if !config.is_enabled() {
        forward_only(from_loader, to_viewer, cancel).await
    } else {
        run_with_effects(from_loader, to_viewer, cancel, config).await
    }
}

async fn forward_only(
    mut from_loader: Receiver<PhotoLoaded>,
    to_viewer: Sender<PhotoLoaded>,
    cancel: CancellationToken,
) -> Result<()> {
    loop {
        select! {
            _ = cancel.cancelled() => break,
            maybe_loaded = from_loader.recv() => {
                match maybe_loaded {
                    Some(photo) => {
                        if to_viewer.send(photo).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }
    Ok(())
}

async fn run_with_effects(
    mut from_loader: Receiver<PhotoLoaded>,
    to_viewer: Sender<PhotoLoaded>,
    cancel: CancellationToken,
    config: PhotoEffectConfig,
) -> Result<()> {
    let mut rng = StdRng::from_os_rng();

    loop {
        select! {
            _ = cancel.cancelled() => break,
            maybe_loaded = from_loader.recv() => {
                let Some(PhotoLoaded { mut prepared, priority, mut partner }) = maybe_loaded else {
                    break;
                };

                // One option per screen: a pair's two members get the same effect.
                if let Some(option) = config.choose_option(&mut rng) {
                    apply_to_prepared(&mut prepared, &option);
                    if let Some(partner) = partner.as_mut() {
                        apply_to_prepared(&mut partner.prepared, &option);
                    }
                }

                if to_viewer
                    .send(PhotoLoaded { prepared, priority, partner })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    }

    Ok(())
}

fn apply_to_prepared(prepared: &mut crate::events::PreparedImageCpu, option: &PhotoEffectOptions) {
    if let Some(mut image) = reconstruct_image(prepared) {
        apply_effect(&mut image, option);
        prepared.pixels = image.into_raw();
    } else {
        warn!(
            path = %prepared.path.display(),
            width = prepared.width,
            height = prepared.height,
            "failed to reconstruct RGBA image for photo effect"
        );
    }
}

fn reconstruct_image(prepared: &mut crate::events::PreparedImageCpu) -> Option<RgbaImage> {
    let width = prepared.width;
    let height = prepared.height;
    if width == 0 || height == 0 {
        return None;
    }
    let expected_len = width as usize * height as usize * 4;
    if prepared.pixels.len() != expected_len {
        return None;
    }
    let pixels = std::mem::take(&mut prepared.pixels);
    RgbaImage::from_raw(width, height, pixels)
}

fn apply_effect(image: &mut RgbaImage, option: &PhotoEffectOptions) {
    match option {
        PhotoEffectOptions::PrintSimulation(settings) => {
            crate::processing::print_simulation::apply_print_simulation(image, settings);
        }
    }
    debug!("applied photo effect {:?}", option.kind());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::PreparedImageCpu;
    use image::RgbaImage;
    use rand::{SeedableRng, rngs::StdRng};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn forwards_without_effect_when_disabled() {
        let (tx_in, rx_in) = mpsc::channel(1);
        let (tx_out, mut rx_out) = mpsc::channel(1);
        let cancel = CancellationToken::new();

        tx_in
            .send(PhotoLoaded {
                prepared: PreparedImageCpu {
                    path: std::path::PathBuf::from("dummy"),
                    width: 1,
                    height: 1,
                    source_size: (1, 1),
                    pixels: vec![10, 20, 30, 255],
                },
                priority: false,
                partner: None,
            })
            .await
            .unwrap();
        drop(tx_in);

        run(rx_in, tx_out, cancel.clone(), PhotoEffectConfig::default())
            .await
            .unwrap();

        let received = rx_out.try_recv().unwrap();
        let PhotoLoaded {
            prepared, priority, ..
        } = received;
        assert_eq!(prepared.pixels, vec![10, 20, 30, 255]);
        assert!(!priority);
    }

    #[tokio::test]
    async fn applies_print_simulation_when_enabled() {
        let yaml = r#"
selection: random
active:
  - kind: print-simulation
    relief-strength: 1.0
    sheen-strength: 0.5
"#;
        let config: PhotoEffectConfig = serde_yaml::from_str(yaml).unwrap();

        let mut expected_image =
            RgbaImage::from_raw(2, 1, vec![10, 20, 30, 255, 200, 150, 100, 255]).unwrap();
        let mut rng = StdRng::seed_from_u64(42);
        let option = config.choose_option(&mut rng).unwrap();
        apply_effect(&mut expected_image, &option);
        let expected_pixels = expected_image.into_raw();

        let (tx_in, rx_in) = mpsc::channel(1);
        let (tx_out, mut rx_out) = mpsc::channel(1);
        let cancel = CancellationToken::new();

        tx_in
            .send(PhotoLoaded {
                prepared: PreparedImageCpu {
                    path: std::path::PathBuf::from("dummy"),
                    width: 2,
                    height: 1,
                    source_size: (2, 1),
                    pixels: vec![10, 20, 30, 255, 200, 150, 100, 255],
                },
                priority: false,
                partner: None,
            })
            .await
            .unwrap();
        drop(tx_in);

        run(rx_in, tx_out, cancel, config).await.unwrap();

        let PhotoLoaded {
            prepared, priority, ..
        } = rx_out.try_recv().unwrap();
        assert_eq!(prepared.pixels, expected_pixels);
        assert!(!priority);
    }

    #[tokio::test]
    async fn one_effect_per_screen_applies_to_both_members() {
        use crate::events::LoadedPartner;
        let yaml = r#"
selection: random
active:
  - kind: print-simulation
    relief-strength: 1.0
    sheen-strength: 0.5
"#;
        let config: PhotoEffectConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.is_enabled());
        let member = |name: &str| PreparedImageCpu {
            path: std::path::PathBuf::from(name),
            width: 2,
            height: 1,
            source_size: (2, 1),
            pixels: vec![10, 20, 30, 255, 200, 150, 100, 255],
        };
        let (tx_in, rx_in) = mpsc::channel(1);
        let (tx_out, mut rx_out) = mpsc::channel(1);
        tx_in
            .send(PhotoLoaded {
                prepared: member("a"),
                priority: false,
                partner: Some(LoadedPartner {
                    prepared: member("b"),
                    priority: true,
                }),
            })
            .await
            .unwrap();
        drop(tx_in);
        run(rx_in, tx_out, CancellationToken::new(), config)
            .await
            .unwrap();
        let PhotoLoaded {
            prepared, partner, ..
        } = rx_out.try_recv().unwrap();
        let partner = partner.expect("the pair stays a pair");
        assert!(partner.priority);
        assert_ne!(prepared.pixels, member("a").pixels, "effect applied");
        assert_eq!(
            prepared.pixels, partner.prepared.pixels,
            "same effect on both"
        );
    }
}
