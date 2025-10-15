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
  selection: fixed
  active:
    - kind: studio
      bevel-width-px: 5.0
      bevel-color: [200, 210, 220]
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();

    assert!(matches!(
        cfg.matting.selection(),
        MattingSelection::Fixed(entry)
            if entry.kind == MattingKind::Studio && entry.index == 0
    ));
    let selected = cfg
        .matting
        .primary_selected()
        .expect("expected studio matting option");
    assert_eq!(selected.entry.kind, MattingKind::Studio);
    assert_eq!(selected.entry.index, 0);
    match &selected.option.style {
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
  selection: fixed
  active:
    - kind: studio
      texture-strength: 0.35
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();

    let selected = cfg
        .matting
        .primary_selected()
        .expect("expected primary matting option");
    match selected.option.style {
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
  selection: fixed
  active:
    - kind: studio
      warp-period-px: 8.5
      weft-period-px: 4.25
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();

    let selected = cfg
        .matting
        .primary_selected()
        .expect("expected primary matting option");
    match selected.option.style {
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
  selection: random
  active:
    - kind: fixed-color
      colors:
        - [10, 20, 30]
        - [5, 15, 25]
      color-selection: random
    - kind: blur
      minimum-mat-percentage: 7.5
      sigma: 12.0
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    let MattingSelection::Random(entries) = cfg.matting.selection() else {
        panic!("expected random matting selection");
    };
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].index, 0);
    assert_eq!(entries[0].kind, MattingKind::FixedColor);
    assert_eq!(entries[1].index, 1);
    assert_eq!(entries[1].kind, MattingKind::FixedColor);
    assert_eq!(entries[2].index, 2);
    assert_eq!(entries[2].kind, MattingKind::Blur);
    let selected: Vec<_> = cfg.matting.iter_selected().collect();
    assert_eq!(selected.len(), 3);
    let fixed_first = &selected[0];
    if let rust_photo_frame::config::MattingMode::FixedColor {
        colors,
        color_selection,
    } = &fixed_first.option.style
    {
        assert_eq!(colors.as_slice(), &[[10, 20, 30]]);
        assert_eq!(*color_selection, ColorSelection::Random);
    } else {
        panic!("expected fixed-color matting");
    }
    let fixed_second = &selected[1];
    if let rust_photo_frame::config::MattingMode::FixedColor { colors, .. } =
        &fixed_second.option.style
    {
        assert_eq!(colors.as_slice(), &[[5, 15, 25]]);
    } else {
        panic!("expected second fixed-color matting");
    }
    let blur = &selected[2];
    if let rust_photo_frame::config::MattingMode::Blur { sigma, .. } = blur.option.style {
        assert!((sigma - 12.0).abs() < f32::EPSILON);
        assert!((blur.option.minimum_mat_percentage - 7.5).abs() < f32::EPSILON);
    } else {
        panic!("expected blur matting");
    }
}

#[test]
fn multiple_fixed_color_entries_preserve_all_options() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  selection: random
  active:
    - kind: fixed-color
      colors:
        - [1, 2, 3]
    - kind: fixed-color
      colors:
        - [4, 5, 6]
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    let selected: Vec<_> = cfg.matting.iter_selected().collect();
    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].entry.index, 0);
    assert_eq!(selected[0].entry.kind, MattingKind::FixedColor);
    assert_eq!(selected[1].entry.index, 1);
    assert_eq!(selected[1].entry.kind, MattingKind::FixedColor);
    let palettes: Vec<_> = selected
        .iter()
        .map(|selected| match &selected.option.style {
            MattingMode::FixedColor { colors, .. } => colors.clone(),
            other => panic!("expected fixed-color entry, got {other:?}"),
        })
        .collect();
    assert_eq!(palettes, vec![vec![[1, 2, 3]], vec![[4, 5, 6]]]);
}

