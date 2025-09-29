use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};

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

#[derive(Debug)]
struct DisplayPowerInner {
    sysfs: Option<BacklightSysfs>,
    sleep_command: Option<String>,
    wake_command: Option<String>,
}

impl DisplayPowerController {
    pub fn new(plan: DisplayPowerPlan) -> Result<Self> {
        if plan.sysfs.is_none() && plan.sleep_command.is_none() && plan.wake_command.is_none() {
            return Err(anyhow!(
                "display power plan must configure at least one sysfs path or command"
            ));
        }

        if let Some(cmd) = plan.sleep_command.as_deref() {
            ensure_not_blank(cmd, "sleep command")?;
        }
        if let Some(cmd) = plan.wake_command.as_deref() {
            ensure_not_blank(cmd, "wake command")?;
        }

        Ok(Self {
            inner: Arc::new(DisplayPowerInner {
                sysfs: plan.sysfs,
                sleep_command: plan.sleep_command,
                wake_command: plan.wake_command,
            }),
        })
    }

    pub fn sleep(&self) -> Result<()> {
        self.inner.perform(PowerAction::Sleep)
    }

    pub fn wake(&self) -> Result<()> {
        self.inner.perform(PowerAction::Wake)
    }
}

impl DisplayPowerInner {
    fn perform(&self, action: PowerAction) -> Result<()> {
        let mut errors = Vec::new();

        if let Some(sysfs) = &self.sysfs {
            if let Err(err) = sysfs.write(action) {
                errors.push(err);
            }
        }

        if let Some(command) = self.command_for(action) {
            if let Err(err) = run_command(command) {
                errors.push(anyhow!(
                    "failed to run {action:?} command '{command}': {err}"
                ));
            }
        }

        match errors.len() {
            0 => Ok(()),
            1 => Err(errors.into_iter().next().unwrap()),
            _ => {
                let message = errors
                    .into_iter()
                    .map(|err| err.to_string())
                    .collect::<Vec<_>>()
                    .join("; ");
                Err(anyhow!(message))
            }
        }
    }

    fn command_for(&self, action: PowerAction) -> Option<&str> {
        match action {
            PowerAction::Sleep => self.sleep_command.as_deref(),
            PowerAction::Wake => self.wake_command.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum PowerAction {
    Sleep,
    Wake,
}

impl BacklightSysfs {
    fn write(&self, action: PowerAction) -> Result<()> {
        let value = match action {
            PowerAction::Sleep => &self.sleep_value,
            PowerAction::Wake => &self.wake_value,
        };
        fs::write(&self.path, value)
            .with_context(|| format!("failed to write '{}' to {}", value, self.path.display()))
    }
}

fn run_command(command: &str) -> Result<()> {
    let status = Command::new("sh")
        .arg("-c")
        .arg(command)
        .status()
        .with_context(|| format!("failed to spawn shell for command: {command}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "command exited with status {}: {command}",
            status.code().unwrap_or(-1)
        ))
    }
}

fn ensure_not_blank(value: &str, label: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(anyhow!("{label} must not be blank"))
    } else {
        Ok(())
    }
}
