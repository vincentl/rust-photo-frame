use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use super::{RenderCtx, RenderResult, Scene, SceneContext, ScenePresentEvent};
use crate::events::PhotoLoaded;
use tracing::debug;

pub struct AwakeScene {
    current: Option<PhotoLoaded>,
    pending_display_report: Option<PathBuf>,
    display_color: wgpu::Color,
    needs_redraw: bool,
}

impl AwakeScene {
    pub fn new() -> Self {
        Self {
            current: None,
            pending_display_report: None,
            display_color: wgpu::Color::BLACK,
            needs_redraw: false,
        }
    }

    pub fn queue_photo(&mut self, photo: PhotoLoaded) {
        debug!(path = %photo.prepared.path.display(), "awake_scene_queue_photo");
        self.display_color = color_from_path(&photo.prepared.path);
        self.pending_display_report = Some(photo.prepared.path.clone());
        self.needs_redraw = true;
        self.current = Some(photo);
    }
}

impl Scene for AwakeScene {
    fn on_enter(&mut self, _ctx: &SceneContext) {
        self.needs_redraw = true;
    }

    fn on_exit(&mut self, _ctx: &SceneContext) {
        self.needs_redraw = false;
    }

    fn handle_resize(
        &mut self,
        _ctx: &SceneContext,
        _new_size: winit::dpi::PhysicalSize<u32>,
        _scale_factor: f64,
    ) {
        self.needs_redraw = true;
    }

    fn render(&mut self, ctx: &mut RenderCtx<'_, '_>) -> RenderResult {
        let size = ctx.scene.surface_size();
        let _ = ctx.scene.device.limits().max_texture_dimension_2d; // touch device to keep lints happy for now

        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("awake-scene-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: ctx.target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(self.display_color),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        });
        pass.set_viewport(0.0, 0.0, size.width as f32, size.height as f32, 0.0, 1.0);
        drop(pass);

        if self.needs_redraw {
            self.needs_redraw = false;
            RenderResult::Idle
        } else {
            RenderResult::Idle
        }
    }

    fn after_present(&mut self, _ctx: &SceneContext) -> Option<ScenePresentEvent> {
        if let Some(path) = self.pending_display_report.take() {
            self.current = None;
            return Some(ScenePresentEvent::PhotoDisplayed(path));
        }
        None
    }
}

fn color_from_path(path: &PathBuf) -> wgpu::Color {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    let bits = hasher.finish();
    let r = ((bits & 0xFF) as f64) / 255.0;
    let g = (((bits >> 8) & 0xFF) as f64) / 255.0;
    let b = (((bits >> 16) & 0xFF) as f64) / 255.0;
    wgpu::Color { r, g, b, a: 1.0 }
}
