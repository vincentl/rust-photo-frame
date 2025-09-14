use rust_photo_frame::config::Configuration;
use std::path::PathBuf;

#[test]
fn parse_kebab_case_config() {
    let yaml = r#"
photo-library-path: "/photos"
photo-quarantine-path: "/quarantine"
"#;
    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(cfg.photo_library_path, PathBuf::from("/photos"));
    assert_eq!(cfg.photo_quarantine_path, PathBuf::from("/quarantine"));
}

#[test]
fn parse_snake_case_aliases() {
    let yaml = r#"
photo_library_path: "/p"
photo_quarantine_path: "/q"
"#;
    let cfg: Configuration = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(cfg.photo_library_path, PathBuf::from("/p"));
    assert_eq!(cfg.photo_quarantine_path, PathBuf::from("/q"));
}
