use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitStatus};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Default)]
pub struct DisplayPowerPlan {
    pub sysfs: Option<BacklightSysfs>,
    pub sleep_command: Option<String>,
    pub wake_command: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BacklightSysfs {
    pub path: PathBuf,
    pub sleep_value: String,
    pub wake_value: String,
}

#[derive(Debug, Clone)]
pub struct DisplayPowerController {
    inner: Arc<DisplayPowerInner>,
}

#[derive(Debug, Clone)]
pub struct PowerCommandReport {
    pub action: PowerAction,
    pub output: Option<OutputSelection>,
    pub sysfs: Vec<SysfsExecution>,
    pub commands: Vec<CommandExecution>,
}

impl PowerCommandReport {
    pub fn success(&self) -> bool {
        self.sysfs.iter().any(|s| s.success) || self.commands.iter().any(|c| c.success)
    }
}

#[derive(Debug, Clone)]
pub struct SysfsExecution {
    pub path: PathBuf,
    pub value: String,
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CommandExecution {
    pub command: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone)]
pub struct OutputSelection {
    pub name: String,
    pub source: OutputSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputSource {
    Autodetected,
    Fallback,
}

type CommandRunner = Arc<dyn Fn(&str) -> Result<CommandOutput> + Send + Sync>;

#[derive(Debug, Clone)]
struct CommandTemplate {
    raw: String,
    needs_output: bool,
}

struct DisplayPowerInner {
    sysfs: Option<BacklightSysfs>,
    sleep_command: Option<CommandTemplate>,
    wake_command: Option<CommandTemplate>,
    runner: CommandRunner,
    output_cache: Mutex<Option<OutputSelection>>,
}

impl fmt::Debug for DisplayPowerInner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DisplayPowerInner")
            .field("sysfs", &self.sysfs)
            .field("sleep_command", &self.sleep_command)
            .field("wake_command", &self.wake_command)
            .field(
                "has_output_cache",
                &self.output_cache.lock().map(|c| c.is_some()),
            )
            .finish()
    }
}

impl DisplayPowerController {
    pub fn new(plan: DisplayPowerPlan) -> Result<Self> {
        Self::build(plan, default_runner())
    }

    pub fn sleep(&self) -> PowerCommandReport {
        self.inner.perform(PowerAction::Sleep)
    }

    pub fn wake(&self) -> PowerCommandReport {
        self.inner.perform(PowerAction::Wake)
    }

    fn build(plan: DisplayPowerPlan, runner: CommandRunner) -> Result<Self> {
        let DisplayPowerPlan {
            sysfs,
            sleep_command,
            wake_command,
        } = plan;

        if sysfs.is_none() && sleep_command.is_none() && wake_command.is_none() {
            return Err(anyhow!(
                "display power plan must configure at least one sysfs path or command"
            ));
        }

        let sleep_command = sleep_command.map(|cmd| {
            ensure_not_blank(&cmd, "sleep command")?;
            Ok::<_, anyhow::Error>(CommandTemplate::new(cmd))
        });
        let wake_command = wake_command.map(|cmd| {
            ensure_not_blank(&cmd, "wake command")?;
            Ok::<_, anyhow::Error>(CommandTemplate::new(cmd))
        });

        Ok(Self {
            inner: Arc::new(DisplayPowerInner {
                sysfs,
                sleep_command: sleep_command.transpose()?,
                wake_command: wake_command.transpose()?,
                runner,
                output_cache: Mutex::new(None),
            }),
        })
    }

    #[cfg(test)]
    fn with_runner(plan: DisplayPowerPlan, runner: CommandRunner) -> Result<Self> {
        Self::build(plan, runner)
    }
}

