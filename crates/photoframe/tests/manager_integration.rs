use photoframe::config::{PlaylistOptions, PlaylistOrder};
use photoframe::events::{CreatedSource, Displayed, InventoryEvent, LoadPhoto, PhotoInfo};
use photoframe::tasks::manager;
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
        Some(42),
        false,
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

    let LoadPhoto { path: p, priority } =
        tokio::time::timeout(std::time::Duration::from_secs(5), to_load_rx.recv())
            .await
            .expect("timeout waiting for LoadPhoto")
            .expect("channel closed");
    assert!(priority, "first load for new photo should be prioritized");
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
        Some(42),
        false,
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
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    inv_tx
        .send(InventoryEvent::PhotoAdded(photo_info(
            newcomer.clone(),
            SystemTime::now(),
        )))
        .await
        .unwrap();

    let mut seen_newcomer = false;
    let mut seen_older = HashSet::new();
    // Generous receive budget: under parallel-test load the exact interleaving
    // of adds vs. shows varies, and the lowest-weight photo can take a number of
    // shows to surface. 40 is far more than enough for all three to appear (the
    // loop breaks as soon as they do).
    for _ in 0..40 {
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
    // Generous timeout: these async tests run alongside the rest of the suite,
    // and under heavy parallel load the manager task can be slow to get
    // scheduled. 5s is far above the real latency but keeps the suite robust.
    let LoadPhoto { path, .. } = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for LoadPhoto")
        .expect("loader channel closed unexpectedly");
    path
}

fn photo_info(path: PathBuf, created_at: SystemTime) -> PhotoInfo {
    PhotoInfo {
        path,
        created_at,
        created_source: CreatedSource::Birthtime,
    }
}

#[test]
fn simulate_playlist_respects_seed_and_weights() {
    let options = PlaylistOptions {
        new_multiplicity: 3,
        half_life: Duration::from_secs(86_400),
        ..Default::default()
    };
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let fresh_path = PathBuf::from("fresh.jpg");
    // Use several old photos so weighting is observable: with only two photos
    // the no-immediate-repeat guarantee forces strict alternation, which would
    // mask the weight difference.
    let old_paths: Vec<PathBuf> = (0..3)
        .map(|i| PathBuf::from(format!("old_{i}.jpg")))
        .collect();
    let mut photos = vec![photo_info(
        fresh_path.clone(),
        now - Duration::from_secs(3_600),
    )];
    for p in &old_paths {
        photos.push(photo_info(
            p.clone(),
            now - Duration::from_secs(86_400 * 30),
        ));
    }

    let plan = manager::simulate_playlist(photos.clone(), options.clone(), now, 60, Some(42));

    assert!(plan.len() >= 30, "expected a full plan");
    // Fresh photo (weight 3) should be shown more often than any single old
    // photo (weight 1), even though the no-repeat rule caps its share.
    let fresh_count = plan.iter().filter(|p| *p == &fresh_path).count();
    for p in &old_paths {
        let old_count = plan.iter().filter(|q| *q == p).count();
        assert!(
            fresh_count > old_count,
            "fresh photo should repeat more often than each old one ({fresh_count} vs {old_count})"
        );
    }

    let plan_again = manager::simulate_playlist(photos, options, now, 60, Some(42));
    assert_eq!(plan, plan_again, "seeded runs should be deterministic");
}

#[test]
fn simulate_playlist_has_no_back_to_back_repeats() {
    let options = PlaylistOptions {
        new_multiplicity: 3,
        half_life: Duration::from_secs(86_400),
        ..Default::default()
    };
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    // A small library is the worst case for back-to-back repeats.
    let photos: Vec<PhotoInfo> = (0..3)
        .map(|i| photo_info(PathBuf::from(format!("p_{i}.jpg")), now))
        .collect();

    // Several seeds, to make sure the guarantee is not seed-specific.
    for seed in [1u64, 7, 42, 1000] {
        let plan = manager::simulate_playlist(photos.clone(), options.clone(), now, 60, Some(seed));
        for pair in plan.windows(2) {
            assert_ne!(
                pair[0], pair[1],
                "no photo should appear twice in a row (seed {seed})"
            );
        }
    }
}

