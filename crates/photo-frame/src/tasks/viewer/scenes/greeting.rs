use winit::dpi::PhysicalSize;

use super::{RenderCtx, RenderResult, Scene, SceneContext, ScenePresentEvent};
use crate::config::GreetingScreenConfig;
use crate::tasks::greeting_screen::{GreetingScreen, LayoutStatus};

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
        Self {
            screen,
            needs_redraw: true,
        }
    }

    fn resize(&mut self, new_size: PhysicalSize<u32>, scale_factor: f64) {
        self.screen.resize(new_size, scale_factor);
        self.needs_redraw = true;
    }
}

impl Scene for GreetingScene {
    fn on_enter(&mut self, ctx: &SceneContext) {
        self.resize(ctx.surface_size(), ctx.window.scale_factor());
        match self.screen.ensure_layout_ready() {
            LayoutStatus::Ready => {
                tracing::debug!(size = ?ctx.surface_size(), "greeting_scene_layout_ready")
            }
            LayoutStatus::WaitingForSize => tracing::debug!("greeting_scene_waiting_for_size"),
            LayoutStatus::WaitingForFont => tracing::debug!("greeting_scene_waiting_for_font"),
        }
    }

    fn handle_resize(
        &mut self,
        _ctx: &SceneContext,
        new_size: PhysicalSize<u32>,
        scale_factor: f64,
    ) {
        self.resize(new_size, scale_factor);
    }

    fn render(&mut self, ctx: &mut RenderCtx<'_, '_>) -> RenderResult {
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
