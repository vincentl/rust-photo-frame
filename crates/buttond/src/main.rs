use std::fs;
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use evdev::{Device, EventSummary, KeyCode};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use serde::Deserialize;
use serde_yaml::{Mapping, Value as YamlValue};
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Parser)]
#[command(
    name = "photo-buttond",
    about = "Power button handler for the Rust photo frame"
)]
struct Args {
    /// Path to the photo frame configuration file.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Input device path (evdev). Auto-detects when omitted.
    #[arg(long)]
    device: Option<PathBuf>,

    /// Maximum press duration to treat as a short press (milliseconds).
    #[arg(long)]
    single_window_ms: Option<u64>,

    /// Window to detect a second press and trigger shutdown (milliseconds).
    #[arg(long)]
    double_window_ms: Option<u64>,

    /// Debounce window applied to press/release transitions (milliseconds).
    #[arg(long)]
    debounce_ms: Option<u64>,

    /// Photo frame control socket.
    #[arg(long)]
    control_socket: Option<PathBuf>,

    /// Shutdown helper to execute on a double press.
    #[arg(long)]
    shutdown: Option<PathBuf>,

    /// Logging level (error|warn|info|debug|trace).
    #[arg(long, default_value = "info")]
    log_level: String,

    /// PID file written by the photo frame kiosk process.
    #[arg(long)]
    pidfile: Option<PathBuf>,

    /// Expected process name for the kiosk PID (matches `/proc/<pid>/comm`).
    #[arg(long)]
    procname: Option<String>,

    /// Override the display connector name reported by `wlr-randr`.
    #[arg(long)]
    screen_output: Option<String>,

    /// Delay before powering the screen off after requesting sleep (milliseconds).
    #[arg(long)]
    screen_off_delay_ms: Option<u64>,

    /// Wayland display name when invoking `wlr-randr`.
    #[arg(long)]
    wayland_display: Option<String>,
}

const DEFAULT_SINGLE_WINDOW_MS: u64 = 250;
const DEFAULT_DOUBLE_WINDOW_MS: u64 = 400;
const DEFAULT_DEBOUNCE_MS: u64 = 20;
const DEFAULT_CONTROL_SOCKET: &str = "/run/photo-frame/control.sock";
const DEFAULT_SHUTDOWN_PATH: &str = "/opt/photo-frame/bin/photo-safe-shutdown";
const DEFAULT_SCREEN_OFF_DELAY_MS: u64 = 3500;

#[derive(Debug, Clone)]
struct Settings {
    device: Option<PathBuf>,
    durations: Durations,
    control_socket: PathBuf,
    shutdown: PathBuf,
    pidfile: Option<PathBuf>,
    procname: Option<String>,
    screen: ScreenSettings,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
struct ButtondConfig {
    device: Option<PathBuf>,
    single_window_ms: Option<u64>,
    double_window_ms: Option<u64>,
    debounce_ms: Option<u64>,
    shutdown_command: Option<PathBuf>,
    pidfile: Option<PathBuf>,
    procname: Option<String>,
    #[serde(default)]
    screen: ButtondScreenConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
struct ButtondScreenConfig {
    output: Option<String>,
    off_delay_ms: Option<u64>,
    wayland_display: Option<String>,
}

struct ConfigFile {
    control_socket_path: Option<PathBuf>,
    buttond: ButtondConfig,
}

impl Settings {
    fn from_args(args: Args) -> Result<Self> {
        let config_file =
            if let Some(path) = args.config.as_deref() {
                Some(load_config_file(path).with_context(|| {
                    format!("failed to load configuration from {}", path.display())
                })?)
            } else {
                None
            };

        let buttond_config = config_file.as_ref().map(|file| &file.buttond);

        let device = args
            .device
            .or_else(|| buttond_config.and_then(|cfg| cfg.device.clone()));

        let single_window_ms = args
            .single_window_ms
            .or_else(|| buttond_config.and_then(|cfg| cfg.single_window_ms))
            .unwrap_or(DEFAULT_SINGLE_WINDOW_MS);
        ensure!(
            single_window_ms > 0,
            "single-window-ms must be greater than zero"
        );

        let double_window_ms = args
            .double_window_ms
            .or_else(|| buttond_config.and_then(|cfg| cfg.double_window_ms))
            .unwrap_or(DEFAULT_DOUBLE_WINDOW_MS);
        ensure!(
            double_window_ms > 0,
            "double-window-ms must be greater than zero"
        );

        let debounce_ms = args
            .debounce_ms
            .or_else(|| buttond_config.and_then(|cfg| cfg.debounce_ms))
            .unwrap_or(DEFAULT_DEBOUNCE_MS);
        ensure!(debounce_ms > 0, "debounce-ms must be greater than zero");

        let durations = Durations::from_values(
            Duration::from_millis(debounce_ms),
            Duration::from_millis(single_window_ms),
            Duration::from_millis(double_window_ms),
        );

        let control_socket = args
            .control_socket
            .or_else(|| {
                config_file
                    .as_ref()
                    .and_then(|file| file.control_socket_path.clone())
            })
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CONTROL_SOCKET));
        ensure!(
            !control_socket.as_os_str().is_empty(),
            "control-socket path must not be empty"
        );
        ensure!(
            control_socket.file_name().is_some(),
            "control-socket path must include a socket file name"
        );

