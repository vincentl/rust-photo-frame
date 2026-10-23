//! The Wi-Fi recovery overlay, drawn by the viewer itself (both outputs):
//! wifi-manager sends the hotspot state over the control socket
//! (`wifi-overlay-show`), and `presentation.rs` decides when it is the layer
//! on screen.
//!
//! It looks like the greeting and sleep screens (their background, colours,
//! font and double-stroke frame, from `greeting-screen`). Inside the frame
//! one panel carries the instructions and two QR codes (join the hotspot;
//! open the setup page), in the same order and words as wifi-manager's
//! fallback window. The panel is rasterized on the CPU into one opaque sRGB
//! texture and composited as a single scissored triangle (`panel.rs`), and
//! rebuilt only when the content or the surface size changes, so the lease
//! refreshes wifi-manager sends every few seconds cost nothing.

use glyphon::{
    Attrs, Buffer, Color as GlyphonColor, FamilyOwned, FontSystem, Metrics, Shaping, SwashCache,
    Wrap,
};
use palette::{LinSrgba, Srgba};
use qrcode::{Color as QrColor, EcLevel, QrCode};
use winit::dpi::PhysicalSize;

use config_model::control::WifiOverlay;

use super::panel::{self, Encoding, PanelCompositor};
use crate::config::ScreenMessageConfig;
use crate::gpu::debug_overlay;
use crate::tasks::greeting_screen::{self, FrameRenderer};

const STEP_JOIN: &str = "1. Scan the QR code OR join the hotspot manually:";
const STEP_PASSWORD: &str = "2. If joining manually, enter this password (include hyphens):";
const STEP_SCAN_JOIN: &str = "3. Scan to join the hotspot:";
const STEP_SCAN_SETUP: &str = "4. Then scan to open setup:";

/// QR quiet zone, in modules (the standard's minimum).
const QUIET_MODULES: u32 = 4;

/// A QR code as a square of modules, row-major, `true` = dark.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QrModules {
    pub(crate) width: u32,
    pub(crate) dark: Vec<bool>,
}

impl QrModules {
    pub(crate) fn is_dark(&self, x: u32, y: u32) -> bool {
        self.dark[(y * self.width + x) as usize]
    }
}

/// Encode `payload` at error-correction level M (a phone camera reads it
/// from across a room; M survives a glare spot).
pub(crate) fn qr_modules(payload: &str) -> Result<QrModules, String> {
    let code = QrCode::with_error_correction_level(payload.as_bytes(), EcLevel::M)
        .map_err(|err| format!("cannot encode as a QR code: {err}"))?;
    let width = code.width() as u32;
    let dark = code
        .to_colors()
        .into_iter()
        .map(|c| c == QrColor::Dark)
        .collect();
    Ok(QrModules { width, dark })
}

/// A pixel rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rect {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) w: u32,
    pub(crate) h: u32,
}

impl Rect {
    fn right(&self) -> u32 {
        self.x + self.w
    }
    fn bottom(&self) -> u32 {
        self.y + self.h
    }
    #[cfg(test)]
    pub(crate) fn contains(&self, other: &Rect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }
    #[cfg(test)]
    pub(crate) fn overlaps(&self, other: &Rect) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

/// Where everything goes. `panel` is in surface pixels; everything else is
/// relative to the panel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OverlayLayout {
    pub(crate) panel: Rect,
    /// Pixels per design unit (the design is 1800 × 1000 units).
    pub(crate) unit: f32,
    /// The instructions: headline, body, steps 1 and 2, footer at its foot.
    pub(crate) text: Rect,
    /// Steps 3 and 4: one label above each QR card.
    pub(crate) join_label: Rect,
    pub(crate) portal_label: Rect,
    /// The QR cards: white squares holding the code and its quiet zone.
    pub(crate) join_card: Rect,
    pub(crate) portal_card: Rect,
    /// "Or type: <url>" under the cards.
    pub(crate) url: Rect,
}

