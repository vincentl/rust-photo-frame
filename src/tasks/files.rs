use crate::config::Configuration;
use crate::events::{InventoryEvent, InvalidPhoto};
use anyhow::Result;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;
use walkdir::WalkDir;

pub async fn run(
    cfg: Configuration,
    to_manager: Sender<InventoryEvent>,
    mut invalid_rx: Receiver<InvalidPhoto>,
    cancel: CancellationToken,
) -> Result<()> {
    // Startup scan (recursive)
    for entry in WalkDir::new(&cfg.photo_library_path)
        .follow_links(true)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path().to_path_buf();
        if is_image(&path) {
            let _ = to_manager.send(InventoryEvent::PhotoAdded(path)).await;
        }
    }

    loop {
        select! {
            _ = cancel.cancelled() => break,
            Some(InvalidPhoto(path)) = invalid_rx.recv() => {
                quarantine(&cfg, &path)?;
                let _ = to_manager.send(InventoryEvent::PhotoRemoved(path)).await;
            }
        }
    }
    Ok(())
}

fn is_image(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(OsStr::to_str)
            .map(|s| s.to_ascii_lowercase()),
        Some(ref e) if ["jpg","jpeg","png","webp"].contains(&e.as_str())
    )
}

fn quarantine(cfg: &Configuration, p: &Path) -> Result<()> {
    fs::create_dir_all(&cfg.photo_quarantine_path)?;

    let file = p.file_name().unwrap_or_default();
    let mut dst = PathBuf::from(&cfg.photo_quarantine_path);
    dst.push(file);

    if dst.exists() {
        let stem = p.file_stem().and_then(OsStr::to_str).unwrap_or("file");
        let ext  = p.extension().and_then(OsStr::to_str).unwrap_or("");
        let mut n = 1u32;
        loop {
            let candidate = if ext.is_empty() {
                format!("{}_{}", stem, n)
            } else {
                format!("{}_{}.{}", stem, n, ext)
            };
            let mut alt = PathBuf::from(&cfg.photo_quarantine_path);
            alt.push(candidate);
            if !alt.exists() {
                dst = alt;
                break;
            }
            n += 1;
        }
    }

    fs::rename(p, &dst)?;
    Ok(())
}