impl DisplayPowerInner {
    fn perform(&self, action: PowerAction) -> PowerCommandReport {
        let mut report = PowerCommandReport {
            action,
            output: None,
            sysfs: Vec::new(),
            commands: Vec::new(),
        };

        if let Some(sysfs) = &self.sysfs {
            report.sysfs.push(sysfs.execute(action));
        }

        if let Some(template) = self.command_for(action) {
            match self.prepare_command(template) {
                PreparedCommand::Ready { command, selection } => {
                    if report.output.is_none() {
                        report.output = selection.clone();
                    }
                    let execution = self.run_shell(&command);
                    report.commands.push(execution.clone());
                    if execution.success {
                        if let Some(sel) = selection {
                            debug!(
                                ?action,
                                output = sel.name,
                                source = ?sel.source,
                                command = command,
                                "display power command succeeded"
                            );
                        } else {
                            debug!(
                                ?action,
                                command = command,
                                "display power command succeeded"
                            );
                        }
                    } else {
                        let exit = execution
                            .exit_code
                            .map(|code| code.to_string())
                            .unwrap_or_else(|| "signal".to_string());
                        warn!(
                            ?action,
                            exit_code = exit,
                            stderr = execution.stderr,
                            command = command,
                            "display power command failed"
                        );
                    }
                }
                PreparedCommand::Skipped { reason } => {
                    report.commands.push(CommandExecution {
                        command: reason.clone(),
                        success: false,
                        exit_code: None,
                        stdout: String::new(),
                        stderr: String::new(),
                    });
                    warn!(?action, reason, "skipping display power command");
                }
            }
        }

        report
    }

    fn command_for(&self, action: PowerAction) -> Option<&CommandTemplate> {
        match action {
            PowerAction::Sleep => self.sleep_command.as_ref(),
            PowerAction::Wake => self.wake_command.as_ref(),
        }
    }

    fn prepare_command(&self, template: &CommandTemplate) -> PreparedCommand {
        if !template.needs_output {
            return PreparedCommand::Ready {
                command: template.raw.clone(),
                selection: None,
            };
        }

        let selection = match self.resolve_output() {
            Some(sel) => sel,
            None => {
                return PreparedCommand::Skipped {
                    reason: "no connected outputs detected".to_string(),
                };
            }
        };
        let command = template.raw.replace("@OUTPUT@", &selection.name);
        PreparedCommand::Ready {
            command,
            selection: Some(selection),
        }
    }

    fn resolve_output(&self) -> Option<OutputSelection> {
        let mut cache = self.output_cache.lock().unwrap();
        if let Some(sel) = cache.clone() {
            return Some(sel);
        }

        match detect_output(&*self.runner) {
            OutputDetection::Detected { name } => {
                info!(output = name, "auto-detected Wayland output");
                let selection = OutputSelection {
                    name,
                    source: OutputSource::Autodetected,
                };
                *cache = Some(selection.clone());
                Some(selection)
            }
            OutputDetection::Fallback { name } => {
                warn!(output = name, "falling back to default output name");
                let selection = OutputSelection {
                    name,
                    source: OutputSource::Fallback,
                };
                *cache = Some(selection.clone());
                Some(selection)
            }
            OutputDetection::Unavailable => None,
        }
    }

    fn run_shell(&self, command: &str) -> CommandExecution {
        match (self.runner)(command) {
            Ok(output) => CommandExecution {
                command: command.to_string(),
                success: output.status.success(),
                exit_code: exit_code(&output.status),
                stdout: output.stdout,
                stderr: output.stderr,
            },
            Err(err) => CommandExecution {
                command: command.to_string(),
                success: false,
                exit_code: None,
                stdout: String::new(),
                stderr: err.to_string(),
            },
        }
    }
}

