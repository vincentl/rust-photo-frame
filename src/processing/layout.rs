pub fn resize_to_cover(
    canvas_w: u32,
    canvas_h: u32,
    src_w: u32,
    src_h: u32,
    max_dim: u32,
) -> (u32, u32) {
    let iw = src_w.max(1) as f32;
    let ih = src_h.max(1) as f32;
    let cw = canvas_w.max(1) as f32;
    let ch = canvas_h.max(1) as f32;
    let scale = (cw / iw).max(ch / ih).max(1.0);
    let w = (iw * scale).round().clamp(1.0, max_dim as f32);
    let h = (ih * scale).round().clamp(1.0, max_dim as f32);
    (w as u32, h as u32)
}

pub fn resize_to_contain(
    canvas_w: u32,
    canvas_h: u32,
    src_w: u32,
    src_h: u32,
    max_dim: u32,
) -> (u32, u32) {
    let iw = src_w.max(1) as f32;
    let ih = src_h.max(1) as f32;
    let cw = canvas_w.max(1) as f32;
    let ch = canvas_h.max(1) as f32;
    let scale = (cw / iw).min(ch / ih).max(0.0);
    let scale = if scale.is_finite() { scale } else { 1.0 };
    let w = (iw * scale).round().clamp(1.0, max_dim as f32);
    let h = (ih * scale).round().clamp(1.0, max_dim as f32);
    (w as u32, h as u32)
}

pub fn center_offset(inner_w: u32, inner_h: u32, outer_w: u32, outer_h: u32) -> (u32, u32) {
    let ox = outer_w.saturating_sub(inner_w) / 2;
    let oy = outer_h.saturating_sub(inner_h) / 2;
    (ox, oy)
}