#[test]
fn inline_fixed_color_array_expands_to_multiple_entries() {
    let yaml = r#"
photo-library-path: "/photos"
matting: { selection: random, active: [ { kind: fixed-color, colors: [[8, 16, 24], [32, 40, 48]], color-selection: random } ] }
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    let MattingSelection::Random(entries) = cfg.matting.selection() else {
        panic!("expected random matting selection");
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].index, 0);
    assert_eq!(entries[1].index, 1);
    let selected: Vec<_> = cfg.matting.iter_selected().collect();
    assert_eq!(selected.len(), 2);
    let first_colors = match &selected[0].option.style {
        MattingMode::FixedColor { colors, .. } => colors.clone(),
        other => panic!("expected fixed-color entry, got {other:?}"),
    };
    let second_colors = match &selected[1].option.style {
        MattingMode::FixedColor { colors, .. } => colors.clone(),
        other => panic!("expected fixed-color entry, got {other:?}"),
    };
    assert_eq!(first_colors, vec![[8, 16, 24]]);
    assert_eq!(second_colors, vec![[32, 40, 48]]);
}

#[test]
fn sequential_iteration_over_expanded_fixed_colors() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  selection: sequential
  active:
    - kind: fixed-color
      colors:
        - [10, 20, 30]
        - [40, 50, 60]
        - [70, 80, 90]
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    let MattingSelection::Sequential { entries, .. } = cfg.matting.selection() else {
        panic!("expected sequential matting selection");
    };
    assert_eq!(entries.len(), 3);
    let indices: Vec<_> = entries.iter().map(|entry| entry.index).collect();
    assert_eq!(indices, vec![0, 1, 2]);

    let mut rng = StdRng::seed_from_u64(42);
    let cycle: Vec<_> = (0..5)
        .map(|_| cfg.matting.select_active(&mut rng).entry.index)
        .collect();
    assert_eq!(cycle, vec![0, 1, 2, 0, 1]);
}