/// Recurrence gaps: number of plan positions between successive showings of the
/// same photo. The mean approximates the inventory size; the spread of these
/// gaps is the knob `weighted-spread` tightens.
fn recurrence_gaps(plan: &[PathBuf]) -> Vec<f64> {
    let mut last: std::collections::HashMap<&PathBuf, usize> = std::collections::HashMap::new();
    let mut gaps = Vec::new();
    for (i, p) in plan.iter().enumerate() {
        if let Some(prev) = last.insert(p, i) {
            gaps.push((i - prev) as f64);
        }
    }
    gaps
}

fn mean_std(xs: &[f64]) -> (f64, f64) {
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
    (mean, var.sqrt())
}

#[test]
fn weighted_spread_with_zero_min_spacing_matches_weighted_random() {
    // min-spacing 0.0 zeroes the refractory floor, leaving the plain exponential
    // gap, so the scheduler must reproduce weighted-random bit-for-bit per seed.
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let old = now - Duration::from_secs(86_400 * 30);
    let photos: Vec<PhotoInfo> = (0..20)
        .map(|i| photo_info(PathBuf::from(format!("p_{i}.jpg")), old))
        .collect();

    let random = PlaylistOptions {
        order: PlaylistOrder::WeightedRandom,
        ..Default::default()
    };
    let spread0 = PlaylistOptions {
        order: PlaylistOrder::WeightedSpread,
        min_spacing: 0.0,
        ..Default::default()
    };

    let a = manager::simulate_playlist(photos.clone(), random, now, 200, Some(99));
    let b = manager::simulate_playlist(photos, spread0, now, 200, Some(99));
    assert_eq!(a, b, "weighted-spread@0.0 must equal weighted-random");
}

#[test]
fn weighted_spread_enforces_minimum_gap_and_tightens_spread() {
    // Equal-weight library so the only difference is the gap distribution.
    let n = 50usize;
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000_000);
    let old = now - Duration::from_secs(86_400 * 30);
    let photos: Vec<PhotoInfo> = (0..n)
        .map(|i| photo_info(PathBuf::from(format!("p_{i}.jpg")), old))
        .collect();

    let f = 0.8;
    let random = PlaylistOptions {
        order: PlaylistOrder::WeightedRandom,
        ..Default::default()
    };
    let spread = PlaylistOptions {
        order: PlaylistOrder::WeightedSpread,
        min_spacing: f,
        ..Default::default()
    };

    let rgaps = recurrence_gaps(&manager::simulate_playlist(
        photos.clone(),
        random,
        now,
        3000,
        Some(7),
    ));
    let splan = manager::simulate_playlist(photos, spread, now, 3000, Some(7));
    let sgaps = recurrence_gaps(&splan);

    let (rmean, rstd) = mean_std(&rgaps);
    let (smean, sstd) = mean_std(&sgaps);

    // Mean cadence is preserved (both ~ inventory size); only the spread shrinks.
    assert!(
        (rmean - smean).abs() < 0.15 * rmean,
        "mean gap should be preserved: random {rmean:.1} vs spread {smean:.1}"
    );
    // CV ~ 1 for exponential, = 1 - f = 0.2 for the refractory law: clear drop.
    assert!(
        sstd < 0.7 * rstd,
        "weighted-spread should tighten gap spread: random std {rstd:.1} vs spread std {sstd:.1}"
    );

    // The defining guarantee: a hard minimum gap of ~ f · N displays. Allow
    // slack for the steady-state approximation and the no-back-to-back guard.
    let floor = f * n as f64;
    let min_gap = sgaps.iter().cloned().fold(f64::INFINITY, f64::min);
    let random_min = rgaps.iter().cloned().fold(f64::INFINITY, f64::min);
    assert!(
        min_gap >= 0.6 * floor,
        "weighted-spread must enforce a refractory floor: min gap {min_gap} vs expected ~{floor}"
    );
    assert!(
        random_min < 0.6 * floor,
        "sanity: weighted-random should produce short gaps (min {random_min})"
    );
}

