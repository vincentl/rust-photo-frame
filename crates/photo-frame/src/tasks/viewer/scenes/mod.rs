//! Viewer scene definitions.
//!
//! This module will house the logic for state-specific viewer behaviour.

use std::collections::VecDeque;
use std::time::Instant;

use rand::Rng;
use tokio::sync::mpsc::Sender;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::WindowId;

use crate::config::TransitionConfig;
use crate::events::Displayed;

use super::{ImgTex, TransitionState};

/// Shared callbacks that each viewer scene must implement.
///
/// The [`Scene`] trait mirrors the hooks currently implemented inside
/// `tasks/viewer.rs` on the top-level application type. Each concrete scene
/// will provide state-specific behaviour for these callbacks in a future
/// refactor.
#[allow(dead_code)]
pub(super) trait Scene {
    /// Called when the viewer should transition into the greeting scene.
    fn enter_greeting(&mut self) {}

    /// Called when the viewer should transition into the wake (slideshow) scene.
    fn enter_wake(&mut self) {}

    /// Called when the viewer should transition into the sleep scene.
    fn enter_sleep(&mut self) {}

    /// Called on each tick from the control loop.
    fn process_tick(&mut self, _event_loop: &ActiveEventLoop) {}

    /// Called when the scene should request a redraw.
    fn request_redraw(&mut self) {}

    /// Handles window events targeted at the viewer window.
    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        _event: WindowEvent,
    ) {
    }

    /// Called right before the event loop goes idle.
    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {}

    /// Handles viewer-specific user events dispatched through the event loop.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: super::ViewerEvent) {}
}

/// State container for the wake (slideshow) scene.
pub(super) struct WakeScene {
    current: Option<ImgTex>,
    next: Option<ImgTex>,
    transition_state: Option<TransitionState>,
    displayed_at: Option<Instant>,
    pending: VecDeque<ImgTex>,
    pending_redraw: bool,
    dwell_ms: u64,
    transition_cfg: TransitionConfig,
}

impl WakeScene {
    /// Creates a new [`WakeScene`] configured with the slideshow dwell and transition settings.
    pub(super) fn new(dwell_ms: u64, transition_cfg: TransitionConfig) -> Self {
        Self {
            current: None,
            next: None,
            transition_state: None,
            displayed_at: None,
            pending: VecDeque::new(),
            pending_redraw: false,
            dwell_ms,
            transition_cfg,
        }
    }

    /// Clears all slideshow state, returning the scene to its initial idle state.
    pub(super) fn reset(&mut self) {
        self.current = None;
        self.next = None;
        self.transition_state = None;
        self.displayed_at = None;
        self.pending.clear();
        self.pending_redraw = false;
    }

    /// Returns the currently displayed image, if present.
    pub(super) fn current(&self) -> Option<&ImgTex> {
        self.current.as_ref()
    }

    /// Sets the currently displayed image.
    pub(super) fn set_current(&mut self, current: Option<ImgTex>) {
        self.current = current;
    }

    /// Returns the next staged image.
    pub(super) fn next(&self) -> Option<&ImgTex> {
        self.next.as_ref()
    }

    /// Sets the next staged image.
    pub(super) fn set_next(&mut self, next: Option<ImgTex>) {
        self.next = next;
    }

    /// Takes the next staged image, if present.
    pub(super) fn take_next(&mut self) -> Option<ImgTex> {
        self.next.take()
    }

    /// Provides mutable access to the pending slideshow queue.
    pub(super) fn pending_mut(&mut self) -> &mut VecDeque<ImgTex> {
        &mut self.pending
    }

    /// Provides immutable access to the pending slideshow queue.
    pub(super) fn pending(&self) -> &VecDeque<ImgTex> {
        &self.pending
    }

    /// Marks that the wake scene should request a redraw on the next loop.
    pub(super) fn mark_redraw_needed(&mut self) {
        self.pending_redraw = true;
    }

    /// Returns whether a redraw is pending for the wake scene.
    pub(super) fn needs_redraw(&self) -> bool {
        self.pending_redraw
    }

    /// Clears the redraw flag, returning whether one had been set.
    pub(super) fn take_redraw_needed(&mut self) -> bool {
        std::mem::take(&mut self.pending_redraw)
    }

    /// Returns the timestamp when the current image started displaying.
    pub(super) fn displayed_at(&self) -> Option<Instant> {
        self.displayed_at
    }

    /// Updates the timestamp when the current image started displaying.
    pub(super) fn set_displayed_at(&mut self, instant: Option<Instant>) {
        self.displayed_at = instant;
    }

    /// Exposes the current transition state for rendering.
    pub(super) fn transition_state(&self) -> Option<&TransitionState> {
        self.transition_state.as_ref()
    }

    /// Replaces the active transition state.
    pub(super) fn set_transition_state(&mut self, state: Option<TransitionState>) {
        self.transition_state = state;
    }

    /// Finalizes an in-flight transition, promoting the staged image to current if complete.
    pub(super) fn finalize_transition(&mut self, to_manager_displayed: &Sender<Displayed>) {
        if self
            .transition_state
            .as_ref()
            .is_some_and(TransitionState::is_complete)
        {
            let state = self
                .transition_state
                .take()
                .expect("transition state should exist when complete");
            if let Some(next) = self.next.take() {
                let path = next.path.clone();
                tracing::debug!(
                    "transition_end kind={} path={} queue_depth={}",
                    state.kind(),
                    path.display(),
                    self.pending.len()
                );
                self.current = Some(next);
                self.pending_redraw = true;
                self.displayed_at = Some(Instant::now());
                let _ = to_manager_displayed.try_send(Displayed(path));
            }
        }
    }

    /// Starts a transition when the dwell time elapses and staged images are available.
    pub(super) fn maybe_start_transition(&mut self, rng: &mut impl Rng) {
        if self.transition_state.is_some() {
            return;
        }
        let Some(shown_at) = self.displayed_at else {
            return;
        };
        if shown_at.elapsed() < std::time::Duration::from_millis(self.dwell_ms) {
            return;
        }
        if self.next.is_none() {
            if let Some(stage) = self.pending.pop_front() {
                tracing::debug!(
                    "transition_stage path={} queue_depth={}",
                    stage.path.display(),
                    self.pending.len()
                );
                self.next = Some(stage);
            }
        }
        if self.next.is_some() && self.current.is_some() {
            let option = self.transition_cfg.choose_option(rng);
            let kind = option.kind();
            let state = TransitionState::new(option, Instant::now(), rng);
            if let Some(next) = &self.next {
                tracing::debug!(
                    "transition_start kind={} path={} queue_depth={}",
                    kind,
                    next.path.display(),
                    self.pending.len()
                );
            }
            self.transition_state = Some(state);
        }
    }
}

impl Scene for WakeScene {
    fn enter_wake(&mut self) {
        self.pending_redraw = true;
        if self.displayed_at.is_some() {
            self.displayed_at = Some(Instant::now());
        }
    }
}
