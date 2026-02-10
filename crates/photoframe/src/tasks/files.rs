use crate::config::Configuration;
use crate::events::{InvalidPhoto, InventoryEvent, PhotoInfo};
use anyhow::Result;
use notify::event::{CreateKind, ModifyKind, RemoveKind};
use notify::{Event, EventKind, RecursiveMode, Watcher, recommended_watcher};
use rand::{SeedableRng, seq::SliceRandom};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
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
    // 1) Startup scan (recursive) -> collect, shuffle, emit
    let initial = discover_startup_photos(&cfg)?;
    for info in &initial {
        debug!(action = "startup_add", path = %info.path.display());
        let _ = to_manager
            .send(InventoryEvent::PhotoAdded(info.clone()))
            .await;
    }
    info!(
        discovered = initial.len(),
        "startup recursive scan complete (shuffled)"
    );

    // 2) Bridge notify callback -> async channel
    let (watch_tx, mut watch_rx) = mpsc::channel::<notify::Result<Event>>(128);
    let mut _watcher = recommended_watcher(move |res| {
        let _ = watch_tx.blocking_send(res);
    })?;

    // Log what we’re watching
    match cfg.photo_library_path.canonicalize() {
        Ok(abs) => info!(watching = %abs.display(), "notify watcher initialized (recursive)"),
        Err(_) => {
            info!(watching = %cfg.photo_library_path.display(), "notify watcher initialized (recursive)")
        }
    }
    _watcher.watch(&cfg.photo_library_path, RecursiveMode::Recursive)?;

    // 3) Event loop
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("cancel received; exiting files task");
                break;
            }

            // From Manager/Loader: delete the bad file, then tell Manager it disappeared.
            Some(InvalidPhoto(path)) = invalid_rx.recv() => {
                info!(path = %path.display(), "deleting invalid photo");
                delete_if_exists(&path)?;
                let _ = to_manager.send(InventoryEvent::PhotoRemoved(path)).await;
            }

            // Filesystem notifications -> InventoryEvent
            Some(res) = watch_rx.recv() => match res {
                Ok(event) => {
                    debug!(kind = ?event.kind, paths = ?event.paths, "notify event");
                    match &event.kind {
                        EventKind::Create(CreateKind::File) => {
                            for p in event.paths.into_iter().filter(|p| is_image(p.as_path())) {
                                debug!(path = %p.display(), "fs: add (create)");
                                let created_at = photo_created_at(&p);
                                let info = PhotoInfo { path: p.clone(), created_at };
                                let _ = to_manager.send(InventoryEvent::PhotoAdded(info)).await;
                            }
                        }
                        EventKind::Remove(RemoveKind::File) => {
                            for p in event.paths.into_iter().filter(|p| is_image(p.as_path())) {
                                debug!(path = %p.display(), "fs: remove (remove)");
                                let _ = to_manager.send(InventoryEvent::PhotoRemoved(p)).await;
                            }
                        }
                        EventKind::Modify(ModifyKind::Name(_)) => {
                            // macOS often reports moves as Name(Any). Decide per-path by existence.
                            for p in event.paths.into_iter().filter(|p| is_image(p.as_path())) {
                                if p.exists() {
                                    debug!(path = %p.display(), "fs: add (rename/name)");
                                    let created_at = photo_created_at(&p);
                                    let info = PhotoInfo { path: p.clone(), created_at };
                                    let _ = to_manager.send(InventoryEvent::PhotoAdded(info)).await;
                                } else {
                                    debug!(path = %p.display(), "fs: remove (rename/name)");
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

fn delete_if_exists(p: &Path) -> Result<()> {
    if !p.exists() {
        debug!(path = %p.display(), "delete: source missing; skipping");
        return Ok(());
    }
    debug!(path = %p.display(), "delete: removing file");
    match fs::remove_file(p) {
        Ok(_) => {
            info!(path = %p.display(), "delete: removed");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            debug!(path = %p.display(), "delete: source vanished during remove; skipping");
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

fn photo_created_at(path: &Path) -> SystemTime {
    match fs::metadata(path) {
        Ok(meta) => meta
            .created()
            .or_else(|_| meta.modified())
            .unwrap_or_else(|_| SystemTime::now()),
        Err(_) => SystemTime::now(),
    }
}

pub fn discover_startup_photos(cfg: &Configuration) -> Result<Vec<PhotoInfo>> {
    let mut initial = Vec::<PathBuf>::new();
    for entry in WalkDir::new(&cfg.photo_library_path)
        .follow_links(true)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path().to_path_buf();
        if is_image(&path) {
            initial.push(path);
        }
    }

    let mut rng = match cfg.startup_shuffle_seed {
        Some(seed) => rand::rngs::StdRng::seed_from_u64(seed),
        None => rand::rngs::StdRng::from_os_rng(),
    };
    initial.shuffle(&mut rng);

    Ok(initial
        .into_iter()
        .map(|path| {
            let created_at = photo_created_at(&path);
            PhotoInfo { path, created_at }
        })
        .collect())
}
