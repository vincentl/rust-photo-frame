use std::fs;
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use evdev::{Device, EventSummary, KeyCode};
use humantime::format_duration;
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use serde::Deserialize;
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Parser)]
#[command(
    name = "buttond",
    about = "Power button handler for the Rust photo frame"
)]
struct Args {
    /// Path to the shared configuration file.
    #[arg(long, default_value = "/etc/photo-frame/config.yaml")]
    config: PathBuf,

    /// Input device path (evdev). Auto-detects when omitted.
    #[arg(long)]
    device: Option<PathBuf>,

    /// Logging level (error|warn|info|debug|trace).
    #[arg(long, default_value = "info")]
    log_level: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    init_tracing(&args.log_level)?;

    let settings = ButtondSettings::load(&args.config, args.device.clone()).with_context(|| {
        format!(
            "failed to load configuration from {}",
            args.config.display()
        )
    })?;
    let device_override = settings.device.clone();
    let durations = settings.durations;
    let mut runtime = settings.into_runtime()?;

    let (mut device, path) = open_device(device_override.as_ref())?;
    set_nonblocking(&device)
        .with_context(|| format!("failed to set {} non-blocking", path.display()))?;
    info!(device = %path.display(), "listening for power button events");

    let mut tracker = ButtonTracker::new(durations);

