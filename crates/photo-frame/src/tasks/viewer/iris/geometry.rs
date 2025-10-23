use std::f32::consts::{FRAC_PI_2, TAU};

use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Cubic {
    pub p0: [f32; 2],
    pub p1: [f32; 2],
    pub p2: [f32; 2],
    pub p3: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Params {
    pub mvp: [[f32; 4]; 4],
    pub viewport_px: [f32; 2],
    pub half_width_px: f32,
    pub _pad0: f32,
    pub segments_per_cubic: u32,
    pub petal_count: u32,
    pub cubics_count: u32,
    pub _pad1: u32,
    pub color_rgba: [f32; 4],
}

#[derive(Clone)]
pub struct IrisStrokeBuffers {
    pub cubics: wgpu::Buffer,
    pub params: wgpu::Buffer,
}

pub fn build_iris_petal_cubics(
    radius: f32,
    count: u32,
    value: f32,
    segments_per_90deg: u32,
) -> Vec<Cubic> {
    if !radius.is_finite() || radius <= 0.0 || count == 0 {
        return Vec::new();
    }

    let count_f = count as f32;
    let step = std::f32::consts::PI * (0.5 + 2.0 / count_f);
    let p1x = step.cos() * radius;
    let p1y = step.sin() * radius;

    let cos_v = (-value).cos();
    let sin_v = (-value).sin();

    let c1x = p1x - cos_v * p1x - sin_v * p1y;
    let c1y = p1y - cos_v * p1y + sin_v * p1x;

    let dx = -sin_v * radius - c1x;
    let dy = radius - cos_v * radius - c1y;
    let dc = (dx * dx + dy * dy).sqrt();
    let denom = (2.0 * radius).max(f32::EPSILON);
    let cos_term = (dc / denom).clamp(-1.0, 1.0);
    let a = dy.atan2(dx) - cos_term.acos();
    let x = c1x + a.cos() * radius;
    let y = c1y + a.sin() * radius;

    let mut cubics = Vec::new();

    let theta_p1 = p1y.atan2(p1x);
    let theta_q = FRAC_PI_2;
    // Move along the circle counter-clockwise from p1 to q so endpoints slide
    // along the next curve as the aperture animates.
    cubics.extend(arc_to_cubics(
        [0.0, 0.0],
        radius,
        theta_p1,
        theta_q,
        true,
        segments_per_90deg,
    ));

    let theta_q_c1 = (radius - c1y).atan2(0.0 - c1x);
    let theta_r_c1 = (y - c1y).atan2(x - c1x);
    cubics.extend(arc_to_cubics(
        [c1x, c1y],
        radius,
        theta_q_c1,
        theta_r_c1,
        true,
        segments_per_90deg,
    ));

    cubics
}

fn arc_to_cubics(
    center: [f32; 2],
    radius: f32,
    a0: f32,
    a1: f32,
    ccw: bool,
    segments_per_90deg: u32,
) -> Vec<Cubic> {
    if !radius.is_finite() || radius <= 0.0 {
        return Vec::new();
    }

    let mut total_angle = a1 - a0;
    if ccw {
        while total_angle <= 0.0 {
            total_angle += TAU;
        }
    } else {
        while total_angle >= 0.0 {
            total_angle -= TAU;
        }
    }

    let abs_angle = total_angle.abs();
    if abs_angle < f32::EPSILON {
        return Vec::new();
    }

    let quarter_count = ((abs_angle / FRAC_PI_2).ceil() as u32).max(1);
    let seg_per_quarter = segments_per_90deg.max(1);
    let total_segments = quarter_count.saturating_mul(seg_per_quarter).max(1);
    let delta = total_angle / total_segments as f32;

    let mut cubics = Vec::with_capacity(total_segments as usize);
    for idx in 0..total_segments {
        let theta0 = a0 + delta * idx as f32;
        let theta1 = theta0 + delta;

        let p0 = point_on_circle(center, radius, theta0);
        let p3 = point_on_circle(center, radius, theta1);

        let k = 4.0 / 3.0 * (delta.abs() / 4.0).tan();
        let t0 = tangent(theta0, ccw);
        let t1 = tangent(theta1, ccw);

        let p1 = [p0[0] + k * radius * t0[0], p0[1] + k * radius * t0[1]];
        let p2 = [p3[0] - k * radius * t1[0], p3[1] - k * radius * t1[1]];

        cubics.push(Cubic { p0, p1, p2, p3 });
    }

    cubics
}

fn point_on_circle(center: [f32; 2], radius: f32, theta: f32) -> [f32; 2] {
    [
        center[0] + radius * theta.cos(),
        center[1] + radius * theta.sin(),
    ]
}

fn tangent(theta: f32, ccw: bool) -> [f32; 2] {
    if ccw {
        [-theta.sin(), theta.cos()]
    } else {
        [theta.sin(), -theta.cos()]
    }
}

fn make_mvp(radius: f32, rotation: f32, aspect: f32) -> [[f32; 4]; 4] {
    // Scale by 1/radius to normalize geometry, then aspect-correct Y so that
    // circles remain circular in screen pixels (width-based sizing).
    let scale = if radius.abs() < f32::EPSILON { 1.0 } else { 1.0 / radius };
    let (sin_r, cos_r) = rotation.sin_cos();

    [
        [cos_r * scale, -sin_r * scale, 0.0, 0.0],
        [sin_r * scale * aspect, cos_r * scale * aspect, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

pub fn rebuild_buffers(
    device: &wgpu::Device,
    cfg: &super::IrisConfig,
    viewport: [f32; 2],
) -> (IrisStrokeBuffers, usize, u32, Params) {
    let mut cubics = build_iris_petal_cubics(
        cfg.radius.max(f32::EPSILON),
        cfg.petal_count.max(1),
        cfg.value,
        cfg.segments_per_90deg.max(1),
    );

    let actual_cubic_count = cubics.len() as u32;
    if cubics.is_empty() {
        cubics.push(Cubic {
            p0: [0.0; 2],
            p1: [0.0; 2],
            p2: [0.0; 2],
            p3: [0.0; 2],
        });
    }

    let segments_per_cubic = cfg.segments_per_cubic.max(1);
    let vertex_count = 2 * segments_per_cubic as usize * actual_cubic_count as usize;
    let instance_count = cfg.petal_count.max(1);

    let aspect = if viewport[1] > 0.0 { viewport[0] / viewport[1] } else { 1.0 };
    let params = Params {
        mvp: make_mvp(cfg.radius.max(f32::EPSILON), cfg.rotation, aspect),
        viewport_px: viewport,
        half_width_px: 0.5 * cfg.stroke_px,
        _pad0: 0.0,
        segments_per_cubic,
        petal_count: cfg.petal_count.max(1),
        cubics_count: actual_cubic_count,
        _pad1: 0,
        color_rgba: cfg.color_rgba,
    };

    let cubics_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("iris-cubics"),
        contents: bytemuck::cast_slice(&cubics),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });
    let params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("iris-params"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });

    (
        IrisStrokeBuffers {
            cubics: cubics_buf,
            params: params_buf,
        },
        vertex_count,
        instance_count,
        params,
    )
}
