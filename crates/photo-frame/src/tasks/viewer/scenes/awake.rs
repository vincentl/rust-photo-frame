use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use image::RgbaImage;
use rand::{Rng, SeedableRng, rngs::StdRng};
use tracing::{debug, warn};
use winit::dpi::PhysicalSize;

use super::{RenderCtx, RenderResult, Scene, SceneContext, ScenePresentEvent};
use crate::config::{AnglePicker, TransitionConfig, TransitionMode, TransitionOptions};
use crate::events::PhotoLoaded;
use crate::processing::color::average_color;

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct TransitionUniforms {
    screen_size: [f32; 2],
    progress: f32,
    kind: u32,
    current_dest: [f32; 4],
    next_dest: [f32; 4],
    params0: [f32; 4],
    params1: [f32; 4],
}

struct Frame {
    path: PathBuf,
    _texture: wgpu::Texture,
    _view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    dest: [f32; 4],
    average_color: [f32; 3],
    size: (u32, u32),
}

struct TransitionState {
    start: Instant,
    duration: Duration,
    next_frame: Frame,
    params0: [f32; 4],
    params1: [f32; 4],
    kind_id: u32,
}

pub struct AwakeScene {
    device: wgpu::Device,
    queue: wgpu::Queue,
    transition_config: TransitionConfig,
    dwell_duration: Duration,
    sampler: wgpu::Sampler,
    texture_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    _empty_texture: wgpu::Texture,
    _empty_view: wgpu::TextureView,
    empty_bind_group: wgpu::BindGroup,
    surface_size: PhysicalSize<u32>,
    current: Option<Frame>,
    transition: Option<TransitionState>,
    pending: VecDeque<Frame>,
    pending_display_report: Option<PathBuf>,
    rng: StdRng,
    needs_redraw: bool,
    next_transition_time: Option<Instant>,
}

