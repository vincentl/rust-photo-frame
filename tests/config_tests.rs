use rust_photo_frame::config::{BlurBackend, Configuration, MattingMode};
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
fn parse_snake_case_aliases() {
    let yaml = r#"
photo_library_path: "/p"
"#;
    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(cfg.photo_library_path, PathBuf::from("/p"));
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
fn parse_blur_backend() {
    let yaml = r#"
photo-library-path: "/p"
matting:
  type: blur
  backend: wgpu-compute
"#;
    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    match cfg.matting.style {
        MattingMode::Blur { backend, .. } => {
            assert!(matches!(backend, BlurBackend::WgpuCompute));
        }
        _ => panic!("expected blur matting"),
    }
}
