use rand::{SeedableRng, rngs::StdRng};
use rust_photo_frame::config::{
    ColorSelection, Configuration, FixedImagePathSelection, MattingKind, MattingMode,
    MattingSelection, StudioMatColor, TransitionKind, TransitionSelection,
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
  types: [studio]
  options:
    studio:
      bevel-width-px: 5.0
      bevel-color: [200, 210, 220]
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();

    let options = cfg.matting.options();
    assert!(matches!(
        cfg.matting.selection(),
        MattingSelection::Fixed(entry)
            if entry.kind == MattingKind::Studio && entry.index == 0
    ));
    let mat = options
        .get(&MattingKind::Studio)
        .expect("expected studio matting option");
    match &mat.style {
        rust_photo_frame::config::MattingMode::Studio {
            colors,
            bevel_width_px,
            bevel_color,
            texture_strength,
            warp_period_px,
            weft_period_px,
            ..
        } => {
            assert_eq!(colors.as_slice(), &[StudioMatColor::PhotoAverage]);
            assert!((*bevel_width_px - 5.0).abs() < f32::EPSILON);
            assert_eq!(*bevel_color, [200, 210, 220]);
            assert!((*texture_strength - 1.0).abs() < f32::EPSILON);
            assert!((*warp_period_px - 5.6).abs() < f32::EPSILON);
            assert!((*weft_period_px - 5.2).abs() < f32::EPSILON);
        }
        _ => panic!("expected studio matting"),
    }
}

#[test]
fn parse_studio_with_custom_texture_strength() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  types: [studio]
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
  types: [studio]
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
  types: [fixed-color, blur]
  options:
    fixed-color:
      colors:
        - [10, 20, 30]
        - [5, 15, 25]
      color-selection: random
    blur:
      minimum-mat-percentage: 7.5
      sigma: 12.0
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    let MattingSelection::Random(entries) = cfg.matting.selection() else {
        panic!("expected random matting selection");
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].index, 0);
    assert_eq!(entries[0].kind, MattingKind::FixedColor);
    assert_eq!(entries[1].index, 1);
    assert_eq!(entries[1].kind, MattingKind::Blur);
    let options = cfg.matting.options();
    assert_eq!(options.len(), 2);
    let fixed = options
        .get(&MattingKind::FixedColor)
        .expect("expected fixed-color mat option");
    if let rust_photo_frame::config::MattingMode::FixedColor {
        colors,
        color_selection,
    } = &fixed.style
    {
        assert_eq!(colors.as_slice(), &[[10, 20, 30], [5, 15, 25]]);
        assert_eq!(*color_selection, ColorSelection::Random);
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
fn parse_fixed_color_single_color_alias() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  types: [fixed-color]
  options:
    fixed-color:
      color: [17, 34, 51]
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    let options = cfg.matting.options();
    let fixed = options
        .get(&MattingKind::FixedColor)
        .expect("expected fixed-color mat option");
    if let rust_photo_frame::config::MattingMode::FixedColor { colors, .. } = &fixed.style {
        assert_eq!(colors.as_slice(), &[[17, 34, 51]]);
    } else {
        panic!("expected fixed-color matting");
    }
}

#[test]
fn parse_sequential_matting_configuration() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  types: [fixed-color, blur]
  type-selection: sequential
  options:
    fixed-color:
      colors:
        - [10, 20, 30]
        - [40, 50, 60]
    blur:
      sigma: 12.0
      minimum-mat-percentage: 7.5
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    match cfg.matting.selection() {
        MattingSelection::Sequential { entries, .. } => {
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].index, 0);
            assert_eq!(entries[0].kind, MattingKind::FixedColor);
            assert_eq!(entries[1].index, 1);
            assert_eq!(entries[1].kind, MattingKind::Blur);
        }
        other => panic!("expected sequential matting selection, got {other:?}"),
    }

    let mut rng = StdRng::seed_from_u64(1);
    let first = cfg.matting.choose_option(&mut rng);
    let second = cfg.matting.choose_option(&mut rng);
    let third = cfg.matting.choose_option(&mut rng);

    match &first.style {
        rust_photo_frame::config::MattingMode::FixedColor {
            color_selection, ..
        } => {
            assert_eq!(*color_selection, ColorSelection::Sequential);
        }
        _ => panic!("expected first matting option to be fixed-color"),
    }
    match second.style {
        rust_photo_frame::config::MattingMode::Blur { .. } => {}
        _ => panic!("expected second matting option to be blur"),
    }
    match third.style {
        rust_photo_frame::config::MattingMode::FixedColor { .. } => {}
        _ => panic!("expected third matting option to repeat fixed-color"),
    }
}

