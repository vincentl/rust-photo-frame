use std::fs;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Parser;
use evdev::{Device, InputEventKind, Key};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Parser)]
#[command(
    name = "photo-buttond",
    about = "Power button handler for the Rust photo frame"
)]
struct Args {
    /// Input device path (evdev). Auto-detects when omitted.
    #[arg(long)]
    device: Option<PathBuf>,

    /// Maximum press duration to treat as a short press (milliseconds).
    #[arg(long, default_value_t = 250)]
    single_window_ms: u64,

    /// Window to detect a second press and trigger shutdown (milliseconds).
    #[arg(long, default_value_t = 400)]
    double_window_ms: u64,

    /// Debounce window applied to press/release transitions (milliseconds).
    #[arg(long, default_value_t = 20)]
    debounce_ms: u64,

    /// Photo frame control socket.
    #[arg(long, default_value = "/run/photo-frame/control.sock")]
    control_socket: PathBuf,

    /// Shutdown helper to execute on a double press.
    #[arg(long, default_value = "/opt/photo-frame/bin/photo-safe-shutdown")]
    shutdown: PathBuf,

    /// Logging level (error|warn|info|debug|trace).
    #[arg(long, default_value = "info")]
    log_level: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    init_tracing(&args.log_level)?;

    let durations = Durations::from_args(&args);
    let (mut device, path) = open_device(&args)?;
    set_nonblocking(&device)
        .with_context(|| format!("failed to set {} non-blocking", path.display()))?;
    info!(device = %path.display(), "listening for power button events");

    let mut tracker = ButtonTracker::new(durations);

    loop {
        let now = Instant::now();
        if let Some(action) = tracker.handle_timeout(now) {
            perform_action(action, &args);
            continue;
        }

        let idle = match device.fetch_events() {
            Ok(events) => {
                let mut handled = false;
                for event in events {
                    handled = true;
                    match event.kind() {
                        InputEventKind::Key(Key::KEY_POWER) => match event.value() {
                            1 => {
                                tracker.on_press(Instant::now());
                            }
                            0 => {
                                if let Some(action) = tracker.on_release(Instant::now()) {
                                    perform_action(action, &args);
                                }
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                }
                !handled
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => true,
            Err(err) => return Err(err).with_context(|| "failed reading input events"),
        };

        if idle {
            let sleep_for = tracker
                .time_until_deadline(Instant::now())
                .unwrap_or(Duration::from_millis(50));
            if !sleep_for.is_zero() {
                thread::sleep(sleep_for.min(Duration::from_millis(100)));
            }
        }
    }
}

fn set_nonblocking(device: &Device) -> Result<()> {
    let fd = device.as_raw_fd();
    let current = fcntl(fd, FcntlArg::F_GETFL).context("F_GETFL failed")?;
    let flags = OFlag::from_bits_truncate(current) | OFlag::O_NONBLOCK;
    fcntl(fd, FcntlArg::F_SETFL(flags)).context("F_SETFL failed")?;
    Ok(())
}

fn init_tracing(level: &str) -> Result<()> {
    let filter = EnvFilter::builder()
        .parse(level)
        .with_context(|| format!("invalid log level '{level}'"))?;
    tracing_subscriber::fmt().with_env_filter(filter).init();
    Ok(())
}

fn open_device(args: &Args) -> Result<(Device, PathBuf)> {
    if let Some(path) = &args.device {
        let device =
            Device::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        ensure_power_key(&device, path)?;
        return Ok((device, path.clone()));
    }

    if let Some(result) = scan_dir("/dev/input/by-path", true)? {
        return Ok(result);
    }

    if let Some(result) = scan_dir("/dev/input", false)? {
        return Ok(result);
    }

    bail!("no input devices advertising KEY_POWER found");
}

fn scan_dir<P: AsRef<Path>>(dir: P, filter_power_name: bool) -> Result<Option<(Device, PathBuf)>> {
    let dir = dir.as_ref().to_path_buf();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read directory {}", dir.display()));
        }
    };

    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        if filter_power_name {
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) => name.to_ascii_lowercase(),
                None => continue,
            };
            if !name.contains("power") {
                continue;
            }
        }