/// The overlay's layout for a `surface`, with the frame taking `padding_px`
/// on each side. Landscape surfaces put the instructions on the left and
/// the two QR cards side by side on the right; narrow ones stack them.
pub(crate) fn overlay_layout(surface: (u32, u32), padding_px: f32) -> OverlayLayout {
    let pad = padding_px.ceil().max(0.0) as u32;
    let pw = surface.0.saturating_sub(2 * pad).max(1);
    let ph = surface.1.saturating_sub(2 * pad).max(1);
    let panel = Rect {
        x: pad.min(surface.0),
        y: pad.min(surface.1),
        w: pw,
        h: ph,
    };
    let (pwf, phf) = (pw as f32, ph as f32);
    let landscape = pwf >= phf * 1.2;
    let unit = if landscape {
        (pwf / 1800.0).min(phf / 1000.0)
    } else {
        (pwf / 1000.0).min(phf / 1800.0)
    }
    .max(0.1);
    let u = |v: f32| (v * unit).round().max(0.0) as u32;
    let margin = u(60.0);
    let gap = u(40.0);
    let label_h = u(84.0);
    let url_h = u(56.0);

    if landscape {
        let half = pw / 2;
        let text = Rect {
            x: margin,
            y: margin,
            w: half.saturating_sub(margin + gap / 2),
            h: ph.saturating_sub(2 * margin),
        };
        let right_x = half + gap / 2;
        let right_w = pw.saturating_sub(right_x + margin);
        let card_by_width = right_w.saturating_sub(gap) / 2;
        let card_by_height = ph.saturating_sub(2 * margin + label_h + url_h);
        let card = card_by_width.min(card_by_height);
        let group_h = label_h + card + url_h;
        let top = margin + ph.saturating_sub(2 * margin + group_h) / 2;
        let cards_w = 2 * card + gap;
        let left = right_x + right_w.saturating_sub(cards_w) / 2;
        let join_card = Rect {
            x: left,
            y: top + label_h,
            w: card,
            h: card,
        };
        let portal_card = Rect {
            x: left + card + gap,
            ..join_card
        };
        OverlayLayout {
            panel,
            unit,
            text,
            join_label: Rect {
                x: join_card.x,
                y: top,
                w: card,
                h: label_h,
            },
            portal_label: Rect {
                x: portal_card.x,
                y: top,
                w: card,
                h: label_h,
            },
            join_card,
            portal_card,
            url: Rect {
                x: left,
                y: join_card.bottom(),
                w: cards_w,
                h: url_h,
            },
        }
    } else {
        let text_h = ph * 44 / 100;
        let text = Rect {
            x: margin,
            y: margin,
            w: pw.saturating_sub(2 * margin),
            h: text_h.saturating_sub(margin),
        };
        let below = text.bottom() + gap;
        let card_by_width = pw.saturating_sub(2 * margin + gap) / 2;
        let room = ph.saturating_sub(below + margin);
        let card = card_by_width.min(room.saturating_sub(label_h + url_h));
        // The code group is centred in the space under the instructions.
        let top = below + room.saturating_sub(label_h + card + url_h) / 2;
        let cards_w = 2 * card + gap;
        let left = pw.saturating_sub(cards_w) / 2;
        let join_card = Rect {
            x: left,
            y: top + label_h,
            w: card,
            h: card,
        };
        let portal_card = Rect {
            x: left + card + gap,
            ..join_card
        };
        OverlayLayout {
            panel,
            unit,
            text,
            join_label: Rect {
                x: join_card.x,
                y: top,
                w: card,
                h: label_h,
            },
            portal_label: Rect {
                x: portal_card.x,
                y: top,
                w: card,
                h: label_h,
            },
            join_card,
            portal_card,
            url: Rect {
                x: left,
                y: join_card.bottom(),
                w: cards_w,
                h: url_h,
            },
        }
    }
}

