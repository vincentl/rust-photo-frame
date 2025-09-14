use rust_photo_frame::config::Configuration;
use rust_photo_frame::events::{InvalidPhoto, InventoryEvent};
use rust_photo_frame::tasks::files;
use std::fs;
use std::path::PathBuf;
use tempfile::tempdir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_recursive_scan_emits_photo_added() {
    let tmp = tempdir().unwrap();
    let lib = tmp.path().join("lib");
    fs::create_dir_all(lib.join("nested")).unwrap();

    // Create files before the task starts (startup scan)
    fs::write(lib.join("a.jpg"), b"x").unwrap();
    fs::write(lib.join("nested").join("b.jpeg"), b"x").unwrap();
    fs::write(lib.join("c.txt"), b"x").unwrap();

    let cfg = Configuration {
        photo_library_path: lib.clone(),
    };

    let (inv_tx, mut inv_rx) = mpsc::channel::<InventoryEvent>(16);
    let (_invalid_tx, invalid_rx) = mpsc::channel::<InvalidPhoto>(16);
    let cancel = CancellationToken::new();

    let handle = tokio::spawn(files::run(cfg, inv_tx, invalid_rx, cancel.clone()));

    // Collect two PhotoAdded events (for a.jpg, nested/b.jpeg)
    let mut added: Vec<PathBuf> = Vec::new();
    while added.len() < 2 {
        if let Some(InventoryEvent::PhotoAdded(p)) =
            tokio::time::timeout(std::time::Duration::from_secs(5), inv_rx.recv())
                .await
                .expect("timeout waiting for inventory event")
        {
            added.push(p);
        }
    }

    // Normalize filenames and assert expected set
    let mut names: Vec<String> = added
        .into_iter()
        .map(|p| p.strip_prefix(&lib).unwrap().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["a.jpg".to_string(), "nested/b.jpeg".to_string()]
    );

    cancel.cancel();
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_photo_is_deleted_and_emits_removed() {
    let tmp = tempdir().unwrap();
    let lib = tmp.path().join("lib");
    fs::create_dir_all(&lib).unwrap();

    let bad = lib.join("bad.jpg");
    fs::write(&bad, b"x").unwrap();

    let cfg = Configuration {
        photo_library_path: lib.clone(),
    };

    let (inv_tx, mut inv_rx) = mpsc::channel::<InventoryEvent>(16);
    let (invalid_tx, invalid_rx) = mpsc::channel::<InvalidPhoto>(16);
    let cancel = CancellationToken::new();

    let handle = tokio::spawn(files::run(cfg, inv_tx, invalid_rx, cancel.clone()));

    // Wait for startup scan to pick up the file
    let mut saw_added = false;
    while !saw_added {
        if let Some(InventoryEvent::PhotoAdded(p)) =
            tokio::time::timeout(std::time::Duration::from_secs(5), inv_rx.recv())
                .await
                .expect("timeout waiting for inventory event")
        {
            if p == bad {
                saw_added = true;
            }
        }
    }

    // Send InvalidPhoto (simulate Manager/Loader decision)
    invalid_tx.send(InvalidPhoto(bad.clone())).await.unwrap();

    // Expect at least one PhotoRemoved for the same path
    let mut saw_removed = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if let Ok(Some(InventoryEvent::PhotoRemoved(p))) =
            tokio::time::timeout(std::time::Duration::from_millis(200), inv_rx.recv()).await
        {
            if p == bad {
                saw_removed = true;
                break;
            }
        }
    }
    assert!(saw_removed, "did not see PhotoRemoved for quarantined file");

    // Original should be gone
    assert!(!bad.exists(), "original file should be deleted");

    cancel.cancel();
    let _ = handle.await;
}
