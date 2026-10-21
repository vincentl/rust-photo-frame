//! The channels that carry decoded photos (loader → photo-effect → viewer),
//! bounded in **photos**, not messages.
//!
//! A message is one screen: a photo, or a pair's two (portrait pairing). A
//! channel of `capacity` messages would therefore hold up to twice the
//! decoded pixels with pairing on. These channels count photos instead: a
//! sender takes one permit per photo of its message from a budget of
//! `capacity`, and the receiver returns them when it takes the message. So a
//! channel never holds more decoded photos than it would with pairing off for
//! the same `viewer-preload-count`.
//!
//! The one exception is a screen larger than the whole budget (a pair with
//! `viewer-preload-count: 1`): it takes the full budget, so it always fits an
//! empty channel and pairs still flow.

use crate::events::PhotoLoaded;
use std::sync::Arc;
use tokio::sync::mpsc::{
    self,
    error::{SendError, TryRecvError, TrySendError},
};
use tokio::sync::{Semaphore, TryAcquireError};

/// A photo-bounded channel holding at most `capacity` decoded photos.
pub fn photo_channel(capacity: usize) -> (PhotoSender, PhotoReceiver) {
    let capacity = capacity.max(1);
    // Every message costs at least one permit, so the message queue can
    // never fill before the budget does.
    let (tx, rx) = mpsc::channel(capacity);
    let budget = Arc::new(Semaphore::new(capacity));
    (
        PhotoSender {
            tx,
            budget: Arc::clone(&budget),
            capacity,
        },
        PhotoReceiver {
            rx,
            budget,
            capacity,
        },
    )
}

/// Permits a message takes: one per photo, at most the whole budget.
fn cost(photo: &PhotoLoaded, capacity: usize) -> u32 {
    photo.photos().min(capacity) as u32
}

#[derive(Clone, Debug)]
pub struct PhotoSender {
    tx: mpsc::Sender<PhotoLoaded>,
    budget: Arc<Semaphore>,
    capacity: usize,
}

impl PhotoSender {
    /// Wait until the channel has room for every photo of `photo`, then send
    /// it. Cancel-safe: dropped before it completes, nothing is sent and no
    /// permit is kept. Fails once the receiver is gone.
    pub async fn send(&self, photo: PhotoLoaded) -> Result<(), SendError<PhotoLoaded>> {
        let permit = match self.budget.acquire_many(cost(&photo, self.capacity)).await {
            Ok(permit) => permit,
            Err(_) => return Err(SendError(photo)),
        };
        self.tx.send(photo).await?;
        // The receiver returns the permits when it takes the message.
        permit.forget();
        Ok(())
    }

    /// Send without waiting; `Full` when the photos do not fit now.
    // Mirrors `mpsc::Sender::try_send`, which hands the message back.
    #[allow(dead_code, clippy::result_large_err)] // tests and harnesses
    pub fn try_send(&self, photo: PhotoLoaded) -> Result<(), TrySendError<PhotoLoaded>> {
        let permit = match self.budget.try_acquire_many(cost(&photo, self.capacity)) {
            Ok(permit) => permit,
            Err(TryAcquireError::NoPermits) => return Err(TrySendError::Full(photo)),
            Err(TryAcquireError::Closed) => return Err(TrySendError::Closed(photo)),
        };
        self.tx.try_send(photo)?;
        permit.forget();
        Ok(())
    }
}

#[derive(Debug)]
pub struct PhotoReceiver {
    rx: mpsc::Receiver<PhotoLoaded>,
    budget: Arc<Semaphore>,
    capacity: usize,
}

impl PhotoReceiver {
    /// The next message; its photos leave the channel's budget.
    /// Cancel-safe, as `mpsc::Receiver::recv`.
    pub async fn recv(&mut self) -> Option<PhotoLoaded> {
        let photo = self.rx.recv().await?;
        self.budget
            .add_permits(cost(&photo, self.capacity) as usize);
        Some(photo)
    }

