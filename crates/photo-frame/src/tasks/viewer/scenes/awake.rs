use super::{RenderCtx, RenderResult, Scene, SceneContext};

pub struct AwakeScene {
    pub paused: bool, // TODO: wire in pipelines
}

impl AwakeScene {
    pub fn new(/* deps */) -> Self {
        Self { paused: false }
    }

    pub fn pause(&mut self) {
        self.paused = true; // stop advancing
    }

    pub fn resume(&mut self) {
        self.paused = false; // resume advancing
    }
}

impl Scene for AwakeScene {
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
        if self.paused {
            return RenderResult::Idle;
        }
        // TODO: advance photo transitions once pipeline is ready.
        RenderResult::NeedsRedraw
    }
}