#[test]
fn parse_fixed_color_single_color_alias() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  selection: fixed
  active:
    - kind: fixed-color
      color: [17, 34, 51]
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    let selected = cfg
        .matting
        .primary_selected()
        .expect("expected fixed-color mat option");
    assert_eq!(selected.entry.kind, MattingKind::FixedColor);
    if let rust_photo_frame::config::MattingMode::FixedColor { colors, .. } = &selected.option.style
    {
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
  selection: sequential
  active:
    - kind: fixed-color
      colors:
        - [10, 20, 30]
        - [40, 50, 60]
      color-selection: sequential
    - kind: blur
      sigma: 12.0
      minimum-mat-percentage: 7.5
    - kind: fixed-color
      colors:
        - [10, 20, 30]
        - [40, 50, 60]
      color-selection: sequential
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    match cfg.matting.selection() {
        MattingSelection::Sequential { entries, .. } => {
            assert_eq!(entries.len(), 5);
            let kinds: Vec<_> = entries.iter().map(|entry| entry.kind).collect();
            assert_eq!(
                kinds,
                vec![
                    MattingKind::FixedColor,
                    MattingKind::FixedColor,
                    MattingKind::Blur,
                    MattingKind::FixedColor,
                    MattingKind::FixedColor,
                ]
            );
            let indexes: Vec<_> = entries.iter().map(|entry| entry.index).collect();
            assert_eq!(indexes, vec![0, 1, 2, 3, 4]);
        }
        other => panic!("expected sequential matting selection, got {other:?}"),
    }

    let mut rng = StdRng::seed_from_u64(1);
    let sequence: Vec<_> = (0..7)
        .map(|_| cfg.matting.select_active(&mut rng))
        .collect();

    let indices: Vec<_> = sequence
        .iter()
        .map(|selected| selected.entry.index)
        .collect();
    assert_eq!(indices, vec![0, 1, 2, 3, 4, 0, 1]);
    let kinds: Vec<_> = sequence
        .iter()
        .map(|selected| selected.entry.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![
            MattingKind::FixedColor,
            MattingKind::FixedColor,
            MattingKind::Blur,
            MattingKind::FixedColor,
            MattingKind::FixedColor,
            MattingKind::FixedColor,
            MattingKind::FixedColor,
        ]
    );
    if let rust_photo_frame::config::MattingMode::FixedColor {
        color_selection, ..
    } = &sequence[0].option.style
    {
        assert_eq!(*color_selection, ColorSelection::Sequential);
    } else {
        panic!("expected first matting option to be fixed-color");
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
  selection: sequential
  active:
    - kind: fixed-image
      path: ["{first}", "{second}"]
      path-selection: sequential
      fit: contain
"#,
        first = first.display(),
        second = second.display()
    );

    let cfg: Configuration = serde_yaml::from_str(&yaml).unwrap();
    let selected: Vec<_> = cfg.matting.iter_selected().collect();
    assert_eq!(selected.len(), 2);
    for (entry, path) in selected.iter().zip([&first, &second]) {
        match &entry.option.style {
            MattingMode::FixedImage {
                paths,
                path_selection,
                fit,
            } => {
                assert_eq!(paths, &vec![path.clone()]);
                assert_eq!(*path_selection, FixedImagePathSelection::Sequential);
                assert!(matches!(
                    fit,
                    rust_photo_frame::config::FixedImageFit::Contain
                ));
            }
            other => panic!("expected fixed-image matting, got {other:?}"),
        }
    }

    let prepared: Vec<_> = selected
        .iter()
        .map(|selected| {
            let mut option = selected.option.clone();
            option.prepare_runtime().unwrap();
            option
        })
        .collect();
    let bg0 = prepared[0]
        .runtime
        .fixed_image()
        .expect("expected first fixed background");
    let bg1 = prepared[1]
        .runtime
        .fixed_image()
        .expect("expected second fixed background");
    assert_eq!(bg0.path(), first.as_path());
    assert_eq!(bg1.path(), second.as_path());

    let mut rng = StdRng::seed_from_u64(1);
    let cycle: Vec<_> = (0..3)
        .map(|_| cfg.matting.select_active(&mut rng).entry.index)
        .collect();
    assert_eq!(cycle, vec![0, 1, 0]);
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
  selection: fixed
  active:
    - kind: fixed-image
      path: "{only}"
"#,
        only = only.display()
    );

    let cfg: Configuration = serde_yaml::from_str(&yaml).unwrap();
    let selected = cfg
        .matting
        .primary_selected()
        .expect("expected fixed-image matting");

    match &selected.option.style {
        MattingMode::FixedImage { paths, .. } => {
            assert_eq!(paths, &vec![only.clone()]);
        }
        other => panic!("expected fixed-image matting, got {other:?}"),
    }

    let mut mat = selected.option.clone();
    mat.prepare_runtime().unwrap();
    let bg = mat
        .runtime
        .fixed_image()
        .expect("expected fixed background");
    assert_eq!(bg.path(), only.as_path());
}

#[test]
fn fixed_image_with_empty_paths_is_disabled() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  selection: fixed
  active:
    - kind: fixed-image
      path: []
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    let mut mat = cfg
        .matting
        .primary_selected()
        .expect("expected fixed-image matting")
        .option
        .clone();

    mat.prepare_runtime().unwrap();
    assert!(mat.runtime.fixed_image().is_none());
}

#[test]
fn matting_entry_without_required_fields_is_rejected() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  selection: fixed
  active:
    - kind: fixed-image
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    assert!(
        err.to_string()
            .contains("matting.active entry for fixed-image must include a path")
    );
}

#[test]
fn matting_with_empty_active_is_rejected() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  selection: random
  active: []
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    assert!(
        err.to_string()
            .contains("matting configuration must include at least one active entry")
    );
}

#[test]
fn matting_fixed_selection_requires_single_entry() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  selection: fixed
  active:
    - kind: fixed-color
      color: [1, 2, 3]
    - kind: blur
      sigma: 4.0
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    assert!(
        err.to_string()
            .contains("matting selection 'fixed' requires exactly one active entry")
    );
}

