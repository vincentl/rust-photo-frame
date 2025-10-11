use winit::dpi::PhysicalSize;

use super::{RenderCtx, RenderResult, Scene, SceneContext, ScenePresentEvent};
use crate::config::SleepScreenConfig;
use crate::tasks::greeting_screen::GreetingScreen;

pub struct AsleepScene {
    screen: GreetingScreen,
    needs_redraw: bool,
}

impl AsleepScene {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        config: &SleepScreenConfig,
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

impl Scene for AsleepScene {
    fn on_enter(&mut self, ctx: &SceneContext) {
        self.resize(ctx.surface_size(), ctx.window.scale_factor());
        let _ = self.screen.update_layout();
    }

    fn handle_resize(
        &mut self,
        _ctx: &SceneContext,
        new_size: PhysicalSize<u32>,
        scale_factor: f64,
    ) {
        self.resize(new_size, scale_factor);
        let _ = self.screen.update_layout();
    }

    fn render(&mut self, ctx: &mut RenderCtx<'_, '_>) -> RenderResult {
        if self.needs_redraw {
            let _ = self.screen.render(ctx.encoder, ctx.target_view);
            self.needs_redraw = false;
        }
        RenderResult::Idle
    }

    fn after_present(&mut self, _ctx: &SceneContext) -> Option<ScenePresentEvent> {
        self.screen.after_submit();
        None
    }
}
