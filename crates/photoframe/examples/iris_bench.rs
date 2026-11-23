//! Headless GPU benchmark for the iris transition's petal-layer passes.
//!
//! Runs the viewer's own `fs_iris_cover` and `fs_iris_layer` (and, with
//! `--main`, `fs_iris` into a 1920x1080 intermediate) offscreen, with the
//! per-frame petal constants solved exactly as `viewer.rs` does, and reports
//! GPU pass times from timestamp queries across the transition's progress.
//! Needs no display: on the Pi it uses the render node, so it runs beside a
//! live slideshow (take the median; a live transition can land on a sample).
//!
//! ```text
//! cargo run --release -p photoframe --example iris_bench -- \
//!   [--layer-scale 4] [--blades 12] [--steps 11] [--reps 40] [--main] \
//!   [--variant baseline|sym] [--dump DIR]
//! ```
//!
//! `--variant sym` runs the symmetric cover pass from `iris_sym.wgsl` (the
//! cover computed once for the fundamental sector, then rotated into place);
//! `--dump DIR` writes each step's layer texels so two variants can be
//! compared (`--compare A B`).

use std::path::PathBuf;
use std::time::Instant;

const SCREEN_W: u32 = 3840;
const SCREEN_H: u32 = 2160;
const IRIS_EXTRA_WIDTH_RAD: f32 = 1.308_997;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TransitionUniforms {
    screen_size: [f32; 2],
    progress: f32,
    lod: f32,
    current_xform: [f32; 4],
    next_xform: [f32; 4],
    params0: [f32; 4],
    params1: [f32; 4],
    params3: [f32; 4],
    background: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct IrisPetals {
    petals_a: [[f32; 4]; 16],
    petals_b: [[f32; 4]; 16],
}

#[derive(Clone, Copy)]
struct IrisCfg {
    blades: u32,
    petal_sheen: f32,
    overlap_shadow: f32,
    min_aperture: f32,
    swirl: f32,
    layer_scale: u32,
    color: [f32; 3],
}

/// The Iris arm of the viewer's draw setup (viewer.rs), verbatim in its math.
fn solve(cfg: &IrisCfg, eased_progress: f32) -> (TransitionUniforms, IrisPetals) {
    let screen_w = SCREEN_W as f32;
    let screen_h = SCREEN_H as f32;
    let mut u = <TransitionUniforms as bytemuck::Zeroable>::zeroed();
    let mut petals = <IrisPetals as bytemuck::Zeroable>::zeroed();
    u.screen_size = [screen_w, screen_h];
    u.progress = eased_progress;
    u.lod = 1.0;
    // Full-screen planes: texture coordinate = screen_uv.
    u.current_xform = [1.0, 1.0, 0.0, 0.0];
    u.next_xform = [1.0, 1.0, 0.0, 0.0];
    u.background = [0.0, 0.0, 0.0, 1.0];
    let n = cfg.blades.clamp(1, 16) as usize;
    let t = eased_progress;
    let x = if t < 0.5 { t * 2.0 } else { 2.0 - t * 2.0 };
    let f = x * x * (3.0 - 2.0 * x);
    let r_in = 1.02 * 0.5 * (screen_w * screen_w + screen_h * screen_h).sqrt();
    let r_mid = 1.5 * r_in;
    let e = f * r_in * (1.0 - cfg.min_aperture);
    let psi = 2.0 * (e / (2.0 * r_mid)).clamp(0.0, 1.0).asin();
    let spin = cfg.swirl * psi;
    let sigma = std::f32::consts::TAU / n as f32 + IRIS_EXTRA_WIDTH_RAD;
    let swap = ((t - 0.42) / 0.16).clamp(0.0, 1.0);
    let swap = swap * swap * (3.0 - 2.0 * swap);
    u.params0 = [n as f32, cfg.petal_sheen, cfg.overlap_shadow, swap];
    u.params1 = [r_in - e, cfg.color[0], cfg.color[1], cfg.color[2]];
    let edge_band = 0.75 * 2.0;
    u.params3 = [cfg.layer_scale as f32, r_mid, 0.5 * r_in, edge_band];
    let (s_psi, c_psi) = psi.sin_cos();
    for i in 0..n {
        let ai = std::f32::consts::TAU * i as f32 / n as f32 + spin;
        let (s_ai, c_ai) = ai.sin_cos();
        let piv = [r_mid * c_ai, r_mid * s_ai];
        let center = [
            piv[0] - (c_psi * piv[0] - s_psi * piv[1]),
            piv[1] - (s_psi * piv[0] + c_psi * piv[1]),
        ];
        let trail = ai + psi;
        let tip = trail + sigma;
        let facing = (ai + 0.5 * sigma + psi - 2.3).cos();
        petals.petals_a[i] = [center[0], center[1], tip.cos(), tip.sin()];
        petals.petals_b[i] = [trail.cos(), trail.sin(), facing, 0.0];
    }
    (u, petals)
}

struct Args {
    layer_scale: u32,
    blades: u32,
    steps: u32,
    reps: u32,
    main: bool,
    variant: String,
    dump: Option<PathBuf>,
    compare: Option<(PathBuf, PathBuf)>,
    sequence_ms: Option<u32>,
}

fn parse_args() -> Args {
    let mut a = Args {
        layer_scale: 4,
        blades: 12,
        steps: 11,
        reps: 40,
        main: false,
        variant: "baseline".into(),
        dump: None,
        compare: None,
        sequence_ms: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        match k.as_str() {
            "--layer-scale" => a.layer_scale = it.next().unwrap().parse().unwrap(),
            "--blades" => a.blades = it.next().unwrap().parse().unwrap(),
            "--steps" => a.steps = it.next().unwrap().parse().unwrap(),
            "--reps" => a.reps = it.next().unwrap().parse().unwrap(),
            "--main" => a.main = true,
            "--variant" => a.variant = it.next().unwrap(),
            "--sequence" => a.sequence_ms = Some(it.next().unwrap().parse().unwrap()),
            "--dump" => a.dump = Some(it.next().unwrap().into()),
            "--compare" => {
                let x = it.next().unwrap().into();
                let y = it.next().unwrap().into();
                a.compare = Some((x, y));
            }
            other => panic!("unknown argument {other}"),
        }
    }
    a
}

fn main() {
    let args = parse_args();
    if let Some((a, b)) = &args.compare {
        compare_dumps(a, b);
        return;
    }
    pollster::block_on(run(args));
}

struct Target {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    w: u32,
    h: u32,
}

fn target(
    device: &wgpu::Device,
    label: &str,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
) -> Target {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    Target { tex, view, w, h }
}

async fn run(args: Args) {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .expect("no adapter");
    let info = adapter.get_info();
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("iris-bench"),
            required_features: adapter.features() & wgpu::Features::TIMESTAMP_QUERY,
            required_limits: adapter.limits(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::default(),
        })
        .await
        .expect("no device");
    let timed = device.features().contains(wgpu::Features::TIMESTAMP_QUERY);
    println!(
        "# adapter: {} ({:?}), timestamps: {timed}, variant: {}, layer-scale {}, blades {}",
        info.name, info.backend, args.variant, args.layer_scale, args.blades
    );

    let base_src = include_str!("../src/tasks/shaders/viewer_quad.wgsl");
    let old = args.variant == "old";
    let src = if args.variant == "baseline" {
        base_src.to_string()
    } else if old {
        // The shader on main (v1.2.1): an Rgba16Float layer, color baked in.
        include_str!("viewer_quad_main.wgsl").to_string()
    } else {
        format!("{base_src}\n{}", include_str!("iris_sym.wgsl"))
    };
    let src = format!("{src}\n{}", include_str!("iris_exp.wgsl"));
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("viewer_quad"),
        source: wgpu::ShaderSource::Wgsl(src.into()),
    });

    let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("uniform-layout"),
        entries: &[0, 1].map(|binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }),
    });
    let img_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("img-layout"),
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
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let ubuf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("u"),
        size: std::mem::size_of::<TransitionUniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let pbuf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("p"),
        size: std::mem::size_of::<IrisPetals>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let uniform_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &uniform_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: ubuf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: pbuf.as_entire_binding(),
            },
        ],
    });
    let img_bind = |view: &wgpu::TextureView| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &img_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        })
    };

    let f16 = wgpu::TextureFormat::Rgba16Float;
    let lw = SCREEN_W / args.layer_scale;
    let lh = SCREEN_H / args.layer_scale;
    let layer_format = match std::env::var("BENCH_LAYER_FORMAT").as_deref() {
        Ok("srgb8") => wgpu::TextureFormat::Rgba8UnormSrgb,
        Ok("rgba16f") => f16,
        _ if old || std::env::var("BENCH_LAYER_RGBA").is_ok_and(|v| v == "1") => f16,
        Ok("rg16f") => wgpu::TextureFormat::Rg16Float,
        _ => wgpu::TextureFormat::Rgba8UnormSrgb,
    };
    let layer = target(&device, "layer", lw, lh, layer_format);
    let cover = target(&device, "cover", lw, lh, f16);
    // The symmetric variant's fundamental-sector cover (polar: x = radius,
    // y = angle within one sector), sized in iris_sym.wgsl's terms.
    let (sw, sh) = sym_sector_size(lw, lh, args.blades);
    let sector = target(&device, "sector", sw, sh, f16);
    let inter = target(
        &device,
        "intermediate",
        SCREEN_W / 2,
        SCREEN_H / 2,
        wgpu::TextureFormat::Rgba8UnormSrgb,
    );
    // Two photo planes at the intermediate's size with a mip level 1, as the
    // viewer's canvases carry; filled with a gradient so fetches are real.
    let photo = |label: &str| {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: SCREEN_W,
                height: SCREEN_H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 2,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for level in 0..2u32 {
            let (w, h) = (SCREEN_W >> level, SCREEN_H >> level);
            let mut px = vec![0u8; (w * h * 4) as usize];
            for (i, p) in px.chunks_mut(4).enumerate() {
                let x = (i as u32 % w) as u8;
                let y = (i as u32 / w) as u8;
                p.copy_from_slice(&[x, y, x ^ y, 255]);
            }
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: level,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &px,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 4),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        }
        tex.create_view(&Default::default())
    };
    let cur = photo("cur");
    let next = photo("next");
    let cur_bind = img_bind(&cur);
    let next_bind = img_bind(&next);
    let cover_bind = img_bind(&cover.view);
    let sector_bind = img_bind(&sector.view);
    let layer_bind = img_bind(&layer.view);

    let pipeline = |label: &str,
                    layouts: &[&wgpu::BindGroupLayout],
                    entry: &str,
                    format: wgpu::TextureFormat| {
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: layouts,
            push_constant_ranges: &[],
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(entry),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview: None,
            cache: None,
        })
    };
    let sym = args.variant == "sym";
    // `cached`, or BENCH_SKIP_LAYER=1 with any variant: no layer passes.
    let main_entry = std::env::var("BENCH_MAIN_ENTRY").unwrap_or_else(|_| "fs_iris".into());
    let cached =
        args.variant == "cached" || std::env::var("BENCH_SKIP_LAYER").is_ok_and(|v| v == "1");
    let cover_pipe = if sym {
        pipeline("sector", &[&uniform_layout], "fs_iris_sector", f16)
    } else {
        pipeline("cover", &[&uniform_layout], "fs_iris_cover", f16)
    };
    let layer_pipe = if sym {
        pipeline(
            "layer-sym",
            &[&uniform_layout, &img_layout],
            "fs_iris_layer_sym",
            layer_format,
        )
    } else {
        pipeline(
            "layer",
            &[&uniform_layout, &img_layout],
            "fs_iris_layer",
            layer_format,
        )
    };
    let main_pipe = pipeline(
        "main",
        &[&uniform_layout, &img_layout, &img_layout, &img_layout],
        &main_entry,
        wgpu::TextureFormat::Rgba8UnormSrgb,
    );

    let qset = timed.then(|| {
        device.create_query_set(&wgpu::QuerySetDescriptor {
            label: None,
            ty: wgpu::QueryType::Timestamp,
            count: 6,
        })
    });
    let qresolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 6 * 8,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let qread = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 6 * 8,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let period_ns = queue.get_timestamp_period() as f64;

    let cfg = IrisCfg {
        blades: args.blades,
        petal_sheen: 0.5,
        overlap_shadow: 0.6,
        min_aperture: 0.01,
        swirl: -0.45,
        layer_scale: args.layer_scale,
        color: [0.023, 0.023, 0.030],
    };

    let encode_frame = |enc: &mut wgpu::CommandEncoder| {
        let tw = |b: u32| {
            qset.as_ref().map(|q| wgpu::RenderPassTimestampWrites {
                query_set: q,
                beginning_of_pass_write_index: Some(b),
                end_of_pass_write_index: Some(b + 1),
            })
        };
        let pass = |enc: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, ts| {
            enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: ts,
                occlusion_query_set: None,
            })
            .forget_lifetime()
        };
        // `cached`: what a frame whose step is in the viewer's iris layer
        // cache does: no layer passes, the main pass reads the stored layer.
        if !cached {
            let mut p = pass(enc, if sym { &sector.view } else { &cover.view }, tw(0));
            p.set_pipeline(&cover_pipe);
            p.set_bind_group(0, &uniform_bind, &[]);
            p.draw(0..3, 0..1);
        }
        if !cached {
            let mut p = pass(enc, &layer.view, tw(2));
            p.set_pipeline(&layer_pipe);
            p.set_bind_group(0, &uniform_bind, &[]);
            p.set_bind_group(1, if sym { &sector_bind } else { &cover_bind }, &[]);
            p.draw(0..3, 0..1);
        }
        if args.main {
            let mut p = pass(enc, &inter.view, tw(4));
            p.set_pipeline(&main_pipe);
            p.set_bind_group(0, &uniform_bind, &[]);
            p.set_bind_group(1, &cur_bind, &[]);
            p.set_bind_group(2, &next_bind, &[]);
            p.set_bind_group(3, &layer_bind, &[]);
            p.draw(0..3, 0..1);
        }
        if let Some(q) = &qset {
            enc.resolve_query_set(q, 0..6, &qresolve, 0);
            enc.copy_buffer_to_buffer(&qresolve, 0, &qread, 0, 6 * 8);
        }
    };

    let read_ts = || -> [f64; 3] {
        let slice = qread.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let ts: Vec<u64> = bytemuck::cast_slice(&slice.get_mapped_range()).to_vec();
        qread.unmap();
        let d = |a: usize| ts[a + 1].saturating_sub(ts[a]) as f64 * period_ns / 1e6;
        if cached {
            [0.0, 0.0, d(4)]
        } else {
            [d(0), d(2), d(4)]
        }
    };

    // Warm the V3D clock (it idles at 500 MHz and needs ~200 ms of load).
    let warm = Instant::now();
    let (u, p) = solve(&cfg, 0.5);
    queue.write_buffer(&ubuf, 0, bytemuck::bytes_of(&u));
    queue.write_buffer(&pbuf, 0, bytemuck::bytes_of(&p));
    while warm.elapsed().as_millis() < 600 {
        let mut enc = device.create_command_encoder(&Default::default());
        encode_frame(&mut enc);
        queue.submit([enc.finish()]);
        if timed {
            read_ts();
        } else {
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        }
    }

    if let Some(ms) = args.sequence_ms {
        // Every 60 Hz refresh of a transition of `ms`, eased as the viewer
        // does (smoothstep of linear progress): what a precompute must cover.
        let frames = (ms as f64 / (1000.0 / 60.0)).round() as u32;
        let mut per = Vec::new();
        let mut over = 0u32;
        for fi in 0..=frames {
            let b = fi as f32 / frames as f32;
            let t = b * b * (3.0 - 2.0 * b);
            let (u, p) = solve(&cfg, t);
            queue.write_buffer(&ubuf, 0, bytemuck::bytes_of(&u));
            queue.write_buffer(&pbuf, 0, bytemuck::bytes_of(&p));
            let mut v = Vec::new();
            let mut mm = Vec::new();
            for _ in 0..args.reps {
                let mut enc = device.create_command_encoder(&Default::default());
                encode_frame(&mut enc);
                queue.submit([enc.finish()]);
                let [a, b2, c] = read_ts();
                v.push(a + b2);
                mm.push(c);
            }
            let layer_ms = pct(&mut v, 0.5);
            let main_ms = pct(&mut mm, 0.5);
            // Frame budget at scale 2: layer + main + ~11 ms upscale within 16.7.
            if layer_ms + main_ms + 11.0 > 16.2 {
                over += 1;
            }
            per.push((fi, t, layer_ms, main_ms));
        }
        for (fi, t, l, m) in &per {
            if fi % 6 == 0 {
                println!("  frame {fi:3}  progress {t:5.3}  layer {l:7.3} ms  main {m:6.3} ms");
            }
        }
        let total: f64 = per.iter().map(|x| x.2).sum();
        let worst = per.iter().map(|x| x.2).fold(0.0, f64::max);
        println!(
            "# {} frames: layer total {:.1} ms (a precompute's GPU time), worst frame {:.3} ms, \
             frames over a 16.2 ms budget with an 11 ms upscale: {over}",
            per.len(),
            total,
            worst
        );
        return;
    }

    println!(
        "# progress  cover_med  cover_p90  layer_med  layer_p90  main_med  layer_total_med  (ms)"
    );
    let mut worst_total = 0.0f64;
    let mut sum_total = 0.0f64;
    for s in 0..args.steps {
        let t = if args.steps == 1 {
            0.5
        } else {
            s as f32 / (args.steps - 1) as f32
        };
        let (u, p) = solve(&cfg, t);
        queue.write_buffer(&ubuf, 0, bytemuck::bytes_of(&u));
        queue.write_buffer(&pbuf, 0, bytemuck::bytes_of(&p));
        let mut c = Vec::new();
        let mut l = Vec::new();
        let mut m = Vec::new();
        let mut tot = Vec::new();
        for _ in 0..args.reps {
            let mut enc = device.create_command_encoder(&Default::default());
            encode_frame(&mut enc);
            queue.submit([enc.finish()]);
            if timed {
                let [a, b, cm] = read_ts();
                c.push(a);
                l.push(b);
                m.push(cm);
                tot.push(a + b);
            } else {
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            }
        }
        if let Some(dir) = &args.dump {
            if args.main {
                dump_rgba8(
                    &device,
                    &queue,
                    &inter,
                    &dir.join(format!("main{s:02}.bin")),
                );
                continue;
            }
            dump_layer(
                &device,
                &queue,
                &layer,
                &dir.join(format!("step{s:02}.bin")),
            );
        }
        let med = |v: &mut Vec<f64>| pct(v, 0.5);
        let p90 = |v: &mut Vec<f64>| pct(v, 0.9);
        let tm = med(&mut tot);
        worst_total = worst_total.max(tm);
        sum_total += tm;
        println!(
            "  {t:5.2}   {:8.3}   {:8.3}   {:8.3}   {:8.3}   {:7.3}   {:8.3}",
            med(&mut c),
            p90(&mut c),
            med(&mut l),
            p90(&mut l),
            med(&mut m),
            tm
        );
    }
    println!(
        "# layer (cover+shade) median over steps: mean {:.3} ms, worst step {:.3} ms",
        sum_total / args.steps as f64,
        worst_total
    );
}