/// The pixel size of one module in a `card`-sized square holding a
/// `modules`-wide code plus its quiet zone: an integer, so every module
/// is the same size and the code stays crisp.
pub(crate) fn module_px(card: u32, modules: u32) -> u32 {
    card / (modules + 2 * QUIET_MODULES)
}

/// Where one QR code landed, in surface pixels (for tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QrPlacement {
    pub(crate) origin: (u32, u32),
    pub(crate) module_px: u32,
    pub(crate) modules: QrModules,
}

fn srgb_bytes(colour: LinSrgba<f32>) -> [u8; 4] {
    let srgb: Srgba<u8> = Srgba::<f32>::from_linear(colour).into_format();
    [srgb.red, srgb.green, srgb.blue, 255]
}

fn glyph_colour(colour: LinSrgba<f32>) -> GlyphonColor {
    let [r, g, b, _] = srgb_bytes(colour);
    GlyphonColor::rgb(r, g, b)
}

fn mix(a: LinSrgba<f32>, b: LinSrgba<f32>, t: f32) -> LinSrgba<f32> {
    LinSrgba::new(
        a.red + (b.red - a.red) * t,
        a.green + (b.green - a.green) * t,
        a.blue + (b.blue - a.blue) * t,
        1.0,
    )
}

/// An opaque sRGB RGBA canvas.
struct Canvas {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Canvas {
    fn new(w: u32, h: u32, fill: [u8; 4]) -> Self {
        let mut px = vec![0u8; (w * h * 4) as usize];
        for p in px.chunks_exact_mut(4) {
            p.copy_from_slice(&fill);
        }
        Self { w, h, px }
    }

    fn fill(&mut self, r: Rect, colour: [u8; 4]) {
        let x1 = r.right().min(self.w);
        let y1 = r.bottom().min(self.h);
        for y in r.y.min(self.h)..y1 {
            for x in r.x.min(self.w)..x1 {
                let i = ((y * self.w + x) * 4) as usize;
                self.px[i..i + 4].copy_from_slice(&colour);
            }
        }
    }
}

/// The overlay renderer; one per GPU context, like the greeting scenes.
pub(crate) struct WifiOverlayScreen {
    queue: wgpu::Queue,
    panel: PanelCompositor,
    frame: FrameRenderer,
    font_system: FontSystem,
    swash_cache: SwashCache,
    family: FamilyOwned,
    background: LinSrgba<f32>,
    font_colour: LinSrgba<f32>,
    accent_colour: LinSrgba<f32>,
    stroke_dip: f32,
    corner_dip: f32,
    size: PhysicalSize<u32>,
    scale_factor: f64,
    content: Option<WifiOverlay>,
    dirty: bool,
    redraw_pending: bool,
    layout: Option<OverlayLayout>,
    placements: Vec<QrPlacement>,
}

impl WifiOverlayScreen {
    /// `style` is the greeting screen's config: the overlay shares its
    /// colours, font and frame.
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        style: &ScreenMessageConfig,
    ) -> Self {
        let mut font_system = FontSystem::new();
        greeting_screen::initialize_font_database(font_system.db_mut());
        let family = greeting_screen::resolve_font_family(&font_system, style.font.as_deref());
        let stroke_dip = style.effective_stroke_width_dip();
        Self {
            queue: queue.clone(),
            // sRGB texels: the opaque panel must match the clear colour
            // around it exactly.
            panel: PanelCompositor::new(
                device,
                queue,
                format,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                "wifi-overlay-composite",
            ),
            frame: FrameRenderer::new(device, format),
            font_system,
            swash_cache: SwashCache::new(),
            family,
            background: greeting_screen::resolve_background_colour(
                style.colors.background.as_deref(),
            ),
            font_colour: greeting_screen::resolve_font_colour(style.colors.font.as_deref()),
            accent_colour: greeting_screen::resolve_accent_colour(style.colors.accent.as_deref()),
            stroke_dip,
            corner_dip: style.effective_corner_radius_dip(stroke_dip),
            size: PhysicalSize::new(0, 0),
            scale_factor: 1.0,
            content: None,
            dirty: true,
            redraw_pending: true,
            layout: None,
            placements: Vec::new(),
        }
    }

