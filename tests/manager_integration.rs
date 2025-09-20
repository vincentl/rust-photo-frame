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
async fn manager_rotates_actual_sent_item() {
    let (inv_tx, inv_rx) = mpsc::channel::<InventoryEvent>(16);
    let (_displayed_tx, displayed_rx) = mpsc::channel::<Displayed>(16);
    let (to_load_tx, mut to_load_rx) = mpsc::channel::<LoadPhoto>(1);
    let cancel = CancellationToken::new();

    let handle = tokio::spawn(manager::run(
        inv_rx,
        displayed_rx,
        to_load_tx,
        cancel.clone(),
    ));

    let initial_a = PathBuf::from("/photos/a.jpg");
    let initial_b = PathBuf::from("/photos/b.jpg");
    let newcomer = PathBuf::from("/photos/new.jpg");

    inv_tx
        .send(InventoryEvent::PhotoAdded(initial_a.clone()))
        .await
        .unwrap();
    assert_eq!(receive_with_timeout(&mut to_load_rx).await, initial_a);

    inv_tx
        .send(InventoryEvent::PhotoAdded(initial_b.clone()))
        .await
        .unwrap();

    // Allow the manager to enqueue the second photo and start waiting to resend the first.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    inv_tx
        .send(InventoryEvent::PhotoAdded(newcomer.clone()))
        .await
        .unwrap();

    let mut order = Vec::new();
    for _ in 0..3 {
        order.push(receive_with_timeout(&mut to_load_rx).await);
    }

    assert!(
        order.contains(&newcomer),
        "new photo should be enqueued promptly, got {:?}",
        order
    );

    cancel.cancel();
    let _ = handle.await;
}

async fn receive_with_timeout(rx: &mut mpsc::Receiver<LoadPhoto>) -> PathBuf {
    let LoadPhoto(path) = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("timed out waiting for LoadPhoto")
        .expect("loader channel closed unexpectedly");
    path
}