fn pct(v: &mut [f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * q).round() as usize]
}

/// Size of the symmetric variant's polar sector target: radius along x
/// (texels as fine as the layer's), the sector's angle along y (as many
/// texels as the arc at the screen corner needs at the layer's spacing).
fn sym_sector_size(lw: u32, lh: u32, blades: u32) -> (u32, u32) {
    let r_max = 0.5 * ((lw * lw + lh * lh) as f32).sqrt();
    let w = r_max.ceil() as u32 + 2;
    let arc = std::f32::consts::TAU / blades.max(1) as f32 * r_max;
    let h = arc.ceil() as u32 + 2;
    (w, h)
}

fn dump_layer(device: &wgpu::Device, queue: &wgpu::Queue, t: &Target, path: &std::path::Path) {
    let bpr = (t.w * 4).div_ceil(256) * 256;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (bpr * t.h) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &t.tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(t.h),
            },
        },
        wgpu::Extent3d {
            width: t.w,
            height: t.h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([enc.finish()]);
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = slice.get_mapped_range();
    let mut out = Vec::with_capacity((t.w * t.h * 4) as usize + 8);
    out.extend_from_slice(&t.w.to_le_bytes());
    out.extend_from_slice(&t.h.to_le_bytes());
    for row in 0..t.h {
        let s = (row * bpr) as usize;
        out.extend_from_slice(&data[s..s + (t.w * 4) as usize]);
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, out).unwrap();
}

