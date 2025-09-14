use crate::config::Configuration;
use crate::events::{InvalidPhoto, InventoryEvent};
use anyhow::Result;
use notify::event::{CreateKind, ModifyKind, RemoveKind};
use notify::{recommended_watcher, Event, EventKind, RecursiveMode, Watcher};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use tokio::select;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio_util::sync::CancellationToken;
use tracing::instrument;
use tracing::{debug, error, info};
use walkdir::WalkDir;

#[instrument(
    skip(to_manager, invalid_rx, cancel),
    fields(root = %cfg.photo_library_path.display())
)]
pub async fn run(
    cfg: Configuration,
    to_manager: Sender<InventoryEvent>,
    mut invalid_rx: Receiver<InvalidPhoto>,
    cancel: CancellationToken,
) -> Result<()> {
    // 1) Startup scan (recursive)
    let mut discovered = 0usize;
    for entry in WalkDir::new(&cfg.photo_library_path)
        .follow_links(true)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path().to_path_buf();
        if is_image(&path) {
            discovered += 1;
            debug!(action = "startup_add", path = %path.display());
            let _ = to_manager.send(InventoryEvent::PhotoAdded(path)).await;
        }
    }
    info!(discovered, "startup recursive scan complete");

    // 2) Bridge notify callback -> async channel
    let (watch_tx, mut watch_rx) = mpsc::channel::<notify::Result<Event>>(128);
    let mut _watcher = recommended_watcher(move |res| {
        let _ = watch_tx.blocking_send(res);
    })?;

    // Try to log the canonical path we ended up watching (helps spot symlink issues).
    match cfg.photo_library_path.canonicalize() {
        Ok(abs) => info!(watching = %abs.display(), "notify watcher initialized (recursive)"),
        Err(_) => {
            info!(watching = %cfg.photo_library_path.display(), "notify watcher initialized (recursive)")
        }
    }
    _watcher.watch(&cfg.photo_library_path, RecursiveMode::Recursive)?;

    // 3) Event loop
    loop {
        select! {
                    _ = cancel.cancelled() => {
                        info!("cancel received; exiting files task");
                        break;
                    }

                    // From Manager/Loader: quarantine then tell Manager it disappeared.
                    Some(InvalidPhoto(path)) = invalid_rx.recv() => {
                        info!(path = %path.display(), "quarantining invalid photo");
                        quarantine(&cfg, &path)?;
                        let _ = to_manager.send(InventoryEvent::PhotoRemoved(path)).await;
                    }

                    // Filesystem notifications -> InventoryEvent via compact match
                    Some(res) = watch_rx.recv() => match res {
            Ok(event) => {
                debug!(kind = ?event.kind, paths = ?event.paths, "notify event");

                match &event.kind {
                    EventKind::Create(CreateKind::File) => {
                        for p in event.paths.into_iter().filter(|p| is_image(p.as_path())) {
                            info!(path = %p.display(), "fs: add (create)");
                            let _ = to_manager.send(InventoryEvent::PhotoAdded(p)).await;
                        }
                    }
                    EventKind::Remove(RemoveKind::File) => {
                        for p in event.paths.into_iter().filter(|p| is_image(p.as_path())) {
                            info!(path = %p.display(), "fs: remove (remove)");
                            let _ = to_manager.send(InventoryEvent::PhotoRemoved(p)).await;
                        }
                    }
                    EventKind::Modify(ModifyKind::Name(_mode)) => {
                        // macOS often reports moves as Name(Any). Decide per-path by existence.
                        for p in event.paths.into_iter().filter(|p| is_image(p.as_path())) {
                            let exists = p.exists();
                            if exists {
                                info!(path = %p.display(), "fs: add (rename/name)");
                                let _ = to_manager.send(InventoryEvent::PhotoAdded(p)).await;
                            } else {
                                info!(path = %p.display(), "fs: remove (rename/name)");
                                let _ = to_manager.send(InventoryEvent::PhotoRemoved(p)).await;
                            }
                        }
                    }
                    _ => {
                        debug!(kind = ?event.kind, "fs: ignored");
                    }
                }
            }
            Err(err) => error!("watch error: {err}"),
        }
                }
    }
    Ok(())
}

#[inline]
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
        let ext = p.extension().and_then(OsStr::to_str).unwrap_or("");
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
