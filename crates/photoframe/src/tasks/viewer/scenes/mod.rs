//! Viewer scene definitions.
//!
//! This module will house the logic for state-specific viewer behaviour.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use glyphon::{
    Attrs, Buffer, Color as GlyphonColor, FamilyOwned, FontSystem, Metrics, Shaping, SwashCache,
    Wrap,
};
use rand::Rng;
use wgpu::{CommandEncoder, TextureView};
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::config::{Configuration, MattingKind, TransitionConfig, TransitionKind};
use crate::tasks::greeting_screen::GreetingScreen;

use super::{ImgTex, TransitionState};

// ── Caption overlay ───────────────────────────────────────────────────────────

/// Uniform for compositing the cached caption texture (must match caption_composite.wgsl).
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct CompositeUniforms {
    resolution: [f32; 2],
    _pad0: [f32; 2],
    rect: [f32; 4],
}

/// Lightweight text overlay rendered on top of the live photo via `LoadOp::Load`.
/// Draws a single short line in the bottom-left corner, on a solid backing panel
/// so it stays legible over any mat (light, dark, or busy).
pub(super) struct CaptionOverlay {
    device: wgpu::Device,
    queue: wgpu::Queue,
    // Text is shaped (cosmic-text) and rasterized to a CPU buffer (swash), then
    // uploaded as one plain texture. We deliberately do NOT use glyphon's GPU
    // glyph atlas: its eviction/growth corrupts glyphs on the Pi's V3D driver.
    text_buffer: Buffer,
    font_system: FontSystem,
    swash_cache: SwashCache,
    // The finished caption (backing + text), rebuilt only on text/size change and
    // composited as a single quad every frame.
    cache_texture: Option<wgpu::Texture>,
    cache_view: Option<wgpu::TextureView>,
    cache_dims: (u32, u32),
    composite_pipeline: wgpu::RenderPipeline,
    composite_layout: wgpu::BindGroupLayout,
    composite_sampler: wgpu::Sampler,
    composite_uniform_buffer: wgpu::Buffer,
    composite_bind_group: Option<wgpu::BindGroup>,
    text: String,
    size: PhysicalSize<u32>,
    dirty: bool,
}

