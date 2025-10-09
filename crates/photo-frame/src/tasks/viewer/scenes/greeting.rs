use winit::dpi::PhysicalSize;

use super::{RenderCtx, RenderResult, Scene, SceneContext};
use crate::config::GreetingScreenConfig;
use crate::tasks::greeting_screen::GreetingScreen;

pub struct GreetingScene {
    screen: GreetingScreen,
    config: GreetingScreenConfig,
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
            config: config.clone(),
            needs_redraw: true,
        }
    }

    pub fn update_config(&mut self, config: &GreetingScreenConfig) {
        self.config = config.clone();
        self.screen.update_screen(self.config.screen());
        self.needs_redraw = true;
    }

    fn resize(&mut self, new_size: PhysicalSize<u32>, scale_factor: f64) {
        self.screen.resize(new_size, scale_factor);
        self.needs_redraw = true;
    }
}

impl Scene for GreetingScene {
    fn on_enter(&mut self, ctx: &SceneContext) {
        self.resize(ctx.surface_size(), ctx.window.scale_factor());
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
            let _ = self.screen.render(ctx.encoder, ctx.target_view);
            self.needs_redraw = false;
        }
        RenderResult::Idle
    }
}