        let shutdown = args
            .shutdown
            .or_else(|| buttond_config.and_then(|cfg| cfg.shutdown_command.clone()))
            .unwrap_or_else(|| PathBuf::from(DEFAULT_SHUTDOWN_PATH));

        let pidfile = args
            .pidfile
            .or_else(|| buttond_config.and_then(|cfg| cfg.pidfile.clone()));

        let procname = sanitize_string(
            args.procname
                .or_else(|| buttond_config.and_then(|cfg| cfg.procname.clone())),
        );

        let screen_output = sanitize_string(
            args.screen_output
                .or_else(|| buttond_config.and_then(|cfg| cfg.screen.output.clone())),
        );
        let screen_off_delay_ms = args
            .screen_off_delay_ms
            .or_else(|| buttond_config.and_then(|cfg| cfg.screen.off_delay_ms))
            .unwrap_or(DEFAULT_SCREEN_OFF_DELAY_MS);
        let wayland_display = sanitize_string(
            args.wayland_display
                .or_else(|| buttond_config.and_then(|cfg| cfg.screen.wayland_display.clone())),
        );
        let screen = ScreenSettings::new(screen_output, wayland_display, screen_off_delay_ms)?;

        Ok(Self {
            device,
            durations,
            control_socket,
            shutdown,
            pidfile,
            procname,
            screen,
        })
    }
}

