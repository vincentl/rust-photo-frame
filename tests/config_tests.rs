use rust_photo_frame::config::{Configuration, MattingKind, MattingSelection};
use std::path::PathBuf;

#[test]
fn parse_kebab_case_config() {
    let yaml = r#"
photo-library-path: "/photos"
"#;
    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(cfg.photo_library_path, PathBuf::from("/photos"));
    assert!((cfg.oversample - 1.0).abs() < f32::EPSILON);
}

#[test]
fn parse_with_oversample() {
    let yaml = r#"
photo-library-path: "/photos"
oversample: 1.5
"#;
    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(cfg.photo_library_path, PathBuf::from("/photos"));
    assert!((cfg.oversample - 1.5).abs() < f32::EPSILON);
}

#[test]
fn parse_with_startup_shuffle_seed() {
    let yaml = r#"
photo-library-path: "/p"
startup-shuffle-seed: 7
"#;
    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(cfg.startup_shuffle_seed, Some(7));
}

#[test]
fn parse_with_studio_matting() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  type: studio
  options:
    studio:
      bevel-width-px: 5.0
      bevel-color: [200, 210, 220]
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();

    let options = cfg.matting.options();
    assert_eq!(
        cfg.matting.selection(),
        MattingSelection::Fixed(MattingKind::Studio)
    );
    let mat = options
        .get(&MattingKind::Studio)
        .expect("expected studio matting option");
    match mat.style {
        rust_photo_frame::config::MattingMode::Studio {
            bevel_width_px,
            bevel_color,
            texture_strength,
            warp_period_px,
            weft_period_px,
            ..
        } => {
            assert!((bevel_width_px - 5.0).abs() < f32::EPSILON);
            assert_eq!(bevel_color, [200, 210, 220]);
            assert!((texture_strength - 1.0).abs() < f32::EPSILON);
            assert!((warp_period_px - 5.6).abs() < f32::EPSILON);
            assert!((weft_period_px - 5.2).abs() < f32::EPSILON);
        }
        _ => panic!("expected studio matting"),
    }
}

#[test]
fn parse_studio_with_custom_texture_strength() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  type: studio
  texture-strength: 0.35
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();

    match cfg.matting.style {
        rust_photo_frame::config::MattingMode::Studio {
            texture_strength, ..
        } => {
            assert!((texture_strength - 0.35).abs() < f32::EPSILON);
        }
        _ => panic!("expected studio matting"),
    }
}

#[test]
fn parse_studio_with_custom_weave_periods() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  type: studio
  warp-period-px: 8.5
  weft-period-px: 4.25
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();

    match cfg.matting.style {
        rust_photo_frame::config::MattingMode::Studio {
            warp_period_px,
            weft_period_px,
            ..
        } => {
            assert!((warp_period_px - 8.5).abs() < f32::EPSILON);
            assert!((weft_period_px - 4.25).abs() < f32::EPSILON);
        }
        _ => panic!("expected studio matting"),
    }
}

#[test]
fn parse_random_matting_configuration() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  type: random
  options:
    fixed-color:
      color: [10, 20, 30]
    blur:
      minimum-mat-percentage: 7.5
      sigma: 12.0
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(cfg.matting.selection(), MattingSelection::Random);
    let options = cfg.matting.options();
    assert_eq!(options.len(), 2);
    let fixed = options
        .get(&MattingKind::FixedColor)
        .expect("expected fixed-color mat option");
    if let rust_photo_frame::config::MattingMode::FixedColor { color } = fixed.style {
        assert_eq!(color, [10, 20, 30]);
    } else {
        panic!("expected fixed-color matting");
    }
    let blur = options
        .get(&MattingKind::Blur)
        .expect("expected blur mat option");
    if let rust_photo_frame::config::MattingMode::Blur { sigma, .. } = blur.style {
        assert!((sigma - 12.0).abs() < f32::EPSILON);
        assert!((blur.minimum_mat_percentage - 7.5).abs() < f32::EPSILON);
    } else {
        panic!("expected blur matting");
    }
}

#[test]
fn random_matting_without_options_is_rejected() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  type: random
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    assert!(err.to_string().contains("matting.options"));
}

#[test]
fn selecting_missing_option_is_rejected() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  type: studio
  options:
    blur:
      sigma: 12.0
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    assert!(err
        .to_string()
        .contains("matting.type studio must match a key"));
}

#[test]
fn validated_rejects_zero_preload() {
    let cfg = Configuration {
        viewer_preload_count: 0,
        ..Default::default()
    };
    assert!(cfg.validated().is_err());
}

#[test]
fn validated_rejects_invalid_numeric_ranges() {
    let cfg = Configuration {
        loader_max_concurrent_decodes: 0,
        ..Default::default()
    };
    assert!(cfg.validated().is_err());

    let cfg = Configuration {
        oversample: 0.0,
        ..Default::default()
    };
    assert!(cfg.validated().is_err());
}