impl BacklightSysfs {
    fn execute(&self, action: PowerAction) -> SysfsExecution {
        let value = match action {
            PowerAction::Sleep => &self.sleep_value,
            PowerAction::Wake => &self.wake_value,
        };

        match fs::write(&self.path, value) {
            Ok(()) => {
                debug!(
                    path = %self.path.display(),
                    value,
                    ?action,
                    "wrote backlight value"
                );
                SysfsExecution {
                    path: self.path.clone(),
                    value: value.clone(),
                    success: true,
                    error: None,
                }
            }
            Err(err) => {
                warn!(
                    path = %self.path.display(),
                    value,
                    ?action,
                    error = %err,
                    "failed to write backlight value"
                );
                SysfsExecution {
                    path: self.path.clone(),
                    value: value.clone(),
                    success: false,
                    error: Some(err.to_string()),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PowerAction {
    Sleep,
    Wake,
}

#[derive(Debug)]
struct CommandOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

#[derive(Debug)]
enum PreparedCommand {
    Ready {
        command: String,
        selection: Option<OutputSelection>,
    },
    Skipped {
        reason: String,
    },
}

#[derive(Debug)]
enum OutputDetection {
    Detected { name: String },
    Fallback { name: String },
    Unavailable,
}

fn detect_output(runner: &dyn Fn(&str) -> Result<CommandOutput>) -> OutputDetection {
    match runner("wlr-randr") {
        Ok(output) if output.status.success() => {
            if let Some(name) = parse_wlr_randr_outputs(&output.stdout) {
                OutputDetection::Detected { name }
            } else {
                warn!("wlr-randr returned no connected outputs");
                OutputDetection::Unavailable
            }
        }
        Ok(output) => {
            warn!(
                exit = ?exit_code(&output.status),
                stderr = output.stderr,
                "wlr-randr command failed"
            );
            OutputDetection::Fallback {
                name: "HDMI-A-1".to_string(),
            }
        }
        Err(err) => {
            warn!(error = %err, "failed to invoke wlr-randr; using fallback output");
            OutputDetection::Fallback {
                name: "HDMI-A-1".to_string(),
            }
        }
    }
}

fn parse_wlr_randr_outputs(stdout: &str) -> Option<String> {
    let mut fallback = None;
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let name = parts.next()?;
        let status = parts.next().unwrap_or("");
        if status != "connected" {
            continue;
        }
        if !name.starts_with("eDP") && !name.starts_with("LVDS") {
            return Some(name.to_string());
        }
        if fallback.is_none() {
            fallback = Some(name.to_string());
        }
    }
    fallback
}

fn ensure_not_blank(value: &str, label: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(anyhow!("{label} must not be blank"))
    } else {
        Ok(())
    }
}

fn default_runner() -> CommandRunner {
    Arc::new(|command| run_shell(command))
}

fn run_shell(command: &str) -> Result<CommandOutput> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .with_context(|| format!("failed to spawn shell for command: {command}"))?;

    Ok(CommandOutput {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn exit_code(status: &ExitStatus) -> Option<i32> {
    status.code()
}

impl CommandTemplate {
    fn new(raw: String) -> Self {
        let needs_output = raw.contains("@OUTPUT@");
        Self { raw, needs_output }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex as StdMutex};

    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(not(unix))]
    use std::process::Command;

    fn status(code: i32) -> ExitStatus {
        #[cfg(unix)]
        {
            ExitStatus::from_raw((code & 0xff) << 8)
        }
        #[cfg(not(unix))]
        {
            if code == 0 {
                Command::new("true").status().unwrap()
            } else {
                let mut cmd = Command::new("sh");
                let status = cmd.arg("-c").arg(format!("exit {code}")).status().unwrap();
                status
            }
        }
    }

    #[derive(Clone)]
    struct StubRunner {
        responses: Arc<StdMutex<HashMap<String, Vec<CommandOutput>>>>,
    }

    impl StubRunner {
        fn new(map: HashMap<String, Vec<CommandOutput>>) -> Self {
            Self {
                responses: Arc::new(StdMutex::new(map)),
            }
        }

        fn into_runner(self) -> CommandRunner {
            Arc::new(move |command: &str| {
                let mut guard = self.responses.lock().unwrap();
                let list = guard
                    .get_mut(command)
                    .ok_or_else(|| anyhow!("no stubbed response for command '{command}'"))?;
                if list.is_empty() {
                    return Err(anyhow!("no more stubbed responses for command '{command}'"));
                }
                Ok(list.remove(0))
            })
        }
    }

    fn command_output(code: i32, stdout: &str, stderr: &str) -> CommandOutput {
        CommandOutput {
            status: status(code),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        }
    }

    #[test]
    fn replaces_output_placeholder_when_detected() {
        let mut map = HashMap::new();
        map.insert(
            "wlr-randr".to_string(),
            vec![command_output(0, "HDMI-A-1 connected 3840x2160@60Hz\n", "")],
        );
        map.insert(
            "wlr-randr --output HDMI-A-1 --off || vcgencmd display_power 0".to_string(),
            vec![command_output(0, "", "")],
        );

        let runner = StubRunner::new(map).into_runner();
        let plan = DisplayPowerPlan {
            sysfs: None,
            sleep_command: Some(
                "wlr-randr --output @OUTPUT@ --off || vcgencmd display_power 0".to_string(),
            ),
            wake_command: None,
        };
        let controller = DisplayPowerController::with_runner(plan, runner).unwrap();
        let report = controller.sleep();
        assert!(report.success());
        assert_eq!(report.commands.len(), 1);
        assert_eq!(
            report.commands[0].command,
            "wlr-randr --output HDMI-A-1 --off || vcgencmd display_power 0"
        );
        assert_eq!(
            report.output.as_ref().map(|sel| sel.name.clone()),
            Some("HDMI-A-1".to_string())
        );
        assert_eq!(
            report.output.as_ref().map(|sel| sel.source),
            Some(OutputSource::Autodetected)
        );
    }

    #[test]
    fn falls_back_when_detection_fails() {
        let mut map = HashMap::new();
        map.insert(
            "wlr-randr".to_string(),
            vec![command_output(1, "", "missing binary")],
        );
        map.insert(
            "wlr-randr --output HDMI-A-1 --on  || vcgencmd display_power 1".to_string(),
            vec![command_output(0, "", "")],
        );
        let runner = StubRunner::new(map).into_runner();
        let plan = DisplayPowerPlan {
            sysfs: None,
            sleep_command: None,
            wake_command: Some(
                "wlr-randr --output @OUTPUT@ --on  || vcgencmd display_power 1".to_string(),
            ),
        };

        let controller = DisplayPowerController::with_runner(plan, runner).unwrap();
        let report = controller.wake();
        assert!(report.success());
        assert_eq!(
            report.output.as_ref().map(|sel| sel.source),
            Some(OutputSource::Fallback)
        );
        assert_eq!(
            report.commands[0].command,
            "wlr-randr --output HDMI-A-1 --on  || vcgencmd display_power 1"
        );
    }

    #[test]
    fn reports_failure_when_no_outputs_present() {
        let mut map = HashMap::new();
        map.insert(
            "wlr-randr".to_string(),
            vec![command_output(
                0,
                "HDMI-A-1 disconnected\nDP-1 disconnected\n",
                "",
            )],
        );
        let runner = StubRunner::new(map).into_runner();
        let plan = DisplayPowerPlan {
            sysfs: None,
            sleep_command: Some("echo should-not-run @OUTPUT@".to_string()),
            wake_command: None,
        };
        let controller = DisplayPowerController::with_runner(plan, runner).unwrap();
        let report = controller.sleep();
        assert!(!report.success());
        assert!(report.commands.iter().all(|c| !c.success));
    }

    #[test]
    fn sysfs_execution_is_recorded() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        let sysfs = BacklightSysfs {
            path: path.clone(),
            sleep_value: "1".to_string(),
            wake_value: "0".to_string(),
        };
        let plan = DisplayPowerPlan {
            sysfs: Some(sysfs),
            sleep_command: None,
            wake_command: None,
        };
        let controller = DisplayPowerController::with_runner(plan, default_runner()).unwrap();
        let report = controller.sleep();
        assert_eq!(report.sysfs.len(), 1);
        assert!(report.sysfs[0].success);
        let contents = std::fs::read_to_string(path).unwrap();
        assert_eq!(contents, "1");
    }
}