#[test]
fn matting_type_field_is_rejected() {
    let yaml = r#"
photo-library-path: "/photos"
matting:
  selection: random
  type: random
  active:
    - kind: fixed-color
      colors: [[5, 15, 25]]
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    let err_msg = err.to_string();
    assert!(err_msg.contains("unknown field `type`"));
    assert!(err_msg.contains("selection"));
    assert!(err_msg.contains("active"));
}

#[test]
fn parse_inline_fade_transition() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  selection: fixed
  active:
    - kind: fade
      duration-ms: 750
      through-black: true
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    assert!(matches!(
        cfg.transition.selection(),
        TransitionSelection::Fixed(entry)
            if entry.kind == TransitionKind::Fade && entry.index == 0
    ));
    let selected = cfg
        .transition
        .primary_selected()
        .expect("expected fade transition option");
    assert_eq!(selected.entry.kind, TransitionKind::Fade);
    assert_eq!(selected.entry.index, 0);
    assert_eq!(selected.option.duration().as_millis(), 750);
    match selected.option.mode() {
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
  selection: fixed
  active:
    - kind: iris
      duration-ms: 880
      blades: 9
      rotate-radians: 1.25
      direction: close
      fill-rgba: [0.75, 0.8, 0.85, 1.0]
      stroke-rgba: [0.2, 0.25, 0.3, 1.0]
      stroke-width: 2.0
      tolerance: 0.2
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    assert!(matches!(
        cfg.transition.selection(),
        TransitionSelection::Fixed(entry)
            if entry.kind == TransitionKind::Iris && entry.index == 0
    ));
    let selected = cfg
        .transition
        .primary_selected()
        .expect("expected iris transition option");
    assert_eq!(selected.entry.kind, TransitionKind::Iris);
    assert_eq!(selected.entry.index, 0);
    assert_eq!(selected.option.duration().as_millis(), 880);
    match selected.option.mode() {
        rust_photo_frame::config::TransitionMode::Iris(cfg) => {
            assert_eq!(cfg.blades, 9);
            assert!((cfg.rotate_radians - 1.25).abs() < f32::EPSILON);
            assert_eq!(
                cfg.direction,
                rust_photo_frame::config::IrisDirection::Close
            );
            assert_eq!(cfg.fill_rgba, [0.75, 0.8, 0.85, 1.0]);
            assert_eq!(cfg.stroke_rgba, [0.2, 0.25, 0.3, 1.0]);
            assert!((cfg.stroke_width - 2.0).abs() < f32::EPSILON);
            assert!((cfg.tolerance - 0.2).abs() < f32::EPSILON);
        }
        _ => panic!("expected iris transition"),
    }
}