#[test]
fn parse_fixed_image_with_multiple_paths() {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use std::fs;

    let png = STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z/C/HwAFgwJ/lc7BnwAAAABJRU5ErkJggg==")
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("bg-0.png");
    let second = dir.path().join("bg-1.png");
    fs::write(&first, &png).unwrap();
    fs::write(&second, &png).unwrap();

    let yaml = format!(
        r#"
photo-library-path: "/photos"
matting:
  types: [fixed-image]
  options:
    fixed-image:
      path: ["{first}", "{second}"]
      path-selection: sequential
      fit: contain
"#,
        first = first.display(),
        second = second.display()
    );

    let cfg: Configuration = serde_yaml::from_str(&yaml).unwrap();
    let option = cfg
        .matting
        .primary_option()
        .expect("expected fixed-image matting");

    match &option.style {
        MattingMode::FixedImage {
            paths,
            path_selection,
            fit,
        } => {
            assert_eq!(paths, &vec![first.clone(), second.clone()]);
            assert_eq!(*path_selection, FixedImagePathSelection::Sequential);
            assert!(matches!(
                fit,
                rust_photo_frame::config::FixedImageFit::Contain
            ));
        }
        other => panic!("expected fixed-image matting, got {other:?}"),
    }

    let mut mat = option.clone();
    mat.prepare_runtime().unwrap();
    let mut rng = StdRng::seed_from_u64(1);
    let bg0 = mat
        .runtime
        .select_fixed_image(&mut rng)
        .expect("expected first fixed background");
    let bg1 = mat
        .runtime
        .select_fixed_image(&mut rng)
        .expect("expected second fixed background");
    let bg2 = mat
        .runtime
        .select_fixed_image(&mut rng)
        .expect("expected rotation to repeat");

    assert_eq!(bg0.path(), first.as_path());
    assert_eq!(bg1.path(), second.as_path());
    assert_eq!(bg2.path(), first.as_path());
}

#[test]
fn parse_fixed_image_with_single_string_path() {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use std::fs;

    let png = STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z/C/HwAFgwJ/lc7BnwAAAABJRU5ErkJggg==")
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let only = dir.path().join("bg.png");
    fs::write(&only, &png).unwrap();

    let yaml = format!(
        r#"
photo-library-path: "/photos"
matting:
  types: [fixed-image]
  options:
    fixed-image:
      path: "{only}"
"#,
        only = only.display()
    );

    let cfg: Configuration = serde_yaml::from_str(&yaml).unwrap();
    let option = cfg
        .matting
        .primary_option()
        .expect("expected fixed-image matting");

    match &option.style {
        MattingMode::FixedImage { paths, .. } => {
            assert_eq!(paths, &vec![only.clone()]);
        }
        other => panic!("expected fixed-image matting, got {other:?}"),
    }

    let mut mat = option.clone();
    mat.prepare_runtime().unwrap();
    let mut rng = StdRng::seed_from_u64(3);
    let bg = mat
        .runtime
        .select_fixed_image(&mut rng)
        .expect("expected fixed background");
    assert_eq!(bg.path(), only.as_path());
}

#[test]
fn fixed_image_with_empty_paths_is_disabled() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  types: [fixed-image]
  options:
    fixed-image:
      path: []
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    let mut mat = cfg
        .matting
        .primary_option()
        .expect("expected fixed-image matting")
        .clone();

    mat.prepare_runtime().unwrap();
    let mut rng = StdRng::seed_from_u64(5);
    assert!(mat.runtime.select_fixed_image(&mut rng).is_none());
}

#[test]
fn random_matting_without_options_is_rejected() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  types: [fixed-color, blur]
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    assert!(
        err.to_string()
            .contains("matting.types entry fixed-color must match a key")
    );
}

#[test]
fn selecting_missing_option_is_rejected() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  types: [studio]
  options:
    blur:
      sigma: 12.0
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    assert!(
        err.to_string()
            .contains("matting.types entry studio must match a key")
    );
}

#[test]
fn matting_type_field_is_rejected() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  type: random
  options:
    fixed-color:
      colors: [[5, 15, 25]]
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    assert!(
        err.to_string()
            .contains("matting.type is no longer supported")
    );
}

#[test]
fn parse_inline_fade_transition() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  types: [fade]
  duration-ms: 750
  through-black: true
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    assert!(matches!(
        cfg.transition.selection(),
        TransitionSelection::Fixed(entry)
            if entry.kind == TransitionKind::Fade && entry.index == 0
    ));
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
fn parse_inline_iris_transition() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  types: [iris]
  duration-ms: 880
  blades: 9
  blade-rgba: [0.2, 0.22, 0.24, 0.85]
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    assert!(matches!(
        cfg.transition.selection(),
        TransitionSelection::Fixed(entry)
            if entry.kind == TransitionKind::Iris && entry.index == 0
    ));
    let options = cfg.transition.options();
    let iris = options
        .get(&TransitionKind::Iris)
        .expect("expected iris transition option");
    assert_eq!(iris.duration().as_millis(), 880);
    match iris.mode() {
        rust_photo_frame::config::TransitionMode::Iris(cfg) => {
            assert_eq!(cfg.blades, 9);
            assert_eq!(cfg.blade_rgba, [0.2, 0.22, 0.24, 0.85]);
        }
        _ => panic!("expected iris transition"),
    }
}

