use crate::config::{PhotoAffectConfig, PhotoAffectOptions};
use crate::events::PhotoLoaded;
use anyhow::Result;
use image::RgbaImage;
use rand::{rngs::StdRng, SeedableRng};
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Applies optional photo affects to decoded images before they reach the viewer.
pub async fn run(
    from_loader: Receiver<PhotoLoaded>,
    to_viewer: Sender<PhotoLoaded>,
    cancel: CancellationToken,
    config: PhotoAffectConfig,
) -> Result<()> {
    if !config.is_enabled() {
        forward_only(from_loader, to_viewer, cancel).await
    } else {
        run_with_affects(from_loader, to_viewer, cancel, config).await
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

async fn run_with_affects(
    mut from_loader: Receiver<PhotoLoaded>,
    to_viewer: Sender<PhotoLoaded>,
    cancel: CancellationToken,
    config: PhotoAffectConfig,
) -> Result<()> {
    let mut rng = StdRng::from_os_rng();

    loop {
        select! {
            _ = cancel.cancelled() => break,
            maybe_loaded = from_loader.recv() => {
                let Some(PhotoLoaded(mut prepared)) = maybe_loaded else {
                    break;
                };

                if let Some(option) = config.choose_option(&mut rng) {
                    if let Some(mut image) = reconstruct_image(&prepared) {
                        apply_affect(&mut image, &option);
                        prepared.pixels = image.into_raw();
                    } else {
                        warn!(
                            path = %prepared.path.display(),
                            width = prepared.width,
                            height = prepared.height,
                            "failed to reconstruct RGBA image for photo affect"
                        );
                    }
                }

                if to_viewer.send(PhotoLoaded(prepared)).await.is_err() {
                    break;
                }
            }
        }
    }

    Ok(())
}

fn reconstruct_image(prepared: &crate::events::PreparedImageCpu) -> Option<RgbaImage> {
    let width = prepared.width;
    let height = prepared.height;
    let pixels = prepared.pixels.clone();
    let expected_len = width as usize * height as usize * 4;
    if pixels.len() != expected_len || width == 0 || height == 0 {
        return None;
    }
    RgbaImage::from_raw(width, height, pixels)
}

fn apply_affect(image: &mut RgbaImage, option: &PhotoAffectOptions) {
    match option {
        PhotoAffectOptions::PrintSimulation(settings) => {
            crate::processing::print_simulation::apply_print_simulation(image, settings);
        }
    }
    debug!("applied photo affect {:?}", option.kind());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::PreparedImageCpu;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn forwards_without_affect_when_disabled() {
        let (tx_in, rx_in) = mpsc::channel(1);
        let (tx_out, mut rx_out) = mpsc::channel(1);
        let cancel = CancellationToken::new();

        tx_in
            .send(PhotoLoaded(PreparedImageCpu {
                path: std::path::PathBuf::from("dummy"),
                width: 1,
                height: 1,
                pixels: vec![10, 20, 30, 255],
            }))
            .await
            .unwrap();
        drop(tx_in);

        run(rx_in, tx_out, cancel.clone(), PhotoAffectConfig::default())
            .await
            .unwrap();

        let received = rx_out.try_recv().unwrap();
        let PhotoLoaded(prepared) = received;
        assert_eq!(prepared.pixels, vec![10, 20, 30, 255]);
    }

    #[tokio::test]
    async fn applies_print_simulation_when_enabled() {
        let yaml = r#"
types: [print-simulation]
options:
  print-simulation:
    relief-strength: 1.0
    sheen-strength: 0.5
"#;
        let config: PhotoAffectConfig = serde_yaml::from_str(yaml).unwrap();

        let (tx_in, rx_in) = mpsc::channel(1);
        let (tx_out, mut rx_out) = mpsc::channel(1);
        let cancel = CancellationToken::new();

        tx_in
            .send(PhotoLoaded(PreparedImageCpu {
                path: std::path::PathBuf::from("dummy"),
                width: 2,
                height: 1,
                pixels: vec![10, 20, 30, 255, 200, 150, 100, 255],
            }))
            .await
            .unwrap();
        drop(tx_in);

        run(rx_in, tx_out, cancel, config).await.unwrap();

        let PhotoLoaded(prepared) = rx_out.try_recv().unwrap();
        assert_ne!(prepared.pixels, vec![10, 20, 30, 255, 200, 150, 100, 255]);
    }
}