    /// New content; a no-op (no rebuild) when only the lease changed.
    pub(crate) fn set_content(&mut self, content: &WifiOverlay) {
        if self
            .content
            .as_ref()
            .is_some_and(|c| c.same_content(content))
        {
            return;
        }
        self.content = Some(content.clone());
        self.dirty = true;
        self.redraw_pending = true;
    }

    pub(crate) fn clear(&mut self) {
        self.content = None;
        self.dirty = true;
    }

    pub(crate) fn resize(&mut self, size: PhysicalSize<u32>, scale_factor: f64) {
        if self.size != size || (self.scale_factor - scale_factor).abs() > f64::EPSILON {
            self.size = size;
            self.scale_factor = scale_factor;
            self.dirty = true;
            self.redraw_pending = true;
        }
    }

    pub(crate) fn mark_redraw_needed(&mut self) {
        self.redraw_pending = true;
    }

    pub(crate) fn needs_redraw(&self) -> bool {
        self.redraw_pending
    }

    fn padding_px(&self) -> (f32, f32, f32) {
        let scale = self.scale_factor.max(0.0) as f32;
        let stroke = (self.stroke_dip * scale).max(0.0);
        let corner = (self.corner_dip * scale).max(0.0);
        (
            greeting_screen::frame_padding_px(stroke, corner),
            stroke,
            corner,
        )
    }