impl CaptionOverlay {
    pub(super) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
    ) -> Self {
        let mut font_system = FontSystem::new();
        let mut text_buffer = Buffer::new(&mut font_system, Metrics::new(28.0, 34.0));
        text_buffer.set_wrap(&mut font_system, Wrap::None);
        let swash_cache = SwashCache::new();

        // Composite pipeline: samples the cached caption texture and draws it as a
        // single premultiplied-alpha quad positioned in pixel space.
        let composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("caption-composite-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("caption_composite.wgsl").into()),
        });
        let composite_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caption-composite-uniforms"),
            size: std::mem::size_of::<CompositeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let composite_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("caption-composite-sampler"),
            // 1:1 blit (cache rendered at exact panel pixel size) keeps text crisp.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("caption-composite-bind-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<CompositeUniforms>() as u64,
                        ),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let composite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("caption-composite-pipeline-layout"),
                bind_group_layouts: &[&composite_layout],
                push_constant_ranges: &[],
            });
        let composite_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("caption-composite-pipeline"),
            layout: Some(&composite_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &composite_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &composite_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // Cache holds premultiplied alpha: src + dst * (1 - src.a).
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        Self {
            device: device.clone(),
            queue: queue.clone(),
            text_buffer,
            font_system,
            swash_cache,
            cache_texture: None,
            cache_view: None,
            cache_dims: (0, 0),
            composite_pipeline,
            composite_layout,
            composite_sampler,
            composite_uniform_buffer,
            composite_bind_group: None,
            text: String::new(),
            size: PhysicalSize::new(0, 0),
            dirty: false,
        }
    }

    pub(super) fn set_text(&mut self, text: impl Into<String>) {
        let t = text.into();
        if self.text != t {
            self.text = t;
            self.dirty = true;
        }
    }

    pub(super) fn resize(&mut self, new_size: PhysicalSize<u32>) {
        if self.size != new_size {
            self.size = new_size;
            self.dirty = true;
        }
    }

    pub(super) fn render(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
    ) -> bool {
        if self.size.width == 0 || self.size.height == 0 || self.text.is_empty() {
            return false;
        }

        // Rebuild the cached caption only when the text or surface changed.
        if self.dirty || self.cache_texture.is_none() {
            if !self.rebuild_cache() {
                return false;
            }
            self.dirty = false;
        }

        let Some(bind_group) = self.composite_bind_group.as_ref() else {
            return false;
        };

        // Place the cached panel at the bottom-left of the current surface.
        let margin = 20.0_f32;
        let pad_x = 14.0_f32;
        let pad_y = 8.0_f32;
        let line_h = 34.0_f32;
        let top = (self.size.height as f32 - line_h - margin).max(0.0);
        let rect_x = (margin - pad_x).max(0.0).floor();
        let rect_y = (top - pad_y).max(0.0).floor();
        let (cw, ch) = self.cache_dims;
        let uniforms = CompositeUniforms {
            resolution: [self.size.width as f32, self.size.height as f32],
            _pad0: [0.0, 0.0],
            rect: [rect_x, rect_y, cw as f32, ch as f32],
        };
        self.queue.write_buffer(
            &self.composite_uniform_buffer,
            0,
            bytemuck::bytes_of(&uniforms),
        );

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("caption-composite"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            occlusion_query_set: None,
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.composite_pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..6, 0..1);
        true
    }

    /// Shape the caption and rasterize it (dark backing + text) to a CPU buffer,
    /// then upload it as one plain texture. No GPU glyph atlas is involved, so the
    /// result can't be corrupted by atlas eviction/growth on any driver.
    fn rebuild_cache(&mut self) -> bool {
        fn srgb_to_linear(c: f32) -> f32 {
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }

        let pad_x = 14.0_f32;
        let pad_y = 8.0_f32;
        let line_h = 34.0_f32;

        // Shape the full line before measuring.
        self.text_buffer.set_metrics_and_size(
            &mut self.font_system,
            Metrics::new(28.0, 34.0),
            Some(self.size.width as f32),
            Some(self.size.height as f32),
        );
        let attrs = Attrs::new().family(FamilyOwned::SansSerif.as_family());
        self.text_buffer.set_text(
            &mut self.font_system,
            &self.text,
            &attrs,
            Shaping::Advanced,
            None,
        );
        self.text_buffer
            .shape_until_scroll(&mut self.font_system, false);

        let mut text_w = 0.0_f32;
        for run in self.text_buffer.layout_runs() {
            text_w = text_w.max(run.line_w);
        }
        if text_w <= 0.0 {
            return false;
        }

        let cw = (text_w + 2.0 * pad_x).ceil().max(1.0) as u32;
        let ch = (line_h + 2.0 * pad_y).ceil().max(1.0) as u32;

        // (Re)allocate the cache texture + composite bind group on size change.
        if self.cache_texture.is_none() || self.cache_dims != (cw, ch) {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("caption-cache-texture"),
                size: wgpu::Extent3d {
                    width: cw,
                    height: ch,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                // Linear (non-sRGB): the premultiplied bytes we upload are sampled
                // and blended as-is. COPY_DST for the upload; COPY_SRC for tests.
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("caption-composite-bind-group"),
                layout: &self.composite_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.composite_uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.composite_sampler),
                    },
                ],
            });
            self.cache_texture = Some(texture);
            self.cache_view = Some(view);
            self.composite_bind_group = Some(bind_group);
            self.cache_dims = (cw, ch);
        }

        // CPU pixel buffer in premultiplied-linear RGBA. Fill with the dark, ~72%
        // backing panel, then blend the glyphs on top.
        let panel_a = 0.72_f32;
        let panel = [
            0u8,
            (0.04 * panel_a * 255.0).round() as u8,
            (0.06 * panel_a * 255.0).round() as u8,
            (panel_a * 255.0).round() as u8,
        ];
        let mut buf = vec![0u8; (cw * ch * 4) as usize];
        for px in buf.chunks_exact_mut(4) {
            px.copy_from_slice(&panel);
        }

        let base_x = pad_x as i32;
        let base_y = pad_y as i32;
        let cw_i = cw as i32;
        let ch_i = ch as i32;
        let text_color = GlyphonColor::rgb(170, 244, 244);
        self.text_buffer.draw(
            &mut self.font_system,
            &mut self.swash_cache,
            text_color,
            |gx, gy, gw, gh, color| {
                let a = color.a() as f32 / 255.0;
                if a <= 0.0 {
                    return;
                }
                // Premultiplied-linear source.
                let sr = srgb_to_linear(color.r() as f32 / 255.0) * a;
                let sg = srgb_to_linear(color.g() as f32 / 255.0) * a;
                let sb = srgb_to_linear(color.b() as f32 / 255.0) * a;
                let inv = 1.0 - a;
                for row in 0..gh as i32 {
                    let py = base_y + gy + row;
                    if py < 0 || py >= ch_i {
                        continue;
                    }
                    for col in 0..gw as i32 {
                        let pxx = base_x + gx + col;
                        if pxx < 0 || pxx >= cw_i {
                            continue;
                        }
                        let idx = ((py * cw_i + pxx) * 4) as usize;
                        let dr = buf[idx] as f32 / 255.0;
                        let dg = buf[idx + 1] as f32 / 255.0;
                        let db = buf[idx + 2] as f32 / 255.0;
                        let da = buf[idx + 3] as f32 / 255.0;
                        buf[idx] = ((sr + dr * inv) * 255.0).round().clamp(0.0, 255.0) as u8;
                        buf[idx + 1] = ((sg + dg * inv) * 255.0).round().clamp(0.0, 255.0) as u8;
                        buf[idx + 2] = ((sb + db * inv) * 255.0).round().clamp(0.0, 255.0) as u8;
                        buf[idx + 3] = ((a + da * inv) * 255.0).round().clamp(0.0, 255.0) as u8;
                    }
                }
            },
        );

        let texture = self
            .cache_texture
            .as_ref()
            .expect("cache texture created above");
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &buf,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(cw * 4),
                rows_per_image: Some(ch),
            },
            wgpu::Extent3d {
                width: cw,
                height: ch,
                depth_or_array_layers: 1,
            },
        );
        true
    }

    pub(super) fn after_submit(&mut self) {
        // Non-blocking: process any ready GPU callbacks without stalling the
        // winit event loop on full GPU completion. This runs on every showcase
        // slideshow frame, so blocking here (wait_indefinitely) serialized the
        // event loop on the GPU and defeated AutoVsync pacing.
        let _ = self.device.poll(wgpu::PollType::Poll);
    }
}

