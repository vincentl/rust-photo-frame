use crate::config::Configuration;
use crate::events::{CreatedSource, InvalidPhoto, InventoryEvent, MetaSource, PhotoInfo};
use crate::processing::header::read_display_size;
use anyhow::Result;
use notify::event::{ModifyKind, RemoveKind};
use notify::{Event, EventKind, RecursiveMode, Watcher, recommended_watcher};
use rand::{SeedableRng, seq::SliceRandom};
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio_util::sync::CancellationToken;
use tracing::instrument;
use tracing::{debug, error, info, warn};

/// Image file extensions recognised by the scanner (lowercase, without leading dot).
const SUPPORTED_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];
use walkdir::WalkDir;

#[instrument(
    skip(cfg, to_manager, invalid_rx, cancel),
    fields(root = %cfg.photo_library_path.display())
)]
pub async fn run(
    cfg: Arc<Configuration>,
    to_manager: Sender<InventoryEvent>,
    mut invalid_rx: Receiver<InvalidPhoto>,
    cancel: CancellationToken,
) -> Result<()> {
    let library = LibraryRoot::new(&cfg.photo_library_path);

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
    // Portrait pairing needs each photo's displayed size before it is
    // planned. `PhotoAdded` went out first so the first photo is not
    // delayed; the headers are read afterwards, off the async runtime.
    let read_headers = cfg.playlist.pairing.enabled;
    if read_headers {
        let paths: Vec<PathBuf> = initial.iter().map(|info| info.path.clone()).collect();
        let to_manager = to_manager.clone();
        tokio::task::spawn_blocking(move || header_pass(paths, &to_manager));
    }

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
                            for p in event.paths.into_iter().filter(|p| !library.hides(p)) {
                                add_path(&p, &to_manager, read_headers).await;
                            }
                        }
                        EventKind::Remove(RemoveKind::File) => {
                            for p in event
                                .paths
                                .into_iter()
                                .filter(|p| is_image(p.as_path()) && !library.hides(p))
                            {
                                debug!(path = %p.display(), "fs: remove (remove)");
                                let _ = to_manager.send(InventoryEvent::PhotoRemoved(p)).await;
                            }
                        }
                        EventKind::Modify(ModifyKind::Name(_)) => {
                            // macOS often reports moves as Name(Any); a directory may
                            // also be renamed/moved into the library wholesale. Decide
                            // per-path by existence, recursing into moved-in dirs.
                            // A hidden path was never in inventory (a photo moved
                            // into `.trash/` surfaces as a removal of its old,
                            // visible path), so it is skipped either way.
                            for p in event.paths.into_iter().filter(|p| !library.hides(p)) {
                                if p.exists() {
                                    add_path(&p, &to_manager, read_headers).await;
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

/// A dot-name (`.cloud-staging`, `.Trash`, `._IMG_1.jpg`): hidden by Unix
/// convention, and never part of the library.
#[inline]
fn is_dot_name(name: &OsStr) -> bool {
    name.as_encoded_bytes().first() == Some(&b'.')
}

/// Walk `dir`, pruning hidden entries below it (`dir` itself is the caller's
/// to judge, so a library that lives under a dot-directory such as
/// `~/.local/share/photos` still scans). Pruning a hidden directory skips its
/// whole subtree without descending into it.
fn walk_visible(dir: &Path, follow_links: bool) -> impl Iterator<Item = walkdir::DirEntry> {
    WalkDir::new(dir)
        .follow_links(follow_links)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !is_dot_name(e.file_name()))
        .filter_map(Result::ok)
}

/// The library root as configured and as canonicalized, so watcher paths can
/// be judged relative to it whichever form the platform reports (inotify
/// echoes the watched path; macOS FSEvents reports the canonical one).
struct LibraryRoot {
    given: PathBuf,
    canonical: Option<PathBuf>,
}

impl LibraryRoot {
    fn new(root: &Path) -> Self {
        Self {
            given: root.to_path_buf(),
            canonical: root.canonicalize().ok(),
        }
    }

    /// Whether `path` is hidden inside the library: any component below the
    /// root is a dot-name. This keeps the sync service's `.cloud-staging/`
    /// (hard-linked copies of every `cloud/` photo) and transfer temp files out
    /// of the inventory. A path outside the root is judged by its file name.
    fn hides(&self, path: &Path) -> bool {
        let rel = path
            .strip_prefix(&self.given)
            .ok()
            .or_else(|| path.strip_prefix(self.canonical.as_deref()?).ok());
        match rel {
            Some(rel) => rel.components().any(|c| match c {
                Component::Normal(name) => is_dot_name(name),
                _ => false,
            }),
            None => path.file_name().is_some_and(is_dot_name),
        }
    }
}

/// Emit `PhotoAdded` for `path` if it is a supported image, or for every image
/// found by recursing if it is a directory (e.g. a whole album pushed or moved
/// into the library at once — inotify reports only the top-level create, and the
/// recursive watch does not replay files already inside it). Idempotent: the
/// manager treats an already-known path as a metadata refresh, so overlap with
/// later per-file watch events is harmless.
async fn add_path(path: &Path, to_manager: &Sender<InventoryEvent>, read_headers: bool) {
    if path.is_dir() {
        debug!(path = %path.display(), "fs: add (dir, recursing)");
        for entry in walk_visible(path, false) {
            let p = entry.path();
            if p.is_file() && is_image(p) {
                send_added(p, to_manager, read_headers).await;
            }
        }
    } else if is_image(path) {
        debug!(path = %path.display(), "fs: add (file)");
        send_added(path, to_manager, read_headers).await;
    }
}

/// `read_headers` (portrait pairing enabled): the displayed size is read
/// inline from the header, so a runtime-added photo is pairable from its
/// first appearance. A `PhotoAdded` for a path the manager already knows (a
/// rename or replace) overwrites its size when this read succeeds.
async fn send_added(path: &Path, to_manager: &Sender<InventoryEvent>, read_headers: bool) {
    let (created_at, created_source) = photo_created_at(path);
    let info = PhotoInfo {
        path: path.to_path_buf(),
        created_at,
        created_source,
        // Discovered at runtime → eligible for the priority FIFO.
        runtime_added: true,
        display_size: if read_headers {
            // A header read is small but blocking; keep it off the runtime.
            let owned = path.to_path_buf();
            tokio::task::spawn_blocking(move || read_display_size(&owned))
                .await
                .ok()
                .flatten()
        } else {
            None
        },
    };
    let _ = to_manager.send(InventoryEvent::PhotoAdded(info)).await;
}

/// The startup header pass (portrait pairing): read every startup photo's
/// displayed size and report it as `PhotoMeta { source: Header }`, which
/// only fills sizes still unknown. Runs on a blocking thread; logs its cost
/// once as `pairing_header_pass` (acceptance: median <= 2 ms per photo on
/// the Pi).
fn header_pass(paths: Vec<PathBuf>, to_manager: &Sender<InventoryEvent>) {
    let started = Instant::now();
    let mut per_photo_us: Vec<u128> = Vec::with_capacity(paths.len());
    let mut read = 0usize;
    for path in paths {
        let t0 = Instant::now();
        let size = read_display_size(&path);
        per_photo_us.push(t0.elapsed().as_micros());
        if let Some(display_size) = size {
            read += 1;
            let event = InventoryEvent::PhotoMeta {
                path,
                display_size,
                source: MetaSource::Header,
            };
            if to_manager.blocking_send(event).is_err() {
                return;
            }
        }
    }
    per_photo_us.sort_unstable();
    let median_us = per_photo_us
        .get(per_photo_us.len() / 2)
        .copied()
        .unwrap_or(0);
    info!(
        photos = per_photo_us.len(),
        read,
        median_ms = format_args!("{:.3}", median_us as f64 / 1000.0),
        total_ms = started.elapsed().as_millis(),
        "pairing_header_pass"
    );
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
    // inode tracker prevents infinite loops from circular symlinks. Hidden entries are pruned
    // (see `LibraryRoot::hides`).
    for entry in walk_visible(&cfg.photo_library_path, true).filter(|e| e.file_type().is_file()) {
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
                // Read by the header pass after the adds (or inline by the
                // dry run), only when pairing is enabled.
                display_size: None,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_dot_components_below_the_root_only() {
        let lib = LibraryRoot {
            given: PathBuf::from("/var/lib/photoframe/photos"),
            canonical: None,
        };
        let hidden = [
            "/var/lib/photoframe/photos/.cloud-staging",
            "/var/lib/photoframe/photos/.cloud-staging/IMG_1.jpeg",
            "/var/lib/photoframe/photos/.cloud-staging/2024/IMG_2.jpeg",
            "/var/lib/photoframe/photos/local/.Trash/IMG_3.jpg",
            "/var/lib/photoframe/photos/local/._IMG_4.jpg",
            "/var/lib/photoframe/photos/cloud/.IMG_5.jpeg.a1B2c3",
        ];
        for p in hidden {
            assert!(lib.hides(Path::new(p)), "{p} should be hidden");
        }
        let visible = [
            "/var/lib/photoframe/photos/cloud/IMG_1.jpeg",
            "/var/lib/photoframe/photos/local/2008/DSC_0074 (1).jpeg",
            "/var/lib/photoframe/photos/local/v1.2.jpg",
            "/var/lib/photoframe/photos",
        ];
        for p in visible {
            assert!(!lib.hides(Path::new(p)), "{p} should be visible");
        }
    }

    #[test]
    fn a_library_under_a_dot_directory_is_not_itself_hidden() {
        let lib = LibraryRoot {
            given: PathBuf::from("/home/kiosk/.local/share/photos"),
            canonical: None,
        };
        assert!(!lib.hides(Path::new("/home/kiosk/.local/share/photos/a.jpg")));
        assert!(lib.hides(Path::new("/home/kiosk/.local/share/photos/.staging/a.jpg")));
    }

    #[test]
    fn watcher_paths_in_canonical_form_are_judged_relative_to_the_root() {
        // macOS FSEvents reports /private/var/... for a root configured as /var/...;
        // `.local` is above the canonical root and must not count.
        let lib = LibraryRoot {
            given: PathBuf::from("/var/photos"),
            canonical: Some(PathBuf::from("/private/.local/photos")),
        };
        assert!(!lib.hides(Path::new("/private/.local/photos/cloud/a.jpg")));
        assert!(lib.hides(Path::new("/private/.local/photos/.cloud-staging/a.jpg")));
    }

    #[test]
    fn a_path_outside_the_root_is_judged_by_its_file_name() {
        let lib = LibraryRoot {
            given: PathBuf::from("/var/photos"),
            canonical: None,
        };
        assert!(lib.hides(Path::new("/elsewhere/._a.jpg")));
        assert!(!lib.hides(Path::new("/elsewhere/.x/a.jpg")));
    }
}
