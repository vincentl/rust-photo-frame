use std::path::PathBuf;
use crossbeam_channel::Sender;
use notify::{
    event::{CreateKind, ModifyKind, RemoveKind, RenameMode},
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Result as NotifyResult, Watcher,
};

#[derive(Clone, Debug)]
pub enum FileAction { Add, Remove }

#[derive(Clone, Debug)]
pub struct FileEvent {
    pub action: FileAction,
    pub path: PathBuf,
}

pub fn start_watcher(
    dirs: &[PathBuf],
    tx: Sender<FileEvent>,
) -> NotifyResult<RecommendedWatcher> {
    // Closure receives notify events and forwards filtered file paths to channel
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        match res {
            Ok(event) => handle_event(event, &tx),
            Err(e) => eprintln!("watch error: {e}"),
        }
    })?;

    // (Optional) tweak config if you want. Default is fine for most setups.
    watcher.configure(Config::default())?;

    for d in dirs {
        watcher.watch(d, RecursiveMode::Recursive)?;
    }
    Ok(watcher)
}

fn handle_event(event: Event, tx: &Sender<FileEvent>) {
    use FileAction::*;
    match &event.kind {
        // New file created
        EventKind::Create(CreateKind::File) => {
            for p in event.paths {
                if is_candidate_add(&p) {
                    let _ = tx.send(FileEvent { action: Add, path: p });
                }
            }
        }
        // Rename: From (old path) -> Remove, To (new path) -> Add
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
            for p in event.paths {
                let _ = tx.send(FileEvent { action: Remove, path: p });
            }
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
            for p in event.paths {
                if is_candidate_add(&p) {
                    let _ = tx.send(FileEvent { action: Add, path: p });
                }
            }
        }
        // Content/metadata changed (sometimes editors write then replace)
        EventKind::Modify(ModifyKind::Data(_)) => {
            for p in event.paths {
                if is_candidate_add(&p) {
                    let _ = tx.send(FileEvent { action: Add, path: p });
                }
            }
        }
        // File removed
        EventKind::Remove(RemoveKind::File) => {
            for p in event.paths {
                let _ = tx.send(FileEvent { action: Remove, path: p });
            }
        }
        _ => { /* ignore directory/perm events etc. */ }
    }
}

fn is_candidate_add(p: &PathBuf) -> bool {
    std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
        && crate::scan::is_supported_image(p)
}
