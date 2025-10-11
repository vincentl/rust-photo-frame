//! Viewer scene definitions.
//!
//! This module will house the logic for state-specific viewer behaviour.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use rand::Rng;
use tokio::sync::mpsc::Sender;
use wgpu::{CommandEncoder, TextureView};
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::config::{Configuration, TransitionConfig};
use crate::events::Displayed;
use crate::tasks::greeting_screen::GreetingScreen;

use super::{ImgTex, TransitionState};

struct OverlayScene {
    screen: GreetingScreen,
    layout_dirty: bool,
    redraw_pending: bool,
    size: PhysicalSize<u32>,
    scale_factor: f64,
}

impl OverlayScene {
    fn new(screen: GreetingScreen) -> Self {
        Self {
            screen,
            layout_dirty: true,
            redraw_pending: false,
            size: PhysicalSize::new(0, 0),
            scale_factor: 1.0,
        }
    }

    fn resize(&mut self, new_size: PhysicalSize<u32>, scale_factor: f64) {
        if self.size == new_size && (self.scale_factor - scale_factor).abs() < f64::EPSILON {
            return;
        }
        self.size = new_size;
        self.scale_factor = scale_factor;
        self.screen.resize(new_size, scale_factor);
        self.mark_layout_dirty();
    }

    fn set_message(&mut self, message: impl Into<String>) {
        if self.screen.set_message(message) {
            self.mark_layout_dirty();
        }
    }

    fn ensure_layout_ready(&mut self) -> bool {
        if !self.layout_dirty {
            return true;
        }
        if self.screen.update_layout() {
            self.layout_dirty = false;
            true
        } else {
            false
        }
    }

    fn render(&mut self, encoder: &mut CommandEncoder, target_view: &TextureView) -> bool {
        if !self.ensure_layout_ready() {
            return false;
        }
        if !self.screen.render(encoder, target_view) {
            return false;
        }
        self.redraw_pending = false;
        true
    }

    fn mark_layout_dirty(&mut self) {
        self.layout_dirty = true;
        self.redraw_pending = true;
    }

    fn mark_redraw_needed(&mut self) {
        self.redraw_pending = true;
    }

    fn needs_redraw(&self) -> bool {
        self.redraw_pending
    }

    fn after_submit(&mut self) {
        self.screen.after_submit();
    }
}

/// State container for the greeting overlay scene.
pub(super) struct GreetingScene {
    overlay: OverlayScene,
}

impl GreetingScene {
    pub(super) fn new(screen: GreetingScreen) -> Self {
        Self {
            overlay: OverlayScene::new(screen),
        }
    }

    pub(super) fn resize(&mut self, new_size: PhysicalSize<u32>, scale_factor: f64) {
        self.overlay.resize(new_size, scale_factor);
    }

    pub(super) fn set_message(&mut self, message: impl Into<String>) {
        self.overlay.set_message(message);
    }

    pub(super) fn ensure_layout_ready(&mut self) -> bool {
        self.overlay.ensure_layout_ready()
    }

    pub(super) fn render(
        &mut self,
        encoder: &mut CommandEncoder,
        target_view: &TextureView,
    ) -> bool {
        self.overlay.render(encoder, target_view)
    }

    pub(super) fn mark_redraw_needed(&mut self) {
        self.overlay.mark_redraw_needed();
    }

    pub(super) fn needs_redraw(&self) -> bool {
        self.overlay.needs_redraw()
    }

    pub(super) fn after_submit(&mut self) {
        self.overlay.after_submit();
    }
}

impl Scene for GreetingScene {
    fn enter(&mut self, mut ctx: SceneContext<'_>) {
        if let Some(window) = ctx.window() {
            self.resize(window.inner_size(), window.scale_factor());
        }
        let message = ctx
            .config()
            .greeting_screen
            .screen()
            .message_or_default()
            .into_owned();
        self.set_message(message);
        self.mark_redraw_needed();
        ctx.request_redraw();
    }

    fn process_tick(&mut self, mut ctx: SceneContext<'_>) {
        if self.needs_redraw() {
            ctx.request_redraw();
        }
    }

    fn handle_resize(
        &mut self,
        mut ctx: SceneContext<'_>,
        new_size: PhysicalSize<u32>,
        scale_factor: f64,
    ) {
        self.resize(new_size, scale_factor);
        self.mark_redraw_needed();
        ctx.request_redraw();
    }

    fn handle_visibility(&mut self, mut ctx: SceneContext<'_>, is_visible: bool) {
        if is_visible {
            self.mark_redraw_needed();
            ctx.request_redraw();
        }
    }
}

/// State container for the sleep overlay scene.
pub(super) struct SleepScene {
    overlay: OverlayScene,
}

impl SleepScene {
    pub(super) fn new(screen: GreetingScreen) -> Self {
        Self {
            overlay: OverlayScene::new(screen),
        }
    }

    pub(super) fn resize(&mut self, new_size: PhysicalSize<u32>, scale_factor: f64) {
        self.overlay.resize(new_size, scale_factor);
    }

    pub(super) fn set_message(&mut self, message: impl Into<String>) {
        self.overlay.set_message(message);
    }

    pub(super) fn ensure_layout_ready(&mut self) -> bool {
        self.overlay.ensure_layout_ready()
    }

    pub(super) fn render(
        &mut self,
        encoder: &mut CommandEncoder,
        target_view: &TextureView,
    ) -> bool {
        self.overlay.render(encoder, target_view)
    }