/// Build the showcase caption string from the current transition and mat.
pub(super) fn showcase_caption(
    transition_kind: Option<TransitionKind>,
    mat_kind: Option<MattingKind>,
) -> String {
    let t = transition_kind
        .map(|k| k.to_string())
        .unwrap_or_else(|| "none".to_string());
    let m = mat_kind
        .map(|k| k.to_string())
        .unwrap_or_else(|| "full-bleed".to_string());
    format!("transition: {t}    mat: {m}")
}

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
            redraw_pending: true,
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
    /// Kind of the most recent transition (in-progress or just-finished), so the
    /// showcase caption can keep naming it after the transition completes.
    last_transition_kind: Option<TransitionKind>,
    displayed_at: Option<Instant>,
    pending: VecDeque<ImgTex>,
    pending_redraw: bool,
    dwell_ms: u64,
    transition_cfg: TransitionConfig,
}

impl WakeScene {
    /// Creates a new [`WakeScene`] configured with the slideshow dwell and transition settings.
    pub(super) fn new(dwell_ms: u64, transition_cfg: TransitionConfig) -> Self {
        if let Some(selected) = transition_cfg.primary_selected() {
            tracing::debug!(
                transition_index = selected.entry.index,
                transition_kind = ?selected.entry.kind,
                duration_ms = selected.option.duration().as_millis(),
                "wake_scene_primary_transition_loaded"
            );
        }

        Self {
            current: None,
            next: None,
            transition_state: None,
            last_transition_kind: None,
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
        self.last_transition_kind = None;
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

    /// Clears any serviced redraw request after the frame has been presented.
    pub(super) fn after_present(&mut self) {
        let _ = self.take_redraw_needed();
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

    /// The most recent transition kind (in-progress or just-finished), for the
    /// showcase caption. Persists after the transition completes.
    pub(super) fn last_transition_kind(&self) -> Option<TransitionKind> {
        self.last_transition_kind
    }

    /// Replaces the active transition state.
    pub(super) fn set_transition_state(&mut self, state: Option<TransitionState>) {
        if let Some(state) = &state {
            self.last_transition_kind = Some(state.kind());
        }
        self.transition_state = state;
    }

    /// Finalizes an in-flight transition, promoting the staged image to current if complete.
    pub(super) fn finalize_transition(&mut self, ctx: &mut SceneContext<'_>) {
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
                ctx.notify_displayed(path);
            }
        }
    }

    fn ensure_current_image(&mut self, ctx: &mut SceneContext<'_>) {
        if self.current.is_some() || self.transition_state().is_some() {
            return;
        }
        if let Some(first) = self.pending_mut().pop_front() {
            let path = first.path.clone();
            tracing::info!(
                "first_image path={} queue_depth={}",
                path.display(),
                self.pending.len()
            );
            self.current = Some(first);
            self.pending_redraw = true;
            self.displayed_at = Some(Instant::now());
            ctx.notify_displayed(path);
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
        if self.next.is_none()
            && let Some(stage) = self.pending.pop_front()
        {
            tracing::debug!(
                "transition_stage path={} queue_depth={}",
                stage.path.display(),
                self.pending.len()
            );
            self.next = Some(stage);
        }
        if self.next.is_some() && self.current.is_some() {
            let selected = self.transition_cfg.select_active(rng);
            let kind = selected.entry.kind;
            let selection_index = selected.entry.index;
            let state = TransitionState::new(selected, Instant::now(), rng);
            if let Some(next) = &self.next {
                tracing::debug!(
                    "transition_start index={} kind={} path={} queue_depth={}",
                    selection_index,
                    kind,
                    next.path.display(),
                    self.pending.len()
                );
            }
            self.last_transition_kind = Some(kind);
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
    rng: &'a mut rand::rngs::ThreadRng,
    notify_displayed: &'a mut dyn FnMut(PathBuf),
    enqueue_matting: &'a mut dyn FnMut(&mut WakeScene),
}

impl<'a> SceneContext<'a> {
    /// Creates a new [`SceneContext`] scoped to the currently active viewer state.
    pub(super) fn new(
        window: Option<&'a Window>,
        redraw: &'a mut dyn FnMut(),
        config: Arc<Configuration>,
        rng: &'a mut rand::rngs::ThreadRng,
        notify_displayed: &'a mut dyn FnMut(PathBuf),
        enqueue_matting: &'a mut dyn FnMut(&mut WakeScene),
    ) -> Self {
        Self {
            window,
            redraw,
            config,
            rng,
            notify_displayed,
            enqueue_matting,
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

    /// Provides mutable access to the viewer RNG for scenes that need randomness.
    pub(super) fn rng(&mut self) -> &mut rand::rngs::ThreadRng {
        self.rng
    }

    /// Notifies the manager that a new image has been displayed.
    pub(super) fn notify_displayed(&mut self, path: PathBuf) {
        (self.notify_displayed)(path);
    }

    /// Requests additional matting work for the wake scene.
    pub(super) fn enqueue_matting(&mut self, wake: &mut WakeScene) {
        (self.enqueue_matting)(wake);
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
        ctx.enqueue_matting(self);
        self.ensure_current_image(&mut ctx);
        ctx.request_redraw();
    }

    fn about_to_wait(&mut self, mut ctx: SceneContext<'_>) {
        self.ensure_redraw_requested(&mut ctx);
    }

    fn process_tick(&mut self, mut ctx: SceneContext<'_>) {
        ctx.enqueue_matting(self);
        self.ensure_current_image(&mut ctx);
        self.finalize_transition(&mut ctx);
        {
            let rng = ctx.rng();
            self.maybe_start_transition(rng);
        }
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

#[cfg(test)]
mod tests {
    use super::CaptionOverlay;
    use winit::dpi::PhysicalSize;

    fn try_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("caption-test-device"),
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::default(),
        }))
        .ok()
    }

    fn read_texture_rgba(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        let bpp = 4u32;
        let unpadded = width * bpp;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caption-readback"),
            size: (padded * height) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("caption-readback-enc"),
        });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((width * height * bpp) as usize);
        for row in 0..height {
            let start = (row * padded) as usize;
            out.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);
        buffer.unmap();
        out
    }

    /// Regression guard for the showcase caption: the cached panel must hold the
    /// full text (no truncation) and the glyphs must actually render (no dropped or
    /// blanked letters). Skips when no GPU adapter is available.
    #[test]
    fn caption_caches_full_text_without_dropping_glyphs() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping caption test: no GPU adapter available");
            return;
        };
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut overlay = CaptionOverlay::new(&device, &queue, format);
        overlay.set_text("transition: crossfade-zoom    mat: passe-partout");
        overlay.resize(PhysicalSize::new(1920, 1080));

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("caption-test-target"),
            size: wgpu::Extent3d {
                width: 1920,
                height: 1080,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("caption-test"),
        });
        assert!(
            overlay.render(&mut encoder, &target_view),
            "overlay.render should succeed"
        );
        queue.submit(Some(encoder.finish()));
        let _ = device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });

        let (cw, ch) = overlay.cache_dims;
        assert!(
            cw > 300 && ch > 0,
            "cache should size to the full caption (got {cw}x{ch})"
        );

        let cache = overlay
            .cache_texture
            .as_ref()
            .expect("cache texture should exist after render");
        let pixels = read_texture_rgba(&device, &queue, cache, cw, ch);

        // Backing panel covers the whole cache: every texel has substantial alpha.
        let min_alpha = pixels.chunks_exact(4).map(|p| p[3]).min().unwrap_or(0);
        assert!(
            min_alpha >= 120,
            "backing panel should fill the cache (min alpha {min_alpha})"
        );

        // Glyphs rendered: many bright cyan text texels.
        let cyan = pixels
            .chunks_exact(4)
            .filter(|p| p[1] > 180 && p[2] > 180 && p[0] < p[1])
            .count();
        assert!(cyan > 100, "expected many cyan glyph pixels, found {cyan}");
    }

    /// Render one caption through `overlay` and return the count of bright-cyan
    /// glyph pixels in the resulting cache.
    fn render_caption_cyan(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        overlay: &mut CaptionOverlay,
        format: wgpu::TextureFormat,
        text: &str,
    ) -> usize {
        overlay.set_text(text.to_string());
        overlay.resize(PhysicalSize::new(1920, 1080));
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("caption-churn-target"),
            size: wgpu::Extent3d {
                width: 1920,
                height: 1080,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("caption-churn"),
        });
        assert!(overlay.render(&mut encoder, &view), "render '{text}'");
        queue.submit(Some(encoder.finish()));
        let _ = device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
        let (cw, ch) = overlay.cache_dims;
        let pixels = read_texture_rgba(
            device,
            queue,
            overlay.cache_texture.as_ref().expect("cache texture"),
            cw,
            ch,
        );
        pixels
            .chunks_exact(4)
            .filter(|p| p[1] > 180 && p[2] > 180 && p[0] < p[1])
            .count()
    }

    /// Regression guard for the live failure mode: the same caption text must
    /// render identically no matter how much the shared glyph atlas has churned
    /// (the live app renders every transition/mat through one long-lived atlas).
    #[test]
    fn caption_glyphs_survive_atlas_churn() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping caption churn test: no GPU adapter available");
            return;
        };
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut overlay = CaptionOverlay::new(&device, &queue, format);

        let reference = "transition: e-ink    mat: studio";
        let fresh = render_caption_cyan(&device, &queue, &mut overlay, format, reference);
        assert!(
            fresh > 100,
            "reference caption should render ({fresh} cyan px)"
        );

        // Churn the shared atlas with many other captions (introduce + evict glyphs).
        for caption in [
            "transition: fade    mat: fixed-color",
            "transition: wipe    mat: cinematic-blur",
            "transition: push    mat: drop-shadow",
            "transition: dissolve    mat: blur",
            "transition: radial-wipe    mat: fixed-image",
            "transition: venetian-blinds    mat: gradient",
            "transition: crossfade-zoom    mat: passe-partout",
            "transition: push    mat: vignette",
        ] {
            let _ = render_caption_cyan(&device, &queue, &mut overlay, format, caption);
        }

        // Identical text + size must yield identical glyph coverage. A drop here is
        // the bug: a glyph evicted by atlas.trim() failed to re-add correctly.
        let after = render_caption_cyan(&device, &queue, &mut overlay, format, reference);
        assert_eq!(
            after, fresh,
            "'{reference}' lost glyph pixels after atlas churn ({fresh} -> {after})"
        );
    }
}
