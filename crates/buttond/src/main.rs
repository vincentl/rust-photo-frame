use std::env;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{Duration as ChronoDuration, Utc};
use clap::Parser;
use config_model::{AwakeScheduleConfig, GreetingScreenConfig};
use evdev::{Device, EventSummary, KeyCode};
use humantime::format_duration;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use serde::Deserialize;
use serde_json::json;
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
    let (mut runtime, scheduler_config) = settings.into_runtime()?;

    let mut scheduler_rx =
        scheduler_config.and_then(|config| spawn_scheduler(config, runtime.shared_state()));

    let (mut device, path) = open_device(device_override.as_ref())?;
    set_nonblocking(&device)
        .with_context(|| format!("failed to set {} non-blocking", path.display()))?;
    info!(device = %path.display(), "listening for power button events");

    let mut tracker = ButtonTracker::new(durations);

    loop {
        if let Some(rx) = scheduler_rx.as_ref() {
            let mut disconnected = false;
            loop {
                match rx.try_recv() {
                    Ok(SchedulerCommand::WakeUp) => {
                        if let Err(err) = runtime.wake_up(TransitionSource::Scheduled) {
                            error!(?err, "failed to process scheduled wake");
                        }
                    }
                    Ok(SchedulerCommand::GoToSleep) => {
                        if let Err(err) = runtime.go_to_sleep(TransitionSource::Scheduled) {
                            error!(?err, "failed to process scheduled sleep");
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        warn!("scheduler channel disconnected");
                        disconnected = true;
                        break;
                    }
                }
            }
            if disconnected {
                scheduler_rx = None;
            }
        }

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
    greeting_screen_delay: Duration,
    awake_schedule: Option<AwakeScheduleConfig>,
    sleep_grace: Duration,
}

const FORCE_SHUTDOWN_FLAG: &str = "-i";
const NO_ASK_PASSWORD_FLAG: &str = "--no-ask-password";

fn configure_shutdown_args(args: &mut Vec<String>, force_shutdown: bool) {
    configure_shutdown_flag(args, FORCE_SHUTDOWN_FLAG, force_shutdown);
    configure_shutdown_flag(args, NO_ASK_PASSWORD_FLAG, force_shutdown);
}

fn configure_shutdown_flag(args: &mut Vec<String>, flag: &str, enabled: bool) {
    if enabled {
        if !args.iter().any(|arg| arg == flag) {
            args.push(flag.to_string());
        }
    } else {
        args.retain(|arg| arg != flag);
    }
}

#[derive(Clone)]
struct SchedulerConfig {
    schedule: AwakeScheduleConfig,
    greeting_delay: Duration,
    sleep_grace: Duration,
}

impl ButtondSettings {
    fn load(config_path: &Path, device_override: Option<PathBuf>) -> Result<Self> {
        let file_config = FileConfig::from_path(config_path)?;
        let FileConfig {
            control_socket_path,
            buttond,
            greeting_screen,
            awake_schedule,
        } = file_config;
        let ButtondFileConfig {
            device,
            single_window_ms,
            double_window_ms,
            debounce_ms,
            sleep_grace_ms,
            shutdown_command,
            screen,
            force_shutdown,
        } = buttond;

        let durations = Durations::from_millis(debounce_ms, single_window_ms, double_window_ms);
        let device = device_override.or(device);
        let mut shutdown_command = shutdown_command.into_spec("shutdown");
        configure_shutdown_args(&mut shutdown_command.args, force_shutdown);
        let ScreenConfig {
            off_delay_ms,
            on_command,
            off_command,
            display_name,
        } = screen;
        let screen_off_delay = Duration::from_millis(off_delay_ms);
        let greeting_screen_delay = greeting_screen.effective_duration();
        let sleep_grace = Duration::from_millis(sleep_grace_ms);

        let mut screen_on_command = on_command.into_spec("screen-on");
        let mut screen_off_command = off_command.into_spec("screen-off");
        if let Some(name) = display_name.as_ref() {
            screen_on_command.args.push(name.clone());
            screen_off_command.args.push(name.clone());
        }

        Ok(Self {
            device,
            durations,
            control_socket_path,
            shutdown_command,
            screen_on_command,
            screen_off_command,
            screen_off_delay,
            screen_display_name: display_name,
            greeting_screen_delay,
            awake_schedule,
            sleep_grace,
        })
    }

    fn into_runtime(self) -> Result<(Runtime, Option<SchedulerConfig>)> {
        let sway_env = Arc::new(SwayEnvironment::prepare()?);
        let executor: Arc<dyn CommandExecutor> =
            Arc::new(SwayCommandExecutor::new(sway_env.clone()));
        let powerctl_program = detect_powerctl_program(
            &self.screen_on_command,
            &self.screen_off_command,
        );
        let detector: Arc<dyn ScreenDetector> = Arc::new(SwayScreenDetector::new(
            sway_env,
            powerctl_program,
        ));

        let screen = ScreenRuntime::new(
            self.screen_on_command,
            self.screen_off_command,
            self.screen_off_delay,
            self.screen_display_name,
            executor.clone(),
            detector,
        );

        let initial_state = screen
            .detect_state()
            .context("failed to detect initial screen state")?
            .state
            .into();

        let control_socket: Arc<dyn ControlSocket> =
            Arc::new(UnixControlSocket::new(self.control_socket_path.clone()));

        let runtime = Runtime::new(
            control_socket,
            self.shutdown_command,
            screen,
            executor,
            initial_state,
        );

        let scheduler = self.awake_schedule.map(|schedule| SchedulerConfig {
            schedule,
            greeting_delay: self.greeting_screen_delay,
            sleep_grace: self.sleep_grace,
        });

        Ok((runtime, scheduler))
    }
}

fn detect_powerctl_program(on: &CommandSpec, off: &CommandSpec) -> Option<PathBuf> {
    for command in [on, off] {
        if command
            .program
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name == "powerctl")
            .unwrap_or(false)
        {
            return Some(command.program.clone());
        }
    }

    None
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct FileConfig {
    #[serde(default = "FileConfig::default_control_socket_path")]
    control_socket_path: PathBuf,
    #[serde(default)]
    buttond: ButtondFileConfig,
    #[serde(default)]
    greeting_screen: GreetingScreenConfig,
    #[serde(default)]
    awake_schedule: Option<AwakeScheduleConfig>,
}

impl FileConfig {
    fn from_path(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let mut parsed: Self = serde_yaml::from_str(&raw)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        if parsed.control_socket_path.as_os_str().is_empty() {
            bail!("control-socket-path must not be empty");
        }
        if parsed.control_socket_path.file_name().is_none() {
            bail!("control-socket-path must include a socket file name");
        }
        parsed
            .greeting_screen
            .validate()
            .context("invalid greeting screen configuration")?;
        if let Some(schedule) = parsed.awake_schedule.as_mut() {
            schedule
                .validate()
                .context("invalid awake schedule configuration")?;
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
    #[serde(default = "ButtondFileConfig::default_sleep_grace_ms")]
    sleep_grace_ms: u64,
    #[serde(default = "ButtondFileConfig::default_force_shutdown")]
    force_shutdown: bool,
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

    const fn default_sleep_grace_ms() -> u64 {
        300_000
    }

    const fn default_force_shutdown() -> bool {
        true
    }

    fn default_shutdown_command() -> CommandConfig {
        CommandConfig {
            label: "shutdown".into(),
            program: PathBuf::from("/usr/bin/systemctl"),
            args: vec!["poweroff".into(), "-i".into()],
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
            sleep_grace_ms: Self::default_sleep_grace_ms(),
            force_shutdown: Self::default_force_shutdown(),
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

trait CommandExecutor: Send + Sync {
    fn execute(&self, command: &CommandSpec) -> Result<()>;
}

struct SwayCommandExecutor {
    env: Arc<SwayEnvironment>,
}

impl SwayCommandExecutor {
    fn new(env: Arc<SwayEnvironment>) -> Self {
        Self { env }
    }
}

impl CommandExecutor for SwayCommandExecutor {
    fn execute(&self, command: &CommandSpec) -> Result<()> {
        debug!(
            program = %command.program.display(),
            args = ?command.args,
            label = %command.label,
            "running command",
        );
        let mut os_command = Command::new(&command.program);
        os_command.args(&command.args);
        self.env.configure(&mut os_command);
        let status = os_command
            .status()
            .with_context(|| format!("failed to execute {}", command.program.display()))?;
        if !status.success() {
            bail!("{} command exited with status {}", command.label, status);
        }
        Ok(())
    }
}

trait ControlSocket: Send + Sync {
    fn send_set_state(&self, state: ViewerMode) -> Result<()>;
}

struct UnixControlSocket {
    path: PathBuf,
}

impl UnixControlSocket {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl ControlSocket for UnixControlSocket {
    fn send_set_state(&self, state: ViewerMode) -> Result<()> {
        const MAX_ATTEMPTS: usize = 3;
        const RETRY_DELAY: Duration = Duration::from_millis(150);

        let payload = serde_json::to_vec(&json!({
            "command": "set-state",
            "state": state.as_str(),
        }))
        .context("failed to serialize control payload")?;

        let mut last_error: Option<anyhow::Error> = None;

        for attempt in 1..=MAX_ATTEMPTS {
            match UnixStream::connect(&self.path) {
                Ok(mut stream) => {
                    if let Err(err) = stream.write_all(&payload) {
                        warn!(
                            attempt,
                            path = %self.path.display(),
                            ?err,
                            "failed to send control payload",
                        );
                        last_error = Some(err.into());
                    } else {
                        return Ok(());
                    }
                }
                Err(err) => {
                    warn!(
                        attempt,
                        path = %self.path.display(),
                        ?err,
                        "failed to connect to control socket",
                    );
                    last_error = Some(err.into());
                }
            }

            if attempt < MAX_ATTEMPTS {
                thread::sleep(RETRY_DELAY);
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow!("failed to send control command after retries")))
    }
}

struct Runtime {
    control_socket: Arc<dyn ControlSocket>,
    shutdown_command: CommandSpec,
    screen: ScreenRuntime,
    executor: Arc<dyn CommandExecutor>,
    state: Arc<Mutex<FrameState>>,
}

impl Runtime {
    fn new(
        control_socket: Arc<dyn ControlSocket>,
        shutdown_command: CommandSpec,
        screen: ScreenRuntime,
        executor: Arc<dyn CommandExecutor>,
        initial_state: ViewerMode,
    ) -> Self {
        let state = Arc::new(Mutex::new(FrameState::new(initial_state)));
        Self {
            control_socket,
            shutdown_command,
            screen,
            executor,
            state,
        }
    }

    fn shared_state(&self) -> Arc<Mutex<FrameState>> {
        Arc::clone(&self.state)
    }

    fn handle_manual_toggle(&mut self) -> Result<()> {
        let detected = self.screen.detect_state()?;
        debug!(output = %detected.name, state = detected.state.as_str(), "detected screen state");

        match detected.state {
            ScreenState::On => {
                info!("single press → putting frame to sleep");
                self.go_to_sleep(TransitionSource::Manual)?;
            }
            ScreenState::Off => {
                info!("single press → waking frame");
                self.wake_up(TransitionSource::Manual)?;
            }
        }

        Ok(())
    }

    fn handle_double(&self) -> Result<()> {
        self.executor.execute(&self.shutdown_command)
    }

    fn wake_up(&mut self, source: TransitionSource) -> Result<()> {
        if self.current_viewer_mode() == ViewerMode::Awake {
            info!(
                reason = source.as_str(),
                "wake_up requested but frame already awake"
            );
            self.record_state(ViewerMode::Awake, source);
            return Ok(());
        }

        self.screen.power_on()?;
        self.control_socket.send_set_state(ViewerMode::Awake)?;
        info!(reason = source.as_str(), "frame wake request completed");
        self.record_state(ViewerMode::Awake, source);
        Ok(())
    }

    fn go_to_sleep(&mut self, source: TransitionSource) -> Result<()> {
        if self.current_viewer_mode() == ViewerMode::Asleep {
            info!(
                reason = source.as_str(),
                "sleep requested but frame already asleep"
            );
            self.record_state(ViewerMode::Asleep, source);
            return Ok(());
        }

        self.control_socket.send_set_state(ViewerMode::Asleep)?;
        let delay = self.screen.off_delay();
        if !delay.is_zero() {
            info!(reason = source.as_str(), delay = %format_duration(delay), "waiting before powering screen off");
            thread::sleep(delay);
        }
        self.screen.power_off()?;
        info!(reason = source.as_str(), "frame sleep request completed");
        self.record_state(ViewerMode::Asleep, source);
        Ok(())
    }

    fn current_viewer_mode(&self) -> ViewerMode {
        let guard = self.state.lock().expect("frame state poisoned");
        guard.mode
    }

    fn record_state(&self, mode: ViewerMode, source: TransitionSource) {
        let mut guard = self.state.lock().expect("frame state poisoned");
        guard.update(mode, source);
    }
}

#[derive(Clone, Copy, Debug)]
enum TransitionSource {
    Manual,
    Scheduled,
}

impl TransitionSource {
    fn as_str(self) -> &'static str {
        match self {
            TransitionSource::Manual => "manual",
            TransitionSource::Scheduled => "scheduled",
        }
    }
}

#[derive(Clone, Copy)]
struct ManualOverride {
    at: Instant,
    target: ViewerMode,
}

struct FrameState {
    mode: ViewerMode,
    last_manual_override: Option<ManualOverride>,
    greeting_complete: bool,
}

impl FrameState {
    fn new(mode: ViewerMode) -> Self {
        Self {
            mode,
            last_manual_override: None,
            greeting_complete: mode == ViewerMode::Awake,
        }
    }

    fn update(&mut self, mode: ViewerMode, source: TransitionSource) {
        self.mode = mode;
        match source {
            TransitionSource::Manual => {
                self.last_manual_override = Some(ManualOverride {
                    at: Instant::now(),
                    target: mode,
                });
            }
            TransitionSource::Scheduled => {
                self.last_manual_override = None;
            }
        }

        if mode == ViewerMode::Awake {
            self.greeting_complete = true;
        }
    }

    fn last_manual_awake(&self) -> Option<Instant> {
        self.last_manual_override
            .as_ref()
            .and_then(|override_info| {
                if override_info.target == ViewerMode::Awake {
                    Some(override_info.at)
                } else {
                    None
                }
            })
    }

    fn greeting_complete(&self) -> bool {
        self.greeting_complete
    }
}

struct ScreenRuntime {
    on_command: CommandSpec,
    off_command: CommandSpec,
    off_delay: Duration,
    display_name: Option<String>,
    executor: Arc<dyn CommandExecutor>,
    detector: Arc<dyn ScreenDetector>,
}

impl ScreenRuntime {
    fn new(
        on_command: CommandSpec,
        off_command: CommandSpec,
        off_delay: Duration,
        display_name: Option<String>,
        executor: Arc<dyn CommandExecutor>,
        detector: Arc<dyn ScreenDetector>,
    ) -> Self {
        Self {
            on_command,
            off_command,
            off_delay,
            display_name,
            executor,
            detector,
        }
    }

    fn power_on(&self) -> Result<()> {
        self.executor.execute(&self.on_command)
    }

    fn power_off(&self) -> Result<()> {
        self.executor.execute(&self.off_command)
    }

    fn off_delay(&self) -> Duration {
        self.off_delay
    }

    fn detect_state(&self) -> Result<ScreenDetection> {
        self.detector.detect(self.display_name.as_deref())
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewerMode {
    Awake,
    Asleep,
}

impl ViewerMode {
    fn as_str(self) -> &'static str {
        match self {
            ViewerMode::Awake => "awake",
            ViewerMode::Asleep => "asleep",
        }
    }
}

impl From<ScreenState> for ViewerMode {
    fn from(state: ScreenState) -> Self {
        match state {
            ScreenState::On => ViewerMode::Awake,
            ScreenState::Off => ViewerMode::Asleep,
        }
    }
}

impl From<ViewerMode> for ScreenState {
    fn from(mode: ViewerMode) -> Self {
        match mode {
            ViewerMode::Awake => ScreenState::On,
            ViewerMode::Asleep => ScreenState::Off,
        }
    }
}

struct ScreenDetection {
    name: String,
    state: ScreenState,
}

trait ScreenDetector: Send + Sync {
    fn detect(&self, display_name: Option<&str>) -> Result<ScreenDetection>;
}

struct SwayScreenDetector {
    env: Arc<SwayEnvironment>,
    powerctl_program: Option<PathBuf>,
}

impl SwayScreenDetector {
    fn new(env: Arc<SwayEnvironment>, powerctl_program: Option<PathBuf>) -> Self {
        Self {
            env,
            powerctl_program,
        }
    }

    fn detect_via_powerctl(&self, program: &Path, display_name: &str) -> Result<ScreenDetection> {
        let mut command = Command::new(program);
        command.arg("state").arg(display_name);
        self.env.configure(&mut command);

        let output = command
            .output()
            .with_context(|| format!("failed to execute {} state probe", program.display()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "{} state probe exited with status {}{}",
                program.display(),
                output.status,
                if stderr.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", stderr.trim())
                }
            );
        }

        let stdout = String::from_utf8(output.stdout)
            .context("powerctl state output was not valid UTF-8")?;
        let state = match stdout.trim() {
            "on" => ScreenState::On,
            "off" => ScreenState::Off,
            other => bail!("unexpected powerctl state output: {other}"),
        };

        Ok(ScreenDetection {
            name: display_name.to_string(),
            state,
        })
    }

    fn detect_via_swaymsg(&self, display_name: Option<&str>) -> Result<ScreenDetection> {
        let mut command = Command::new("swaymsg");
        self.env.configure(&mut command);
        command.arg("-t").arg("get_outputs").arg("--raw");

        let output = command
            .output()
            .context("failed to execute swaymsg for output detection")?;
        if !output.status.success() {
            bail!(
                "swaymsg exited with status {status}",
                status = output.status
            );
        }

        parse_sway_outputs(&output.stdout, display_name)
    }
}

impl ScreenDetector for SwayScreenDetector {
    fn detect(&self, display_name: Option<&str>) -> Result<ScreenDetection> {
        if let (Some(program), Some(name)) = (&self.powerctl_program, display_name) {
            match self.detect_via_powerctl(program, name) {
                Ok(detection) => return Ok(detection),
                Err(err) => {
                    warn!(
                        program = %program.display(),
                        %name,
                        ?err,
                        "powerctl state detection failed; falling back to swaymsg",
                    );
                }
            }
        }

        self.detect_via_swaymsg(display_name)
    }
}

struct SwayEnvironment {
    runtime_dir: PathBuf,
    socket_path: PathBuf,
}

impl SwayEnvironment {
    fn prepare() -> Result<Self> {
        let runtime_dir = match env::var_os("XDG_RUNTIME_DIR") {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => {
                let uid = unsafe { libc::getuid() };
                PathBuf::from(format!("/run/user/{uid}"))
            }
        };

        if !runtime_dir.is_dir() {
            bail!("XDG_RUNTIME_DIR '{}' does not exist", runtime_dir.display());
        }

        if let Some(sock) = env::var_os("SWAYSOCK") {
            let candidate = PathBuf::from(sock);
            if is_socket(&candidate)? {
                return Ok(Self {
                    runtime_dir,
                    socket_path: candidate,
                });
            }
            bail!(
                "SWAYSOCK '{}' is not a valid sway IPC socket",
                candidate.display()
            );
        }

        let socket_path = find_sway_socket(&runtime_dir)?;
        Ok(Self {
            runtime_dir,
            socket_path,
        })
    }

    fn configure(&self, command: &mut Command) {
        command.env("XDG_RUNTIME_DIR", &self.runtime_dir);
        command.env("SWAYSOCK", &self.socket_path);
    }
}

fn is_socket(path: &Path) -> Result<bool> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_socket()),
        Err(err) => {
            if err.kind() == io::ErrorKind::NotFound {
                Ok(false)
            } else {
                Err(err).with_context(|| format!("failed to inspect {}", path.display()))
            }
        }
    }
}

fn find_sway_socket(runtime_dir: &Path) -> Result<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(runtime_dir)
        .with_context(|| format!("failed to list {}", runtime_dir.display()))?
        .filter_map(|entry| {
            entry.ok().map(|e| e.path()).filter(|path| {
                match path.file_name().and_then(|name| name.to_str()) {
                    Some(name) => name.starts_with("sway-ipc.") && name.ends_with(".sock"),
                    None => false,
                }
            })
        })
        .collect();

    entries.sort();
    for candidate in entries {
        if is_socket(&candidate)? {
            return Ok(candidate);
        }
    }

    bail!(
        "failed to locate sway IPC socket in {}",
        runtime_dir.display()
    )
}

fn parse_sway_outputs(stdout: &[u8], display_name: Option<&str>) -> Result<ScreenDetection> {
    let outputs: Vec<SwayOutput> =
        serde_json::from_slice(stdout).context("failed to parse swaymsg outputs")?;

    let mut candidates = outputs.iter().filter(|output| output.power.is_some());

    let record = if let Some(name) = display_name {
        outputs
            .iter()
            .find(|output| output.name == name)
            .with_context(|| format!("no sway output named '{name}'"))?
    } else {
        candidates
            .next()
            .context("no sway outputs expose a power state")?
    };

    let power = record
        .power
        .ok_or_else(|| anyhow!("sway output '{}' did not report power state", record.name))?;

    let state = power.into_state();

    Ok(ScreenDetection {
        name: record.name.clone(),
        state,
    })
}

#[derive(Deserialize)]
struct SwayOutput {
    name: String,
    power: Option<SwayPower>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwayPower {
    On,
    Off,
}

impl SwayPower {
    fn into_state(self) -> ScreenState {
        match self {
            SwayPower::On => ScreenState::On,
            SwayPower::Off => ScreenState::Off,
        }
    }
}

impl<'de> Deserialize<'de> for SwayPower {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct SwayPowerVisitor;

        impl<'de> serde::de::Visitor<'de> for SwayPowerVisitor {
            type Value = SwayPower;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a string or boolean sway power state")
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(match value {
                    true => SwayPower::On,
                    false => SwayPower::Off,
                })
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                match value {
                    "on" => Ok(SwayPower::On),
                    "off" => Ok(SwayPower::Off),
                    other => Err(E::unknown_variant(other, &["on", "off"])),
                }
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_str(&value)
            }
        }

        deserializer.deserialize_any(SwayPowerVisitor)
    }
}

fn perform_action(action: Action, runtime: &mut Runtime) {
    match action {
        Action::Single => {
            info!("single press → toggle frame state");
            if let Err(err) = runtime.handle_manual_toggle() {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchedulerCommand {
    WakeUp,
    GoToSleep,
}

impl SchedulerCommand {
    fn target_mode(self) -> ViewerMode {
        match self {
            SchedulerCommand::WakeUp => ViewerMode::Awake,
            SchedulerCommand::GoToSleep => ViewerMode::Asleep,
        }
    }
}

fn spawn_scheduler(
    config: SchedulerConfig,
    shared_state: Arc<Mutex<FrameState>>,
) -> Option<mpsc::Receiver<SchedulerCommand>> {
    let (tx, rx) = mpsc::channel();
    let builder = thread::Builder::new().name(String::from("buttond-scheduler"));
    match builder.spawn(move || scheduler_loop(config, shared_state, tx)) {
        Ok(_) => Some(rx),
        Err(err) => {
            error!(?err, "failed to spawn scheduler thread");
            None
        }
    }
}

fn scheduler_loop(
    config: SchedulerConfig,
    shared_state: Arc<Mutex<FrameState>>,
    tx: mpsc::Sender<SchedulerCommand>,
) {
    const MAX_SLEEP: Duration = Duration::from_secs(60);
    const COMMAND_SETTLE: Duration = Duration::from_millis(100);
    const COMMAND_RETRY: Duration = Duration::from_secs(1);
    let greeting_ready_at = Instant::now() + config.greeting_delay;
    let mut pending_command: Option<(SchedulerCommand, Instant)> = None;

    loop {
        let now_instant = Instant::now();
        let timezone = config.schedule.timezone();
        let now = Utc::now().with_timezone(&timezone);
        let should_be_awake = config.schedule.is_awake_at(now);

        let (current_mode, last_manual_awake, greeting_complete) = {
            let guard = shared_state.lock().expect("frame state poisoned");
            (
                guard.mode,
                guard.last_manual_awake(),
                guard.greeting_complete(),
            )
        };

        if let Some((command, _)) = pending_command.as_ref() {
            if current_mode == command.target_mode() {
                pending_command = None;
            }
        }

        if should_be_awake && current_mode != ViewerMode::Awake {
            if !greeting_complete && now_instant < greeting_ready_at {
                sleep_for(
                    greeting_ready_at.saturating_duration_since(now_instant),
                    MAX_SLEEP,
                );
                continue;
            }

            if let Some((command, last_sent)) = pending_command.as_ref() {
                if *command == SchedulerCommand::WakeUp
                    && now_instant.duration_since(*last_sent) < COMMAND_RETRY
                {
                    sleep_for(COMMAND_SETTLE, MAX_SLEEP);
                    continue;
                }
            }

            match tx.send(SchedulerCommand::WakeUp) {
                Ok(()) => {
                    pending_command = Some((SchedulerCommand::WakeUp, Instant::now()));
                    sleep_for(COMMAND_SETTLE, MAX_SLEEP);
                    continue;
                }
                Err(_) => {
                    debug!("scheduler exiting after receiver closed");
                    break;
                }
            }
        } else if !should_be_awake && current_mode != ViewerMode::Asleep {
            if let Some(manual_awake_at) = last_manual_awake {
                let deadline = manual_awake_at + config.sleep_grace;
                if now_instant < deadline {
                    sleep_for(deadline.saturating_duration_since(now_instant), MAX_SLEEP);
                    continue;
                }
            }

            if let Some((command, last_sent)) = pending_command.as_ref() {
                if *command == SchedulerCommand::GoToSleep
                    && now_instant.duration_since(*last_sent) < COMMAND_RETRY
                {
                    sleep_for(COMMAND_SETTLE, MAX_SLEEP);
                    continue;
                }
            }

            match tx.send(SchedulerCommand::GoToSleep) {
                Ok(()) => {
                    pending_command = Some((SchedulerCommand::GoToSleep, Instant::now()));
                    sleep_for(COMMAND_SETTLE, MAX_SLEEP);
                    continue;
                }
                Err(_) => {
                    debug!("scheduler exiting after receiver closed");
                    break;
                }
            }
        }

        let mut next_check = now_instant + MAX_SLEEP;

        if let Some((transition, _)) = config.schedule.next_transition_after(now) {
            if let Some(duration) = chrono_duration_to_std(transition.signed_duration_since(now)) {
                let candidate = now_instant + duration;
                if candidate < next_check {
                    next_check = candidate;
                }
            }
        }

        if !greeting_complete && now_instant < greeting_ready_at && greeting_ready_at < next_check {
            next_check = greeting_ready_at;
        }

        if let Some(manual_awake_at) = last_manual_awake {
            let deadline = manual_awake_at + config.sleep_grace;
            if deadline < next_check {
                next_check = deadline;
            }
        }

        let sleep_duration = next_check.saturating_duration_since(Instant::now());
        sleep_for(sleep_duration, MAX_SLEEP);
    }
}

fn sleep_for(duration: Duration, max_sleep: Duration) {
    if duration.is_zero() {
        thread::yield_now();
        return;
    }
    thread::sleep(duration.min(max_sleep));
}

fn chrono_duration_to_std(duration: ChronoDuration) -> Option<Duration> {
    if duration <= ChronoDuration::zero() {
        None
    } else {
        duration.to_std().ok()
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
    fn from_millis(debounce_ms: u64, single_window_ms: u64, double_window_ms: u64) -> Self {
        Self {
            debounce: Duration::from_millis(debounce_ms),
            single_window: Duration::from_millis(single_window_ms),
            double_window: Duration::from_millis(double_window_ms),
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
    use super::{
        Action, ButtonTracker, CommandExecutor, CommandSpec, ControlSocket, Durations,
        FORCE_SHUTDOWN_FLAG, FrameState, NO_ASK_PASSWORD_FLAG, Runtime, SchedulerCommand,
        SchedulerConfig, ScreenDetection, ScreenDetector, ScreenRuntime, ScreenState,
        SwayEnvironment, SwayScreenDetector, TransitionSource, UnixControlSocket, ViewerMode,
        configure_shutdown_args,
        parse_sway_outputs, scheduler_loop,
    };
    use config_model::AwakeScheduleConfig;
    use serde_yaml::from_str;
    use std::ffi::{OsStr, OsString};
    use std::io::Read;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvGuard {
        fn replace(key: &'static str, value: Option<&OsStr>) -> Self {
            let original = std::env::var_os(key);
            match value {
                Some(val) => unsafe { std::env::set_var(key, val) },
                None => unsafe { std::env::remove_var(key) },
            }
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.original.as_ref() {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    const SAMPLE_OUTPUT: &str = r#"
[
  {"name": "HDMI-A-1", "power": "on"},
  {"name": "HDMI-A-2", "power": "off"}
]
"#;

    const SAMPLE_BOOL_OUTPUT: &str = r#"
[
  {"name": "HDMI-A-1", "power": true},
  {"name": "HDMI-A-2", "power": false}
]
"#;

    fn durations() -> Durations {
        Durations {
            debounce: Duration::from_millis(20),
            single_window: Duration::from_millis(250),
            double_window: Duration::from_millis(400),
        }
    }

    fn command(label: &str) -> CommandSpec {
        CommandSpec {
            label: label.to_string(),
            program: PathBuf::from("/bin/true"),
            args: Vec::new(),
        }
    }

    fn always_awake_schedule() -> AwakeScheduleConfig {
        let mut schedule: AwakeScheduleConfig = from_str(
            r#"
timezone: "UTC"
awake-scheduled:
  daily:
    - ["00:00", "23:59"]
"#,
        )
        .expect("valid schedule yaml");
        schedule.validate().expect("valid schedule");
        schedule
    }

    fn always_asleep_schedule() -> AwakeScheduleConfig {
        let mut schedule: AwakeScheduleConfig = from_str(
            r#"
timezone: "UTC"
awake-scheduled: {}
"#,
        )
        .expect("valid schedule yaml");
        schedule.validate().expect("valid schedule");
        schedule
    }

    #[test]
    fn configure_shutdown_args_adds_force_flags() {
        let mut args = vec![String::from("poweroff")];
        configure_shutdown_args(&mut args, true);
        assert!(args.iter().any(|arg| arg == FORCE_SHUTDOWN_FLAG));
        assert!(args.iter().any(|arg| arg == NO_ASK_PASSWORD_FLAG));
    }

    #[test]
    fn configure_shutdown_args_removes_force_flags() {
        let mut args = vec![
            String::from("poweroff"),
            FORCE_SHUTDOWN_FLAG.to_string(),
            NO_ASK_PASSWORD_FLAG.to_string(),
        ];
        configure_shutdown_args(&mut args, false);
        assert_eq!(args, vec![String::from("poweroff")]);
    }

    #[derive(Default, Clone)]
    struct RecordingExecutor {
        calls: Arc<Mutex<Vec<(String, Instant)>>>,
    }

    impl RecordingExecutor {
        fn new() -> Self {
            Self::default()
        }

        fn calls(&self) -> Arc<Mutex<Vec<(String, Instant)>>> {
            Arc::clone(&self.calls)
        }
    }

    impl CommandExecutor for RecordingExecutor {
        fn execute(&self, command: &CommandSpec) -> super::Result<()> {
            self.calls
                .lock()
                .expect("recording executor poisoned")
                .push((command.label.clone(), Instant::now()));
            Ok(())
        }
    }

    #[derive(Default, Clone)]
    struct RecordingControlSocket {
        events: Arc<Mutex<Vec<(ViewerMode, Instant)>>>,
    }

    impl RecordingControlSocket {
        fn new() -> Self {
            Self::default()
        }

        fn events(&self) -> Arc<Mutex<Vec<(ViewerMode, Instant)>>> {
            Arc::clone(&self.events)
        }
    }

    impl ControlSocket for RecordingControlSocket {
        fn send_set_state(&self, state: ViewerMode) -> super::Result<()> {
            self.events
                .lock()
                .expect("recording control socket poisoned")
                .push((state, Instant::now()));
            Ok(())
        }
    }

    #[derive(Clone)]
    struct StaticDetector {
        state: ScreenState,
    }

    impl StaticDetector {
        fn new(state: ScreenState) -> Self {
            Self { state }
        }
    }

    impl ScreenDetector for StaticDetector {
        fn detect(&self, display_name: Option<&str>) -> super::Result<ScreenDetection> {
            Ok(ScreenDetection {
                name: display_name.unwrap_or("mock").to_string(),
                state: self.state,
            })
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
    fn sway_environment_uses_explicit_socket() {
        let _lock = ENV_LOCK.lock().expect("env lock poisoned");
        let runtime = tempdir().expect("tempdir");
        let runtime_path = runtime.path().to_path_buf();
        let socket_path = runtime_path.join("sway-ipc.1234.1.sock");
        let _listener = UnixListener::bind(&socket_path).expect("socket created");
        let _runtime_guard = EnvGuard::replace("XDG_RUNTIME_DIR", Some(runtime_path.as_os_str()));
        let _socket_guard = EnvGuard::replace("SWAYSOCK", Some(socket_path.as_os_str()));

        let env = SwayEnvironment::prepare().expect("env configured");
        assert_eq!(env.runtime_dir, runtime_path);
        assert_eq!(env.socket_path, socket_path);
    }

    #[test]
    fn sway_environment_discovers_socket_in_runtime() {
        let _lock = ENV_LOCK.lock().expect("env lock poisoned");
        let runtime = tempdir().expect("tempdir");
        let runtime_path = runtime.path().to_path_buf();
        let socket_path = runtime_path.join("sway-ipc.1234.2.sock");
        let _listener = UnixListener::bind(&socket_path).expect("socket created");
        let _runtime_guard = EnvGuard::replace("XDG_RUNTIME_DIR", Some(runtime_path.as_os_str()));
        let _socket_guard = EnvGuard::replace("SWAYSOCK", None);

        let env = SwayEnvironment::prepare().expect("env configured");
        assert_eq!(env.runtime_dir, runtime_path);
        assert_eq!(env.socket_path, socket_path);
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

    #[test]
    fn parse_defaults_to_first_connected_output() {
        let detection =
            parse_sway_outputs(SAMPLE_OUTPUT.as_bytes(), None).expect("expected connected output");
        assert_eq!(detection.name, "HDMI-A-1");
        assert_eq!(detection.state, ScreenState::On);
    }

    #[test]
    fn parse_respects_disabled_named_output() {
        let detection = parse_sway_outputs(SAMPLE_OUTPUT.as_bytes(), Some("HDMI-A-2"))
            .expect("expected named output");
        assert_eq!(detection.name, "HDMI-A-2");
        assert_eq!(detection.state, ScreenState::Off);
    }

    #[test]
    fn parse_accepts_boolean_power_values() {
        let detection = parse_sway_outputs(SAMPLE_BOOL_OUTPUT.as_bytes(), None)
            .expect("expected connected output");
        assert_eq!(detection.name, "HDMI-A-1");
        assert_eq!(detection.state, ScreenState::On);

        let detection = parse_sway_outputs(SAMPLE_BOOL_OUTPUT.as_bytes(), Some("HDMI-A-2"))
            .expect("expected named output");
        assert_eq!(detection.name, "HDMI-A-2");
        assert_eq!(detection.state, ScreenState::Off);
    }

    #[test]
    fn detector_uses_powerctl_state_when_available() {
        let dir = tempdir().expect("tempdir");
        let script_path = dir.path().join("powerctl");
        fs::write(&script_path, "#!/usr/bin/env bash\necho on\n").expect("write script");
        let mut perms = fs::metadata(&script_path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).expect("set perms");

        let env = Arc::new(SwayEnvironment {
            runtime_dir: dir.path().to_path_buf(),
            socket_path: dir.path().join("sway-ipc.test.sock"),
        });

        let detector = SwayScreenDetector::new(env, Some(script_path));
        let detection = detector
            .detect(Some("HDMI-A-1"))
            .expect("powerctl state detection");

        assert_eq!(detection.name, "HDMI-A-1");
        assert_eq!(detection.state, ScreenState::On);
    }

    #[test]
    fn wake_up_runs_power_on_before_viewer_notification() {
        let executor = RecordingExecutor::new();
        let control = RecordingControlSocket::new();
        let detector = StaticDetector::new(ScreenState::Off);

        let screen = ScreenRuntime::new(
            command("screen-on"),
            command("screen-off"),
            Duration::from_millis(0),
            Some("HDMI-A-1".into()),
            Arc::new(executor.clone()),
            Arc::new(detector),
        );

        let runtime_control: Arc<dyn ControlSocket> = Arc::new(control.clone());
        let mut runtime = Runtime::new(
            runtime_control,
            command("shutdown"),
            screen,
            Arc::new(executor.clone()),
            ViewerMode::Asleep,
        );

        runtime
            .wake_up(TransitionSource::Manual)
            .expect("wake should succeed");

        let calls = executor.calls();
        let events = control.events();
        let call_guard = calls.lock().expect("executor calls poisoned");
        let event_guard = events.lock().expect("control events poisoned");
        assert_eq!(call_guard.len(), 1);
        assert_eq!(call_guard[0].0, "screen-on");
        assert_eq!(event_guard.len(), 1);
        assert_eq!(event_guard[0].0, ViewerMode::Awake);
        assert!(call_guard[0].1 <= event_guard[0].1);
    }

    #[test]
    fn sleep_request_waits_before_power_off() {
        let executor = RecordingExecutor::new();
        let control = RecordingControlSocket::new();
        let detector = StaticDetector::new(ScreenState::On);
        let off_delay = Duration::from_millis(40);

        let screen = ScreenRuntime::new(
            command("screen-on"),
            command("screen-off"),
            off_delay,
            Some("HDMI-A-1".into()),
            Arc::new(executor.clone()),
            Arc::new(detector),
        );

        let runtime_control: Arc<dyn ControlSocket> = Arc::new(control.clone());
        let mut runtime = Runtime::new(
            runtime_control,
            command("shutdown"),
            screen,
            Arc::new(executor.clone()),
            ViewerMode::Awake,
        );

        runtime
            .go_to_sleep(TransitionSource::Manual)
            .expect("sleep should succeed");

        let calls = executor.calls();
        let events = control.events();
        let call_guard = calls.lock().expect("executor calls poisoned");
        let event_guard = events.lock().expect("control events poisoned");
        assert_eq!(call_guard.len(), 1);
        assert_eq!(call_guard[0].0, "screen-off");
        assert_eq!(event_guard.len(), 1);
        assert_eq!(event_guard[0].0, ViewerMode::Asleep);
        assert!(event_guard[0].1 <= call_guard[0].1);
        assert!(
            call_guard[0].1.saturating_duration_since(event_guard[0].1)
                >= off_delay - Duration::from_millis(5)
        );
    }

    #[test]
    fn control_socket_emits_set_state_json() {
        let dir = tempdir().expect("tempdir");
        let socket_path = dir.path().join("control.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind control socket");

        let socket = UnixControlSocket::new(socket_path.clone());
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut buf = Vec::new();
            stream.read_to_end(&mut buf).expect("read payload");
            String::from_utf8(buf).expect("utf8 payload")
        });

        socket
            .send_set_state(ViewerMode::Awake)
            .expect("send payload");

        let payload = handle.join().expect("server thread");
        assert_eq!(payload, r#"{"command":"set-state","state":"awake"}"#);
    }

    #[test]
    fn scheduler_delays_initial_wake_until_greeting() {
        let config = SchedulerConfig {
            schedule: always_awake_schedule(),
            greeting_delay: Duration::from_millis(60),
            sleep_grace: Duration::from_millis(0),
        };
        let state = Arc::new(Mutex::new(FrameState::new(ViewerMode::Asleep)));
        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn({
            let config = config.clone();
            let state = Arc::clone(&state);
            move || scheduler_loop(config, state, tx)
        });

        let start = Instant::now();
        let command = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("scheduler wake");
        assert_eq!(command, SchedulerCommand::WakeUp);
        assert!(start.elapsed() >= Duration::from_millis(60));

        drop(rx);
        handle.join().expect("scheduler thread");
    }

    #[test]
    fn scheduler_respects_manual_sleep_grace() {
        let config = SchedulerConfig {
            schedule: always_asleep_schedule(),
            greeting_delay: Duration::from_millis(0),
            sleep_grace: Duration::from_millis(80),
        };
        let state = Arc::new(Mutex::new(FrameState::new(ViewerMode::Awake)));
        {
            let mut guard = state.lock().expect("state poisoned");
            guard.update(ViewerMode::Awake, TransitionSource::Manual);
        }

        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn({
            let config = config.clone();
            let state = Arc::clone(&state);
            move || scheduler_loop(config, state, tx)
        });

        assert!(rx.recv_timeout(Duration::from_millis(40)).is_err());
        let command = rx
            .recv_timeout(Duration::from_millis(200))
            .expect("scheduler sleep");
        assert_eq!(command, SchedulerCommand::GoToSleep);

        drop(rx);
        handle.join().expect("scheduler thread");
    }

    #[test]
    fn scheduler_throttles_duplicate_commands() {
        let config = SchedulerConfig {
            schedule: always_awake_schedule(),
            greeting_delay: Duration::from_millis(0),
            sleep_grace: Duration::from_millis(0),
        };
        let state = Arc::new(Mutex::new(FrameState::new(ViewerMode::Asleep)));

        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn({
            let config = config.clone();
            let state = Arc::clone(&state);
            move || scheduler_loop(config, state, tx)
        });

        let command = rx
            .recv_timeout(Duration::from_millis(200))
            .expect("scheduler wake");
        assert_eq!(command, SchedulerCommand::WakeUp);

        thread::sleep(Duration::from_millis(250));
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));

        drop(rx);
        handle.join().expect("scheduler thread");
    }
}
