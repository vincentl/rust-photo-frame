use rust_photo_frame::config::{
    Configuration, MattingKind, MattingSelection, TransitionKind, TransitionSelection,
};
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

    let mat = cfg
        .matting
        .primary_option()
        .expect("expected primary matting option");
    match mat.style {
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

    let mat = cfg
        .matting
        .primary_option()
        .expect("expected primary matting option");
    match mat.style {
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
fn parse_inline_fade_transition() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  type: fade
  duration-ms: 750
  through-black: true
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(
        cfg.transition.selection(),
        TransitionSelection::Fixed(TransitionKind::Fade)
    );
    let options = cfg.transition.options();
    let fade = options
        .get(&TransitionKind::Fade)
        .expect("expected fade transition option");
    assert_eq!(fade.duration().as_millis(), 750);
    match fade.mode() {
        rust_photo_frame::config::TransitionMode::Fade(cfg) => {
            assert!(cfg.through_black);
        }
        _ => panic!("expected fade transition"),
    }
}

#[test]
fn parse_random_transition_configuration() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  type: random
  options:
    fade:
      duration-ms: 450
    wipe:
      duration-ms: 600
      angle-deg: 90.0
      softness: 0.1
    push:
      duration-ms: 640
      angle-deg: 0.0
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(cfg.transition.selection(), TransitionSelection::Random);
    let options = cfg.transition.options();
    assert_eq!(options.len(), 3);
    let wipe = options
        .get(&TransitionKind::Wipe)
        .expect("expected wipe transition option");
    assert_eq!(wipe.duration().as_millis(), 600);
    match wipe.mode() {
        rust_photo_frame::config::TransitionMode::Wipe(cfg) => {
            assert!((cfg.angle_deg - 90.0).abs() < f32::EPSILON);
            assert!((cfg.softness - 0.1).abs() < f32::EPSILON);
        }
        _ => panic!("expected wipe transition"),
    }
}

#[test]
fn random_transition_without_options_is_rejected() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  type: random
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    assert!(err.to_string().contains("transition.options"));
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

#[test]
fn wipe_transition_rejects_negative_jitter() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  type: wipe
  angle-jitter-deg: -15.0
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    assert!(err
        .to_string()
        .contains("requires wipe.angle-jitter-deg >= 0"));
}

#[test]
fn push_transition_rejects_negative_jitter() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  type: push
  angle-jitter-deg: -30.0
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    assert!(err
        .to_string()
        .contains("requires push.angle-jitter-deg >= 0"));
}
