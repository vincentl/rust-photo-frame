use rust_photo_frame::events::{Displayed, InventoryEvent, LoadPhoto};
use rust_photo_frame::tasks::manager;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manager_ignores_spurious_remove_and_sends_load_on_add() {
    let (inv_tx, inv_rx) = mpsc::channel::<InventoryEvent>(16);
    let (_displayed_tx, displayed_rx) = mpsc::channel::<Displayed>(16);
    let (to_load_tx, mut to_load_rx) = mpsc::channel::<LoadPhoto>(2);
    let cancel = CancellationToken::new();

    let handle = tokio::spawn(manager::run(
        inv_rx,
        displayed_rx,
        to_load_tx,
        cancel.clone(),
    ));

    // Spurious remove for path never added
    let ghost = PathBuf::from("/ghost/never-added.jpg");
    inv_tx
        .send(InventoryEvent::PhotoRemoved(ghost))
        .await
        .unwrap();

    // Ensure no load arrives within a short window
    let none = tokio::time::timeout(std::time::Duration::from_millis(300), to_load_rx.recv()).await;
    assert!(
        none.is_err(),
        "should not receive LoadPhoto after spurious remove"
    );

    // Now add a real file and expect a load
    let real = PathBuf::from("/real/a.jpg");
    inv_tx
        .send(InventoryEvent::PhotoAdded(real.clone()))
        .await
        .unwrap();

    let LoadPhoto(p) = tokio::time::timeout(std::time::Duration::from_secs(2), to_load_rx.recv())
        .await
        .expect("timeout waiting for LoadPhoto")
        .expect("channel closed");
    assert_eq!(p, real);

    cancel.cancel();
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_photos_are_boosted_then_decay() {
    let (inv_tx, inv_rx) = mpsc::channel::<InventoryEvent>(16);
    let (_displayed_tx, displayed_rx) = mpsc::channel::<Displayed>(16);
    let (to_load_tx, mut to_load_rx) = mpsc::channel::<LoadPhoto>(8);
    let cancel = CancellationToken::new();

    let handle = tokio::spawn(manager::run(
        inv_rx,
        displayed_rx,
        to_load_tx,
        cancel.clone(),
    ));

    let old = PathBuf::from("/photos/old.jpg");
    inv_tx
        .send(InventoryEvent::PhotoAdded(old.clone()))
        .await
        .unwrap();

    let LoadPhoto(first) =
        tokio::time::timeout(std::time::Duration::from_secs(1), to_load_rx.recv())
            .await
            .expect("timeout waiting for initial load")
            .expect("manager channel closed");
    assert_eq!(first, old);

    let new = PathBuf::from("/photos/new.jpg");
    inv_tx
        .send(InventoryEvent::PhotoAdded(new.clone()))
        .await
        .unwrap();

    let mut sequence: Vec<PathBuf> = Vec::new();
    while sequence.len() < 8 {
        let LoadPhoto(p) =
            tokio::time::timeout(std::time::Duration::from_secs(1), to_load_rx.recv())
                .await
                .expect("timeout waiting for weighted load")
                .expect("manager channel closed");
        if sequence.is_empty() && p != new {
            continue;
        }
        sequence.push(p);
    }

    assert_eq!(sequence[0], new, "new photo should surface immediately");
    assert_eq!(
        sequence[1], new,
        "boost should allow back-to-back new displays"
    );
    assert_eq!(
        sequence[2], old,
        "weights must decay to reintroduce older shots"
    );

    let new_count = sequence.iter().filter(|p| **p == new).count();
    let old_count = sequence.iter().filter(|p| **p == old).count();
    assert!(
        new_count > old_count,
        "new photo should appear more often early on"
    );
    assert!(
        (new_count as isize - old_count as isize).abs() <= 2,
        "boost should taper so counts stay within a small margin"
    );

    cancel.cancel();
    let _ = handle.await;
}
