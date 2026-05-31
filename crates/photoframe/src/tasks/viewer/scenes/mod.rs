//! Viewer scene definitions.
//!
//! This module will house the logic for state-specific viewer behaviour.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use glyphon::{
    Attrs, Buffer, Cache, Color as GlyphonColor, FamilyOwned, FontSystem, Metrics, Resolution,
    Shaping, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Wrap,
};
use rand::Rng;
use wgpu::{CommandEncoder, TextureView};
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::config::{Configuration, MattingKind, TransitionConfig, TransitionKind};
use crate::tasks::greeting_screen::GreetingScreen;

use super::{ImgTex, TransitionState};

// ── Caption overlay ───────────────────────────────────────────────────────────

/// Uniform for the caption backing panel (must match caption_panel.wgsl).
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct PanelUniforms {
    resolution: [f32; 2],
    _pad0: [f32; 2],
    rect: [f32; 4],
    color: [f32; 4],
}

/// Lightweight text overlay rendered on top of the live photo via `LoadOp::Load`.
/// Draws a single short line in the bottom-left corner, on a solid backing panel
/// so it stays legible over any mat (light, dark, or busy).
pub(super) struct CaptionOverlay {
    device: wgpu::Device,
    queue: wgpu::Queue,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    text_buffer: Buffer,
    font_system: FontSystem,
    swash_cache: SwashCache,
    panel_pipeline: wgpu::RenderPipeline,
    panel_bind_group: wgpu::BindGroup,
    panel_uniform_buffer: wgpu::Buffer,
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

        let cache = Cache::new(device);
        let viewport = Viewport::new(device, &cache);
        let mut atlas = TextAtlas::new(device, queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);
        let swash_cache = SwashCache::new();

        // Backing-panel pipeline: one alpha-blended quad positioned in pixel space.
        let panel_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("caption-panel-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("caption_panel.wgsl").into()),
        });
        let panel_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("caption-panel-uniforms"),
            size: std::mem::size_of::<PanelUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let panel_bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("caption-panel-bind-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<PanelUniforms>() as u64,
                    ),
                },
                count: None,
            }],
        });
        let panel_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("caption-panel-bind-group"),
            layout: &panel_bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: panel_uniform_buffer.as_entire_binding(),
            }],
        });
        let panel_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("caption-panel-pipeline-layout"),
                bind_group_layouts: &[&panel_bind_layout],
                push_constant_ranges: &[],
            });
        let panel_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("caption-panel-pipeline"),
            layout: Some(&panel_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &panel_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &panel_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
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
            viewport,
            atlas,
            text_renderer,
            text_buffer,
            font_system,
            swash_cache,
            panel_pipeline,
            panel_bind_group,
            panel_uniform_buffer,
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
        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.size.width,
                height: self.size.height,
            },
        );
        if self.dirty {
            self.text_buffer.set_metrics_and_size(
                &mut self.font_system,
                Metrics::new(28.0, 34.0),
                Some(self.size.width as f32),
                Some(self.size.height as f32),
            );
            let attrs = Attrs::new().family(FamilyOwned::SansSerif.as_family());
            self.text_buffer
                .set_text(&mut self.font_system, &self.text, &attrs, Shaping::Basic, None);
            self.dirty = false;
        }

        let margin = 20.0_f32;
        let line_h = 34.0_f32;
        // Position at bottom-left; offset up by one line height.
        let top = (self.size.height as f32 - line_h - margin).max(0.0);

        // Measure the laid-out text so the backing panel fits it.
        let mut text_w = 0.0_f32;
        for run in self.text_buffer.layout_runs() {
            text_w = text_w.max(run.line_w);
        }
        if text_w <= 0.0 {
            return false;
        }

        // Backing panel: a near-opaque dark rectangle so the caption is legible
        // over ANY mat. Drawn first (under the text) in the same pass.
        let pad_x = 14.0_f32;
        let pad_y = 8.0_f32;
        let rect_x = (margin - pad_x).max(0.0);
        let rect_y = (top - pad_y).max(0.0);
        let rect_w = text_w + 2.0 * pad_x;
        let rect_h = line_h + 2.0 * pad_y;
        let panel = PanelUniforms {
            resolution: [self.size.width as f32, self.size.height as f32],
            _pad0: [0.0, 0.0],
            rect: [rect_x, rect_y, rect_w, rect_h],
            color: [0.0, 0.04, 0.06, 0.72], // dark, ~72% opaque
        };
        self.queue
            .write_buffer(&self.panel_uniform_buffer, 0, bytemuck::bytes_of(&panel));

        let fill_color = GlyphonColor::rgb(170, 244, 244); // light cyan
        let bounds = TextBounds {
            left: 0,
            top: 0,
            right: self.size.width as i32,
            bottom: self.size.height as i32,
        };
        let main = TextArea {
            buffer: &self.text_buffer,
            left: margin,
            top,
            scale: 1.0,
            bounds,
            default_color: fill_color,
            custom_glyphs: &[],
        };

        if let Err(err) = self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            [main],
            &mut self.swash_cache,
        ) {
            tracing::warn!(error = %err, "caption_overlay_prepare_failed");
            return false;
        }

        let mut render_error = None;
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("caption-overlay"),
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
            // Panel first (under the text), then the glyphs on top.
            pass.set_pipeline(&self.panel_pipeline);
            pass.set_bind_group(0, &self.panel_bind_group, &[]);
            pass.draw(0..6, 0..1);
            if let Err(err) = self.text_renderer.render(&self.atlas, &self.viewport, &mut pass) {
                render_error = Some(err);
            }
        }
        if let Some(err) = render_error {
            tracing::warn!(error = %err, "caption_overlay_render_failed");
        }
        self.atlas.trim();
        true
    }

    pub(super) fn after_submit(&mut self) {
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
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
