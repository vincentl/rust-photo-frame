//! Viewer scene definitions.
//!
//! This module will house the logic for state-specific viewer behaviour.

use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::WindowId;

/// Shared callbacks that each viewer scene must implement.
///
/// The [`Scene`] trait mirrors the hooks currently implemented inside
/// `tasks/viewer.rs` on the top-level application type. Each concrete scene
/// will provide state-specific behaviour for these callbacks in a future
/// refactor.
pub trait Scene {
    /// Called when the viewer should transition into the greeting scene.
    fn enter_greeting(&mut self);

    /// Called when the viewer should transition into the wake (slideshow) scene.
    fn enter_wake(&mut self);

    /// Called when the viewer should transition into the sleep scene.
    fn enter_sleep(&mut self);

    /// Called on each tick from the control loop.
    fn process_tick(&mut self, event_loop: &ActiveEventLoop);

    /// Called when the scene should request a redraw.
    fn request_redraw(&mut self);

    /// Handles window events targeted at the viewer window.
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    );

    /// Called right before the event loop goes idle.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop);

    /// Handles viewer-specific user events dispatched through the event loop.
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: super::ViewerEvent);
}