impl AwakeScene {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        cfg: &crate::config::Configuration,
    ) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("awake-scene-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("awake-scene-texture-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("awake-scene-uniform-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("awake-scene-uniform-buffer"),
            size: std::mem::size_of::<TransitionUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("awake-scene-uniform-bind-group"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("awake-scene-shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "../../shaders/viewer_quad.wgsl"
            ))),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("awake-scene-pipeline-layout"),
            bind_group_layouts: &[&uniform_layout, &texture_layout, &texture_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("awake-scene-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
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

        let empty_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("awake-scene-empty-texture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &empty_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0, 0, 0, 0],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let empty_view = empty_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let empty_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("awake-scene-empty-bind-group"),
            layout: &texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&empty_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let rng = StdRng::from_os_rng();

        Self {
            device: device.clone(),
            queue: queue.clone(),
            transition_config: cfg.transition.clone(),
            dwell_duration: Duration::from_millis(cfg.dwell_ms.max(1)),
            sampler,
            texture_layout,
            uniform_buffer,
            uniform_bind_group,
            pipeline,
            _empty_texture: empty_texture,
            _empty_view: empty_view,
            empty_bind_group,
            surface_size: PhysicalSize::new(0, 0),
            current: None,
            transition: None,
            pending: VecDeque::new(),
            pending_display_report: None,
            rng,
            needs_redraw: false,
            next_transition_time: None,
        }
    }

    pub fn queue_photo(&mut self, photo: PhotoLoaded) {
        debug!(path = %photo.prepared.path.display(), "awake_scene_queue_photo");
        if let Some(frame) = self.prepare_frame(photo) {
            let now = Instant::now();
            if self.current.is_none() && self.transition.is_none() {
                self.set_current_frame(frame, now);
            } else if self.transition.is_none() && self.pending.is_empty() {
                self.start_transition(frame, now);
            } else {
                self.pending.push_back(frame);
            }
            self.needs_redraw = true;
        }
    }

    fn prepare_frame(&mut self, photo: PhotoLoaded) -> Option<Frame> {
        let mut prepared = photo.prepared;
        if prepared.width == 0 || prepared.height == 0 {
            warn!(
                path = %prepared.path.display(),
                "skipping photo with zero dimensions"
            );
            return None;
        }
        let Some(image) = RgbaImage::from_raw(
            prepared.width,
            prepared.height,
            std::mem::take(&mut prepared.pixels),
        ) else {
            warn!(path = %prepared.path.display(), "invalid pixel buffer for photo");
            return None;
        };
        let avg = average_color(&image);
        let pixels = image.into_raw();

        let extent = wgpu::Extent3d {
            width: prepared.width,
            height: prepared.height,
            depth_or_array_layers: 1,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("awake-scene-photo-texture"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * prepared.width),
                rows_per_image: Some(prepared.height),
            },
            extent,
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("awake-scene-photo-bind-group"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        let mut frame = Frame {
            path: prepared.path,
            _texture: texture,
            _view: view,
            bind_group,
            dest: [0.0; 4],
            average_color: avg,
            size: (extent.width, extent.height),
        };
        frame.dest = self.compute_dest(frame.size.0, frame.size.1);
        Some(frame)
    }

    fn compute_dest(&self, width: u32, height: u32) -> [f32; 4] {
        Self::compute_dest_for_surface(self.surface_size, width, height)
    }

    fn compute_dest_for_surface(
        surface_size: PhysicalSize<u32>,
        width: u32,
        height: u32,
    ) -> [f32; 4] {
        if surface_size.width == 0 || surface_size.height == 0 {
            return [0.0; 4];
        }
        let screen_w = surface_size.width as f32;
        let screen_h = surface_size.height as f32;
        let img_w = width.max(1) as f32;
        let img_h = height.max(1) as f32;
        let scale = (screen_w / img_w)
            .min(screen_h / img_h)
            .max(f32::MIN_POSITIVE);
        let dest_w = img_w * scale;
        let dest_h = img_h * scale;
        let dest_x = (screen_w - dest_w) * 0.5;
        let dest_y = (screen_h - dest_h) * 0.5;
        [dest_x, dest_y, dest_w, dest_h]
    }

    fn set_current_frame(&mut self, frame: Frame, now: Instant) {
        self.next_transition_time = Some(now + self.dwell_duration);
        self.pending_display_report = Some(frame.path.clone());
        self.current = Some(frame);
    }

    fn start_transition(&mut self, next_frame: Frame, now: Instant) {
        let option = self.transition_config.choose_option(&mut self.rng);
        let duration = option.duration();
        let (kind_id, params0, params1) = self.build_transition_params(&option);
        self.transition = Some(TransitionState {
            start: now,
            duration,
            next_frame,
            params0,
            params1,
            kind_id,
        });
        self.next_transition_time = None;
    }

    fn build_transition_params(&mut self, option: &TransitionOptions) -> (u32, [f32; 4], [f32; 4]) {
        match option.mode() {
            TransitionMode::Fade(fade) => (
                1,
                [if fade.through_black { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
                [0.0; 4],
            ),
            TransitionMode::Wipe(wipe) => {
                let angle_deg = self.pick_angle(&wipe.angles);
                let dir = angle_to_dir(angle_deg);
                let (min_proj, inv_span) = Self::wipe_projection(self.surface_size, dir);
                let params0 = [dir[0], dir[1], min_proj, inv_span];
                let params1 = [wipe.softness.clamp(0.0, 0.5), 0.0, 0.0, 0.0];
                (2, params0, params1)
            }
            TransitionMode::Push(push) => {
                let angle_deg = self.pick_angle(&push.angles);
                let dir = angle_to_dir(angle_deg);
                let translation = Self::push_translation(self.surface_size, dir);
                (3, [translation[0], translation[1], 0.0, 0.0], [0.0; 4])
            }
            TransitionMode::EInk(eink) => {
                let seed0 = self.rng.random_range(0.0..1.0);
                let seed1 = self.rng.random_range(0.0..1.0);
                let flash_rgb = [
                    (eink.flash_color[0] as f32) / 255.0,
                    (eink.flash_color[1] as f32) / 255.0,
                    (eink.flash_color[2] as f32) / 255.0,
                ];
                let params0 = [
                    eink.flash_count as f32,
                    eink.reveal_portion,
                    eink.stripe_count.max(1) as f32,
                    seed0,
                ];
                let params1 = [seed1, flash_rgb[0], flash_rgb[1], flash_rgb[2]];
                (4, params0, params1)
            }
        }
    }

    fn pick_angle(&mut self, picker: &AnglePicker) -> f32 {
        picker.pick_angle(&mut self.rng)
    }

    fn wipe_projection(surface: PhysicalSize<u32>, dir: [f32; 2]) -> (f32, f32) {
        let corners = [
            [0.0, 0.0],
            [surface.width as f32, 0.0],
            [0.0, surface.height as f32],
            [surface.width as f32, surface.height as f32],
        ];
        let mut min_proj = f32::INFINITY;
        let mut max_proj = f32::NEG_INFINITY;
        for corner in corners {
            let proj = dir[0] * corner[0] + dir[1] * corner[1];
            min_proj = min_proj.min(proj);
            max_proj = max_proj.max(proj);
        }
        if min_proj.is_infinite() || max_proj.is_infinite() {
            return (0.0, 1.0);
        }
        let span = (max_proj - min_proj).abs().max(1.0);
        (min_proj, 1.0 / span)
    }

    fn push_translation(surface: PhysicalSize<u32>, dir: [f32; 2]) -> [f32; 2] {
        [
            dir[0] * surface.width as f32,
            dir[1] * surface.height as f32,
        ]
    }

    fn finish_transition(&mut self, now: Instant) {
        if let Some(state) = self.transition.take() {
            let path = state.next_frame.path.clone();
            let previous = self.current.replace(state.next_frame);
            drop(previous);
            self.pending_display_report = Some(path);
            self.next_transition_time = Some(now + self.dwell_duration);
        }
    }

    fn update_surface_size(&mut self, new_size: PhysicalSize<u32>) {
        if new_size == self.surface_size {
            return;
        }
        self.surface_size = new_size;
        let surface = self.surface_size;
        if let Some(current) = self.current.as_mut() {
            current.dest = Self::compute_dest_for_surface(surface, current.size.0, current.size.1);
        }
        if let Some(state) = self.transition.as_mut() {
            state.next_frame.dest = Self::compute_dest_for_surface(
                surface,
                state.next_frame.size.0,
                state.next_frame.size.1,
            );
            Self::update_transition_params_for_surface(surface, state);
        }
        for frame in &mut self.pending {
            frame.dest = Self::compute_dest_for_surface(surface, frame.size.0, frame.size.1);
        }
        self.needs_redraw = true;
    }

    fn update_transition_params_for_surface(
        surface: PhysicalSize<u32>,
        state: &mut TransitionState,
    ) {
        match state.kind_id {
            2 => {
                let dir = [state.params0[0], state.params0[1]];
                let (min_proj, inv_span) = Self::wipe_projection(surface, dir);
                state.params0[2] = min_proj;
                state.params0[3] = inv_span;
            }
            3 => {
                let translation = Self::push_translation(
                    surface,
                    normalize_or_zero([state.params0[0], state.params0[1]]),
                );
                state.params0[0] = translation[0];
                state.params0[1] = translation[1];
            }
            _ => {}
        }
    }
}

impl Scene for AwakeScene {
    fn on_enter(&mut self, ctx: &SceneContext) {
        self.update_surface_size(ctx.surface_size());
        self.needs_redraw = true;
    }

    fn on_exit(&mut self, _ctx: &SceneContext) {
        self.needs_redraw = false;
    }

    fn handle_resize(
        &mut self,
        _ctx: &SceneContext,
        new_size: PhysicalSize<u32>,
        _scale_factor: f64,
    ) {
        self.update_surface_size(new_size);
        self.needs_redraw = true;
    }

    fn render(&mut self, ctx: &mut RenderCtx<'_, '_>) -> RenderResult {
        self.update_surface_size(ctx.scene.surface_size());

        let now = Instant::now();
        if self.current.is_none() {
            if let Some(frame) = self.pending.pop_front() {
                self.set_current_frame(frame, now);
            }
        }

        if self.transition.is_none() {
            let ready_for_transition = self
                .next_transition_time
                .map(|deadline| deadline <= now)
                .unwrap_or(true);
            if ready_for_transition {
                if let Some(frame) = self.pending.pop_front() {
                    self.start_transition(frame, now);
                }
            }
        }

        let Some(current) = self.current.as_ref() else {
            let pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("awake-scene-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: ctx.target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            drop(pass);
            return RenderResult::Idle;
        };

        let mut progress = 0.0f32;
        let mut kind_id = 0u32;
        let mut params0 = [0.0; 4];
        let mut params1 = [0.0; 4];
        let mut next_dest = current.dest;
        let mut next_color = current.average_color;
        let mut next_bind_group = &self.empty_bind_group;
        let mut transition_complete = false;

        if let Some(state) = self.transition.as_mut() {
            let duration = state.duration.as_secs_f32().max(f32::MIN_POSITIVE);
            progress = now
                .saturating_duration_since(state.start)
                .as_secs_f32()
                .clamp(0.0, duration)
                / duration;
            if progress >= 0.999_9 {
                progress = 1.0;
                transition_complete = true;
            }
            kind_id = state.kind_id;
            params0 = state.params0;
            params1 = state.params1;
            next_dest = state.next_frame.dest;
            next_color = state.next_frame.average_color;
            next_bind_group = &state.next_frame.bind_group;
        }

        let uniforms = TransitionUniforms {
            screen_size: [
                self.surface_size.width as f32,
                self.surface_size.height as f32,
            ],
            progress,
            kind: kind_id,
            current_dest: current.dest,
            next_dest,
            params0,
            params1,
        };
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let blended_color = mix_color(current.average_color, next_color, progress);
        let clear = wgpu::Color {
            r: blended_color[0] as f64,
            g: blended_color[1] as f64,
            b: blended_color[2] as f64,
            a: 1.0,
        };

        {
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("awake-scene-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: ctx.target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            pass.set_bind_group(1, &current.bind_group, &[]);
            pass.set_bind_group(2, next_bind_group, &[]);
            pass.draw(0..6, 0..1);
        }

        if transition_complete {
            self.finish_transition(now);
        }

        let mut needs_more = self.transition.is_some();
        if !needs_more {
            if let Some(deadline) = self.next_transition_time {
                if !self.pending.is_empty() && deadline > now {
                    needs_more = true;
                }
            }
        }
        if self.needs_redraw {
            needs_more = true;
            if self.transition.is_none() {
                self.needs_redraw = false;
            }
        }

        if needs_more {
            RenderResult::NeedsRedraw
        } else {
            RenderResult::Idle
        }
    }

    fn after_present(&mut self, _ctx: &SceneContext) -> Option<ScenePresentEvent> {
        self.pending_display_report
            .take()
            .map(ScenePresentEvent::PhotoDisplayed)
    }
}

fn mix_color(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let clamped = t.clamp(0.0, 1.0);
    [
        a[0] * (1.0 - clamped) + b[0] * clamped,
        a[1] * (1.0 - clamped) + b[1] * clamped,
        a[2] * (1.0 - clamped) + b[2] * clamped,
    ]
}

fn angle_to_dir(angle_deg: f32) -> [f32; 2] {
    let radians = angle_deg.to_radians();
    [radians.cos(), radians.sin()]
}

fn normalize_or_zero(vec: [f32; 2]) -> [f32; 2] {
    let mag = (vec[0] * vec[0] + vec[1] * vec[1]).sqrt();
    if mag <= f32::MIN_POSITIVE {
        [0.0, 0.0]
    } else {
        [vec[0] / mag, vec[1] / mag]
    }
}