#[test]
fn parse_random_transition_configuration() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  types: [fade, wipe, push]
  options:
    fade:
      duration-ms: 450
    wipe:
      duration-ms: 600
      angle-list-degrees: [90.0]
      softness: 0.1
    push:
      duration-ms: 640
      angle-list-degrees: [0.0, 180.0]
      angle-selection: sequential
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    let TransitionSelection::Random(entries) = cfg.transition.selection() else {
        panic!("expected random transition selection");
    };
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].index, 0);
    assert_eq!(entries[0].kind, TransitionKind::Fade);
    assert_eq!(entries[1].index, 1);
    assert_eq!(entries[1].kind, TransitionKind::Wipe);
    assert_eq!(entries[2].index, 2);
    assert_eq!(entries[2].kind, TransitionKind::Push);
    let options = cfg.transition.options();
    assert_eq!(options.len(), 3);
    let wipe = options
        .get(&TransitionKind::Wipe)
        .expect("expected wipe transition option");
    assert_eq!(wipe.duration().as_millis(), 600);
    match wipe.mode() {
        rust_photo_frame::config::TransitionMode::Wipe(cfg) => {
            assert_eq!(cfg.angles.angles_deg.as_ref(), &[90.0]);
            assert_eq!(
                cfg.angles.selection,
                rust_photo_frame::config::AngleSelection::Random
            );
            assert!((cfg.softness - 0.1).abs() < f32::EPSILON);
        }
        _ => panic!("expected wipe transition"),
    }

    let push = options
        .get(&TransitionKind::Push)
        .expect("expected push transition option");
    match push.mode() {
        rust_photo_frame::config::TransitionMode::Push(cfg) => {
            assert_eq!(cfg.angles.angles_deg.as_ref(), &[0.0, 180.0]);
            assert_eq!(
                cfg.angles.selection,
                rust_photo_frame::config::AngleSelection::Sequential
            );
        }
        _ => panic!("expected push transition"),
    }
}

#[test]
fn parse_sequential_transition_configuration() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  types: [push, wipe]
  type-selection: sequential
  options:
    push:
      duration-ms: 640
      angle-list-degrees: [0.0, 180.0]
      angle-selection: sequential
    wipe:
      duration-ms: 520
      angle-list-degrees: [90.0]
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    match cfg.transition.selection() {
        TransitionSelection::Sequential { entries, .. } => {
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].index, 0);
            assert_eq!(entries[0].kind, TransitionKind::Push);
            assert_eq!(entries[1].index, 1);
            assert_eq!(entries[1].kind, TransitionKind::Wipe);
        }
        other => panic!("expected sequential transition selection, got {other:?}"),
    }

    let mut rng = StdRng::seed_from_u64(42);
    let first = cfg.transition.choose_option(&mut rng);
    let second = cfg.transition.choose_option(&mut rng);
    let third = cfg.transition.choose_option(&mut rng);

    match first.mode() {
        rust_photo_frame::config::TransitionMode::Push(_) => {}
        _ => panic!("expected first transition to be push"),
    }
    match second.mode() {
        rust_photo_frame::config::TransitionMode::Wipe(_) => {}
        _ => panic!("expected second transition to be wipe"),
    }
    match third.mode() {
        rust_photo_frame::config::TransitionMode::Push(_) => {}
        _ => panic!("expected third transition to return to push"),
    }
}

#[test]
fn random_transition_without_options_is_rejected() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  types: [fade, wipe]
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    assert!(
        err.to_string()
            .contains("transition.types entry fade must match a key")
    );
}

#[test]
fn transition_type_field_is_rejected() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  type: random
  options:
    fade:
      duration-ms: 520
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    assert!(
        err.to_string()
            .contains("transition.type is no longer supported")
    );
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
  types: [wipe]
  angle-jitter-degrees: -15.0
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    assert!(
        err.to_string()
            .contains("requires angle-jitter-degrees >= 0")
    );
}

#[test]
fn push_transition_rejects_negative_jitter() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  types: [push]
  angle-jitter-degrees: -30.0
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    assert!(
        err.to_string()
            .contains("requires angle-jitter-degrees >= 0")
    );
}

#[test]
fn iris_transition_clamps_blade_count() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  types: [iris]
  blades: 0
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    let iris = cfg
        .transition
        .options()
        .get(&TransitionKind::Iris)
        .expect("expected iris transition option");
    match iris.mode() {
        rust_photo_frame::config::TransitionMode::Iris(cfg) => {
            assert_eq!(cfg.blades, 5);
        }
        _ => panic!("expected iris transition"),
    }
}

#[test]
fn push_transition_configures_multiple_angles() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  types: [push]
  duration-ms: 725
  angle-list-degrees: [90.0, 270.0]
  angle-selection: sequential
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    let option = cfg
        .transition
        .options()
        .get(&TransitionKind::Push)
        .expect("expected push transition option");
    assert_eq!(option.duration().as_millis(), 725);
    match option.mode() {
        rust_photo_frame::config::TransitionMode::Push(push) => {
            assert_eq!(push.angles.angles_deg.as_ref(), &[90.0, 270.0]);
            assert_eq!(
                push.angles.selection,
                rust_photo_frame::config::AngleSelection::Sequential
            );
        }
        _ => panic!("expected push transition"),
    }
}
