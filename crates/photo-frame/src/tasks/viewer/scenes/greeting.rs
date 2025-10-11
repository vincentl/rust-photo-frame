use winit::dpi::PhysicalSize;

use super::{RenderCtx, RenderResult, Scene, SceneContext, ScenePresentEvent};
use crate::config::GreetingScreenConfig;
use crate::tasks::greeting_screen::GreetingScreen;

pub struct GreetingScene {
    screen: GreetingScreen,
    needs_redraw: bool,
}

impl GreetingScene {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        config: &GreetingScreenConfig,
    ) -> Self {
        let screen = GreetingScreen::new(device, queue, format, config.screen());
        tracing::debug!("greeting_screen_new completed {config:?}");
        Self {
            screen,
            needs_redraw: true,
        }
    }

    fn resize(&mut self, new_size: PhysicalSize<u32>, scale_factor: f64) {
        self.screen.resize(new_size, scale_factor);
        tracing::debug!("greeting_screen_resize {new_size:?} {scale_factor:?}");
        self.needs_redraw = true;
    }
}

impl Scene for GreetingScene {
    fn on_enter(&mut self, ctx: &SceneContext) {
        self.resize(ctx.surface_size(), ctx.window.scale_factor());
        tracing::debug!("greeting_screen about to call update layout");
        if self.screen.update_layout() {
            tracing::debug!(size = ?ctx.surface_size(), "greeting_scene_layout_ready");
        }
    }

    fn handle_resize(
        &mut self,
        _ctx: &SceneContext,
        new_size: PhysicalSize<u32>,
        scale_factor: f64,
    ) {
        self.resize(new_size, scale_factor);
        if self.screen.update_layout() {
            tracing::debug!("greeting_scene_layout_ready_after_resize");
        }
    }

    fn render(&mut self, ctx: &mut RenderCtx<'_, '_>) -> RenderResult {
        tracing::debug!(self.needs_redraw, "greeting_screen render");
        if self.needs_redraw {
            let drew = self.screen.render(ctx.encoder, ctx.target_view);
            self.needs_redraw = !drew;
            if !drew {
                tracing::debug!("greeting_scene_render_pending");
                return RenderResult::NeedsRedraw;
            }
            tracing::debug!("greeting_scene_render_complete");
        }
        RenderResult::Idle
    }

    fn after_present(&mut self, _ctx: &SceneContext) -> Option<ScenePresentEvent> {
        self.screen.after_submit();
        None
    }
}
