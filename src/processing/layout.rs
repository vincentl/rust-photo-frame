pub fn resize_to_cover(
    canvas_w: u32,
    canvas_h: u32,
    src_w: u32,
    src_h: u32,
    max_dim: u32,
) -> (u32, u32) {
    let iw = src_w.max(1) as f64;
    let ih = src_h.max(1) as f64;
    let cw = canvas_w.max(1) as f64;
    let ch = canvas_h.max(1) as f64;
    let mut scale = (cw / iw).max(ch / ih);
    if !scale.is_finite() || scale <= 0.0 {
        scale = 1.0;
    }

    let w = (iw * scale).ceil().clamp(1.0, max_dim as f64);
    let h = (ih * scale).ceil().clamp(1.0, max_dim as f64);
    (w as u32, h as u32)
}

pub fn resize_to_contain(
    canvas_w: u32,
    canvas_h: u32,
    src_w: u32,
    src_h: u32,
    max_dim: u32,
) -> (u32, u32) {
    let iw = src_w.max(1) as f64;
    let ih = src_h.max(1) as f64;
    let cw = canvas_w.max(1) as f64;
    let ch = canvas_h.max(1) as f64;
    let scale = (cw / iw).min(ch / ih).max(0.0);
    let scale = if scale.is_finite() { scale } else { 1.0 };
    let w = (iw * scale).round().clamp(1.0, max_dim as f64);
    let h = (ih * scale).round().clamp(1.0, max_dim as f64);
    (w as u32, h as u32)
}

pub fn center_offset(inner_w: u32, inner_h: u32, outer_w: u32, outer_h: u32) -> (u32, u32) {
    let ox = outer_w.saturating_sub(inner_w) / 2;
    let oy = outer_h.saturating_sub(inner_h) / 2;
    (ox, oy)
}

#[cfg(test)]
mod tests {
    use super::resize_to_cover;

    #[test]
    fn cover_upscales_to_fill_canvas() {
        let (w, h) = resize_to_cover(1920, 1080, 800, 600, 8192);
        assert_eq!((w, h), (1920, 1440));
    }

    #[test]
    fn cover_downscales_when_source_is_larger() {
        let (w, h) = resize_to_cover(1920, 1080, 4000, 3000, 8192);
        assert_eq!((w, h), (1920, 1440));
    }

    #[test]
    fn cover_never_underfills_within_limits() {
        let (w, h) = resize_to_cover(1921, 1080, 3217, 2000, 8192);
        assert!(w >= 1921);
        assert!(h >= 1080);
    }
}
