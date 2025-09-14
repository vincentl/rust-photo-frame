use crate::config::Configuration;
use crate::events::{InvalidPhoto, InventoryEvent};
use anyhow::Result;
use notify::event::{CreateKind, ModifyKind, RemoveKind, RenameMode};
use notify::{recommended_watcher, Event, EventKind, RecursiveMode, Watcher};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio_util::sync::CancellationToken;
use walkdir::WalkDir;

pub async fn run(
    cfg: Configuration,
    to_manager: Sender<InventoryEvent>,
    mut invalid_rx: Receiver<InvalidPhoto>,
    cancel: CancellationToken,
) -> Result<()> {
    // 1) Startup scan (recursive)
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

    // 2) Bridge notify callback -> async channel
    let (watch_tx, mut watch_rx) = mpsc::channel::<notify::Result<Event>>(128);
    let mut _watcher = recommended_watcher(move |res| {
        let _ = watch_tx.blocking_send(res);
    })?;
    _watcher.watch(&cfg.photo_library_path, RecursiveMode::Recursive)?;

    // 3) Event loop
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                break;
            }

            Some(InvalidPhoto(path)) = invalid_rx.recv() => {
                quarantine(&cfg, &path)?;
                let _ = to_manager.send(InventoryEvent::PhotoRemoved(path)).await;
            }

            Some(res) = watch_rx.recv() => {
                match res {
                    Ok(event) => {
                        match classify_kind(&event.kind) {
                            Some(Action::Add) => {
                                for p in event.paths.into_iter().filter(|p| is_image(p.as_path())) {
                                    let _ = to_manager.send(InventoryEvent::PhotoAdded(p)).await;
                                }
                            }
                            Some(Action::Remove) => {
                                for p in event.paths.into_iter().filter(|p| is_image(p.as_path())) {
                                    let _ = to_manager.send(InventoryEvent::PhotoRemoved(p)).await;
                                }
                            }
                            None => {} // ignore
                        }
                    }
                    Err(err) => {
                        eprintln!("watch error: {err}");
                    }
                }
            }
        }
    }
    Ok(())
}

#[derive(Copy, Clone)]
enum Action {
    Add,
    Remove,
}

#[inline]
fn classify_kind(kind: &EventKind) -> Option<Action> {
    use Action::*;
    match kind {
        EventKind::Create(CreateKind::File) => Some(Add),
        EventKind::Remove(RemoveKind::File) => Some(Remove),
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => Some(Add),
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => Some(Remove),
        _ => None,
    }
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
