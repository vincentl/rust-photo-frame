use crate::config::Configuration;
use crate::events::{CreatedSource, InvalidPhoto, InventoryEvent, PhotoInfo};
use anyhow::Result;
use notify::event::{ModifyKind, RemoveKind};
use notify::{Event, EventKind, RecursiveMode, Watcher, recommended_watcher};
use rand::{SeedableRng, seq::SliceRandom};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio_util::sync::CancellationToken;
use tracing::instrument;
use tracing::{debug, error, info, warn};

/// Image file extensions recognised by the scanner (lowercase, without leading dot).
const SUPPORTED_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];
use walkdir::WalkDir;

#[instrument(
    skip(to_manager, invalid_rx, cancel),
    fields(root = %cfg.photo_library_path.display())
)]
pub async fn run(
    cfg: Arc<Configuration>,
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

            // From Manager/Loader: a photo failed to decode. Do NOT delete it.
            // A decode failure can be transient (the file is still being copied
            // in by the sync job, or a momentary read error), and destroying a
            // user's photo is never acceptable. Drop it from the current rotation
            // only; it is retried on the next startup scan or re-add event.
            Some(InvalidPhoto(path)) = invalid_rx.recv() => {
                warn!(path = %path.display(), "photo failed to decode; skipping (left on disk)");
                let _ = to_manager.send(InventoryEvent::PhotoRemoved(path)).await;
            }

            // Filesystem notifications -> InventoryEvent
            Some(res) = watch_rx.recv() => match res {
                Ok(event) => {
                    debug!(kind = ?event.kind, paths = ?event.paths, "notify event");
                    match &event.kind {
                        EventKind::Create(_) => {
                            // A create may be a file or a whole directory (e.g. a
                            // pushed album). `add_path` recurses into directories,
                            // which also closes the race where files land before the
                            // recursive watch attaches to the new subdir.
                            for p in event.paths {
                                add_path(&p, &to_manager).await;
                            }
                        }
                        EventKind::Remove(RemoveKind::File) => {
                            for p in event.paths.into_iter().filter(|p| is_image(p.as_path())) {
                                debug!(path = %p.display(), "fs: remove (remove)");
                                let _ = to_manager.send(InventoryEvent::PhotoRemoved(p)).await;
                            }
                        }
                        EventKind::Modify(ModifyKind::Name(_)) => {
                            // macOS often reports moves as Name(Any); a directory may
                            // also be renamed/moved into the library wholesale. Decide
                            // per-path by existence, recursing into moved-in dirs.
                            for p in event.paths {
                                if p.exists() {
                                    add_path(&p, &to_manager).await;
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
    p.extension()
        .and_then(OsStr::to_str)
        .map(|s| s.to_ascii_lowercase())
        .is_some_and(|ext| SUPPORTED_EXTENSIONS.contains(&ext.as_str()))
}

/// Emit `PhotoAdded` for `path` if it is a supported image, or for every image
/// found by recursing if it is a directory (e.g. a whole album pushed or moved
/// into the library at once — inotify reports only the top-level create, and the
/// recursive watch does not replay files already inside it). Idempotent: the
/// manager treats an already-known path as a metadata refresh, so overlap with
/// later per-file watch events is harmless.
async fn add_path(path: &Path, to_manager: &Sender<InventoryEvent>) {
    if path.is_dir() {
        debug!(path = %path.display(), "fs: add (dir, recursing)");
        for entry in WalkDir::new(path).into_iter().filter_map(Result::ok) {
            let p = entry.path();
            if p.is_file() && is_image(p) {
                send_added(p, to_manager).await;
            }
        }
    } else if is_image(path) {
        debug!(path = %path.display(), "fs: add (file)");
        send_added(path, to_manager).await;
    }
}

async fn send_added(path: &Path, to_manager: &Sender<InventoryEvent>) {
    let (created_at, created_source) = photo_created_at(path);
    let info = PhotoInfo {
        path: path.to_path_buf(),
        created_at,
        created_source,
        // Discovered at runtime → eligible for the priority FIFO.
        runtime_added: true,
    };
    let _ = to_manager.send(InventoryEvent::PhotoAdded(info)).await;
}

/// Age a photo by when its file was staged to the frame. Prefer the filesystem
/// birth time (`st_birthtime`); fall back to mtime if the filesystem has no
/// birth time, then to now if metadata can't be read at all. EXIF capture dates
/// are deliberately ignored — staging time is the intent. The chosen source is
/// returned so `metrics` logging can confirm the frame is actually getting birth
/// times rather than silently falling back.
fn photo_created_at(path: &Path) -> (SystemTime, CreatedSource) {
    match fs::metadata(path) {
        Ok(meta) => {
            if let Ok(t) = meta.created() {
                (t, CreatedSource::Birthtime)
            } else if let Ok(t) = meta.modified() {
                (t, CreatedSource::Mtime)
            } else {
                (SystemTime::now(), CreatedSource::Now)
            }
        }
        Err(_) => (SystemTime::now(), CreatedSource::Now),
    }
}

pub fn discover_startup_photos(cfg: &Configuration) -> Result<Vec<PhotoInfo>> {
    let mut initial = Vec::<PathBuf>::new();
    // follow_links(true) is intentional so symlinked sub-directories work. WalkDir's internal
    // inode tracker prevents infinite loops from circular symlinks.
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
            let (created_at, created_source) = photo_created_at(&path);
            PhotoInfo {
                path,
                created_at,
                created_source,
                // Startup scan → schedule straight onto the timeline, not the FIFO.
                runtime_added: false,
            }
        })
        .collect())
}
