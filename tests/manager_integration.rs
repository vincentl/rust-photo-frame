use rust_photo_frame::config::PlaylistOptions;
use rust_photo_frame::events::{Displayed, InventoryEvent, LoadPhoto, PhotoInfo};
use rust_photo_frame::tasks::manager;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
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
        PlaylistOptions::default(),
        None,
        None,
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
        .send(InventoryEvent::PhotoAdded(photo_info(
            real.clone(),
            SystemTime::now(),
        )))
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
        PlaylistOptions::default(),
        None,
        None,
    ));

    let initial_a = PathBuf::from("/photos/a.jpg");
    let initial_b = PathBuf::from("/photos/b.jpg");
    let newcomer = PathBuf::from("/photos/new.jpg");

    inv_tx
        .send(InventoryEvent::PhotoAdded(photo_info(
            initial_a.clone(),
            SystemTime::now() - Duration::from_secs(86_400),
        )))
        .await
        .unwrap();
    assert_eq!(receive_with_timeout(&mut to_load_rx).await, initial_a);

    inv_tx
        .send(InventoryEvent::PhotoAdded(photo_info(
            initial_b.clone(),
            SystemTime::now() - Duration::from_secs(172_800),
        )))
        .await
        .unwrap();

    // Allow the manager to enqueue the second photo and start waiting to resend the first.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    inv_tx
        .send(InventoryEvent::PhotoAdded(photo_info(
            newcomer.clone(),
            SystemTime::now(),
        )))
        .await
        .unwrap();

    let mut seen_newcomer = false;
    let mut seen_older = HashSet::new();
    for _ in 0..6 {
        let next = receive_with_timeout(&mut to_load_rx).await;
        if next == newcomer {
            seen_newcomer = true;
        } else {
            seen_older.insert(next);
        }
        if seen_newcomer && seen_older.len() == 2 {
            break;
        }
    }

    assert!(
        seen_newcomer,
        "new photo should appear early in the rotation"
    );
    assert_eq!(
        seen_older.len(),
        2,
        "all older photos should remain in the cycle"
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

fn photo_info(path: PathBuf, created_at: SystemTime) -> PhotoInfo {
    PhotoInfo { path, created_at }
}

#[test]
fn simulate_playlist_respects_seed_and_weights() {
    let options = PlaylistOptions {
        new_multiplicity: 3,
        half_life: Duration::from_secs(86_400),
    };
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let fresh_path = PathBuf::from("fresh.jpg");
    let old_path = PathBuf::from("old.jpg");
    let photos = vec![
        photo_info(fresh_path.clone(), now - Duration::from_secs(3_600)),
        photo_info(old_path.clone(), now - Duration::from_secs(86_400 * 30)),
    ];

    let plan = manager::simulate_playlist(photos.clone(), options.clone(), now, 8, Some(42));

    assert!(plan.len() >= 4, "expected several scheduled items");
    assert_eq!(plan[0], fresh_path, "fresh photo should appear first");

    let fresh_count = plan.iter().filter(|p| *p == &fresh_path).count();
    let old_count = plan.iter().filter(|p| *p == &old_path).count();
    assert!(
        fresh_count > old_count,
        "fresh photo should repeat more often than old ones"
    );

    let plan_again = manager::simulate_playlist(photos, options, now, 8, Some(42));
    assert_eq!(plan, plan_again, "seeded runs should be deterministic");
}