    pub fn try_recv(&mut self) -> Result<PhotoLoaded, TryRecvError> {
        let photo = self.rx.try_recv()?;
        self.budget
            .add_permits(cost(&photo, self.capacity) as usize);
        Ok(photo)
    }

    /// Messages (screens) waiting.
    #[allow(dead_code)] // tests
    pub fn len(&self) -> usize {
        self.rx.len()
    }

    #[allow(dead_code)] // tests
    pub fn is_empty(&self) -> bool {
        self.rx.is_empty()
    }

    /// Decoded photos the channel holds now.
    #[allow(dead_code)] // tests
    pub fn photos(&self) -> usize {
        self.capacity - self.budget.available_permits()
    }
}

impl Drop for PhotoReceiver {
    /// A sender waiting for room would otherwise wait forever.
    fn drop(&mut self) {
        self.budget.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{LoadedPartner, PreparedImageCpu};

    fn prepared(name: &str) -> PreparedImageCpu {
        PreparedImageCpu {
            path: name.into(),
            width: 1,
            height: 1,
            source_size: (1, 1),
            pixels: vec![0; 4],
        }
    }

    fn solo(name: &str) -> PhotoLoaded {
        PhotoLoaded {
            prepared: prepared(name),
            priority: false,
            partner: None,
        }
    }

    fn pair(a: &str, b: &str) -> PhotoLoaded {
        PhotoLoaded {
            partner: Some(LoadedPartner {
                prepared: prepared(b),
                priority: false,
            }),
            ..solo(a)
        }
    }

    /// Review M4: pairs never hold more photos than solos would.
    #[test]
    fn the_bound_counts_photos_not_screens() {
        let (tx, mut rx) = photo_channel(3);
        // Solos: three screens, three photos, as an mpsc channel of 3.
        for name in ["a", "b", "c"] {
            tx.try_send(solo(name)).unwrap();
        }
        assert!(matches!(tx.try_send(solo("d")), Err(TrySendError::Full(_))));
        assert_eq!(rx.photos(), 3);
        while rx.try_recv().is_ok() {}
        assert_eq!(rx.photos(), 0);
        // Pairs: one pair and a solo fill the three photos; a second pair
        // waits (a screen-counted channel would have taken three pairs).
        tx.try_send(pair("a", "b")).unwrap();
        assert!(matches!(
            tx.try_send(pair("c", "d")),
            Err(TrySendError::Full(_))
        ));
        tx.try_send(solo("e")).unwrap();
        assert_eq!(rx.photos(), 3);
        assert_eq!(rx.len(), 2);
        assert_eq!(rx.try_recv().unwrap().photos(), 2);
        assert_eq!(rx.photos(), 1);
        tx.try_send(pair("c", "d")).unwrap();
        assert_eq!(rx.photos(), 3);
    }

    #[test]
    fn a_pair_fits_an_empty_channel_of_one() {
        let (tx, mut rx) = photo_channel(1);
        tx.try_send(pair("a", "b")).unwrap();
        assert!(matches!(tx.try_send(solo("c")), Err(TrySendError::Full(_))));
        assert_eq!(rx.try_recv().unwrap().photos(), 2);
        tx.try_send(solo("c")).unwrap();
    }

    #[tokio::test]
    async fn a_waiting_sender_gets_room_when_photos_are_taken_and_fails_when_the_receiver_goes() {
        let (tx, mut rx) = photo_channel(2);
        tx.send(pair("a", "b")).await.unwrap();
        let waiting = tokio::spawn({
            let tx = tx.clone();
            async move { tx.send(solo("c")).await }
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished(), "no room until the pair is taken");
        rx.recv().await.unwrap();
        waiting.await.unwrap().unwrap();
        assert_eq!(
            rx.recv().await.unwrap().prepared.path,
            std::path::Path::new("c")
        );

        tx.send(pair("d", "e")).await.unwrap();
        let blocked = tokio::spawn({
            let tx = tx.clone();
            async move { tx.send(solo("f")).await }
        });
        tokio::task::yield_now().await;
        drop(rx);
        assert!(
            blocked.await.unwrap().is_err(),
            "a closed channel fails the send"
        );
    }
}