    /// Draw the overlay over the whole target. False when there is nothing
    /// to draw (no content, no size).
    pub(crate) fn render(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
    ) -> bool {
        if self.size.width == 0 || self.size.height == 0 || self.content.is_none() {
            return false;
        }
        if self.dirty || self.layout.is_none() {
            if !self.rebuild() {
                return false;
            }
            self.dirty = false;
        }
        let Some(layout) = self.layout else {
            return false;
        };
        debug_overlay::render(
            encoder,
            target_view,
            "wifi-overlay-background",
            greeting_screen::to_wgpu_color(self.background),
            None::<fn(&mut wgpu::RenderPass<'_>)>,
        );
        self.frame.render(encoder, target_view);
        let drawn = self.panel.composite(
            encoder,
            target_view,
            (self.size.width, self.size.height),
            layout.panel.x,
            layout.panel.y,
        );
        if drawn {
            self.redraw_pending = false;
        }
        drawn
    }

    pub(crate) fn after_submit(&mut self) {
        let _ = self.panel.poll();
    }

    fn shape(&mut self, text: &str, size_px: f32, width: f32) -> (Buffer, f32) {
        let metrics = Metrics::new(size_px.max(1.0), (size_px * 1.25).max(1.0));
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_wrap(&mut self.font_system, Wrap::WordOrGlyph);
        buffer.set_size(&mut self.font_system, Some(width.max(1.0)), None);
        let attrs = Attrs::new().family(self.family.as_family());
        buffer.set_text(&mut self.font_system, text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);
        let height = buffer
            .layout_runs()
            .map(|run| run.line_top + run.line_height)
            .fold(0.0_f32, f32::max);
        (buffer, height)
    }

    fn draw(
        &mut self,
        canvas: &mut Canvas,
        buffer: &Buffer,
        colour: LinSrgba<f32>,
        at: (i32, i32),
    ) {
        let clip = (0, 0, canvas.w as i32, canvas.h as i32);
        panel::blend_glyphs(
            buffer,
            &mut self.font_system,
            &mut self.swash_cache,
            glyph_colour(colour),
            &mut canvas.px,
            canvas.w,
            at,
            clip,
            Encoding::SrgbOpaque,
        );
    }

    /// Lay out, rasterize and upload the panel.
    fn rebuild(&mut self) -> bool {
        let Some(content) = self.content.clone() else {
            return false;
        };
        let (padding, stroke, corner) = self.padding_px();
        self.frame.update(
            &self.queue,
            self.size,
            stroke,
            corner,
            self.accent_colour,
            self.background,
        );
        let layout = overlay_layout((self.size.width, self.size.height), padding);
        let mut canvas = Canvas::new(layout.panel.w, layout.panel.h, srgb_bytes(self.background));

        // Instructions, flowed top-down; shrunk until they clear the footer.
        let mut scale = 1.0_f32;
        loop {
            if self.flow_text(&mut canvas, &layout, &content, scale, true) || scale < 0.5 {
                break;
            }
            scale *= 0.85;
        }
        self.flow_text(&mut canvas, &layout, &content, scale, false);

        // The QR column.
        let u = layout.unit;
        let label_size = 28.0 * u;
        let mut placements = Vec::with_capacity(2);
        for (label, label_rect, card, payload) in [
            (
                STEP_SCAN_JOIN,
                layout.join_label,
                layout.join_card,
                &content.join_qr,
            ),
            (
                STEP_SCAN_SETUP,
                layout.portal_label,
                layout.portal_card,
                &content.portal_url,
            ),
        ] {
            let (buffer, h) = self.shape(label, label_size, label_rect.w as f32);
            let y = label_rect.bottom() as f32 - h - 12.0 * u;
            self.draw(
                &mut canvas,
                &buffer,
                self.font_colour,
                (label_rect.x as i32, y.max(label_rect.y as f32) as i32),
            );
            match qr_modules(payload) {
                Ok(modules) => {
                    let m = module_px(card.w, modules.width);
                    if m == 0 {
                        continue;
                    }
                    let side = m * (modules.width + 2 * QUIET_MODULES);
                    let card = Rect {
                        x: card.x + (card.w - side) / 2,
                        y: card.y + (card.h - side) / 2,
                        w: side,
                        h: side,
                    };
                    canvas.fill(card, [255, 255, 255, 255]);
                    let origin = (card.x + QUIET_MODULES * m, card.y + QUIET_MODULES * m);
                    for y in 0..modules.width {
                        for x in 0..modules.width {
                            if modules.is_dark(x, y) {
                                canvas.fill(
                                    Rect {
                                        x: origin.0 + x * m,
                                        y: origin.1 + y * m,
                                        w: m,
                                        h: m,
                                    },
                                    [0, 0, 0, 255],
                                );
                            }
                        }
                    }
                    placements.push(QrPlacement {
                        origin: (layout.panel.x + origin.0, layout.panel.y + origin.1),
                        module_px: m,
                        modules,
                    });
                }
                Err(err) => {
                    // validate_wifi_overlay rejects these before they get here.
                    tracing::warn!("wifi_overlay_qr_skipped: {err}");
                }
            }
        }
        let (buffer, _) = self.shape(
            &format!("Or type: {}", content.portal_url),
            label_size,
            layout.url.w as f32,
        );
        self.draw(
            &mut canvas,
            &buffer,
            self.font_colour,
            (layout.url.x as i32, (layout.url.y as f32 + 12.0 * u) as i32),
        );

        self.panel.upload(canvas.w, canvas.h, &canvas.px);
        self.layout = Some(layout);
        self.placements = placements;
        tracing::debug!(
            width = canvas.w,
            height = canvas.h,
            unit = layout.unit,
            text_scale = scale,
            "wifi_overlay_rebuilt"
        );
        true
    }

    /// Flow headline, body and steps 1-2 down the text column, with the
    /// footer pinned to its foot. With `measure_only`, draw nothing and
    /// report whether everything fits above the footer.
    fn flow_text(
        &mut self,
        canvas: &mut Canvas,
        layout: &OverlayLayout,
        content: &WifiOverlay,
        scale: f32,
        measure_only: bool,
    ) -> bool {
        let u = layout.unit * scale;
        let col = layout.text;
        let width = col.w as f32;
        let mut y = col.y as f32;
        let x = col.x as i32;

        let (footer, footer_h) = self.shape(&content.footer, 26.0 * u, width);
        let footer_top = col.bottom() as f32 - footer_h;

        let blocks: [(&str, f32, f32, LinSrgba<f32>); 2] = [
            (&content.headline, 60.0, 20.0, self.font_colour),
            (&content.body, 32.0, 52.0, self.font_colour),
        ];
        for (text, size, after, colour) in blocks {
            let (buffer, h) = self.shape(text, size * u, width);
            if !measure_only {
                self.draw(canvas, &buffer, colour, (x, y as i32));
            }
            y += h + after * u;
        }
        let box_fill = srgb_bytes(mix(self.background, self.accent_colour, 0.18));
        for (label, value) in [
            (STEP_JOIN, content.ssid.as_str()),
            (STEP_PASSWORD, content.password.as_str()),
        ] {
            let (buffer, h) = self.shape(label, 28.0 * u, width);
            if !measure_only {
                self.draw(canvas, &buffer, self.font_colour, (x, y as i32));
            }
            y += h + 12.0 * u;
            let pad = 14.0 * u;
            let (value_buffer, vh) = self.shape(value, 44.0 * u, width - 2.0 * pad);
            let box_h = vh + 2.0 * pad;
            if !measure_only {
                canvas.fill(
                    Rect {
                        x: col.x,
                        y: y as u32,
                        w: col.w,
                        h: box_h.ceil() as u32,
                    },
                    box_fill,
                );
                self.draw(
                    canvas,
                    &value_buffer,
                    self.accent_colour,
                    ((col.x as f32 + pad) as i32, (y + pad) as i32),
                );
            }
            y += box_h + 36.0 * u;
        }
        if !measure_only {
            self.draw(
                canvas,
                &footer,
                mix(self.font_colour, self.background, 0.3),
                (x, footer_top as i32),
            );
        }
        y <= footer_top
    }

    #[cfg(test)]
    pub(crate) fn placements(&self) -> &[QrPlacement] {
        &self.placements
    }

    #[cfg(test)]
    pub(crate) fn layout(&self) -> Option<OverlayLayout> {
        self.layout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOIN: &str = "WIFI:T:WPA;S:PhotoFrame-Setup;P:apple-river-stone;;";
    const PORTAL: &str = "http://192.168.4.1:8080/";

    fn content() -> WifiOverlay {
        WifiOverlay {
            ttl_sec: 30,
            headline: "Reconnect to Wi-Fi".into(),
            body: "Use another device to restore connectivity.".into(),
            ssid: "PhotoFrame-Setup".into(),
            password: "apple-river-stone".into(),
            join_qr: JOIN.into(),
            portal_url: PORTAL.into(),
            footer: "The slideshow resumes automatically after the frame reconnects.".into(),
        }
    }

    #[test]
    fn qr_codes_encode_at_their_version_width() {
        for payload in [JOIN, PORTAL] {
            let code = qr_modules(payload).expect("encodes");
            assert_eq!((code.width - 17) % 4, 0, "width {} is 4v+17", code.width);
            assert_eq!(code.dark.len() as u32, code.width * code.width);
            // Finder pattern: the top-left corner module is dark.
            assert!(code.is_dark(0, 0));
        }
        // The protocol's longest payload still encodes.
        let long = "x".repeat(config_model::control::MAX_QR_PAYLOAD_BYTES);
        assert!(qr_modules(&long).is_ok());
    }

    #[test]
    fn layout_stays_inside_the_panel_without_overlaps() {
        for surface in [
            (1920, 1080),
            (3840, 2160),
            (1280, 800),
            (1080, 1920),
            (800, 1280),
        ] {
            let layout = overlay_layout(surface, 28.0);
            let panel = Rect {
                x: 0,
                y: 0,
                w: layout.panel.w,
                h: layout.panel.h,
            };
            assert!(layout.panel.right() <= surface.0 && layout.panel.bottom() <= surface.1);
            let parts = [
                ("text", layout.text),
                ("join_label", layout.join_label),
                ("portal_label", layout.portal_label),
                ("join_card", layout.join_card),
                ("portal_card", layout.portal_card),
                ("url", layout.url),
            ];
            for (name, rect) in parts {
                assert!(
                    panel.contains(&rect),
                    "{surface:?}: {name} {rect:?} outside {panel:?}"
                );
                assert!(rect.w > 0 && rect.h > 0, "{surface:?}: {name} is empty");
            }
            for (i, (a, ra)) in parts.iter().enumerate() {
                for (b, rb) in &parts[i + 1..] {
                    assert!(
                        !ra.overlaps(rb),
                        "{surface:?}: {a} {ra:?} overlaps {b} {rb:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn qr_modules_are_whole_pixels_and_big_enough_to_scan() {
        let join = qr_modules(JOIN).unwrap();
        for (surface, min_px) in [((1920, 1080), 3), ((3840, 2160), 6)] {
            let layout = overlay_layout(surface, 28.0);
            let m = module_px(layout.join_card.w, join.width);
            assert!(m >= min_px, "{surface:?}: {m} px per module");
            assert!(m * (join.width + 2 * QUIET_MODULES) <= layout.join_card.w);
        }
    }

    /// A test device and the guard that serializes GPU tests
    /// (`crate::gpu::test_device`); keep the guard bound first.
    fn try_device() -> Option<(
        std::sync::MutexGuard<'static, ()>,
        wgpu::Device,
        wgpu::Queue,
    )> {
        crate::gpu::test_device("wifi-overlay-test-device")
    }

    /// Rendered for real: every QR module centre is exactly dark or light as
    /// encoded (a phone can read it), and the panel's background matches the
    /// cleared background around it to the byte (no visible box edge).
    #[test]
    fn rendered_codes_match_their_modules_and_the_panel_is_seamless() {
        let Some((_gpu, device, queue)) = try_device() else {
            eprintln!("skipping wifi overlay render test: no GPU adapter available");
            return;
        };
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        // 1080p, the Pi's 4K panel, and portrait (the stacked layout).
        for (w, h) in [(1920u32, 1080u32), (3840, 2160), (1080, 1920)] {
            let mut screen =
                WifiOverlayScreen::new(&device, &queue, format, &ScreenMessageConfig::default());
            screen.set_content(&content());
            screen.resize(PhysicalSize::new(w, h), 1.0);
            let target = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("wifi-overlay-test-target"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = target.create_view(&wgpu::TextureViewDescriptor::default());
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("wifi-overlay-test"),
            });
            assert!(screen.render(&mut encoder, &view), "{w}x{h}");
            assert!(!screen.needs_redraw(), "{w}x{h}");
            queue.submit(Some(encoder.finish()));
            let pixels = read_back(&device, &queue, &target, w, h);
            let at = |x: u32, y: u32| {
                let i = ((y * w + x) * 4) as usize;
                [pixels[i], pixels[i + 1], pixels[i + 2]]
            };

            assert_eq!(screen.placements().len(), 2, "{w}x{h}: both codes drawn");
            for placement in screen.placements() {
                let m = placement.module_px;
                let n = placement.modules.width;
                let mut wrong = 0;
                for y in 0..n {
                    for x in 0..n {
                        let px = at(
                            placement.origin.0 + x * m + m / 2,
                            placement.origin.1 + y * m + m / 2,
                        );
                        let dark = px.iter().all(|c| *c < 40);
                        let light = px.iter().all(|c| *c > 215);
                        if placement.modules.is_dark(x, y) != dark || dark == light {
                            wrong += 1;
                        }
                    }
                }
                assert_eq!(wrong, 0, "{w}x{h}: {wrong} of {} modules misdrawn", n * n);
                // The quiet zone is white all round (a reader needs it).
                let q = QUIET_MODULES * m;
                for (x, y) in [
                    (placement.origin.0 - q / 2, placement.origin.1 + n * m / 2),
                    (
                        placement.origin.0 + n * m + q / 2,
                        placement.origin.1 + n * m / 2,
                    ),
                    (placement.origin.0 + n * m / 2, placement.origin.1 - q / 2),
                    (
                        placement.origin.0 + n * m / 2,
                        placement.origin.1 + n * m + q / 2,
                    ),
                ] {
                    assert!(
                        at(x, y).iter().all(|c| *c > 215),
                        "{w}x{h}: quiet zone at {x},{y}"
                    );
                }
            }

            // The panel's background is exactly the clear colour the frame
            // is drawn on, so there is no visible box edge.
            let layout = screen.layout().expect("laid out");
            let background = srgb_bytes(screen.background);
            let inside = at(layout.panel.x + 2, layout.panel.bottom() - 2);
            assert_eq!(
                inside,
                [background[0], background[1], background[2]],
                "{w}x{h}"
            );
        }
    }

    /// A frame that could not be drawn stays owed (`needs_redraw`), which is
    /// what the viewer re-arms on (review F3).
    #[test]
    fn an_undrawn_overlay_still_needs_its_frame() {
        let Some((_gpu, device, queue)) = try_device() else {
            eprintln!("skipping wifi overlay redraw test: no GPU adapter available");
            return;
        };
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut screen =
            WifiOverlayScreen::new(&device, &queue, format, &ScreenMessageConfig::default());
        screen.set_content(&content());
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wifi-overlay-undrawn"),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
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
            label: Some("wifi-overlay-undrawn"),
        });
        // No size yet: nothing can be drawn.
        assert!(!screen.render(&mut encoder, &view));
        assert!(screen.needs_redraw());
        // A lease refresh with the same content does not clear the debt.
        screen.set_content(&content());
        assert!(screen.needs_redraw());
    }

    /// Visual check, not a gate: `WIFI_OVERLAY_PNG_DIR=/some/dir cargo test
    /// -p photoframe --bin photoframe wifi_overlay_snapshots -- --ignored`
    /// writes the overlay at 1080p, 4K and portrait as PNGs.
    #[test]
    #[ignore]
    fn wifi_overlay_snapshots() {
        let Some(dir) = std::env::var_os("WIFI_OVERLAY_PNG_DIR") else {
            eprintln!("set WIFI_OVERLAY_PNG_DIR to write the snapshots");
            return;
        };
        let Some((_gpu, device, queue)) = try_device() else {
            eprintln!("no GPU adapter available");
            return;
        };
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        for (w, h) in [(1920u32, 1080u32), (3840, 2160), (1080, 1920)] {
            let mut screen =
                WifiOverlayScreen::new(&device, &queue, format, &ScreenMessageConfig::default());
            screen.set_content(&content());
            screen.resize(PhysicalSize::new(w, h), 1.0);
            let target = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("wifi-overlay-snapshot"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = target.create_view(&wgpu::TextureViewDescriptor::default());
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("wifi-overlay-snapshot"),
            });
            assert!(screen.render(&mut encoder, &view));
            queue.submit(Some(encoder.finish()));
            let pixels = read_back(&device, &queue, &target, w, h);
            let path = std::path::Path::new(&dir).join(format!("wifi-overlay-{w}x{h}.png"));
            image::RgbaImage::from_raw(w, h, pixels)
                .expect("buffer size")
                .save(&path)
                .expect("write png");
            eprintln!("wrote {}", path.display());
        }
    }

    fn read_back(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        let unpadded = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wifi-overlay-readback"),
            size: (padded * height) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("wifi-overlay-readback"),
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
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for row in 0..height {
            let start = (row * padded) as usize;
            out.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        out
    }
}