/// The main pass's output (the intermediate, sRGB 8-bit) as raw RGBA.
fn dump_rgba8(device: &wgpu::Device, queue: &wgpu::Queue, t: &Target, path: &std::path::Path) {
    let bpr = (t.w * 4).div_ceil(256) * 256;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (bpr * t.h) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &t.tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(t.h),
            },
        },
        wgpu::Extent3d {
            width: t.w,
            height: t.h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([enc.finish()]);
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = slice.get_mapped_range();
    let mut out = Vec::new();
    out.extend_from_slice(&t.w.to_le_bytes());
    out.extend_from_slice(&t.h.to_le_bytes());
    for row in 0..t.h {
        let s = (row * bpr) as usize;
        out.extend_from_slice(&data[s..s + (t.w * 4) as usize]);
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, out).unwrap();
}

fn half(b: [u8; 2]) -> f32 {
    let h = u16::from_le_bytes(b);
    let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = ((h >> 10) & 0x1f) as i32;
    let m = (h & 0x3ff) as f32;
    s * match e {
        0 => m * 2f32.powi(-24),
        31 => f32::INFINITY,
        _ => (1.0 + m / 1024.0) * 2f32.powi(e - 15),
    }
}

/// Compare two `--dump` directories: per step, the shade (rgb, in 8-bit
/// sRGB-ish LSB of linear*255) and the distance (alpha, screen pixels) where
/// it matters to the main pass (|d| < 64 px).
fn compare_dumps(a: &std::path::Path, b: &std::path::Path) {
    let mut names: Vec<_> = std::fs::read_dir(a)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .collect();
    names.sort();
    println!(
        "# step  shade_max_x255  shade_px_over_1lsb  dist_max_err_px(|d|<64)  dist_p99_err_px"
    );
    for name in names {
        let ra = std::fs::read(a.join(&name)).unwrap();
        let rb = std::fs::read(b.join(&name)).unwrap();
        assert_eq!(ra[..8], rb[..8], "sizes differ");
        let px = (ra.len() - 8) / 4;
        let mut shade_max = 0.0f32;
        let mut over = 0usize;
        let mut derr = Vec::new();
        for i in 0..px {
            let o = 8 + i * 4;
            let ga = |r: &[u8], c: usize| half([r[o + 2 * c], r[o + 2 * c + 1]]);
            let da = ga(&ra, 1);
            let db = ga(&rb, 1);
            if da.abs() < 64.0 || db.abs() < 64.0 {
                derr.push((da - db).abs() as f64);
                // Shade as a multiple of the petal color, in 1/255 steps.
                let e = (ga(&ra, 0) - ga(&rb, 0)).abs() * 255.0;
                shade_max = shade_max.max(e);
                if e > 1.0 {
                    over += 1;
                }
            }
        }
        let dmax = derr.iter().cloned().fold(0.0, f64::max);
        let p99 = pct(&mut derr, 0.99);
        println!(
            "  {:?}  {shade_max:6.2}  {over:8}  {dmax:8.3}  {p99:8.3}",
            name.to_string_lossy()
        );
    }
}
