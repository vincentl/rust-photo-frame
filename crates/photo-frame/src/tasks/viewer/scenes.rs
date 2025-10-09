use winit::{dpi::PhysicalSize, window::Window};

pub mod asleep;
pub mod awake;
pub mod greeting;

#[allow(dead_code)]
pub struct SceneContext<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub surface_config: &'a wgpu::SurfaceConfiguration,
    pub window: &'a Window,
}

impl<'a> SceneContext<'a> {
    pub fn surface_size(&self) -> PhysicalSize<u32> {
        PhysicalSize::new(self.surface_config.width, self.surface_config.height)
    }
}

#[allow(dead_code)]
pub struct RenderCtx<'a, 'b> {
    pub scene: SceneContext<'a>,
    pub encoder: &'b mut wgpu::CommandEncoder,
    pub target_view: &'b wgpu::TextureView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum RenderResult {
    Idle,
    NeedsRedraw,
}

pub trait Scene {
    fn on_enter(&mut self, _ctx: &SceneContext) {}
    fn on_exit(&mut self, _ctx: &SceneContext) {}
    fn handle_resize(
        &mut self,
        _ctx: &SceneContext,
        _new_size: PhysicalSize<u32>,
        _scale_factor: f64,
    ) {
    }
    fn render(&mut self, ctx: &mut RenderCtx<'_, '_>) -> RenderResult;
}