    pub(super) fn mark_redraw_needed(&mut self) {
        self.overlay.mark_redraw_needed();
    }

    pub(super) fn needs_redraw(&self) -> bool {
        self.overlay.needs_redraw()
    }

    pub(super) fn after_submit(&mut self) {
        self.overlay.after_submit();
    }
}

impl Scene for SleepScene {
    fn enter(&mut self, mut ctx: SceneContext<'_>) {
        if let Some(window) = ctx.window() {
            self.resize(window.inner_size(), window.scale_factor());
        }
        let message = ctx
            .config()
            .sleep_screen
            .screen()
            .message_or_default()
            .into_owned();
        self.set_message(message);
        self.mark_redraw_needed();
        ctx.request_redraw();
    }

    fn process_tick(&mut self, mut ctx: SceneContext<'_>) {
        if self.needs_redraw() {
            ctx.request_redraw();
        }
    }

    fn handle_resize(
        &mut self,
        mut ctx: SceneContext<'_>,
        new_size: PhysicalSize<u32>,
        scale_factor: f64,
    ) {
        self.resize(new_size, scale_factor);
        self.mark_redraw_needed();
        ctx.request_redraw();
    }

    fn handle_visibility(&mut self, mut ctx: SceneContext<'_>, is_visible: bool) {
        if is_visible {
            self.mark_redraw_needed();
            ctx.request_redraw();
        }
    }
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
        if let Some(option) = transition_cfg.primary_option() {
            tracing::debug!(
                transition_kind = ?option.kind(),
                duration_ms = option.duration().as_millis(),
                "wake_scene_primary_transition_loaded"
            );
        }

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

    pub(super) fn enter_wake(&mut self) {
        self.pending_redraw = true;
        if self.displayed_at.is_some() {
            self.displayed_at = Some(Instant::now());
        }
    }

    fn ensure_redraw_requested(&mut self, ctx: &mut SceneContext<'_>) {
        let pending_redraw = self.needs_redraw();
        let has_transition = self.transition_state().is_some();
        if pending_redraw {
            self.take_redraw_needed();
        }
        if pending_redraw || has_transition {
            tracing::debug!(pending_redraw, has_transition, "viewer_request_redraw_wake");
            ctx.request_redraw();
        }
    }
}

/// Execution context shared with viewer scenes.
///
/// A [`SceneContext`] exposes the subset of the viewer application state that a scene may
/// interact with. This keeps scene logic focused on presentation concerns while preventing
/// direct access to unrelated subsystems.
pub(super) struct SceneContext<'a> {
    window: Option<&'a Window>,
    redraw: &'a mut dyn FnMut(),
    config: Arc<Configuration>,
}

impl<'a> SceneContext<'a> {
    /// Creates a new [`SceneContext`] scoped to the currently active viewer state.
    pub(super) fn new(
        window: Option<&'a Window>,
        redraw: &'a mut dyn FnMut(),
        config: Arc<Configuration>,
    ) -> Self {
        Self {
            window,
            redraw,
            config,
        }
    }

    /// Returns the active window handle, if the viewer has created one.
    pub(super) fn window(&self) -> Option<&'a Window> {
        self.window
    }

    /// Requests a redraw from the viewer event loop.
    pub(super) fn request_redraw(&mut self) {
        (self.redraw)();
    }

    /// Returns the application configuration.
    pub(super) fn config(&self) -> &Configuration {
        &self.config
    }
}

/// Common interface implemented by each viewer scene (greeting, wake, sleep).
pub(super) trait Scene {
    /// Called when the scene becomes active.
    fn enter(&mut self, _ctx: SceneContext<'_>) {}

    /// Called before the scene is deactivated.
    fn exit(&mut self, _ctx: SceneContext<'_>) {}

    /// Called from the event loop `about_to_wait` hook.
    fn about_to_wait(&mut self, _ctx: SceneContext<'_>) {}

    /// Called when the viewer processes a periodic tick.
    fn process_tick(&mut self, _ctx: SceneContext<'_>) {}

    /// Called when the window is resized.
    fn handle_resize(
        &mut self,
        _ctx: SceneContext<'_>,
        _new_size: PhysicalSize<u32>,
        _scale_factor: f64,
    ) {
    }

    /// Called when the window scale factor changes.
    fn handle_scale_factor_changed(
        &mut self,
        ctx: SceneContext<'_>,
        new_size: PhysicalSize<u32>,
        scale_factor: f64,
    ) {
        self.handle_resize(ctx, new_size, scale_factor);
    }

    /// Called when the window occlusion state changes.
    fn handle_visibility(&mut self, _ctx: SceneContext<'_>, _is_visible: bool) {}
}

impl Scene for WakeScene {
    fn enter(&mut self, mut ctx: SceneContext<'_>) {
        self.enter_wake();
        ctx.request_redraw();
    }

    fn about_to_wait(&mut self, mut ctx: SceneContext<'_>) {
        self.ensure_redraw_requested(&mut ctx);
    }

    fn process_tick(&mut self, mut ctx: SceneContext<'_>) {
        self.ensure_redraw_requested(&mut ctx);
    }

    fn handle_resize(
        &mut self,
        mut ctx: SceneContext<'_>,
        _new_size: PhysicalSize<u32>,
        _scale_factor: f64,
    ) {
        self.mark_redraw_needed();
        ctx.request_redraw();
    }

    fn handle_visibility(&mut self, mut ctx: SceneContext<'_>, is_visible: bool) {
        if is_visible {
            self.mark_redraw_needed();
            ctx.request_redraw();
        }
    }
}