fn sanitize_string(value: Option<String>) -> Option<String> {
    value.and_then(|raw| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn load_config_file(path: &Path) -> Result<ConfigFile> {
    let contents = fs::read_to_string(path)?;
    let root: YamlValue = serde_yaml::from_str(&contents)?;

    let control_socket_path = root
        .get("control-socket-path")
        .and_then(|value| value.as_str())
        .map(|s| PathBuf::from(s));

    let buttond_value = root
        .get("buttond")
        .cloned()
        .unwrap_or_else(|| YamlValue::Mapping(Mapping::new()));
    let buttond: ButtondConfig =
        serde_yaml::from_value(buttond_value).context("invalid buttond configuration block")?;

    Ok(ConfigFile {
        control_socket_path,
        buttond,
    })
}

#[derive(Debug, Clone)]
struct ScreenSettings {
    output: Option<String>,
    off_delay: Duration,
    wayland_display: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenPowerState {
    On,
    Off,
}

#[derive(Debug, Clone)]
struct OutputInfo {
    name: String,
    connected: bool,
    enabled: Option<bool>,
    internal: bool,
}

#[derive(Debug, Default)]
struct OutputBuilder {
    name: String,
    connected: Option<bool>,
    enabled: Option<bool>,
    internal: bool,
}

impl ScreenSettings {
    fn new(
        output: Option<String>,
        wayland_display: Option<String>,
        off_delay_ms: u64,
    ) -> Result<Self> {
        Ok(Self {
            output,
            off_delay: Duration::from_millis(off_delay_ms),
            wayland_display,
        })
    }

    fn toggle(&self) -> Result<()> {
        let outputs = self.query_outputs()?;
        if outputs.is_empty() {
            bail!("wlr-randr did not report any outputs");
        }

        let (target, state) = self.resolve_target(&outputs)?;
        debug!(target = %target.name, state = ?state, "resolved display state");

        match state {
            ScreenPowerState::On => {
                let delay = self.off_delay;
                let controller = self.clone();
                let target_name = target.name.clone();
                thread::spawn(move || {
                    if !delay.is_zero() {
                        thread::sleep(delay);
                    }
                    match controller.set_power(&target_name, ScreenPowerState::Off) {
                        Ok(()) => info!(target = %target_name, "powered display off"),
                        Err(err) => {
                            warn!(target = %target_name, ?err, "failed to power display off")
                        }
                    }
                });
                Ok(())
            }
            ScreenPowerState::Off => {
                self.set_power(&target.name, ScreenPowerState::On)?;
                info!(target = %target.name, "powered display on");
                Ok(())
            }
        }
    }

    fn resolve_target(&self, outputs: &[OutputInfo]) -> Result<(OutputInfo, ScreenPowerState)> {
        let candidate = if let Some(requested) = &self.output {
            outputs
                .iter()
                .find(|out| &out.name == requested)
                .ok_or_else(|| {
                    anyhow::anyhow!("output '{}' not reported by wlr-randr", requested)
                })?
        } else {
            outputs
                .iter()
                .find(|out| out.connected && !out.internal)
                .or_else(|| outputs.iter().find(|out| out.connected))
                .or_else(|| outputs.first())
                .ok_or_else(|| anyhow::anyhow!("no display outputs discovered"))?
        };

        let enabled = candidate.enabled.unwrap_or(candidate.connected);
        let state = if enabled {
            ScreenPowerState::On
        } else {
            ScreenPowerState::Off
        };

        Ok((candidate.clone(), state))
    }

    fn query_outputs(&self) -> Result<Vec<OutputInfo>> {
        let mut cmd = Command::new("wlr-randr");
        if let Some(display) = &self.wayland_display {
            cmd.env("WAYLAND_DISPLAY", display);
        }
        let output = cmd.output().context("failed to execute wlr-randr")?;
        if !output.status.success() {
            bail!("wlr-randr exited with status {}", output.status);
        }
        let stdout =
            String::from_utf8(output.stdout).context("wlr-randr emitted non-UTF-8 output")?;
        Ok(parse_wlr_randr_outputs(&stdout))
    }

    fn set_power(&self, target: &str, state: ScreenPowerState) -> Result<()> {
        let mut cmd = Command::new("wlr-randr");
        if let Some(display) = &self.wayland_display {
            cmd.env("WAYLAND_DISPLAY", display);
        }
        cmd.arg("--output").arg(target);
        match state {
            ScreenPowerState::On => {
                cmd.arg("--on");
            }
            ScreenPowerState::Off => {
                cmd.arg("--off");
            }
        }

        let status = cmd
            .status()
            .with_context(|| format!("failed to execute wlr-randr for output {target}"))?;
        if status.success() {
            return Ok(());
        }

        warn!(
            target = target,
            state = ?state,
            %status,
            "wlr-randr failed; attempting vcgencmd fallback"
        );

        let mut fallback = Command::new("vcgencmd");
        fallback.arg("display_power");
        match state {
            ScreenPowerState::On => fallback.arg("1"),
            ScreenPowerState::Off => fallback.arg("0"),
        };

        let fallback_status = fallback
            .status()
            .context("failed to execute vcgencmd fallback")?;
        ensure!(
            fallback_status.success(),
            "vcgencmd fallback exited with status {fallback_status}"
        );
        Ok(())
    }
}

fn parse_wlr_randr_outputs(raw: &str) -> Vec<OutputInfo> {
    let mut outputs = Vec::new();
    let mut builder: Option<OutputBuilder> = None;

    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let is_header = line.chars().next().map_or(true, |ch| !ch.is_whitespace());
        if is_header {
            if let Some(prev) = builder.take() {
                outputs.push(prev.finish());
            }
            let mut parts = line.split_whitespace();
            let name = parts.next().unwrap_or_default().to_string();
            let internal = name.starts_with("eDP") || name.starts_with("LVDS");
            builder = Some(OutputBuilder {
                name,
                internal,
                ..Default::default()
            });
            continue;
        }

        if let Some(active) = builder.as_mut() {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("Enabled:") {
                if let Some(value) = parse_bool(rest.trim()) {
                    active.enabled = Some(value);
                }
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("Status:") {
                let normalized = rest.trim().to_ascii_lowercase();
                if normalized.starts_with("connected") {
                    active.connected = Some(true);
                } else if normalized.starts_with("disconnected") {
                    active.connected = Some(false);
                }
                continue;
            }
        }
    }

    if let Some(prev) = builder.take() {
        outputs.push(prev.finish());
    }

    outputs
}

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "yes" | "on" | "true" | "1" => Some(true),
        "no" | "off" | "false" | "0" => Some(false),
        _ => None,
    }
}

impl OutputBuilder {
    fn finish(self) -> OutputInfo {
        let enabled = self.enabled;
        let connected = self.connected.unwrap_or_else(|| enabled.unwrap_or(false));
        OutputInfo {
            name: self.name,
            connected,
            enabled,
            internal: self.internal,
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    init_tracing(&args.log_level)?;

    let settings = Settings::from_args(args)?;

    let (mut device, path) = open_device(&settings)?;
    set_nonblocking(&device)
        .with_context(|| format!("failed to set {} non-blocking", path.display()))?;
    info!(device = %path.display(), "listening for power button events");

    let mut tracker = ButtonTracker::new(settings.durations);

    loop {
        let now = Instant::now();
        if let Some(action) = tracker.handle_timeout(now) {
            perform_action(action, &settings);
            continue;
        }

        let idle = match device.fetch_events() {
            Ok(events) => {
                let mut handled = false;
                for event in events {
                    handled = true;
                    match event.destructure() {
                        EventSummary::Key(_, KeyCode::KEY_POWER, value) => match value {
                            1 => {
                                tracker.on_press(Instant::now());
                            }
                            0 => {
                                if let Some(action) = tracker.on_release(Instant::now()) {
                                    perform_action(action, &settings);
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
    let current = fcntl(device.as_fd(), FcntlArg::F_GETFL).context("F_GETFL failed")?;
    let mut flags = OFlag::from_bits_retain(current);
    flags.insert(OFlag::O_NONBLOCK);
    fcntl(device.as_fd(), FcntlArg::F_SETFL(flags)).context("F_SETFL failed")?;
    Ok(())
}

fn init_tracing(level: &str) -> Result<()> {
    let filter = EnvFilter::builder()
        .parse(level)
        .with_context(|| format!("invalid log level '{level}'"))?;
    tracing_subscriber::fmt().with_env_filter(filter).init();
    Ok(())
}

fn open_device(settings: &Settings) -> Result<(Device, PathBuf)> {
    if let Some(path) = &settings.device {
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
    if !keys.contains(KeyCode::KEY_POWER) {
        bail!("{} does not support KEY_POWER", path.display());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct Durations {
    debounce: Duration,
    single_window: Duration,
    double_window: Duration,
}

impl Durations {
    fn from_values(debounce: Duration, single_window: Duration, double_window: Duration) -> Self {
        Self {
            debounce,
            single_window,
            double_window,
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

fn perform_action(action: Action, settings: &Settings) {
    match action {
        Action::Single => {
            info!("single press → toggle-state command");
            if let Err(err) = trigger_single(settings) {
                error!(?err, "failed to send toggle-state command");
            }
        }
        Action::Double => {
            info!("double press → shutdown");
            if let Err(err) = trigger_shutdown(&settings.shutdown) {
                error!(?err, "failed to run shutdown helper");
            }
        }
    }
}

fn trigger_single(settings: &Settings) -> Result<()> {
    if let Some(pidfile) = &settings.pidfile {
        let running = target_process_running(pidfile, settings.procname.as_deref())?;
        if !running {
            warn!(
                pidfile = %pidfile.display(),
                procname = settings.procname.as_deref().unwrap_or("<unspecified>"),
                "skipping toggle-state command: kiosk process not running"
            );
            return Ok(());
        }
    }

    let mut stream = UnixStream::connect(&settings.control_socket).with_context(|| {
        format!(
            "failed to connect to control socket at {}",
            settings.control_socket.display()
        )
    })?;

    stream
        .write_all(br#"{"command":"toggle-state"}"#)
        .context("failed to send toggle-state command")?;

    settings
        .screen
        .toggle()
        .context("failed to toggle display power")?;

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

fn target_process_running(pidfile: &Path, expected_name: Option<&str>) -> Result<bool> {
    let contents = match fs::read_to_string(pidfile) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to read pidfile {}", pidfile.display()));
        }
    };

    let contents = contents.trim();
    if contents.is_empty() {
        bail!("pidfile {} is empty", pidfile.display());
    }

    let pid: i32 = contents
        .parse()
        .with_context(|| format!("invalid pid '{}' in {}", contents, pidfile.display()))?;

    let proc_path = Path::new("/proc").join(pid.to_string());
    if !proc_path.exists() {
        return Ok(false);
    }

    if let Some(expected) = expected_name {
        let comm_path = proc_path.join("comm");
        let comm = fs::read_to_string(&comm_path)
            .with_context(|| format!("failed to read process name from {}", comm_path.display()))?;
        if comm.trim_end() != expected {
            return Ok(false);
        }
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{
        Action, ButtonTracker, Durations, parse_wlr_randr_outputs, target_process_running,
    };
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

    #[cfg(target_os = "linux")]
    #[test]
    fn target_process_running_true_for_current_process() -> anyhow::Result<()> {
        use std::fs;

        let pid = std::process::id();
        let temp_dir = tempfile::tempdir()?;
        let pidfile = temp_dir.path().join("kiosk.pid");
        fs::write(&pidfile, pid.to_string())?;

        let comm = fs::read_to_string("/proc/self/comm")?;
        let name = comm.trim_end().to_string();

        assert!(target_process_running(&pidfile, Some(&name))?);
        assert!(target_process_running(&pidfile, None)?);
        assert!(!target_process_running(
            &pidfile,
            Some("definitely-not-the-name")
        )?);

        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn target_process_running_false_when_pidfile_missing() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let pidfile = temp_dir.path().join("missing.pid");

        assert!(!target_process_running(&pidfile, None)?);
        Ok(())
    }

    #[test]
    fn parse_outputs_detects_enabled_state() {
        let sample = r#"
HDMI-A-1 "Dell Inc. DELL U2720Q" 3840x2160@60.00Hz (preferred, current)
  Modes:
    3840x2160@60.00Hz (preferred)
  Position: 0,0
  Scale: 1.000000
  Enabled: yes
  Status: connected

eDP-1 "Sharp Corp." 1920x1080@60.01Hz
  Enabled: no
  Status: disconnected
"#;

        let outputs = parse_wlr_randr_outputs(sample);
        assert_eq!(outputs.len(), 2);

        let hdmi = outputs.iter().find(|out| out.name == "HDMI-A-1").unwrap();
        assert!(hdmi.connected);
        assert_eq!(hdmi.enabled, Some(true));
        assert!(!hdmi.internal);

        let panel = outputs.iter().find(|out| out.name == "eDP-1").unwrap();
        assert!(!panel.connected);
        assert_eq!(panel.enabled, Some(false));
        assert!(panel.internal);
    }
}