#[test]
fn simulate_playlist_single_photo_repeats() {
    // With only one photo there is nothing else to show, so it must keep
    // repeating — the no-repeat guard must not stall an empty rotation.
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let only = PathBuf::from("only.jpg");
    let plan = manager::simulate_playlist(
        vec![photo_info(only.clone(), now)],
        PlaylistOptions::default(),
        now,
        5,
        Some(1),
    );
    assert_eq!(plan.len(), 5);
    assert!(
        plan.iter().all(|p| *p == only),
        "the sole photo must keep showing"
    );
}

/// Bulk import: 50 brand-new photos plus 10 older ones. Old photos must not be starved
/// behind a wall of newcomers — they should appear within the first 50 entries.
#[test]
fn bulk_import_does_not_starve_old_photos() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000_000);
    let options = PlaylistOptions {
        new_multiplicity: 3,
        half_life: Duration::from_secs(86_400),
        ..Default::default()
    };

    let old_paths: Vec<PathBuf> = (0..10)
        .map(|i| PathBuf::from(format!("old_{i}.jpg")))
        .collect();
    let mut photos: Vec<PhotoInfo> = (0..50)
        .map(|i| photo_info(PathBuf::from(format!("new_{i}.jpg")), now))
        .collect();
    for p in &old_paths {
        photos.push(photo_info(
            p.clone(),
            now - Duration::from_secs(86_400 * 30),
        ));
    }

    let plan = manager::simulate_playlist(photos, options, now, 100, Some(7));

    // Within the first 50 entries at least one old photo must appear.
    let has_old_early = plan[..50].iter().any(|p| old_paths.contains(p));
    assert!(
        has_old_early,
        "old photos should appear within the first 50 entries despite 50 newcomers"
    );
}

/// Tombstone and generation: remove a photo mid-run, verify it eventually disappears;
/// re-add and verify it returns. Covers the lazy-skip and generation-bump code paths.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manager_churn_tombstone_and_generation() {
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
        Some(42),
        false,
    ));

    let path_a = PathBuf::from("/photos/a.jpg");
    let path_b = PathBuf::from("/photos/b.jpg");

    // Add A; receive it a few times to confirm it's in the rotation.
    inv_tx
        .send(InventoryEvent::PhotoAdded(photo_info(
            path_a.clone(),
            SystemTime::now(),
        )))
        .await
        .unwrap();
    for _ in 0..3 {
        let p = receive_with_timeout(&mut to_load_rx).await;
        assert_eq!(p, path_a, "only A should appear before removal");
    }

    // Remove A and immediately add B.
    // Due to channel buffering, one or two more As may arrive before the remove takes
    // effect. Drain until B appears — that confirms the manager has processed the remove.
    inv_tx
        .send(InventoryEvent::PhotoRemoved(path_a.clone()))
        .await
        .unwrap();
    inv_tx
        .send(InventoryEvent::PhotoAdded(photo_info(
            path_b.clone(),
            SystemTime::now(),
        )))
        .await
        .unwrap();

    // Drain until B is seen; the remove precedes the add in the channel so once B
    // appears the remove has already been processed by the manager.
    for _ in 0..20 {
        let p = receive_with_timeout(&mut to_load_rx).await;
        if p == path_b {
            break;
        }
        assert_eq!(p, path_a, "unexpected path during post-remove drain");
    }

    // Now A is confirmed removed: it must not reappear.
    for _ in 0..5 {
        let p = receive_with_timeout(&mut to_load_rx).await;
        assert_ne!(p, path_a, "A must not appear once remove is confirmed");
    }

    // Re-add A; it should reappear in the rotation (generation bump path).
    inv_tx
        .send(InventoryEvent::PhotoAdded(photo_info(
            path_a.clone(),
            SystemTime::now(),
        )))
        .await
        .unwrap();

    let mut seen_a_again = false;
    for _ in 0..10 {
        let p = receive_with_timeout(&mut to_load_rx).await;
        if p == path_a {
            seen_a_again = true;
            break;
        }
    }
    assert!(seen_a_again, "A should reappear after re-add");

    cancel.cancel();
    let _ = handle.await;
}
