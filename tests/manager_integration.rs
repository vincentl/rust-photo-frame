use rust_photo_frame::events::{InventoryEvent, LoadPhoto};
use rust_photo_frame::tasks::manager;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manager_ignores_spurious_remove_and_sends_load_on_add() {
    let (inv_tx, inv_rx) = mpsc::channel::<InventoryEvent>(16);
    let (invalid_tx, _invalid_rx) = mpsc::channel(16);
    let (to_load_tx, mut to_load_rx) = mpsc::channel::<LoadPhoto>(2);
    let cancel = CancellationToken::new();

    let handle = tokio::spawn(manager::run(inv_rx, invalid_tx, to_load_tx, cancel.clone()));

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