#[test]
fn parse_random_transition_configuration() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  selection: random
  active:
    - kind: fade
      duration-ms: 450
    - kind: wipe
      duration-ms: 600
      angle-list-degrees: [90.0]
      softness: 0.1
    - kind: push
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
    let selected: Vec<_> = cfg.transition.iter_selected().collect();
    assert_eq!(selected.len(), 3);
    let wipe = &selected[1];
    assert_eq!(wipe.option.duration().as_millis(), 600);
    match wipe.option.mode() {
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

    let push = &selected[2];
    match push.option.mode() {
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
  selection: sequential
  active:
    - kind: push
      duration-ms: 640
      angle-list-degrees: [0.0, 180.0]
      angle-selection: sequential
    - kind: wipe
      duration-ms: 520
      angle-list-degrees: [90.0]
    - kind: push
      duration-ms: 640
      angle-list-degrees: [0.0, 180.0]
      angle-selection: sequential
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    match cfg.transition.selection() {
        TransitionSelection::Sequential { entries, .. } => {
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].index, 0);
            assert_eq!(entries[0].kind, TransitionKind::Push);
            assert_eq!(entries[1].index, 1);
            assert_eq!(entries[1].kind, TransitionKind::Wipe);
            assert_eq!(entries[2].index, 2);
            assert_eq!(entries[2].kind, TransitionKind::Push);
        }
        other => panic!("expected sequential transition selection, got {other:?}"),
    }

    let mut rng = StdRng::seed_from_u64(42);
    let first = cfg.transition.select_active(&mut rng);
    let second = cfg.transition.select_active(&mut rng);
    let third = cfg.transition.select_active(&mut rng);
    let fourth = cfg.transition.select_active(&mut rng);

    assert_eq!(first.entry.index, 0);
    assert_eq!(first.entry.kind, TransitionKind::Push);
    match first.option.mode() {
        rust_photo_frame::config::TransitionMode::Push(_) => {}
        _ => panic!("expected first transition to be push"),
    }
    assert_eq!(second.entry.index, 1);
    assert_eq!(second.entry.kind, TransitionKind::Wipe);
    match second.option.mode() {
        rust_photo_frame::config::TransitionMode::Wipe(_) => {}
        _ => panic!("expected second transition to be wipe"),
    }
    assert_eq!(third.entry.index, 2);
    assert_eq!(third.entry.kind, TransitionKind::Push);
    match third.option.mode() {
        rust_photo_frame::config::TransitionMode::Push(_) => {}
        _ => panic!("expected third transition to return to push"),
    }
    assert_eq!(fourth.entry.index, 0);
    assert_eq!(fourth.entry.kind, TransitionKind::Push);
}

#[test]
fn transition_with_empty_active_is_rejected() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  selection: random
  active: []
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    assert!(
        err.to_string()
            .contains("transition configuration must include at least one active entry")
    );
}

#[test]
fn transition_fixed_selection_requires_single_entry() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  selection: fixed
  active:
    - kind: fade
      duration-ms: 250
    - kind: wipe
      duration-ms: 400
      angle-list-degrees: [0.0]
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    assert!(
        err.to_string()
            .contains("transition selection 'fixed' requires exactly one active entry")
    );
}

#[test]
fn transition_type_field_is_rejected() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  selection: random
  type: random
  active:
    - kind: fade
      duration-ms: 520
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    let err_msg = err.to_string();
    assert!(err_msg.contains("unknown field `type`"));
    assert!(err_msg.contains("selection"));
    assert!(err_msg.contains("active"));
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
  selection: fixed
  active:
    - kind: wipe
      angle-jitter-degrees: -15.0
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    assert!(
        err.to_string()
            .contains("requires angle-jitter-degrees >= 0")
    );
}

#[test]
fn wipe_transition_requires_angles() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  selection: fixed
  active:
    - kind: wipe
      angle-list-degrees: []
"#;

    let err = serde_yaml::from_str::<Configuration>(yaml).unwrap_err();
    assert!(err.to_string().contains(
        "transition option wipe requires angle-list-degrees to include at least one entry"
    ));
}

#[test]
fn push_transition_rejects_negative_jitter() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  selection: fixed
  active:
    - kind: push
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
  selection: fixed
  active:
    - kind: iris
      blades: 0
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    let selected = cfg
        .transition
        .primary_selected()
        .expect("expected iris transition option");
    assert_eq!(selected.entry.kind, TransitionKind::Iris);
    match selected.option.mode() {
        rust_photo_frame::config::TransitionMode::Iris(cfg) => {
            assert_eq!(cfg.blades, 3);
        }
        _ => panic!("expected iris transition"),
    }
}

#[test]
fn push_transition_configures_multiple_angles() {
    let yaml = r#"
photo-library-path: "/photos"
transition:
  selection: fixed
  active:
    - kind: push
      duration-ms: 725
      angle-list-degrees: [90.0, 270.0]
      angle-selection: sequential
"#;

    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    let selected = cfg
        .transition
        .primary_selected()
        .expect("expected push transition option");
    assert_eq!(selected.entry.kind, TransitionKind::Push);
    assert_eq!(selected.option.duration().as_millis(), 725);
    match selected.option.mode() {
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
