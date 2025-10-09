use super::{RenderCtx, RenderResult, Scene, SceneContext};

pub struct AsleepScene;

impl AsleepScene {
    pub fn new(/* deps from existing config */) -> Self {
        Self
    }
}

impl Scene for AsleepScene {
    fn on_enter(&mut self, _ctx: &SceneContext) {}

    fn on_exit(&mut self, _ctx: &SceneContext) {}

    fn handle_resize(
        &mut self,
        _ctx: &SceneContext,
        _new_size: winit::dpi::PhysicalSize<u32>,
        _scale_factor: f64,
    ) {
    }

    fn render(&mut self, ctx: &mut RenderCtx<'_, '_>) -> RenderResult {
        let _ = ctx;
        // TODO: render sleep banner once assets are in place.
        RenderResult::Idle
    }
}
