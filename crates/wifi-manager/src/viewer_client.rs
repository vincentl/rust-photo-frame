//! A client for the viewer's control socket (protocol: `config_model::control`):
//! one request per connection, half-close, read one reply line.

use config_model::control::{Reply, Request};
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long the viewer gets to answer (it handles commands between frames).
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);

/// What came back.
#[derive(Debug)]
pub enum Outcome {
    /// The viewer answered (`ok` may still be false).
    Answered(Reply),
    /// No viewer, or one too old to answer: use the fallback.
    NotAnswered(String),
}

impl Outcome {
    /// The viewer answered and accepted the request.
    pub fn accepted(&self) -> bool {
        matches!(self, Outcome::Answered(reply) if reply.ok)
    }

    /// Why the viewer did not take the request, for a log line.
    pub fn reason(&self) -> String {
        match self {
            Outcome::Answered(reply) if reply.ok => "accepted".to_string(),
            Outcome::Answered(reply) => format!(
                "{:?}: {}",
                reply.error,
                reply.detail.as_deref().unwrap_or("no detail")
            ),
            Outcome::NotAnswered(why) => why.clone(),
        }
    }
}

/// Send one request and wait for the reply (blocking; see [`send`]).
pub fn send_blocking(socket: &Path, request: &Request) -> Outcome {
    let payload = match serde_json::to_vec(request) {
        Ok(payload) => payload,
        Err(err) => return Outcome::NotAnswered(format!("serialize: {err}")),
    };
    let mut stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(err) => return Outcome::NotAnswered(format!("connect {}: {err}", socket.display())),
    };
    let _ = stream.set_write_timeout(Some(REPLY_TIMEOUT));
    let _ = stream.set_read_timeout(Some(REPLY_TIMEOUT));
    if let Err(err) = stream.write_all(&payload) {
        return Outcome::NotAnswered(format!("write: {err}"));
    }
    if let Err(err) = stream.shutdown(Shutdown::Write) {
        return Outcome::NotAnswered(format!("half-close: {err}"));
    }
    let mut line = String::new();
    if let Err(err) = stream.take(64 * 1024).read_to_string(&mut line) {
        return Outcome::NotAnswered(format!("read reply: {err}"));
    }
    let line = line.trim();
    if line.is_empty() {
        // A viewer from before the reply protocol reads and closes.
        return Outcome::NotAnswered("the viewer sent no reply".to_string());
    }
    match serde_json::from_str::<Reply>(line) {
        Ok(reply) => Outcome::Answered(reply),
        Err(err) => Outcome::NotAnswered(format!("unreadable reply: {err}")),
    }
}

/// Send one request without blocking the runtime.
pub async fn send(socket: PathBuf, request: Request) -> Outcome {
    match tokio::task::spawn_blocking(move || send_blocking(&socket, &request)).await {
        Ok(outcome) => outcome,
        Err(err) => Outcome::NotAnswered(format!("client task failed: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config_model::control::ErrorKind;
    use std::os::unix::net::UnixListener;
    use std::thread;

    /// A one-connection fake viewer that answers with `reply` (None: read
    /// and close without answering, like a viewer from before replies).
    fn fake_viewer(
        reply: Option<Reply>,
    ) -> (tempfile::TempDir, PathBuf, thread::JoinHandle<Vec<u8>>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("control.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = Vec::new();
            stream.read_to_end(&mut request).expect("read request");
            if let Some(reply) = reply {
                stream.write_all(reply.to_line().as_bytes()).expect("reply");
            }
            request
        });
        (dir, path, handle)
    }

    #[test]
    fn an_answer_is_read_after_the_half_close() {
        let (_dir, path, viewer) = fake_viewer(Some(Reply::ok()));
        let outcome = send_blocking(&path, &Request::WifiOverlayHide);
        assert!(outcome.accepted(), "{outcome:?}");
        let request = viewer.join().unwrap();
        assert_eq!(request, br#"{"command":"wifi-overlay-hide"}"#);
    }

    #[test]
    fn a_refusal_is_answered_but_not_accepted() {
        let (_dir, path, _viewer) =
            fake_viewer(Some(Reply::error(ErrorKind::Invalid, "join-qr is empty")));
        let outcome = send_blocking(&path, &Request::WifiOverlayHide);
        assert!(!outcome.accepted());
        assert!(outcome.reason().contains("join-qr"), "{}", outcome.reason());
    }

    #[test]
    fn an_old_viewer_that_never_answers_is_not_answered() {
        let (_dir, path, _viewer) = fake_viewer(None);
        let outcome = send_blocking(&path, &Request::WifiOverlayHide);
        assert!(matches!(outcome, Outcome::NotAnswered(_)), "{outcome:?}");
    }

    #[test]
    fn no_viewer_is_not_answered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outcome = send_blocking(&dir.path().join("missing.sock"), &Request::WifiOverlayHide);
        assert!(matches!(outcome, Outcome::NotAnswered(_)), "{outcome:?}");
    }
}
