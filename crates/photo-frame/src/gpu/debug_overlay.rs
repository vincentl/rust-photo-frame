//! Minimal overlay helpers for isolating clear/draw paths during debugging.

#[allow(dead_code)]
/// Clears the target view with `clear_color` and optionally executes `draw_fn`
/// inside the render pass. The helper makes it easy to rule out pipeline state
/// issues by swapping the closure for a no-op draw.
pub fn render<F>(
    encoder: &mut wgpu::CommandEncoder,
    target: &wgpu::TextureView,
    label: &str,
    clear_color: wgpu::Color,
    draw_fn: Option<F>,
) where
    F: FnOnce(&mut wgpu::RenderPass<'_>),
{
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(clear_color),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        occlusion_query_set: None,
        timestamp_writes: None,
    });
    if let Some(draw) = draw_fn {
        draw(&mut pass);
    }
}