    loop {
        let now = Instant::now();
        if let Some(action) = tracker.handle_timeout(now) {
            perform_action(action, &mut runtime);
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
                                    perform_action(action, &mut runtime);
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

struct ButtondSettings {
    device: Option<PathBuf>,
    durations: Durations,
    control_socket_path: PathBuf,
    shutdown_command: CommandSpec,
    screen_on_command: CommandSpec,
    screen_off_command: CommandSpec,
    screen_off_delay: Duration,
    screen_display_name: Option<String>,
}

impl ButtondSettings {
    fn load(config_path: &Path, device_override: Option<PathBuf>) -> Result<Self> {
        let file_config = FileConfig::from_path(config_path)?;
        let buttond = file_config.buttond;
        let durations = Durations::from_config(&buttond);
        let device = device_override.or(buttond.device);
        let shutdown_command = buttond.shutdown_command.into_spec("shutdown");
        let screen = buttond.screen;
        let ScreenConfig {
            off_delay_ms,
            on_command,
            off_command,
            display_name,
        } = screen;
        let screen_off_delay = Duration::from_millis(off_delay_ms);

        Ok(Self {
            device,
            durations,
            control_socket_path: file_config.control_socket_path,
            shutdown_command,
            screen_on_command: on_command.into_spec("screen-on"),
            screen_off_command: off_command.into_spec("screen-off"),
            screen_off_delay,
            screen_display_name: display_name,
        })
    }

    fn into_runtime(self) -> Result<Runtime> {
        let screen = ScreenRuntime::new(
            self.screen_on_command,
            self.screen_off_command,
            self.screen_off_delay,
            self.screen_display_name,
        );

        Ok(Runtime {
            control_socket_path: self.control_socket_path,
            shutdown_command: self.shutdown_command,
            screen,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct FileConfig {
    #[serde(default = "FileConfig::default_control_socket_path")]
    control_socket_path: PathBuf,
    #[serde(default)]
    buttond: ButtondFileConfig,
}

impl FileConfig {
    fn from_path(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let parsed: Self = serde_yaml::from_str(&raw)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        if parsed.control_socket_path.as_os_str().is_empty() {
            bail!("control-socket-path must not be empty");
        }
        if parsed.control_socket_path.file_name().is_none() {
            bail!("control-socket-path must include a socket file name");
        }
        Ok(parsed)
    }

    fn default_control_socket_path() -> PathBuf {
        PathBuf::from("/run/photo-frame/control.sock")
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct ButtondFileConfig {
    #[serde(default)]
    device: Option<PathBuf>,
    #[serde(default = "ButtondFileConfig::default_single_window_ms")]
    single_window_ms: u64,
    #[serde(default = "ButtondFileConfig::default_double_window_ms")]
    double_window_ms: u64,
    #[serde(default = "ButtondFileConfig::default_debounce_ms")]
    debounce_ms: u64,
    #[serde(default = "ButtondFileConfig::default_shutdown_command")]
    shutdown_command: CommandConfig,
    #[serde(default)]
    screen: ScreenConfig,
}

impl ButtondFileConfig {
    const fn default_single_window_ms() -> u64 {
        250
    }

    const fn default_double_window_ms() -> u64 {
        400
    }

    const fn default_debounce_ms() -> u64 {
        20
    }

    fn default_shutdown_command() -> CommandConfig {
        CommandConfig {
            label: "shutdown".into(),
            program: PathBuf::from("/usr/bin/loginctl"),
            args: vec!["poweroff".into()],
        }
    }
}

impl Default for ButtondFileConfig {
    fn default() -> Self {
        Self {
            device: None,
            single_window_ms: Self::default_single_window_ms(),
            double_window_ms: Self::default_double_window_ms(),
            debounce_ms: Self::default_debounce_ms(),
            shutdown_command: Self::default_shutdown_command(),
            screen: ScreenConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct ScreenConfig {
    #[serde(default = "ScreenConfig::default_off_delay_ms")]
    off_delay_ms: u64,
    #[serde(default = "ScreenConfig::default_on_command")]
    on_command: CommandConfig,
    #[serde(default = "ScreenConfig::default_off_command")]
    off_command: CommandConfig,
    #[serde(default)]
    display_name: Option<String>,
}

impl Default for ScreenConfig {
    fn default() -> Self {
        Self {
            off_delay_ms: Self::default_off_delay_ms(),
            on_command: Self::default_on_command(),
            off_command: Self::default_off_command(),
            display_name: None,
        }
    }
}

impl ScreenConfig {
    const fn default_off_delay_ms() -> u64 {
        3500
    }

    fn default_on_command() -> CommandConfig {
        CommandConfig {
            label: "screen-on".into(),
            program: PathBuf::from("/opt/photo-frame/bin/powerctl"),
            args: vec!["wake".into()],
        }
    }

    fn default_off_command() -> CommandConfig {
        CommandConfig {
            label: "screen-off".into(),
            program: PathBuf::from("/opt/photo-frame/bin/powerctl"),
            args: vec!["sleep".into()],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct CommandConfig {
    #[serde(default)]
    label: String,
    program: PathBuf,
    #[serde(default)]
    args: Vec<String>,
}

impl CommandConfig {
    fn into_spec(self, fallback_label: &str) -> CommandSpec {
        let label = if self.label.is_empty() {
            fallback_label.to_string()
        } else {
            self.label
        };
        CommandSpec {
            label,
            program: self.program,
            args: self.args,
        }
    }
}

#[derive(Debug, Clone)]
struct CommandSpec {
    label: String,
    program: PathBuf,
    args: Vec<String>,
}

impl CommandSpec {
    fn run(&self) -> Result<()> {
        debug!(
            command = %self.program.display(),
            args = ?self.args,
            label = %self.label,
            "running command"
        );
        let status = Command::new(&self.program)
            .args(&self.args)
            .status()
            .with_context(|| format!("failed to execute {}", self.program.display()))?;
        if !status.success() {
            bail!("{} command exited with status {}", self.label, status);
        }
        Ok(())
    }
}

struct Runtime {
    control_socket_path: PathBuf,
    shutdown_command: CommandSpec,
    screen: ScreenRuntime,
}

impl Runtime {
    fn handle_single(&mut self) -> Result<()> {
        send_toggle_command(&self.control_socket_path)?;
        let state = self.screen.toggle_after_frame_toggle()?;
        info!(state = state.as_str(), "completed single-press toggle");
        Ok(())
    }

    fn handle_double(&self) -> Result<()> {
        self.shutdown_command.run()
    }
}

struct ScreenRuntime {
    on_command: CommandSpec,
    off_command: CommandSpec,
    off_delay: Duration,
    display_name: Option<String>,
}

impl ScreenRuntime {
    fn new(
        on_command: CommandSpec,
        off_command: CommandSpec,
        off_delay: Duration,
        display_name: Option<String>,
    ) -> Self {
        Self {
            on_command,
            off_command,
            off_delay,
            display_name,
        }
    }

    fn toggle_after_frame_toggle(&self) -> Result<ScreenState> {
        let detected = match detect_primary_display_state(self.display_name.as_deref()) {
            Ok(info) => {
                debug!(output = %info.name, state = info.state.as_str(), "detected screen state");
                info
            }
            Err(err) => {
                warn!(?err, "failed to detect screen state; assuming on");
                ScreenDetection {
                    name: self
                        .display_name
                        .clone()
                        .unwrap_or_else(|| String::from("unknown")),
                    state: ScreenState::On,
                }
            }
        };

        let next_state = match detected.state {
            ScreenState::On => {
                if !self.off_delay.is_zero() {
                    info!(
                        delay = %format_duration(self.off_delay),
                        "waiting before turning the screen off"
                    );
                    thread::sleep(self.off_delay);
                }
                self.off_command.run()?;
                ScreenState::Off
            }
            ScreenState::Off => {
                self.on_command.run()?;
                ScreenState::On
            }
        };

        Ok(next_state)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenState {
    On,
    Off,
}

impl ScreenState {
    fn as_str(self) -> &'static str {
        match self {
            ScreenState::On => "on",
            ScreenState::Off => "off",
        }
    }
}

struct ScreenDetection {
    name: String,
    state: ScreenState,
}

fn detect_primary_display_state(display_name: Option<&str>) -> Result<ScreenDetection> {
    let output = Command::new("wlr-randr")
        .output()
        .context("failed to execute wlr-randr")?;
    if !output.status.success() {
        bail!(
            "wlr-randr exited with status {status}",
            status = output.status
        );
    }
    let stdout =
        String::from_utf8(output.stdout).context("wlr-randr output was not valid UTF-8")?;

    parse_wlr_randr_outputs(&stdout, display_name)
        .ok_or_else(|| anyhow!("no connected display detected"))
}

fn parse_wlr_randr_outputs(stdout: &str, display_name: Option<&str>) -> Option<ScreenDetection> {
    let mut current: Option<OutputRecord> = None;
    let mut best: Option<ScreenDetection> = None;

    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }

        if is_output_header(line) {
            commit_output(&mut current, &mut best, display_name);
            let Some(name) = line.split_whitespace().next() else {
                current = None;
                continue;
            };
            current = Some(OutputRecord::new(name));
            continue;
        }

        if let Some(record) = current.as_mut() {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("Enabled:") {
                if let Some(value) = rest.trim().split_whitespace().next() {
                    if let Some(enabled) = parse_bool(value) {
                        record.enabled = Some(enabled);
                        record.enabled_seen = true;
                    }
                }
                continue;
            }

            if let Some(rest) = trimmed.strip_prefix("Status:") {
                if let Some(value) = rest.trim().split_whitespace().next() {
                    let value = value.to_ascii_lowercase();
                    if value.starts_with("connected") {
                        record.status_connected = Some(true);
                        record.status_seen = true;
                    } else if value.starts_with("disconnected") {
                        record.status_connected = Some(false);
                        record.status_seen = true;
                    }
                }
            }
        }
    }

    commit_output(&mut current, &mut best, display_name);

    best
}

fn is_output_header(line: &str) -> bool {
    line.chars()
        .next()
        .map(|c| !c.is_whitespace())
        .unwrap_or(false)
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "yes" | "on" | "true" | "1" => Some(true),
        "no" | "off" | "false" | "0" => Some(false),
        _ => None,
    }
}

fn commit_output(
    current: &mut Option<OutputRecord>,
    best: &mut Option<ScreenDetection>,
    display_name: Option<&str>,
) {
    let Some(record) = current.take() else {
        return;
    };

    if let Some(target_name) = display_name {
        if record.name == target_name {
            if let Some(candidate) = record.into_detection(true) {
                *best = Some(candidate);
            }
            return;
        }
    }

    if best.is_some() {
        return;
    }

    if let Some(candidate) = record.into_detection(false) {
        *best = Some(candidate);
    }
}

struct OutputRecord {
    name: String,
    is_internal: bool,
    enabled: Option<bool>,
    enabled_seen: bool,
    status_connected: Option<bool>,
    status_seen: bool,
}

impl OutputRecord {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            is_internal: name.starts_with("eDP") || name.starts_with("LVDS"),
            enabled: None,
            enabled_seen: false,
            status_connected: None,
            status_seen: false,
        }
    }

    fn into_detection(self, allow_disconnected: bool) -> Option<ScreenDetection> {
        if self.is_internal {
            return None;
        }

        let connected = self.is_connected();

        if !connected && !allow_disconnected {
            return None;
        }

        let state = self
            .enabled
            .map(|enabled| {
                if enabled {
                    ScreenState::On
                } else {
                    ScreenState::Off
                }
            })
            .unwrap_or_else(|| {
                if connected {
                    ScreenState::On
                } else {
                    ScreenState::Off
                }
            });

        Some(ScreenDetection {
            name: self.name,
            state,
        })
    }

    fn is_connected(&self) -> bool {
        if self.status_seen {
            self.status_connected.unwrap_or(false)
        } else if self.enabled_seen {
            self.enabled.unwrap_or(false)
        } else {
            false
        }
    }
}

fn perform_action(action: Action, runtime: &mut Runtime) {
    match action {
        Action::Single => {
            info!("single press → toggle frame state");
            if let Err(err) = runtime.handle_single() {
                error!(?err, "failed to process single press");
            }
        }
        Action::Double => {
            info!("double press → shutdown");
            if let Err(err) = runtime.handle_double() {
                error!(?err, "failed to run shutdown command");
            }
        }
    }
}

fn send_toggle_command(path: &Path) -> Result<()> {
    let mut stream = UnixStream::connect(path)
        .with_context(|| format!("failed to connect to control socket at {}", path.display()))?;
    stream
        .write_all(br#"{"command":"toggle-state"}"#)
        .context("failed to send toggle-state command")?;
    Ok(())
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

fn open_device(device_override: Option<&PathBuf>) -> Result<(Device, PathBuf)> {
    if let Some(path) = device_override {
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

#[derive(Clone, Copy)]
struct Durations {
    debounce: Duration,
    single_window: Duration,
    double_window: Duration,
}

impl Durations {
    fn from_config(config: &ButtondFileConfig) -> Self {
        Self {
            debounce: Duration::from_millis(config.debounce_ms),
            single_window: Duration::from_millis(config.single_window_ms),
            double_window: Duration::from_millis(config.double_window_ms),
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

#[cfg(test)]
mod tests {
    use super::{parse_wlr_randr_outputs, Action, ButtonTracker, Durations, ScreenState};
    use std::time::{Duration, Instant};

    const SAMPLE_OUTPUT: &str = r#"
HDMI-A-1 "Primary" (normal left inverted right x axis y axis)
    Enabled: yes
    Status: connected

HDMI-A-2 "Sleeper" (normal left inverted right x axis y axis)
    Enabled: no
    Status: disconnected
"#;

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
        assert!(tracker
            .on_release(start + Duration::from_millis(100))
            .is_none());
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
        assert!(tracker
            .on_release(start + Duration::from_millis(120))
            .is_none());

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
        assert!(tracker
            .on_release(bounce_press + Duration::from_millis(60))
            .is_none());

        assert_eq!(
            tracker.handle_timeout(start + Duration::from_millis(700)),
            Some(Action::Single)
        );
    }

    #[test]
    fn parse_defaults_to_first_connected_output() {
        let detection =
            parse_wlr_randr_outputs(SAMPLE_OUTPUT, None).expect("expected connected output");
        assert_eq!(detection.name, "HDMI-A-1");
        assert_eq!(detection.state, ScreenState::On);
    }

    #[test]
    fn parse_respects_disabled_named_output() {
        let detection = parse_wlr_randr_outputs(SAMPLE_OUTPUT, Some("HDMI-A-2"))
            .expect("expected named output");
        assert_eq!(detection.name, "HDMI-A-2");
        assert_eq!(detection.state, ScreenState::Off);
    }
}
