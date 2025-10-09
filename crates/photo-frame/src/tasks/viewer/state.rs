use std::time::{Duration, Instant};

use crate::events::{ViewerCommand, ViewerState as ControlViewerState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerState {
    Greeting,
    Awake,
    Asleep,
}

#[derive(Debug, Clone, Copy)]
pub struct ViewerStateChange {
    pub from: ViewerState,
    pub to: ViewerState,
}

pub struct ViewerSM {
    state: ViewerState,
    entered_at: Instant,
    greeting_duration: Duration,
}

impl ViewerSM {
    pub fn new(greeting_duration: Duration, now: Instant) -> Self {
        Self {
            state: ViewerState::Greeting,
            entered_at: now,
            greeting_duration,
        }
    }
    pub fn current(&self) -> ViewerState {
        self.state
    }

    pub fn on_tick(&mut self, now: Instant) -> Option<ViewerStateChange> {
        if self.state == ViewerState::Greeting
            && now.duration_since(self.entered_at) >= self.greeting_duration
        {
            return self.goto(ViewerState::Awake, now);
        }
        None
    }

    pub fn on_command(&mut self, cmd: &ViewerCommand, now: Instant) -> Option<ViewerStateChange> {
        match *cmd {
            ViewerCommand::ToggleState => match self.state {
                ViewerState::Awake => self.goto(ViewerState::Asleep, now),
                ViewerState::Asleep => self.goto(ViewerState::Awake, now),
                ViewerState::Greeting => self.goto(ViewerState::Asleep, now),
            },
            ViewerCommand::SetState(ControlViewerState::Awake) => {
                self.goto(ViewerState::Awake, now)
            }
            ViewerCommand::SetState(ControlViewerState::Asleep) => {
                self.goto(ViewerState::Asleep, now)
            }
        }
    }

    pub fn on_photo_ready(&mut self, now: Instant) -> Option<ViewerStateChange> {
        if self.state == ViewerState::Greeting {
            return self.goto(ViewerState::Awake, now);
        }
        None
    }

    fn goto(&mut self, to: ViewerState, now: Instant) -> Option<ViewerStateChange> {
        if self.state == to {
            return None;
        }
        let ch = ViewerStateChange {
            from: self.state,
            to,
        };
        self.state = to;
        self.entered_at = now;
        Some(ch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{ViewerCommand, ViewerState as ControlViewerState};
    #[test]
    fn greeting_to_awake_by_time() {
        let start = Instant::now();
        let mut sm = ViewerSM::new(Duration::from_millis(100), start);
        assert_eq!(sm.current(), ViewerState::Greeting);
        assert!(sm.on_tick(start + Duration::from_millis(50)).is_none());
        let ch = sm.on_tick(start + Duration::from_millis(100)).unwrap();
        assert_eq!(
            (ch.from, ch.to),
            (ViewerState::Greeting, ViewerState::Awake)
        );
    }
    #[test]
    fn toggle_sleep_cycle() {
        let t0 = Instant::now();
        let mut sm = ViewerSM::new(Duration::from_millis(0), t0);
        sm.on_tick(t0); // Greeting → Awake immediately
        sm.on_tick(t0 + Duration::from_millis(1));
        assert_eq!(sm.current(), ViewerState::Awake);
        sm.on_command(&ViewerCommand::ToggleState, t0).unwrap();
        assert_eq!(sm.current(), ViewerState::Asleep);
        sm.on_command(&ViewerCommand::ToggleState, t0).unwrap();
        assert_eq!(sm.current(), ViewerState::Awake);
    }

    #[test]
    fn command_set_state() {
        let start = Instant::now();
        let mut sm = ViewerSM::new(Duration::from_millis(10), start);
        sm.on_photo_ready(start);
        sm.on_command(&ViewerCommand::SetState(ControlViewerState::Asleep), start)
            .unwrap();
        assert_eq!(sm.current(), ViewerState::Asleep);
        sm.on_command(&ViewerCommand::SetState(ControlViewerState::Awake), start)
            .unwrap();
        assert_eq!(sm.current(), ViewerState::Awake);
    }
}