        if !filter_power_name {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if !name.starts_with("event") {
                    continue;
                }
            }
        }

        match open_power_device(&path)? {
            Some(device) => candidates.push((device, path)),
            None => continue,
        }
    }

    candidates.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(candidates.into_iter().next())
}

fn open_power_device(path: &Path) -> Result<Option<Device>> {
    let device = match Device::open(path) {
        Ok(device) => device,
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
            warn!(device = %path.display(), "no permission to read device");
            return Ok(None);
        }
        Err(err) => return Err(err).with_context(|| format!("failed to open {}", path.display())),
    };
    match ensure_power_key(&device, path) {
        Ok(()) => Ok(Some(device)),
        Err(err) => {
            debug!(device = %path.display(), "{}", err);
            Ok(None)
        }
    }
}

fn ensure_power_key(device: &Device, path: &Path) -> Result<()> {
    let Some(keys) = device.supported_keys() else {
        bail!("{} does not advertise any keys", path.display());
    };
    if !keys.contains(Key::KEY_POWER) {
        bail!("{} does not support KEY_POWER", path.display());
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Durations {
    debounce: Duration,
    single_window: Duration,
    double_window: Duration,
}

impl Durations {
    fn from_args(args: &Args) -> Self {
        Self {
            debounce: Duration::from_millis(args.debounce_ms),
            single_window: Duration::from_millis(args.single_window_ms),
            double_window: Duration::from_millis(args.double_window_ms),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Single,
    Double,
}

struct ButtonTracker {
    durations: Durations,
    state: State,
    last_transition: Option<Instant>,
}

#[derive(Debug, Clone, Copy)]
enum State {
    Idle,
    Pressed {
        down_at: Instant,
        is_second: bool,
    },
    WaitingForSecond {
        deadline: Instant,
        first_down_at: Instant,
        released_at: Instant,
    },
}

impl ButtonTracker {
    fn new(durations: Durations) -> Self {
        Self {
            durations,
            state: State::Idle,
            last_transition: None,
        }
    }

    fn on_press(&mut self, now: Instant) {
        if !self.accept(now) {
            return;
        }
        self.state = match self.state {
            State::Idle => State::Pressed {
                down_at: now,
                is_second: false,
            },
            State::WaitingForSecond {
                deadline,
                first_down_at,
                released_at,
            } if now <= deadline => {
                let guard = self.second_press_guard();
                if now.saturating_duration_since(released_at) < guard {
                    debug!(
                        since_release = ?now.saturating_duration_since(released_at),
                        ?guard,
                        "press treated as bounce"
                    );
                    self.last_transition = Some(released_at);
                    State::Pressed {
                        down_at: first_down_at,
                        is_second: false,
                    }
                } else {
                    State::Pressed {
                        down_at: now,
                        is_second: true,
                    }
                }
            }
            State::WaitingForSecond { .. } => State::Pressed {
                down_at: now,
                is_second: false,
            },
            State::Pressed { down_at, is_second } => State::Pressed { down_at, is_second },
        };
    }

    fn on_release(&mut self, now: Instant) -> Option<Action> {
        if !self.accept(now) {
            return None;
        }

        match self.state {
            State::Pressed { down_at, is_second } => {
                let held = now.saturating_duration_since(down_at);
                self.state = State::Idle;
                if held > self.durations.single_window {
                    debug!(duration = ?held, "ignored long press");
                    return None;
                }
                if is_second {
                    return Some(Action::Double);
                }
                self.state = State::WaitingForSecond {
                    deadline: now + self.durations.double_window,
                    first_down_at: down_at,
                    released_at: now,
                };
                None
            }
            State::Idle | State::WaitingForSecond { .. } => None,
        }
    }

    fn handle_timeout(&mut self, now: Instant) -> Option<Action> {
        match self.state {
            State::WaitingForSecond { deadline, .. } if now >= deadline => {
                self.state = State::Idle;
                Some(Action::Single)
            }
            _ => None,
        }
    }

    fn time_until_deadline(&self, now: Instant) -> Option<Duration> {
        match self.state {
            State::WaitingForSecond { deadline, .. } if deadline > now => Some(deadline - now),
            State::WaitingForSecond { .. } => Some(Duration::from_millis(0)),
            _ => None,
        }
    }

    fn accept(&mut self, now: Instant) -> bool {
        if let Some(last) = self.last_transition {
            if now.saturating_duration_since(last) < self.durations.debounce {
                debug!("debounced transition");
                return false;
            }
        }
        self.last_transition = Some(now);
        true
    }

    fn second_press_guard(&self) -> Duration {
        const MIN_GUARD_MS: u64 = 75;
        self.durations
            .debounce
            .max(Duration::from_millis(MIN_GUARD_MS))
    }
}

fn perform_action(action: Action, args: &Args) {
    match action {
        Action::Single => {
            info!("single press → ToggleState command");
            if let Err(err) = trigger_single(args) {
                error!(?err, "failed to send ToggleState command");
            }
        }
        Action::Double => {
            info!("double press → shutdown");
            if let Err(err) = trigger_shutdown(&args.shutdown) {
                error!(?err, "failed to run shutdown helper");
            }
        }
    }
}

fn trigger_single(args: &Args) -> Result<()> {
    let mut stream = UnixStream::connect(&args.control_socket).with_context(|| {
        format!(
            "failed to connect to control socket at {}",
            args.control_socket.display()
        )
    })?;

    stream
        .write_all(br#"{"command":"ToggleState"}"#)
        .context("failed to send ToggleState command")?;

    Ok(())
}

fn trigger_shutdown(path: &Path) -> Result<()> {
    let status = std::process::Command::new(path)
        .status()
        .with_context(|| format!("failed to execute {}", path.display()))?;
    if !status.success() {
        bail!("shutdown helper exited with status {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Action, ButtonTracker, Durations};
    use std::time::{Duration, Instant};

    fn durations() -> Durations {
        Durations {
            debounce: Duration::from_millis(20),
            single_window: Duration::from_millis(250),
            double_window: Duration::from_millis(400),
        }
    }

    #[test]
    fn single_press_triggers_single_action() {
        let mut tracker = ButtonTracker::new(durations());
        let start = Instant::now();

        tracker.on_press(start);
        assert!(
            tracker
                .on_release(start + Duration::from_millis(100))
                .is_none()
        );
        assert_eq!(
            tracker.handle_timeout(start + Duration::from_millis(600)),
            Some(Action::Single)
        );
    }

    #[test]
    fn double_press_triggers_double_action() {
        let mut tracker = ButtonTracker::new(durations());
        let start = Instant::now();

        tracker.on_press(start);
        assert!(
            tracker
                .on_release(start + Duration::from_millis(120))
                .is_none()
        );

        let second_press = start + Duration::from_millis(220);
        tracker.on_press(second_press);
        assert_eq!(
            tracker.on_release(second_press + Duration::from_millis(80)),
            Some(Action::Double)
        );
    }

    #[test]
    fn bouncing_release_does_not_trigger_double() {
        let mut tracker = ButtonTracker::new(durations());
        let start = Instant::now();

        tracker.on_press(start);
        let bounce_release = start + Duration::from_millis(60);
        assert!(tracker.on_release(bounce_release).is_none());

        let bounce_press = bounce_release + Duration::from_millis(40);
        tracker.on_press(bounce_press);
        assert!(
            tracker
                .on_release(bounce_press + Duration::from_millis(60))
                .is_none()
        );

        assert_eq!(
            tracker.handle_timeout(start + Duration::from_millis(700)),
            Some(Action::Single)
        );
    }
}
