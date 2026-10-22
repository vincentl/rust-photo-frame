#[cfg(test)]
mod canvas_publish_tests;
mod display_wait;
mod draw_params;
mod image_ops;
#[cfg(target_os = "linux")]
mod kms;
mod pacing;
#[cfg(test)]
mod pair_canvas_tests;
mod photo_set;
mod presentation;
pub mod scenes;
mod wake;

use self::image_ops::*;
use self::scenes::{GreetingScene, Scene, SceneContext, SleepScene, Viewport};

use crate::config::{
    MattingConfig, MattingMode, MattingOptions, SelectedTransition, TransitionConfig,
    TransitionKind, TransitionMode,
};
use crate::events::{
    Displayed, LoadedPartner, PhotoLoaded, PreparedImageCpu, ViewerCommand,
    ViewerState as ControlViewerState,
};
use crate::processing::blur::apply_blur;
use crate::processing::color::PhotoSwatches;
use crate::processing::layout::center_offset;
use crate::tasks::greeting_screen::GreetingScreen;
use crate::tasks::loader;
use crate::tasks::photo_channel::{PhotoReceiver, PhotoSender};
use crossbeam_channel::{Sender as CbSender, TrySendError, bounded};
use futures::executor::block_on;
use image::{Rgba, RgbaImage, imageops};
use rand::Rng;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::sync::CancellationToken;
use tracing::{Level, debug, info, warn};

/// Iris petal angular width beyond the petal spacing (75°). Validated
/// numerically to keep the aperture boundary on the petals' inner arcs (no
/// end-cap "teeth") for 5..=14 blades at any minimum aperture.
const IRIS_EXTRA_WIDTH_RAD: f32 = 1.308_997;

/// Linear downscale factor of the iris petal layer relative to the surface.
/// The layer holds the petals' signed distance and shade; the main pass
/// draws the outline from the interpolated distance at its own resolution,
/// so the outline stays crisp at any layer scale and the scale sets only
/// how finely the shading is sampled. The layer's cost is its pixels × the
/// blades (Pi, 2026-10-22: up to 12.4 ms at 4, 3.5 ms at 8), so 16 (240x135
/// at 4K) is what fits the frame. Against the 1/4 layer on the Mac GPU,
/// petal interiors are within 5 LSB (0.04% of pixels over 1, all in the
/// overlap-shadow ramps), and where one petal's edge lies over another the
/// step is sampled at 16 px; 8 keeps interiors within 2 LSB. Override with
/// PHOTOFRAME_IRIS_LAYER_SCALE (1..=16).
const IRIS_LAYER_SCALE: u32 = 16;

/// Linear downscale factor for the intermediate that transition frames
/// render into (resting photos always render at native resolution). 2 →
/// roughly a quarter of the per-pixel transition cost; motion hides the
/// brief softness, and the first dwell frame restores full sharpness.
/// Override with PHOTOFRAME_TRANSITION_SCALE for tuning; 1 bypasses the
/// intermediate entirely and renders transitions at native resolution.
const TRANSITION_HALF_SCALE: u32 = 2;

fn scale_from_env(var: &str, default: u32, max: u32) -> u32 {
    match std::env::var(var) {
        Ok(raw) => match raw.parse::<u32>() {
            Ok(value) if (1..=max).contains(&value) => {
                info!(var, value, "viewer_scale_override");
                value
            }
            _ => {
                warn!(var, raw, "invalid scale override; using default");
                default
            }
        },
        Err(_) => default,
    }
}

/// Effective transition-intermediate scale (env-overridable, read once).
fn transition_scale() -> u32 {
    static SCALE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *SCALE.get_or_init(|| scale_from_env("PHOTOFRAME_TRANSITION_SCALE", TRANSITION_HALF_SCALE, 4))
}

/// Effective iris petal-layer scale (env-overridable, read once).
fn iris_layer_scale() -> u32 {
    static SCALE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *SCALE.get_or_init(|| scale_from_env("PHOTOFRAME_IRIS_LAYER_SCALE", IRIS_LAYER_SCALE, 16))
}

/// How the reduced-resolution intermediate is upscaled to the surface
/// (`PHOTOFRAME_UPSCALE`), on both outputs (Wayland and KMS share the
/// upscale pipeline). Each 4K tap costs about 1.75 ms on the Pi 5 (V3D's
/// sample rate, not memory): the upscale takes 11.0 ms bilinear, 18.0 ms
/// with the 5-tap cubic and 25.5 ms with the 9-tap one (Pi, 2026-10-22,
/// scale 2), so only bilinear fits 4K60 there; the cubics are opt-ins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpscaleFilter {
    /// One bilinear tap: the softest; the eye tracks a slow front (a wide
    /// radial wipe) and sees it. The only one that fits 4K60 on the Pi 5.
    Bilinear,
    /// Catmull-Rom from 5 bilinear taps (the corner taps left out and the
    /// rest renormalized): nearly the exact cubic. 18.0 ms at 4K on the Pi.
    Bicubic,
    /// Catmull-Rom from 9 bilinear taps: the exact cubic. 25.5 ms at 4K on
    /// the Pi.
    Bicubic9,
}

impl UpscaleFilter {
    const DEFAULT: Self = Self::Bilinear;

    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "bilinear" => Some(Self::Bilinear),
            "bicubic" => Some(Self::Bicubic),
            "bicubic-9" => Some(Self::Bicubic9),
            _ => None,
        }
    }

    fn entry_point(self) -> &'static str {
        match self {
            Self::Bilinear => "fs_blit",
            Self::Bicubic => "fs_upscale_bicubic",
            Self::Bicubic9 => "fs_upscale_bicubic9",
        }
    }
}

/// The `PHOTOFRAME_UPSCALE` filter (`bilinear`, `bicubic`, `bicubic-9`),
/// read once.
fn upscale_filter() -> UpscaleFilter {
    static FILTER: std::sync::OnceLock<UpscaleFilter> = std::sync::OnceLock::new();
    *FILTER.get_or_init(|| {
        let filter = match std::env::var("PHOTOFRAME_UPSCALE") {
            Ok(raw) => UpscaleFilter::parse(&raw).unwrap_or_else(|| {
                warn!(value = %raw, "unknown PHOTOFRAME_UPSCALE; using the default");
                UpscaleFilter::DEFAULT
            }),
            Err(std::env::VarError::NotUnicode(raw)) => {
                warn!(value = ?raw, "PHOTOFRAME_UPSCALE is not UTF-8; using the default");
                UpscaleFilter::DEFAULT
            }
            Err(std::env::VarError::NotPresent) => UpscaleFilter::DEFAULT,
        };
        if transition_scale() > 1 {
            info!(filter = ?filter, "viewer_upscale_filter");
        } else {
            info!("viewer_upscale_filter: scale=1 (no upscale)");
        }
        filter
    })
}

/// How strongly the canvas level transitions read is unsharp-masked at
/// upload (`fs_mip_sharpen`'s `MIP_SHARPEN`): 1 by default (the lowest
/// error of the 2x bilinear round trip, docs/performance.md), 0 a plain box
/// filter. The range-clamped mask keeps edges halo-free at any strength.
const MIP_SHARPEN_DEFAULT: f32 = 1.0;
const MIP_SHARPEN_MAX: f32 = 2.0;

/// A `PHOTOFRAME_MIP_SHARPEN` value: a finite number, clamped to
/// 0..=`MIP_SHARPEN_MAX` (the second value says whether it was clamped);
/// `None` for anything else.
fn parse_mip_sharpen(raw: &str) -> Option<(f32, bool)> {
    let value: f32 = raw.trim().parse().ok().filter(|v: &f32| v.is_finite())?;
    let clamped = value.clamp(0.0, MIP_SHARPEN_MAX);
    Some((clamped, clamped != value))
}

/// `PHOTOFRAME_MIP_SHARPEN`, read once. It applies to photos uploaded after
/// the process starts (mips are made at upload).
fn mip_sharpen() -> f32 {
    static SHARPEN: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *SHARPEN.get_or_init(|| {
        let sharpen = match std::env::var("PHOTOFRAME_MIP_SHARPEN") {
            Ok(raw) => match parse_mip_sharpen(&raw) {
                Some((value, clamped)) => {
                    if clamped {
                        warn!(raw, value, "PHOTOFRAME_MIP_SHARPEN out of 0..=2; clamped");
                    }
                    value
                }
                None => {
                    warn!(raw, "invalid PHOTOFRAME_MIP_SHARPEN; using the default");
                    MIP_SHARPEN_DEFAULT
                }
            },
            Err(std::env::VarError::NotUnicode(raw)) => {
                warn!(value = ?raw, "PHOTOFRAME_MIP_SHARPEN is not UTF-8; using the default");
                MIP_SHARPEN_DEFAULT
            }
            Err(std::env::VarError::NotPresent) => MIP_SHARPEN_DEFAULT,
        };
        info!(sharpen, "viewer_mip_sharpen");
        sharpen
    })
}

/// DRM/KMS: how long before a transition the viewer starts rendering the
/// resting photo on every refresh (`wake::plan_wake`, "lead-in"). The V3D
/// clock idles at its lowest step (500 MHz) through a dwell and reaches
/// 960 MHz only 150-200 ms after the GPU gets work (Pi measurement,
/// 2026-10-21; the kernel exposes no devfreq knob for v3d), so without it
/// the first two transition frames took 30-46 ms. 300 ms (18 refreshes) is
/// that ramp with margin. The frames are the same picture as the dwell
/// frame, so nothing visible changes; the cost is 18 resting frames per
/// photo change.
const TRANSITION_LEAD_IN: Duration = Duration::from_millis(300);

pub(super) enum ActiveTransition {
    Fade {
        through_black: bool,
    },
    Wipe {
        normal: [f32; 2],
        softness: f32,
    },
    Push {
        direction: [f32; 2],
    },
    EInk {
        flash_count: u32,
        reveal_portion: f32,
        stripe_count: u32,
        flash_color: [f32; 3],
        noise_seed: [f32; 2],
    },
    Dissolve {
        softness: f32,
        scale: f32,
    },
    RadialWipe {
        softness: f32,
        shape_is_diamond: bool,
        center: [f32; 2],
    },
    VenetianBlinds {
        stripe_count: u32,
        softness: f32,
        vertical: bool,
    },
    CrossfadeZoom {
        zoom: f32,
        current_zooms_in: bool,
        next_zooms_in: bool,
    },
    Iris {
        blades: u32,
        color: [f32; 3],
        petal_sheen: f32,
        overlap_shadow: f32,
        min_aperture: f32,
        swirl: f32,
    },
}

pub(super) struct TexturePlane {
    pub(super) bind: wgpu::BindGroup,
    pub(super) w: u32,
    pub(super) h: u32,
    /// Photo canvases only: their GPU bytes, counted while the plane lives
    /// (held for its `Drop`).
    pub(super) _gpu_bytes: Option<CanvasBytes>,
}

/// The GPU memory held by photo canvases: every level of every canvas
/// alive, counted at upload and released when its plane is dropped. Logged
/// at each upload (`canvas_upload`, debug) so the viewer's RSS on the Pi
/// can be checked against what the canvases account for.
pub(super) struct CanvasBytes(u64);

static LIVE_CANVASES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LIVE_CANVAS_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl CanvasBytes {
    fn track(bytes: u64) -> Self {
        LIVE_CANVASES.fetch_add(1, Ordering::Relaxed);
        LIVE_CANVAS_BYTES.fetch_add(bytes, Ordering::Relaxed);
        Self(bytes)
    }
}

impl Drop for CanvasBytes {
    fn drop(&mut self) {
        LIVE_CANVASES.fetch_sub(1, Ordering::Relaxed);
        LIVE_CANVAS_BYTES.fetch_sub(self.0, Ordering::Relaxed);
    }
}

/// Bytes of an RGBA8 texture with `levels` mip levels (each level halves,
/// rounding down, at least 1).
fn rgba8_bytes(width: u32, height: u32, levels: u32) -> u64 {
    (0..levels)
        .map(|l| u64::from((width >> l).max(1)) * u64::from((height >> l).max(1)) * 4)
        .sum()
}

pub(super) struct ImgTex {
    pub(super) plane: TexturePlane,
    pub(super) path: std::path::PathBuf,
    /// Portrait pairing: the pair screen's second photo (drawn on the right).
    pub(super) partner: Option<std::path::PathBuf>,
    /// Mat kind that was baked for this image; `None` means full-bleed (no mat).
    pub(super) mat_kind: Option<crate::config::MatCaption>,
}

pub(super) struct TransitionState {
    started_at: std::time::Instant,
    /// Set once `started_at` has been moved to the first frame's clock
    /// (DRM/KMS; see `anchor_once`).
    anchored: bool,
    /// DRM/KMS: progress of the last frame rendered (`note_rendered`).
    rendered_progress: f32,
    duration: std::time::Duration,
    kind: TransitionKind,
    variant: ActiveTransition,
}

impl TransitionState {
    pub(super) fn new(
        selected: SelectedTransition<'_>,
        started_at: std::time::Instant,
        rng: &mut impl Rng,
    ) -> Self {
        let option = selected.option;
        let duration = option.duration();
        let kind = option.kind();
        let mode = option.mode().clone();
        let variant = match mode {
            TransitionMode::Fade(cfg) => ActiveTransition::Fade {
                through_black: cfg.through_black,
            },
            TransitionMode::Wipe(cfg) => {
                let angle = jittered_angle(&cfg.angles, rng);
                let normal = angle_to_unit_vec(angle);
                ActiveTransition::Wipe {
                    normal,
                    softness: cfg.softness,
                }
            }
            TransitionMode::Push(cfg) => {
                let angle = jittered_angle(&cfg.angles, rng);
                let direction = angle_to_unit_vec(angle);
                ActiveTransition::Push { direction }
            }
            TransitionMode::EInk(cfg) => ActiveTransition::EInk {
                flash_count: cfg.flash_count,
                reveal_portion: cfg.reveal_portion.clamp(0.05, 0.95),
                stripe_count: cfg.stripe_count.max(1),
                flash_color: cfg
                    .flash_color
                    .map(|channel| srgb_to_linear((channel as f32 / 255.0).clamp(0.0, 1.0))),
                noise_seed: [rng.random_range(0.0..=1.0), rng.random_range(0.0..=1.0)],
            },
            TransitionMode::Dissolve(cfg) => ActiveTransition::Dissolve {
                softness: cfg.softness,
                scale: cfg.scale,
            },
            TransitionMode::RadialWipe(cfg) => ActiveTransition::RadialWipe {
                softness: cfg.softness,
                shape_is_diamond: cfg.shape == crate::config::RadialShape::Diamond,
                center: cfg.center,
            },
            TransitionMode::VenetianBlinds(cfg) => ActiveTransition::VenetianBlinds {
                stripe_count: cfg.stripe_count.max(1),
                softness: cfg.softness,
                vertical: cfg.vertical,
            },
            TransitionMode::CrossfadeZoom(cfg) => ActiveTransition::CrossfadeZoom {
                zoom: cfg.zoom,
                current_zooms_in: cfg.current_zooms_in,
                next_zooms_in: cfg.next_zooms_in,
            },
            TransitionMode::Iris(cfg) => ActiveTransition::Iris {
                blades: cfg.blades,
                // Config colors are sRGB; the shader works in linear light
                // (its output is re-encoded by the sRGB render target).
                // Without this conversion the petals display ~2.5x lighter
                // than the configured color.
                color: cfg
                    .color
                    .map(|channel| srgb_to_linear((channel as f32 / 255.0).clamp(0.0, 1.0))),
                petal_sheen: cfg.petal_sheen,
                overlap_shadow: cfg.overlap_shadow,
                min_aperture: cfg.min_aperture,
                swirl: cfg.swirl,
            },
        };

        Self {
            started_at,
            anchored: false,
            rendered_progress: 0.0,
            duration,
            kind,
            variant,
        }
    }

    /// Wayland: the wall clock has run the full duration. DRM/KMS (an
    /// anchored transition): a frame drawn for progress 1 has been rendered.
    /// Frames are rendered ahead of the wall clock there, so the transition
    /// ends when its last frame is queued, not a few refreshes later with
    /// repeats of it.
    pub(super) fn is_complete(&self) -> bool {
        if self.anchored {
            self.rendered_progress >= 1.0
        } else {
            self.progress() >= 1.0
        }
    }

    /// Whether the DRM/KMS frame clock has been anchored (a frame of this
    /// transition was rendered).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))] // read by the KMS loop
    pub(super) fn is_anchored(&self) -> bool {
        self.anchored
    }

    /// DRM/KMS: the progress the frame just rendered was drawn for.
    pub(super) fn note_rendered(&mut self, progress: f32) {
        self.rendered_progress = progress;
    }

    pub(super) fn progress(&self) -> f32 {
        self.progress_at(Instant::now())
    }

    /// Progress at clock time `now`: wall time on Wayland, the vblank a
    /// frame is paced on for DRM/KMS frames (see `viewer/kms.rs`).
    pub(super) fn progress_at(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.started_at).as_secs_f32();
        let duration = self.duration.as_secs_f32().max(f32::EPSILON);
        (elapsed / duration).clamp(0.0, 1.0)
    }

    /// DRM/KMS: start the transition clock at the vblank its first frame is
    /// meant for (`pacing::FrameClock`), instead of when the transition was
    /// created (the end of a dwell, at an arbitrary phase of the refresh).
    /// The first frame therefore shows progress 0, and frame k is drawn for
    /// k refreshes later.
    pub(super) fn anchor_once(&mut self, frame_time: Instant) {
        if !self.anchored {
            self.started_at = frame_time;
            self.anchored = true;
        }
    }

    pub(super) fn kind(&self) -> TransitionKind {
        self.kind
    }

    pub(super) fn variant(&self) -> &ActiveTransition {
        &self.variant
    }
}

/// Frame cadence accumulator for one transition playback, logged when the
/// transition ends so on-device performance is visible in the journal. Only
/// emitted when `metrics` is enabled in the config.
pub(super) struct TransitionFrameMetric {
    kind: TransitionKind,
    started: Instant,
    last_frame: Instant,
    frames: u32,
    worst_frame_ms: f32,
    best_frame_ms: f32,
    /// DRM/KMS only: what the panel showed, from the flip audit. `None` on
    /// Wayland, or when the audit was incomplete (no vblank information for
    /// part of the transition).
    display: Option<DisplayCadence>,
    /// Presented on DRM/KMS, where the loop's own intervals only say how
    /// far ahead frames were rendered: without the audit the line carries
    /// `audited=false` and the frame count, not those intervals.
    kms: bool,
}

/// DRM/KMS: a transition as the panel showed it (`pacing::FlipAudit`).
/// Replaces the loop-side intervals in `transition_frame_metric`, which on
/// KMS only say how far ahead frames were rendered.
#[derive(Clone, Copy, Debug)]
pub(super) struct DisplayCadence {
    frames: u32,
    /// From the first frame's flip to the last's.
    span: Duration,
    best: Duration,
    worst: Duration,
    /// Refreshes on which no new frame appeared.
    late_frames: u32,
}

impl DisplayCadence {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))] // built by the KMS loop
    fn from_audit(summary: pacing::AuditSummary, period: Duration) -> Option<Self> {
        let refreshes = |n: u64| period * u32::try_from(n).unwrap_or(u32::MAX);
        Some(Self {
            frames: summary.frames,
            span: refreshes(summary.span),
            best: refreshes(summary.best?),
            worst: refreshes(summary.worst?),
            late_frames: summary.repeats,
        })
    }
}

impl TransitionFrameMetric {
    fn begin(kind: TransitionKind, now: Instant, kms: bool) -> Self {
        Self {
            kind,
            started: now,
            last_frame: now,
            frames: 1,
            worst_frame_ms: 0.0,
            best_frame_ms: f32::INFINITY,
            display: None,
            kms,
        }
    }

    /// One more presented frame of the same transition, at clock `now`.
    fn record(&mut self, now: Instant) {
        let dt_ms = now.saturating_duration_since(self.last_frame).as_secs_f32() * 1000.0;
        self.worst_frame_ms = self.worst_frame_ms.max(dt_ms);
        self.best_frame_ms = self.best_frame_ms.min(dt_ms);
        self.frames += 1;
        self.last_frame = now;
    }

    fn log(&self, enabled: bool) {
        if !enabled || self.frames < 2 {
            return;
        }
        let span = self
            .last_frame
            .duration_since(self.started)
            .as_secs_f32()
            .max(f32::EPSILON);
        let intervals = (self.frames - 1) as f32;
        if let Some(shown) = self.display
            && shown.frames >= 2
        {
            let intervals = (shown.frames - 1) as f32;
            let span = shown.span.as_secs_f32().max(f32::EPSILON);
            info!(
                transition = %self.kind,
                frames = shown.frames,
                avg_fps = format_args!("{:.1}", intervals / span),
                avg_frame_ms = format_args!("{:.1}", 1000.0 * span / intervals),
                best_frame_ms = format_args!("{:.1}", shown.best.as_secs_f32() * 1000.0),
                worst_frame_ms = format_args!("{:.1}", shown.worst.as_secs_f32() * 1000.0),
                late_frames = shown.late_frames,
                audited = true,
                "transition_frame_metric"
            );
            return;
        }
        if self.kms {
            // Render-ahead spacing (a millisecond or two), not the panel's
            // cadence: name what is missing instead of logging it.
            info!(
                transition = %self.kind,
                frames = self.frames,
                audited = false,
                "transition_frame_metric"
            );
            return;
        }
        info!(
            transition = %self.kind,
            frames = self.frames,
            avg_fps = format_args!("{:.1}", intervals / span),
            avg_frame_ms = format_args!("{:.1}", 1000.0 * span / intervals),
            best_frame_ms = format_args!("{:.1}", self.best_frame_ms),
            worst_frame_ms = format_args!("{:.1}", self.worst_frame_ms),
            "transition_frame_metric"
        );
    }
}

/// DRM/KMS: when a transition frame is meant to be shown (`kms.rs`,
/// "Pacing law"; `pacing::FrameClock`).
#[derive(Clone, Copy, Debug)]
pub(super) struct PacedFrame {
    /// The time of the vblank the transition's first frame is meant for.
    pub(super) anchor: Instant,
    /// The time of the vblank this frame is meant for.
    pub(super) index_time: Instant,
    /// Time the frame's passes on the GPU (`PassTimer`): set while the
    /// per-frame audit is logged.
    pub(super) time_passes: bool,
}

/// GPU time of one transition frame's passes, from timestamp queries:
/// `main` around the main draw pass; `layer` from the start of the iris
/// petal layer's first pass to the end of its last (`None` without one);
/// `draw` from the start of the first pass that draws the frame to the end
/// of the main pass (`main`, or layer through main); `upscale` around the
/// pass that upsamples the intermediate (`None` for a frame drawn
/// natively).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct PassTimes {
    pub(super) draw: Duration,
    pub(super) main: Duration,
    pub(super) layer: Option<Duration>,
    pub(super) upscale: Option<Duration>,
}

/// Filled once the frame's timestamps are read back (a device poll after
/// the frame completes); empty if they never are.
pub(super) type PassTimesCell = Arc<std::sync::OnceLock<PassTimes>>;

/// DRM/KMS: what the loop learns about a transition frame it submitted.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // read by the KMS loop
pub(super) struct PacedSubmission {
    /// Set by the queue's submitted-work-done callback.
    pub(super) done: Arc<AtomicBool>,
    pub(super) passes: Option<PassTimesCell>,
}

/// Convert one sRGB channel (0..1) to linear light, the space the transition
/// shader computes in. Constant colors fed to the shader must be linear or
/// the sRGB render target's encode pass displays them too bright.
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn jittered_angle(picker: &crate::config::AnglePicker, rng: &mut impl Rng) -> f32 {
    let base = picker.base_deg;
    let jitter = picker.jitter_deg;
    if jitter.abs() > f32::EPSILON {
        base + rng.random_range(-jitter..=jitter)
    } else {
        base
    }
}

#[derive(Clone)]
struct MatParams {
    screen_w: u32,
    screen_h: u32,
    oversample: f32,
    max_dim: u32,
    max_upscale_factor: f32,
    matting: MattingOptions,
    /// When true, render the photo full-bleed (cover-crop, no mat); `matting`
    /// is ignored. Chosen by the `fill-when-fits` pre-check at selection time.
    fill_screen: bool,
    /// `pairing.bar-ratio`: a pair's bar as a weight against each side margin.
    bar_ratio: f32,
}

struct MatTask {
    image: PreparedImageCpu,
    params: MatParams,
    priority: bool,
    /// Portrait pairing: the second photo of a pair screen.
    partner: Option<LoadedPartner>,
    mat_kind: Option<crate::config::MatCaption>,
}

struct ImagePlane {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

struct MatResult {
    path: std::path::PathBuf,
    canvas: ImagePlane,
    priority: bool,
    /// Portrait pairing: the pair screen's second member.
    partner: Option<photo_set::Member>,
    /// Where each member was drawn (one window for a solo photo; none for
    /// full-bleed).
    windows: Vec<photo_set::Window>,
    mat_kind: Option<crate::config::MatCaption>,
    /// Decoded photos the task carried (1, or 2 for a pair), whatever the
    /// canvas ended up showing: what `mat_inflight` gives back.
    photos: usize,
}

// Manual so logs and test failures never dump the canvas pixels.
impl std::fmt::Debug for MatResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatResult")
            .field("path", &self.path)
            .field("width", &self.canvas.width)
            .field("height", &self.canvas.height)
            .field("priority", &self.priority)
            .field("partner", &self.partner)
            .finish_non_exhaustive()
    }
}

/// Photos of the largest screen the loader can send: a portrait pair with
/// pairing on, else one photo.
fn largest_screen(pairing: bool) -> usize {
    if pairing { 2 } else { 1 }
}

struct QueuedImage {
    image: PreparedImageCpu,
    priority: bool,
    /// Portrait pairing: the second photo of a pair screen.
    partner: Option<LoadedPartner>,
}

struct MattingPipeline {
    task_tx: CbSender<MatTask>,
    /// A tokio channel (not crossbeam) so the DRM/KMS loop can await
    /// results; the winit path keeps polling it with `try_recv`.
    result_rx: Receiver<MatResult>,
}

impl MattingPipeline {
    fn new(worker_count: usize, capacity: usize) -> Self {
        let worker_count = worker_count.max(1);
        let capacity = capacity.max(worker_count).max(2);
        let (task_tx, task_rx) = bounded::<MatTask>(capacity);
        let (result_tx, result_rx) = tokio::sync::mpsc::channel::<MatResult>(capacity);
        let task_rx = Arc::new(task_rx);
        for _ in 0..worker_count {
            let task_rx = Arc::clone(&task_rx);
            let result_tx = result_tx.clone();
            // Plain OS threads (outside the tokio runtime), so blocking_send
            // is the right call.
            std::thread::spawn(move || {
                while let Ok(task) = task_rx.recv() {
                    // Every task is answered, so the viewer gives back its
                    // photos (`mat_inflight`) even when no canvas came of
                    // it; otherwise a lost task would hold its place in the
                    // preload for good.
                    let (path, photos) = (task.image.path.clone(), task.photos());
                    let result = process_mat_task(task).unwrap_or_else(|| {
                        warn!(path = %path.display(), photos, "matting_produced_no_canvas");
                        MatResult::acknowledgement(path, photos)
                    });
                    if result_tx.blocking_send(result).is_err() {
                        break;
                    }
                }
            });
        }
        Self { task_tx, result_rx }
    }

    // MatTask is large by design (contains image buffers); returning it on error
    // lets the caller reclaim the task without re-allocation.
    #[allow(clippy::result_large_err)]
    fn try_submit(&self, task: MatTask) -> Result<(), MatTask> {
        match self.task_tx.try_send(task) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(task)) | Err(TrySendError::Disconnected(task)) => Err(task),
        }
    }

    fn try_recv(&mut self) -> Option<MatResult> {
        self.result_rx.try_recv().ok()
    }
}

#[derive(Clone, Copy)]
struct SurfaceState {
    width: u32,
    height: u32,
    max_texture_dimension: u32,
}

impl SurfaceState {
    fn from_config(config: &wgpu::SurfaceConfiguration, limits: &wgpu::Limits) -> Self {
        Self {
            width: config.width,
            height: config.height,
            max_texture_dimension: limits.max_texture_dimension_2d,
        }
    }

    #[cfg(test)]
    fn new(width: u32, height: u32, max_texture_dimension: u32) -> Self {
        Self {
            width,
            height,
            max_texture_dimension,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum SurfaceReport {
    InitialConfig,
    ConfigureEvent,
}

#[derive(Clone, Copy, Default)]
struct SurfaceReadyGate {
    ready: bool,
    awaiting: bool,
    last_size: Option<(u32, u32)>,
    same_count: u8,
    stable_since: Option<Instant>,
}

// Enforces minimum stability before declaring the surface ready.
const SURFACE_READY_DWELL: Duration = Duration::from_millis(120);

impl SurfaceReadyGate {
    fn reset(&mut self) {
        self.ready = false;
        self.awaiting = false;
        self.last_size = None;
        self.same_count = 0;
        self.stable_since = None;
    }

    fn arm_for_initial_report(&mut self) {
        self.awaiting = true;
        self.ready = false;
        self.last_size = None;
        self.same_count = 0;
        self.stable_since = None;
    }

    fn update(&mut self, width: u32, height: u32, report: SurfaceReport) -> Option<bool> {
        // Ignore bootstrap config; wait for compositor configure events
        if matches!(report, SurfaceReport::InitialConfig) {
            return self.set_ready(false);
        }

        if width == 0 || height == 0 {
            self.last_size = Some((0, 0));
            self.same_count = 0;
            self.stable_since = None;
            return self.set_ready(false);
        }

        match self.last_size {
            Some((w, h)) if w == width && h == height => {
                self.same_count = self.same_count.saturating_add(1);
            }
            _ => {
                self.last_size = Some((width, height));
                self.same_count = 1;
                self.stable_since = Some(Instant::now());
            }
        }

        // Require two consecutive identical, non-zero reports and a small dwell time
        let dwell_met = self
            .stable_since
            .map(|t| Instant::now().saturating_duration_since(t) >= SURFACE_READY_DWELL)
            .unwrap_or(false);
        let now_ready = self.awaiting && self.same_count >= 2 && dwell_met;
        if now_ready {
            self.awaiting = false;
        }
        self.set_ready(now_ready || self.ready)
    }

    fn is_ready(&self) -> bool {
        self.ready
    }

    fn set_ready(&mut self, ready: bool) -> Option<bool> {
        if self.ready != ready {
            self.ready = ready;
            Some(ready)
        } else {
            None
        }
    }
}

struct MattingBridge<'a> {
    preload_count: usize,
    mat_inflight: &'a mut usize,
    deferred_images: &'a mut VecDeque<QueuedImage>,
    ready_results: &'a mut VecDeque<MatResult>,
    from_loader: &'a mut PhotoReceiver,
    surface: Option<SurfaceState>,
    matting: &'a MattingConfig,
    oversample: f32,
    max_upscale_factor: f32,
    /// `pairing.bar-ratio`.
    bar_ratio: f32,
    /// Portrait pairing's pairable rule (`None` with pairing off): a pair is
    /// drawn only if both members are pairable on the canvas it is drawn on.
    pairing: Option<crate::tasks::manager::pairing::PairingRules>,
    mat_pipeline: &'a MattingPipeline,
}

fn surface_state_for_queue(
    surface_configured: bool,
    surface: Option<SurfaceState>,
) -> Option<SurfaceState> {
    if surface_configured { surface } else { None }
}

impl<'a> MattingBridge<'a> {
    fn queue_for_wake(&mut self, wake: &mut scenes::WakeScene) {
        // Don't kick off new decode/matting work while a transition is
        // animating. The work itself runs on worker threads, but its JPEG
        // decode and NEON blur bursts compete for the same unified memory
        // bandwidth the GPU is using, which shows up as dropped frames.
        // A transition consumes the staged photo and immediately drops the
        // preload depth below target, so without this gate the refill always
        // lands exactly during the animation. Dwell time (seconds) dwarfs
        // prep time; deferring the kickoff costs nothing.
        if wake.transition_state().is_some() {
            return;
        }
        // The preload counts decoded photos, not screens (`wake::screen_fits`):
        // a screen goes in only if it fits, and the loader is read only when
        // the largest screen it can send would.
        loop {
            let used = wake.pending().len() + *self.mat_inflight;
            let largest = largest_screen(self.pairing.is_some());
            let next_img = if let Some(img) = self.deferred_images.pop_front() {
                if !wake::screen_fits(used, img.photos(), self.preload_count) {
                    self.deferred_images.push_front(img);
                    break;
                }
                Some(img)
            } else if !wake::screen_fits(used, largest, self.preload_count) {
                None
            } else {
                match self.from_loader.try_recv() {
                    Ok(PhotoLoaded {
                        prepared,
                        priority,
                        partner,
                    }) => Some(QueuedImage {
                        image: prepared,
                        priority,
                        partner,
                    }),
                    Err(_) => None,
                }
            };
            let Some(mut queued) = next_img else {
                break;
            };
            let path = queued.image.path.clone();
            // Priority is per member (B2), and its purge is by membership (R14).
            for member in queued.priority_paths() {
                photo_set::purge_member(&member, self.deferred_images, self.ready_results, wake);
            }
            let Some(surface) = self.surface else {
                self.deferred_images.push_front(queued);
                break;
            };
            if surface.width <= 1 || surface.height <= 1 {
                debug!(
                    screen_w = surface.width,
                    screen_h = surface.height,
                    path = %path.display(),
                    "deferring_matting_until_surface_configured"
                );
                self.deferred_images.push_front(queued);
                break;
            }
            // C2 holds only on the canvas the pair was planned for: a canvas
            // change since (a mode change, a rotation) can leave the members
            // unpairable here. Such a pair becomes two solo screens, in order.
            let canvas = compute_canvas_size(
                surface.width.max(1),
                surface.height.max(1),
                self.oversample,
                surface.max_texture_dimension,
            );
            if queued.partner.is_some() && !queued.pair_fits(self.pairing.as_ref(), canvas) {
                let (first, second) = queued.split();
                info!(
                    path = %first.image.path.display(),
                    partner = %second.image.path.display(),
                    canvas = ?canvas,
                    "pair_split"
                );
                self.deferred_images.push_front(second);
                queued = first;
            }
            let mut rng = rand::rng();
            let pair = queued.partner.is_some();
            // fill-when-fits is skipped for pairs.
            let fill_screen = !pair
                && self
                    .matting
                    .fill_when_fits()
                    .map(|fill| {
                        fill.should_fill(
                            queued.image.source_size.0,
                            queued.image.source_size.1,
                            surface.width.max(1),
                            surface.height.max(1),
                            self.max_upscale_factor,
                            &mut rng,
                        )
                    })
                    .unwrap_or(false);
            // When filling the screen, the selected mat is ignored. Use a
            // non-mutating placeholder so the sequential selector is not
            // advanced for a photo that renders without a mat.
            let matting = if fill_screen {
                self.matting
                    .primary_option()
                    .cloned()
                    .expect("validated matting configuration has at least one option")
            } else if pair {
                // Mats for pair screens: the non-zero options only.
                self.matting
                    .select_active_nonzero(&mut rng)
                    .expect("validation requires a non-zero mat when pairing is enabled")
                    .option
                    .clone()
            } else {
                self.matting.select_active(&mut rng).option.clone()
            };
            let params = MatParams {
                screen_w: surface.width.max(1),
                screen_h: surface.height.max(1),
                oversample: self.oversample,
                max_dim: surface.max_texture_dimension,
                max_upscale_factor: self.max_upscale_factor,
                matting,
                fill_screen,
                bar_ratio: self.bar_ratio,
            };
            let mat_kind = if fill_screen {
                None
            } else {
                Some(params.matting.caption())
            };
            let QueuedImage {
                image: img,
                priority,
                partner,
            } = queued;
            let task = MatTask {
                image: img,
                params,
                priority,
                partner,
                mat_kind,
            };
            let photos = task.photos();
            match self.mat_pipeline.try_submit(task) {
                Ok(()) => {
                    *self.mat_inflight += photos;
                }
                Err(MatTask {
                    image,
                    priority,
                    partner,
                    ..
                }) => {
                    self.deferred_images.push_front(QueuedImage {
                        image,
                        priority,
                        partner,
                    });
                    break;
                }
            }
        }
    }
}

//
// Viewer state machine overview
// -----------------------------
//  - `ViewerModeKind::Greeting` shows the greeting overlay once after resume and
//    stays idle until the first photo is ready.
//  - `ViewerModeKind::Wake` drives the continuous photo slideshow, requesting
//    redraws while transitions or new frames are pending.
//  - `ViewerModeKind::Sleep` renders a single "Going to Sleep" banner and then
//    idles until the power state changes again.
//
// Redraw policy
//  - Greeting/Sleep request exactly one redraw on entry and after resizes.
//  - Wake continuously requests redraws via `about_to_wait` while work is
//    pending. Each state transition recreates the greeting resources to keep
//    scale/format in sync.

/// Wayland: the period of the control tick (`process_tick`) while it has to
/// run on a cadence. The rule, decided by `wake::plan_wake` (`tick_fast`):
/// tick every `CONTROL_TICK_INTERVAL` only while a transition is animating
/// (it finalizes on a tick) or the surface-ready handshake is still polling
/// the window's size. At any other time the viewer ticks once per event:
/// a matting result, a decoded photo, a control command, a window event, the
/// dwell deadline, the bounded size poll (`SURFACE_POLL_INTERVAL`), and once
/// more after a transition finalizes so the uploads and matting refill it held
/// back run at once. A resting photo wakes the process about once a second,
/// not 250 times.
const CONTROL_TICK_INTERVAL: Duration = Duration::from_millis(4);

/// Wayland: with the surface ready and the viewer awake, the window's size is
/// re-read at least this often. Sway can change the output mode or scale
/// without winit delivering `Resized`; the 4 ms tick used to catch that, and
/// a bounded poll (one wakeup a second at most) replaces the unbounded one.
const SURFACE_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// One member's pixels for matting. The loader may have decoded a JPEG at a
/// reduced DCT scale sized for the canvas as it was then. If the canvas has
/// since grown (rotation, a new output) so that those pixels no longer cover
/// what this task draws, decode in full here, off the render thread, rather
/// than upscale a soft buffer.
fn decoded_member(
    image: PreparedImageCpu,
    canvas: (u32, u32),
    fill_screen: bool,
    bg_scale: Option<f32>,
) -> Option<RgbaImage> {
    let PreparedImageCpu {
        path,
        width,
        height,
        source_size,
        pixels,
    } = image;
    if width == 0 || height == 0 {
        return None;
    }
    let mut src = RgbaImage::from_raw(width, height, pixels)?;
    let (canvas_w, canvas_h) = canvas;
    if !loader::decoded_suffices(
        (width, height),
        source_size,
        (canvas_w, canvas_h),
        fill_screen,
        bg_scale,
    ) {
        match loader::decode_rgba8_apply_exif(&path, None) {
            Ok(full) => {
                debug!(
                    path = %path.display(),
                    reduced_w = width,
                    reduced_h = height,
                    full_w = full.image.width(),
                    full_h = full.image.height(),
                    canvas_w,
                    canvas_h,
                    "reduced_decode_insufficient_redecoding_full"
                );
                src = full.image;
            }
            Err(err) => warn!(
                path = %path.display(),
                "full re-decode of reduced photo failed, using reduced pixels: {err}"
            ),
        }
    }
    Some(src)
}

/// The windows of an overlay mat (every kind but studio and passe-partout):
/// a solo photo fitted with the mat's margin and centred, as before pairing;
/// a pair laid out in that photo space (`photo_set`).
fn overlay_windows(
    set: &photo_set::PhotoSet,
    canvas_w: u32,
    canvas_h: u32,
    margin: f32,
    max_upscale: f32,
) -> Vec<photo_set::Window> {
    if set.is_pair() {
        let margin = margin.clamp(0.0, 0.45);
        return set.pair_windows(photo_set::PhotoSpace {
            canvas_w,
            canvas_h,
            avail_h: (canvas_h.max(1) as f32 * (1.0 - 2.0 * margin)).max(1.0),
            bevel_px: 0.0,
            max_upscale,
        });
    }
    let (width, height) = set.solo_size();
    let (final_w, final_h) =
        resize_to_fit_with_margin(canvas_w, canvas_h, width, height, margin, max_upscale);
    let (offset_x, offset_y) = center_offset(final_w, final_h, canvas_w, canvas_h);
    vec![photo_set::Window {
        x: offset_x,
        y: offset_y,
        w: final_w,
        h: final_h,
    }]
}

fn process_mat_task(task: MatTask) -> Option<MatResult> {
    let MatTask {
        image,
        params,
        priority,
        partner,
        mat_kind,
    } = task;
    let photos = 1 + usize::from(partner.is_some());
    let path = image.path.clone();
    let MatParams {
        screen_w,
        screen_h,
        oversample,
        max_dim,
        max_upscale_factor,
        matting,
        fill_screen,
        bar_ratio,
    } = params;
    if image.width == 0 || image.height == 0 || screen_w == 0 || screen_h == 0 {
        return None;
    }

    let (canvas_w, canvas_h) = compute_canvas_size(screen_w, screen_h, oversample, max_dim);
    let bg_scale = if fill_screen {
        None
    } else {
        loader::bg_sample_scale(&matting.style)
    };
    // D3's sufficiency check runs per member: a pair member's drawn rect is
    // never larger than its solo rect.
    let source_size = image.source_size;
    let first = decoded_member(image, (canvas_w, canvas_h), fill_screen, bg_scale);
    let second = partner.and_then(|LoadedPartner { prepared, priority }| {
        let member = photo_set::Member {
            path: prepared.path.clone(),
            priority,
        };
        let size = prepared.source_size;
        decoded_member(prepared, (canvas_w, canvas_h), fill_screen, bg_scale)
            .map(|img| (img, size, member))
    });
    let margin = matting.margin_fraction();
    let spacing = photo_set::PairSpacing::new(bar_ratio, margin, (canvas_w, canvas_h));
    // A photo is a set of windows (`photo_set`): n = 1 is the solo path, n = 2
    // a pair (first appearance on the left). A member whose pixels cannot be
    // read leaves the other solo rather than dropping the screen.
    let (set, path, priority, partner) = match (first, second) {
        (Some(src), None) => (photo_set::PhotoSet::solo(src), path, priority, None),
        (Some(src), Some((right, right_size, member))) => (
            photo_set::PhotoSet::pair((src, source_size), (right, right_size), spacing),
            path,
            priority,
            Some(member),
        ),
        (None, Some((right, _, member))) => (
            photo_set::PhotoSet::solo(right),
            member.path,
            member.priority,
            None,
        ),
        (None, None) => return None,
    };
    // Swatches and blurred backgrounds see the members abutted with no gap,
    // built only for the mats that read them.
    let whole = std::cell::LazyCell::new(|| set.abutted());

    // `fill-when-fits`: render the photo full-bleed (cover-crop, no mat).
    if fill_screen {
        let canvas = scale_image_to_cover_canvas(&whole, canvas_w, canvas_h, max_dim);
        let canvas = ImagePlane {
            width: canvas_w,
            height: canvas_h,
            pixels: canvas.into_raw(),
        };
        return Some(MatResult {
            path,
            canvas,
            priority,
            partner,
            windows: Vec::new(),
            mat_kind,
            photos,
        });
    }

    let max_upscale = max_upscale_factor.max(1.0);
    let swatches = std::cell::LazyCell::new(|| PhotoSwatches::new(&whole));

    if let MattingMode::Studio {
        bevel_width_px,
        bevel_color,
        texture_strength,
        warp_period_px,
        weft_period_px,
        ..
    } = &matting.style
    {
        let mat_color = matting
            .runtime
            .studio_color(&swatches)
            .unwrap_or_else(|| swatches.average());
        let mut bevel_px = bevel_width_px.max(0.0);
        let margin_x = (canvas_w as f32 * margin).round();
        let margin_y = (canvas_h as f32 * margin).round();
        let inner_w = (canvas_w as f32 - 2.0 * margin_x).max(1.0);
        let inner_h = (canvas_h as f32 - 2.0 * margin_y).max(1.0);
        let max_bevel = 0.5 * inner_w.min(inner_h).max(0.0);
        if max_bevel <= 0.0 {
            bevel_px = 0.0;
        } else {
            bevel_px = bevel_px.min(max_bevel);
        }
        let photo_space_w = (canvas_w as f32 - 2.0 * (margin_x + bevel_px)).max(1.0);
        let photo_space_h = (canvas_h as f32 - 2.0 * (margin_y + bevel_px)).max(1.0);
        let max_photo_w = photo_space_w.floor().max(1.0);
        let max_photo_h = photo_space_h.floor().max(1.0);
        let windows = if set.is_pair() {
            // A pair is laid out in the photo space (`photo_set`): the bar
            // holds both inner bevels, the margins the outer ones.
            set.pair_windows(photo_set::PhotoSpace {
                canvas_w,
                canvas_h,
                avail_h: max_photo_h,
                bevel_px,
                max_upscale,
            })
        } else {
            let (width, height) = set.solo_size();
            let iw = width.max(1) as f32;
            let ih = height.max(1) as f32;
            let mut scale = (photo_space_w / iw)
                .min(photo_space_h / ih)
                .min(max_upscale);
            if !scale.is_finite() || scale <= 0.0 {
                scale = 1.0;
            }
            let mut photo_w = (iw * scale).round().clamp(1.0, max_photo_w);
            let mut photo_h = (ih * scale).round().clamp(1.0, max_photo_h);
            photo_w = photo_w.clamp(1.0, canvas_w as f32);
            photo_h = photo_h.clamp(1.0, canvas_h as f32);
            let photo_w = photo_w as u32;
            let photo_h = photo_h as u32;
            let (offset_x, offset_y) = center_offset(photo_w, photo_h, canvas_w, canvas_h);
            vec![photo_set::Window {
                x: offset_x,
                y: offset_y,
                w: photo_w,
                h: photo_h,
            }]
        };
        let images = set.resize_to(&windows);

        let canvas = render_studio_mat(
            canvas_w,
            canvas_h,
            &photo_set::placed(&windows, &images),
            mat_color,
            bevel_px,
            *bevel_color,
            *texture_strength,
            *warp_period_px,
            *weft_period_px,
        );

        let canvas = ImagePlane {
            width: canvas_w,
            height: canvas_h,
            pixels: canvas.into_raw(),
        };

        return Some(photo_set::matted(MatResult {
            path,
            canvas,
            priority,
            partner,
            windows,
            mat_kind,
            photos,
        }));
    }

    if let MattingMode::PassePartout {
        bevel_width_px,
        bevel_color,
        ..
    } = &matting.style
    {
        let mat_color = matting
            .runtime
            .passe_partout_color(&swatches)
            .unwrap_or_else(|| swatches.average());
        let mut bevel_px = bevel_width_px.max(0.0);
        let margin_x = (canvas_w as f32 * margin).round();
        let margin_y = (canvas_h as f32 * margin).round();
        let inner_w = (canvas_w as f32 - 2.0 * margin_x).max(1.0);
        let inner_h = (canvas_h as f32 - 2.0 * margin_y).max(1.0);
        let max_bevel = 0.5 * inner_w.min(inner_h).max(0.0);
        if max_bevel <= 0.0 {
            bevel_px = 0.0;
        } else {
            bevel_px = bevel_px.min(max_bevel);
        }
        let photo_space_w = (canvas_w as f32 - 2.0 * (margin_x + bevel_px)).max(1.0);
        let photo_space_h = (canvas_h as f32 - 2.0 * (margin_y + bevel_px)).max(1.0);
        let max_photo_w = photo_space_w.floor().max(1.0);
        let max_photo_h = photo_space_h.floor().max(1.0);
        let windows = if set.is_pair() {
            // A pair is laid out in the photo space (`photo_set`): the bar
            // holds both inner bevels, the margins the outer ones.
            set.pair_windows(photo_set::PhotoSpace {
                canvas_w,
                canvas_h,
                avail_h: max_photo_h,
                bevel_px,
                max_upscale,
            })
        } else {
            let (width, height) = set.solo_size();
            let iw = width.max(1) as f32;
            let ih = height.max(1) as f32;
            let mut scale = (photo_space_w / iw)
                .min(photo_space_h / ih)
                .min(max_upscale);
            if !scale.is_finite() || scale <= 0.0 {
                scale = 1.0;
            }
            let mut photo_w = (iw * scale).round().clamp(1.0, max_photo_w);
            let mut photo_h = (ih * scale).round().clamp(1.0, max_photo_h);
            photo_w = photo_w.clamp(1.0, canvas_w as f32);
            photo_h = photo_h.clamp(1.0, canvas_h as f32);
            let photo_w = photo_w as u32;
            let photo_h = photo_h as u32;
            let (offset_x, offset_y) = center_offset(photo_w, photo_h, canvas_w, canvas_h);
            vec![photo_set::Window {
                x: offset_x,
                y: offset_y,
                w: photo_w,
                h: photo_h,
            }]
        };
        let images = set.resize_to(&windows);

        let canvas = render_studio_mat(
            canvas_w,
            canvas_h,
            &photo_set::placed(&windows, &images),
            mat_color,
            bevel_px,
            *bevel_color,
            0.0,
            5.6,
            5.2,
        );

        let canvas = ImagePlane {
            width: canvas_w,
            height: canvas_h,
            pixels: canvas.into_raw(),
        };

        return Some(photo_set::matted(MatResult {
            path,
            canvas,
            priority,
            partner,
            windows,
            mat_kind,
            photos,
        }));
    }

    if let MattingMode::DropShadow {
        shadow_color,
        shadow_opacity,
        shadow_blur_px,
        shadow_offset_px,
        ..
    } = &matting.style
    {
        let color = f32_rgb_to_u8(
            matting
                .runtime
                .drop_shadow_color(&swatches)
                .unwrap_or_else(|| swatches.average()),
        );
        let windows = overlay_windows(&set, canvas_w, canvas_h, margin, max_upscale);
        let images = set.resize_to(&windows);

        let bg_px = Rgba([color[0], color[1], color[2], 255]);
        let mut background = RgbaImage::from_pixel(canvas_w, canvas_h, bg_px);

        render_drop_shadow(
            &mut background,
            &windows,
            DropShadowParams {
                color: *shadow_color,
                opacity: *shadow_opacity,
                blur_px: *shadow_blur_px,
                offset_px: *shadow_offset_px,
            },
        );

        photo_set::overlay_windows(&mut background, &photo_set::placed(&windows, &images));

        let canvas = ImagePlane {
            width: canvas_w,
            height: canvas_h,
            pixels: background.into_raw(),
        };

        return Some(photo_set::matted(MatResult {
            path,
            canvas,
            priority,
            partner,
            windows,
            mat_kind,
            photos,
        }));
    }

    let windows = overlay_windows(&set, canvas_w, canvas_h, margin, max_upscale);
    let images = set.resize_to(&windows);

    let mut background = match &matting.style {
        MattingMode::FixedColor { colors, .. } => {
            let color = matting
                .runtime
                .fixed_color()
                .or_else(|| colors.first().copied())
                .unwrap_or([0, 0, 0]);
            let px = Rgba([color[0], color[1], color[2], 255]);
            RgbaImage::from_pixel(canvas_w, canvas_h, px)
        }
        MattingMode::Blur {
            sigma,
            sample_scale,
            backend,
        } => {
            let bg = scale_image_to_cover_canvas(&whole, canvas_w, canvas_h, max_dim);
            if *sigma > 0.0 {
                let mut sample = bg;
                let mut sigma_px = *sigma;
                let scale = if sample_scale.is_finite() {
                    *sample_scale
                } else {
                    MattingMode::default_blur_sample_scale()
                }
                .clamp(0.01, 1.0);
                if scale < 1.0 {
                    let sample_w = ((canvas_w as f32) * scale)
                        .round()
                        .clamp(1.0, canvas_w as f32) as u32;
                    let sample_h = ((canvas_h as f32) * scale)
                        .round()
                        .clamp(1.0, canvas_h as f32) as u32;
                    sample = imageops::resize(
                        &sample,
                        sample_w,
                        sample_h,
                        imageops::FilterType::CatmullRom,
                    );
                    sigma_px *= scale.max(0.01);
                }

                let mut blurred: RgbaImage = apply_blur(&sample, sigma_px, *backend);
                if blurred.width() != canvas_w || blurred.height() != canvas_h {
                    blurred = imageops::resize(
                        &blurred,
                        canvas_w,
                        canvas_h,
                        imageops::FilterType::CatmullRom,
                    );
                }
                blurred
            } else {
                bg
            }
        }
        MattingMode::Studio { .. } => unreachable!(),
        MattingMode::PassePartout { .. } => unreachable!(),
        MattingMode::DropShadow { .. } => unreachable!(),
        MattingMode::Gradient {
            start_color,
            end_color,
            direction,
            angle_degrees,
        } => render_gradient_canvas(
            canvas_w,
            canvas_h,
            *start_color,
            *end_color,
            *direction,
            *angle_degrees,
        ),
        MattingMode::Vignette {
            strength,
            radius,
            softness,
            ..
        } => {
            let resolved = matting
                .runtime
                .vignette_color(&swatches)
                .unwrap_or_else(|| swatches.average());
            let color = f32_rgb_to_u8(resolved);
            render_vignette_canvas(canvas_w, canvas_h, color, *strength, *radius, *softness)
        }
        MattingMode::CinematicBlur {
            sigma,
            sample_scale,
            backend,
            darken,
            vignette_strength,
        } => {
            let bg = scale_image_to_cover_canvas(&whole, canvas_w, canvas_h, max_dim);
            let mut blurred = if *sigma > 0.0 {
                let mut sample = bg;
                let mut sigma_px = *sigma;
                let scale = if sample_scale.is_finite() {
                    *sample_scale
                } else {
                    MattingMode::default_blur_sample_scale()
                }
                .clamp(0.01, 1.0);
                if scale < 1.0 {
                    let sample_w = ((canvas_w as f32) * scale)
                        .round()
                        .clamp(1.0, canvas_w as f32) as u32;
                    let sample_h = ((canvas_h as f32) * scale)
                        .round()
                        .clamp(1.0, canvas_h as f32) as u32;
                    sample = imageops::resize(
                        &sample,
                        sample_w,
                        sample_h,
                        imageops::FilterType::CatmullRom,
                    );
                    sigma_px *= scale.max(0.01);
                }
                let mut b: RgbaImage = apply_blur(&sample, sigma_px, *backend);
                if b.width() != canvas_w || b.height() != canvas_h {
                    b = imageops::resize(&b, canvas_w, canvas_h, imageops::FilterType::CatmullRom);
                }
                b
            } else {
                bg
            };
            let darken = darken.clamp(0.0, 1.0);
            let brightness = 1.0 - darken;
            for px in blurred.pixels_mut() {
                px[0] = (px[0] as f32 * brightness).round() as u8;
                px[1] = (px[1] as f32 * brightness).round() as u8;
                px[2] = (px[2] as f32 * brightness).round() as u8;
            }
            apply_vignette_overlay(&mut blurred, *vignette_strength, 0.75, 0.5);
            blurred
        }
        MattingMode::FixedImage { fit, .. } => {
            if let Some(bg) = matting.runtime.fixed_image() {
                match bg.canvas_for(*fit, canvas_w, canvas_h, max_dim) {
                    Ok(prepared) => prepared.as_ref().clone(),
                    Err(err) => {
                        warn!(
                            "failed to prepare fixed background from {}: {err}",
                            bg.path().display()
                        );
                        RgbaImage::from_pixel(canvas_w, canvas_h, Rgba([0, 0, 0, 255]))
                    }
                }
            } else {
                RgbaImage::from_pixel(canvas_w, canvas_h, Rgba([0, 0, 0, 255]))
            }
        }
    };

    photo_set::overlay_windows(&mut background, &photo_set::placed(&windows, &images));

    let canvas = ImagePlane {
        width: canvas_w,
        height: canvas_h,
        pixels: background.into_raw(),
    };

    Some(photo_set::matted(MatResult {
        path,
        canvas,
        priority,
        partner,
        windows,
        mat_kind,
        photos,
    }))
}

/// A finished matting result arrives: its photos are no longer in flight,
/// one more canvas to upload.
fn accept_mat_result(
    mat_inflight: &mut usize,
    ready_results: &mut VecDeque<MatResult>,
    result: MatResult,
) {
    *mat_inflight = mat_inflight.saturating_sub(result.photos);
    if result.is_acknowledgement() {
        return;
    }
    photo_set::queue_result(ready_results, result);
}

#[derive(Debug)]
enum ViewerEvent {
    Tick,
    Command(ViewerCommand),
    /// A finished matting result, taken off the channel by the driver.
    MatResult(MatResult),
    /// A decoded photo, taken off the loader channel by the driver.
    Photo(PhotoLoaded),
    Cancelled,
}

/// Wayland: wait for what the viewer needs and tell the winit thread. The
/// waiting itself is `wake::drive_forwarding`.
async fn drive_viewer_events(
    control: Receiver<ViewerCommand>,
    cancel: CancellationToken,
    proxy: winit::event_loop::EventLoopProxy<ViewerEvent>,
    mat_results: Receiver<MatResult>,
    loader: PhotoReceiver,
    published: tokio::sync::watch::Receiver<wake::Published>,
) {
    wake::drive_forwarding(
        cancel,
        control,
        mat_results,
        loader,
        published,
        CONTROL_TICK_INTERVAL,
        |forward| {
            let event = match forward {
                wake::Forward::Tick => ViewerEvent::Tick,
                wake::Forward::Command(cmd) => ViewerEvent::Command(cmd),
                wake::Forward::MatResult(result) => ViewerEvent::MatResult(result),
                wake::Forward::Photo(photo) => ViewerEvent::Photo(photo),
                wake::Forward::Cancelled => ViewerEvent::Cancelled,
            };
            proxy.send_event(event).is_ok()
        },
    )
    .await;
}

use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::window::{Fullscreen, Window, WindowAttributes, WindowId};

fn with_photo_app_id(attrs: WindowAttributes) -> WindowAttributes {
    #[cfg(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    {
        use winit::platform::wayland::WindowAttributesExtWayland;
        attrs.with_name("photoframe", "photoframe")
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )))]
    {
        attrs
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TransitionUniforms {
    screen_size: [f32; 2],
    progress: f32,
    // Canvas mip level the photo planes are sampled at (`canvas_lod`).
    lod: f32,
    // Texture-coordinate maps of the current / next plane
    // (`draw_params::plane_xform`), applied by the vertex stage.
    current_xform: [f32; 4],
    next_xform: [f32; 4],
    params0: [f32; 4],
    params1: [f32; 4],
    params3: [f32; 4],
    // Viewer background (linear RGB, alpha unused). The shader composites
    // letterbox regions over this itself so the pipeline can render
    // opaquely without a per-pixel destination read for blending.
    background: [f32; 4],
}

/// Per-petal constants for the iris transition, solved on the CPU each
/// frame so the fragment loop needs no transcendentals. A uniform of its own
/// (`@group(0) @binding(1)`), read only by `fs_iris_layer`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct IrisPetals {
    // petals_a[i] = (annulus_center.xy, tip_dir.xy)
    petals_a: [[f32; 4]; 16],
    // petals_b[i] = (trail_dir.xy, facing_cosine, unused)
    petals_b: [[f32; 4]; 16],
}

/// What the main photo pass draws, one fragment entry point and one render
/// pipeline each (`viewer_quad.wgsl`). Separate pipelines, not one shader
/// with a `switch`, so each compiles with only its own register pressure:
/// on V3D the union of every kind forced the shader to 2 threads per QPU
/// and starved every transition's texture fetches (see the shader's header).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum DrawKind {
    /// A resting photo (no transition).
    Rest,
    Fade,
    Wipe,
    Push,
    EInk,
    Dissolve,
    RadialWipe,
    VenetianBlinds,
    CrossfadeZoom,
    /// The iris composite (the petals are drawn by `fs_iris_layer`).
    Iris,
    /// `PHOTOFRAME_DEBUG_BEZIER`.
    DebugBezier,
}

impl DrawKind {
    const ALL: [DrawKind; 11] = [
        DrawKind::Rest,
        DrawKind::Fade,
        DrawKind::Wipe,
        DrawKind::Push,
        DrawKind::EInk,
        DrawKind::Dissolve,
        DrawKind::RadialWipe,
        DrawKind::VenetianBlinds,
        DrawKind::CrossfadeZoom,
        DrawKind::Iris,
        DrawKind::DebugBezier,
    ];

    /// Exhaustive: a new `TransitionKind` does not compile until it has a
    /// pipeline.
    fn for_transition(kind: TransitionKind) -> DrawKind {
        match kind {
            TransitionKind::Fade => DrawKind::Fade,
            TransitionKind::Wipe => DrawKind::Wipe,
            TransitionKind::Push => DrawKind::Push,
            TransitionKind::EInk => DrawKind::EInk,
            TransitionKind::Dissolve => DrawKind::Dissolve,
            TransitionKind::RadialWipe => DrawKind::RadialWipe,
            TransitionKind::VenetianBlinds => DrawKind::VenetianBlinds,
            TransitionKind::CrossfadeZoom => DrawKind::CrossfadeZoom,
            TransitionKind::Iris => DrawKind::Iris,
        }
    }

    /// The fragment entry point in `viewer_quad.wgsl`.
    fn entry_point(self) -> &'static str {
        match self {
            DrawKind::Rest => "fs_rest",
            DrawKind::Fade => "fs_fade",
            DrawKind::Wipe => "fs_wipe",
            DrawKind::Push => "fs_push",
            DrawKind::EInk => "fs_eink",
            DrawKind::Dissolve => "fs_dissolve",
            DrawKind::RadialWipe => "fs_radial_wipe",
            DrawKind::VenetianBlinds => "fs_venetian_blinds",
            DrawKind::CrossfadeZoom => "fs_crossfade_zoom",
            DrawKind::Iris => "fs_iris",
            DrawKind::DebugBezier => "fs_debug_bezier",
        }
    }

    fn index(self) -> usize {
        DrawKind::ALL
            .iter()
            .position(|k| *k == self)
            .expect("every DrawKind is in ALL")
    }
}

/// The main pass's render pipelines, one per `DrawKind`.
struct QuadPipelines {
    pipelines: Vec<wgpu::RenderPipeline>,
}

impl QuadPipelines {
    fn get(&self, kind: DrawKind) -> &wgpu::RenderPipeline {
        &self.pipelines[kind.index()]
    }
}

struct GpuCtx {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    limits: wgpu::Limits,
    uniform_buf: wgpu::Buffer,
    /// `IrisPetals`, written only while an iris transition plays.
    petal_buf: wgpu::Buffer,
    uniform_bind: wgpu::BindGroup,
    img_bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    quad_pipelines: QuadPipelines,
    blank_plane: TexturePlane,
    /// The petal layer's first pass (`fs_iris_cover`, the loop over
    /// petals) into `iris_cover`; the second (`fs_iris_layer`, shading)
    /// reads it into `iris_layer`.
    iris_cover_pipeline: wgpu::RenderPipeline,
    iris_layer_pipeline: wgpu::RenderPipeline,
    iris_cover: Option<OffscreenTarget>,
    iris_layer: Option<OffscreenTarget>,
    /// E-ink's per-cell reveal mask (`fs_eink_mask`), stripes x stripes,
    /// read by `fs_eink` at group 3.
    eink_mask_pipeline: wgpu::RenderPipeline,
    eink_mask: Option<OffscreenTarget>,
    blit_pipeline: wgpu::RenderPipeline,
    /// Writes one canvas mip level from the level above it (`fs_blit`, 2:1
    /// bilinear = box filter, in linear light), at upload.
    mip_pipeline: wgpu::RenderPipeline,
    /// The same, unsharp-masked within the taps' range (`fs_mip_sharpen`):
    /// for the level transitions read, to pre-compensate the bilinear
    /// upscale of the intermediate.
    mip_sharpen_pipeline: wgpu::RenderPipeline,
    half_target: Option<OffscreenTarget>,
    /// GPU timestamps around a transition frame's passes, when the device
    /// offers `TIMESTAMP_QUERY`.
    pass_timer: Option<PassTimer>,
}

/// Timestamp queries around a transition frame's passes, for the KMS
/// per-frame audit (`kms_frame`): a begin/end pair per `TimedPass`. Each
/// timed frame takes the next of `SLOTS` slots, its own queries and its own
/// region of the resolve buffer, so a frame rendered ahead never resets
/// queries an earlier frame has not resolved yet (nothing orders a query set
/// between submissions). A frame resolves the pairs it wrote in its own
/// submission and copies them to a small buffer of its own, mapped
/// asynchronously; the callback runs on whichever thread polls the device
/// next (the flip observer, every vblank), never blocking the render path.
/// Only frames the audit logs are timed.
struct PassTimer {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    /// Nanoseconds per timestamp tick.
    period_ns: f64,
    next_slot: std::sync::atomic::AtomicU32,
}

/// What a begin/end query pair times. A pair may span several passes (the
/// petal layer's): its begin is written by the first, its end by the last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimedPass {
    Main = 0,
    Upscale = 1,
    Layer = 2,
}

/// One frame's slot of a `PassTimer`, and which queries its passes wrote.
struct TimedFrame<'a> {
    timer: &'a PassTimer,
    slot: u32,
    /// Bit 2k: pair k's begin was written; bit 2k + 1: its end.
    written: std::cell::Cell<u8>,
}

impl PassTimer {
    const PAIRS: u32 = 3;
    /// Frames that may be in flight at once, with room to spare: the FIFO
    /// swapchain bounds render-ahead to its images, and a slot is only
    /// reused `SLOTS` timed frames later.
    const SLOTS: u32 = 8;
    /// A pair's region of the resolve buffer (each resolve starts aligned).
    const PAIR_STRIDE: u64 = wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT;

    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        Some(Self {
            queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("pass-timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: 2 * Self::PAIRS * Self::SLOTS,
            }),
            resolve: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pass-timestamps-resolve"),
                size: Self::PAIR_STRIDE * u64::from(Self::PAIRS * Self::SLOTS),
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            period_ns: f64::from(queue.get_timestamp_period()),
            next_slot: std::sync::atomic::AtomicU32::new(0),
        })
    }

    /// Time one frame.
    fn frame(&self) -> TimedFrame<'_> {
        let slot = self.next_slot.fetch_add(1, Ordering::Relaxed) % Self::SLOTS;
        TimedFrame {
            timer: self,
            slot,
            written: std::cell::Cell::new(0),
        }
    }
}

impl TimedFrame<'_> {
    /// The query index of `pass`'s begin (`end` false) or end.
    fn query(&self, pass: TimedPass, end: bool) -> u32 {
        (self.slot * PassTimer::PAIRS + pass as u32) * 2 + u32::from(end)
    }

    /// Timestamp writes for a render pass: `pass`'s begin at its start
    /// and/or its end at its end.
    fn writes(
        &self,
        pass: TimedPass,
        begin: bool,
        end: bool,
    ) -> wgpu::RenderPassTimestampWrites<'_> {
        let bits = (u8::from(begin) | (u8::from(end) << 1)) << (2 * pass as u8);
        self.written.set(self.written.get() | bits);
        wgpu::RenderPassTimestampWrites {
            query_set: &self.timer.queries,
            beginning_of_pass_write_index: begin.then(|| self.query(pass, false)),
            end_of_pass_write_index: end.then(|| self.query(pass, true)),
        }
    }

    /// The pairs whose begin and end were both written.
    fn complete(&self) -> [bool; PassTimer::PAIRS as usize] {
        let w = self.written.get();
        [0, 1, 2].map(|k| (w >> (2 * k)) & 0b11 == 0b11)
    }

    /// After the frame's passes: resolve the pairs they wrote (only those:
    /// resolving a query never written would wait forever) into a buffer
    /// for `read`, 16 bytes per pair in `TimedPass` order.
    fn resolve(&self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) -> wgpu::Buffer {
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pass-timestamps-readback"),
            size: 16 * u64::from(PassTimer::PAIRS),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        for (k, pass) in [TimedPass::Main, TimedPass::Upscale, TimedPass::Layer]
            .into_iter()
            .enumerate()
        {
            if !self.complete()[k] {
                continue;
            }
            let first = self.query(pass, false);
            let offset =
                u64::from(self.slot * PassTimer::PAIRS + pass as u32) * PassTimer::PAIR_STRIDE;
            encoder.resolve_query_set(
                &self.timer.queries,
                first..first + 2,
                &self.timer.resolve,
                offset,
            );
            encoder.copy_buffer_to_buffer(
                &self.timer.resolve,
                offset,
                &readback,
                16 * k as u64,
                16,
            );
        }
        readback
    }

    /// After the frame is submitted: map its timestamps; the cell fills
    /// when they arrive.
    fn read(&self, readback: wgpu::Buffer) -> PassTimesCell {
        let cell = PassTimesCell::default();
        let out = Arc::clone(&cell);
        let period_ns = self.timer.period_ns;
        let complete = self.complete();
        let buffer = readback.clone();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                if result.is_err() {
                    return;
                }
                let pairs: [Option<(u64, u64)>; PassTimer::PAIRS as usize] = {
                    let view = buffer.slice(..).get_mapped_range();
                    let tick = |i: usize| {
                        u64::from_le_bytes(view[8 * i..8 * i + 8].try_into().expect("8 bytes"))
                    };
                    [0, 1, 2].map(|k| complete[k].then(|| (tick(2 * k), tick(2 * k + 1))))
                };
                if let Some(times) = pass_times(pairs, period_ns) {
                    let _ = out.set(times);
                }
            });
        cell
    }
}

/// Durations from resolved (begin, end) ticks in `TimedPass` order, `None`
/// when the main pair is missing or runs backwards. A pair whose end reads
/// before its begin (Metal now and then leaves an end sample at 0) is no
/// measurement.
fn pass_times(pairs: [Option<(u64, u64)>; 3], period_ns: f64) -> Option<PassTimes> {
    let ns = |ticks: u64| Duration::from_nanos((ticks as f64 * period_ns).round() as u64);
    let span = |(begin, end): (u64, u64)| end.checked_sub(begin).map(ns);
    let [main, upscale, layer] = pairs;
    let (main_begin, main_end) = main?;
    let main_time = span((main_begin, main_end))?;
    let layer_time = layer.and_then(span);
    let draw = match (layer, layer_time) {
        (Some((layer_begin, _)), Some(_)) => span((layer_begin, main_end))?,
        _ => main_time,
    };
    Some(PassTimes {
        draw,
        main: main_time,
        layer: layer_time,
        upscale: upscale.and_then(span),
    })
}

/// Reduced-resolution offscreen render target that can also be sampled.
/// At 4K the Pi's frame cost is dominated by per-pixel fill, so heavy
/// passes render at a fraction of the surface size and get upsampled:
/// the iris petal layer at 1/IRIS_LAYER_SCALE, and every transition
/// frame at 1/TRANSITION_HALF_SCALE.
struct OffscreenTarget {
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
    w: u32,
    h: u32,
}

impl GpuCtx {
    fn make_offscreen(
        &self,
        label: &str,
        w: u32,
        h: u32,
        format: wgpu::TextureFormat,
    ) -> OffscreenTarget {
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
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
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.img_bind_layout,
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
        OffscreenTarget { view, bind, w, h }
    }

    /// Create (or re-create after a resize) the iris petal layer so it
    /// tracks the current surface size.
    fn ensure_iris_layer(&mut self) {
        let scale = iris_layer_scale();
        let w = (self.config.width / scale).max(1);
        let h = (self.config.height / scale).max(1);
        if let Some(layer) = self.iris_layer.as_ref()
            && layer.w == w
            && layer.h == h
        {
            return;
        }
        // Half-float, not 8-bit: the petals are very dark in linear light,
        // so a smooth shading gradient spans only a handful of 8-bit
        // levels and visibly bands. Rgba16Float has ample dark-range
        // precision; the layer is quarter-res so the extra bytes are
        // negligible.
        self.iris_layer =
            Some(self.make_offscreen("iris-petal-layer", w, h, wgpu::TextureFormat::Rgba16Float));
        // The cover pass's output, read texel for texel by the shading pass
        // (see fs_iris_cover for what half floats hold).
        self.iris_cover =
            Some(self.make_offscreen("iris-petal-cover", w, h, wgpu::TextureFormat::Rgba16Float));
    }

    /// Create (or re-create for another stripe count) e-ink's reveal mask,
    /// one texel per cell: the mask as a half float plus its rounding
    /// remainder (see `fs_eink_mask`).
    fn ensure_eink_mask(&mut self, stripes: u32) {
        if let Some(mask) = self.eink_mask.as_ref()
            && mask.w == stripes
            && mask.h == stripes
        {
            return;
        }
        self.eink_mask = Some(self.make_offscreen(
            "eink-reveal-mask",
            stripes,
            stripes,
            wgpu::TextureFormat::Rgba16Float,
        ));
    }

    /// Create (or re-create after a resize) the reduced-resolution
    /// intermediate that transition frames render into.
    fn ensure_half_target(&mut self) {
        let scale = transition_scale();
        let w = (self.config.width / scale).max(1);
        let h = (self.config.height / scale).max(1);
        if let Some(target) = self.half_target.as_ref()
            && target.w == w
            && target.h == h
        {
            return;
        }
        let format = self.config.format;
        self.half_target = Some(self.make_offscreen("transition-half-target", w, h, format));
    }
}

/// The photo canvases' texture format.
const CANVAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Mip levels a canvas carries: level 0 plus one 2:1 level per halving of
/// the transition intermediate (`transition_scale`), so a transition reads
/// the level whose texels match the intermediate's pixels (`canvas_lod`)
/// instead of minifying level 0: at scale 2 a quarter of the memory per
/// fetch, which on the Pi is the transition's cost (Pi measurement,
/// 2026-10-21: wipe/fade/dissolve flat at ~17.7 ms per frame across scales;
/// push, which fetches one real plane per pixel, held 60). Native transitions
/// (scale 1) need none. Memory: +25% per canvas at scale 2, +31% at 4.
fn canvas_mip_levels(width: u32, height: u32, transition_scale: u32) -> u32 {
    let wanted = transition_scale.max(1).next_power_of_two().trailing_zeros();
    let possible = width.min(height).max(1).ilog2();
    1 + wanted.min(possible)
}

/// The level the photo planes are sampled at: log2 of the intermediate's
/// downscale while drawing into it, 0 for native frames (resting photos and
/// a transition's tail), so those are unchanged.
fn canvas_lod(into_intermediate: bool) -> f32 {
    if into_intermediate {
        (transition_scale().max(1) as f32).log2()
    } else {
        0.0
    }
}

/// `PHOTOFRAME_PROBE`: diagnostic, build-free switches for measuring on
/// the device, comma-separated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Probes {
    /// `single-sample`: every transition reads one photo per pixel (the
    /// next plane's color is a constant), to measure what the second fetch
    /// costs. Transitions look wrong on purpose.
    single_sample: bool,
}

/// The probes named in `value`, and the names it has that are not probes.
fn parse_probes(value: &str) -> (Probes, Vec<&str>) {
    let mut probes = Probes::default();
    let mut unknown = Vec::new();
    for name in value.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        match name {
            "single-sample" => probes.single_sample = true,
            other => unknown.push(other),
        }
    }
    (probes, unknown)
}

/// The `PHOTOFRAME_PROBE` probes, read once.
fn probes() -> Probes {
    static PROBES: std::sync::OnceLock<Probes> = std::sync::OnceLock::new();
    *PROBES.get_or_init(|| {
        let Ok(value) = std::env::var("PHOTOFRAME_PROBE") else {
            return Probes::default();
        };
        let (probes, unknown) = parse_probes(&value);
        for name in unknown {
            warn!(value = %name, "unknown PHOTOFRAME_PROBE; ignored");
        }
        if probes.single_sample {
            warn!(
                "PHOTOFRAME_PROBE=single-sample: transitions show a flat next photo (diagnostic)"
            );
        }
        probes
    })
}

/// The shader's pipeline constants for the probes that are compiled in.
fn probe_pipeline_constants(probes: Probes) -> Vec<(&'static str, f64)> {
    let mut constants = Vec::new();
    if probes.single_sample {
        constants.push(("PROBE_SINGLE_SAMPLE", 1.0));
    }
    constants
}

fn upload_plane(gpu: &GpuCtx, plane: ImagePlane) -> Option<TexturePlane> {
    let ImagePlane {
        width,
        height,
        pixels,
    } = plane;
    if width == 0 || height == 0 {
        return None;
    }
    let mip_levels = canvas_mip_levels(width, height, transition_scale());
    let mut usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
    if mip_levels > 1 {
        usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
    }
    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("photo-texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: mip_levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: CANVAS_FORMAT,
        usage,
        view_formats: &[],
    });
    let stride = 4 * width;
    let padded = compute_padded_stride(stride);
    let upload: Cow<'_, [u8]> = if padded != stride {
        let mut staging = vec![0u8; (padded as usize) * (height as usize)];
        let rs = stride as usize;
        let rd = padded as usize;
        for y in 0..(height as usize) {
            let so = y * rs;
            let doff = y * rd;
            staging[doff..doff + rs].copy_from_slice(&pixels[so..so + rs]);
        }
        Cow::Owned(staging)
    } else {
        Cow::Borrowed(&pixels)
    };
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        upload.as_ref(),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(padded),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    if mip_levels > 1 {
        generate_canvas_mips(gpu, &tex, mip_levels);
    }
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("image-bind"),
        layout: &gpu.img_bind_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&gpu.sampler),
            },
        ],
    });
    let bytes = rgba8_bytes(width, height, mip_levels);
    let account = CanvasBytes::track(bytes);
    debug!(
        width,
        height,
        mip_levels,
        canvas_mb = format_args!("{:.1}", bytes as f64 / 1e6),
        live_canvases = LIVE_CANVASES.load(Ordering::Relaxed),
        live_canvas_mb = format_args!(
            "{:.1}",
            LIVE_CANVAS_BYTES.load(Ordering::Relaxed) as f64 / 1e6
        ),
        "canvas_upload"
    );
    Some(TexturePlane {
        bind,
        w: width,
        h: height,
        _gpu_bytes: Some(account),
    })
}

/// Fill levels 1.. of a canvas, each from the one above: one fullscreen
/// pass per level with `fs_blit`, whose bilinear sample at a target pixel's
/// center lands on the shared corner of a 2x2 source block, so it is that
/// block's average (a box filter), taken in linear light (the sRGB view
/// decodes, the sRGB target encodes). The last level, the one transitions
/// read, is box filtered and unsharp-masked within its taps' range
/// (`fs_mip_sharpen`) to pre-compensate the intermediate's bilinear
/// upscale. Runs at upload, which the viewer keeps off a transition's
/// frames.
fn generate_canvas_mips(gpu: &GpuCtx, tex: &wgpu::Texture, mip_levels: u32) {
    let level_view = |level: u32| {
        tex.create_view(&wgpu::TextureViewDescriptor {
            label: Some("canvas-mip"),
            base_mip_level: level,
            mip_level_count: Some(1),
            ..Default::default()
        })
    };
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("canvas-mips"),
        });
    for level in 1..mip_levels {
        let source = level_view(level - 1);
        let target = level_view(level);
        let source_bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("canvas-mip-source"),
            layout: &gpu.img_bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&gpu.sampler),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("canvas-mip-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target,
                depth_slice: None,
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
        pass.set_pipeline(if level + 1 == mip_levels {
            &gpu.mip_sharpen_pipeline
        } else {
            &gpu.mip_pipeline
        });
        pass.set_bind_group(0, &gpu.uniform_bind, &[]);
        pass.set_bind_group(1, &source_bind, &[]);
        pass.draw(0..3, 0..1);
    }
    gpu.queue.submit(Some(encoder.finish()));
}

fn upload_mat_result(gpu: &GpuCtx, result: MatResult) -> Option<ImgTex> {
    let MatResult {
        path,
        canvas,
        partner,
        mat_kind,
        ..
    } = result;
    let plane = upload_plane(gpu, canvas)?;
    Some(ImgTex {
        plane,
        path,
        partner: partner.map(|member| member.path),
        mat_kind,
    })
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum ViewerModeKind {
    Greeting,
    Wake,
    Sleep,
}

struct ViewerMode {
    kind: ViewerModeKind,
    greeting: Option<GreetingScene>,
    wake: scenes::WakeScene,
    sleep: Option<SleepScene>,
}

#[derive(Copy, Clone, Debug)]
enum SceneHook {
    Enter,
    Exit,
}

impl ViewerMode {
    fn new(kind: ViewerModeKind, wake: scenes::WakeScene) -> Self {
        Self {
            kind,
            greeting: None,
            wake,
            sleep: None,
        }
    }

    fn kind(&self) -> ViewerModeKind {
        self.kind
    }

    fn wake(&self) -> &scenes::WakeScene {
        &self.wake
    }

    fn wake_mut(&mut self) -> &mut scenes::WakeScene {
        &mut self.wake
    }

    fn greeting(&self) -> Option<&GreetingScene> {
        self.greeting.as_ref()
    }

    fn greeting_mut(&mut self) -> Option<&mut GreetingScene> {
        self.greeting.as_mut()
    }

    fn sleep(&self) -> Option<&SleepScene> {
        self.sleep.as_ref()
    }

    fn sleep_mut(&mut self) -> Option<&mut SleepScene> {
        self.sleep.as_mut()
    }

    #[allow(clippy::too_many_arguments)]
    fn with_scene_mut<'a, R>(
        &'a mut self,
        viewport: Option<Viewport>,
        config: Arc<crate::config::Configuration>,
        redraw: &'a mut dyn FnMut(),
        rng: &'a mut rand::rngs::ThreadRng,
        notify_displayed: &'a mut dyn FnMut(Displayed),
        enqueue_matting: &'a mut dyn FnMut(&mut scenes::WakeScene),
        f: impl FnOnce(&mut dyn Scene, SceneContext<'a>) -> R,
    ) -> Option<R> {
        let ctx = SceneContext::new(
            viewport,
            redraw,
            config,
            rng,
            notify_displayed,
            enqueue_matting,
        );
        match self.kind {
            ViewerModeKind::Greeting => Some(f(self.greeting.as_mut()?, ctx)),
            ViewerModeKind::Wake => Some(f(&mut self.wake, ctx)),
            ViewerModeKind::Sleep => Some(f(self.sleep.as_mut()?, ctx)),
        }
    }

    fn set_overlays(&mut self, greeting: Option<GreetingScene>, sleep: Option<SleepScene>) {
        self.greeting = greeting;
        self.sleep = sleep;
    }

    fn into_kind(self, kind: ViewerModeKind) -> Self {
        Self { kind, ..self }
    }
}

/// Where the viewer's frames go. Exactly two kinds, selected at launch by
/// `PHOTOFRAME_SURFACE` (see `ViewerSurface`); no trait, no other backends.
enum Output {
    /// A fullscreen winit window under the Wayland compositor (the default).
    Window(Arc<Window>),
    /// Direct scanout on DRM/KMS through `VK_KHR_display` (see `kms.rs`). The
    /// mode is fixed for the process lifetime.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))] // built only on Linux
    Kms {
        size: PhysicalSize<u32>,
        /// Exact vblank period of the mode.
        refresh_period: Duration,
        /// Set by `SceneContext::request_redraw`; the KMS loop takes it.
        redraw_requested: Arc<AtomicBool>,
    },
}

impl Output {
    fn window(&self) -> Option<&Arc<Window>> {
        match self {
            Output::Window(window) => Some(window),
            Output::Kms { .. } => None,
        }
    }

    fn is_kms(&self) -> bool {
        matches!(self, Output::Kms { .. })
    }

    fn size(&self) -> PhysicalSize<u32> {
        match self {
            Output::Window(window) => window.inner_size(),
            Output::Kms { size, .. } => *size,
        }
    }

    /// 1.0 on KMS: the kiosk sway config runs the panel at `scale 1.0`, so
    /// greeting and sleep text keep the size they have under Wayland.
    fn scale_factor(&self) -> f64 {
        match self {
            Output::Window(window) => window.scale_factor(),
            Output::Kms { .. } => 1.0,
        }
    }

    fn viewport(&self) -> Viewport {
        Viewport {
            size: self.size(),
            scale_factor: self.scale_factor(),
        }
    }

    /// Called immediately before every wake-scene present. On Wayland this
    /// asks winit for a `wl_surface.frame` callback on the commit present()
    /// is about to make; until the compositor answers it (once per output
    /// refresh under sway), winit withholds RedrawRequested, so a transition
    /// submits exactly one frame per refresh instead of racing mailbox. See
    /// WakeScene::ensure_redraw_requested for the model. On KMS nothing is
    /// needed: with FIFO the next acquire blocks until a swapchain image is
    /// free, which is vblank.
    fn before_present(&self) {
        if let Output::Window(window) = self {
            window.pre_present_notify();
        }
    }
}

/// What the viewer presents with when `PHOTOFRAME_PRESENT_MODE` is unset (or
/// names a mode the surface does not support).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PresentDefault {
    /// Mailbox when available, else AutoVsync. FIFO emulated over Wayland
    /// frame callbacks costs two vsyncs per frame on compositors without
    /// wp_fifo_v1 (measured: a bare clear locks at 30fps on a 60Hz output
    /// under sway 1.10), halving every transition. Mailbox latches the
    /// newest complete frame each vsync; the Wake scene paces its
    /// submissions to the compositor's frame callbacks via
    /// pre_present_notify so mailbox never has to discard.
    Wayland,
    /// FIFO, always supported. Page flips are vblank-locked and the acquire
    /// blocks until a swapchain image is free, which is the only clock the
    /// KMS loop has. Mailbox on KMS has no pacing beyond whatever the driver
    /// does, so it stays an explicit opt-in.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))] // built only on Linux
    Kms,
}

impl PresentDefault {
    fn resolve(self, supported: &[wgpu::PresentMode]) -> wgpu::PresentMode {
        match self {
            PresentDefault::Wayland if supported.contains(&wgpu::PresentMode::Mailbox) => {
                wgpu::PresentMode::Mailbox
            }
            PresentDefault::Wayland => wgpu::PresentMode::AutoVsync,
            PresentDefault::Kms => wgpu::PresentMode::Fifo,
        }
    }
}

/// PHOTOFRAME_PRESENT_MODE=fifo|mailbox|immediate overrides the presentation
/// mode for frame-pacing experiments on either surface kind. An unknown
/// value or a mode the surface does not support falls back to the surface
/// kind's default.
fn choose_present_mode(
    requested: Option<&str>,
    supported: &[wgpu::PresentMode],
    default: PresentDefault,
) -> wgpu::PresentMode {
    let fallback = default.resolve(supported);
    let requested_mode = match requested {
        Some("fifo") => wgpu::PresentMode::Fifo,
        Some("mailbox") => wgpu::PresentMode::Mailbox,
        Some("immediate") => wgpu::PresentMode::Immediate,
        Some(other) => {
            warn!(
                requested = other,
                fallback = ?fallback,
                "unknown PHOTOFRAME_PRESENT_MODE; using the default"
            );
            return fallback;
        }
        None => {
            info!(present_mode = ?fallback, ?default, "viewer_present_mode_default");
            return fallback;
        }
    };
    if supported.contains(&requested_mode) {
        info!(present_mode = ?requested_mode, "viewer_present_mode_override");
        requested_mode
    } else {
        warn!(
            requested = ?requested_mode,
            supported = ?supported,
            fallback = ?fallback,
            "requested present mode unsupported; using the default"
        );
        fallback
    }
}

/// Why a redraw could not complete; each driver decides what to do.
#[derive(Debug)]
enum RedrawError {
    /// `MAX_CONSECUTIVE_SURFACE_TIMEOUTS` acquires timed out in a row.
    /// Wayland recreates the GPU context; KMS exits for a systemd restart.
    TimeoutStreak,
    /// Unrecoverable for this process (out of memory; on KMS also a lost or
    /// outdated swapchain). The driver exits the viewer.
    Fatal(String),
}

const MAX_CONSECUTIVE_SURFACE_TIMEOUTS: u32 = 6;

/// Everything created from a fresh surface + device.
struct GpuParts {
    gpu: GpuCtx,
    greeting: GreetingScene,
    sleep: SleepScene,
    caption_overlay: Option<scenes::CaptionOverlay>,
    wifi_overlay: scenes::wifi_overlay::WifiOverlayScreen,
}

/// Adapter, device, surface configuration (not yet applied), pipelines and
/// the overlay scenes for one surface. Shared by the Wayland window and the
/// DRM/KMS output; only the present-mode default differs.
fn build_gpu(
    instance: &wgpu::Instance,
    surface: wgpu::Surface<'static>,
    size: PhysicalSize<u32>,
    present_default: PresentDefault,
    cfg: &crate::config::Configuration,
    reason: &'static str,
) -> Result<GpuParts, String> {
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: Some(&surface),
        force_fallback_adapter: false,
    }))
    .map_err(|err| format!("failed to acquire GPU adapter: {err}"))?;
    let info = adapter.get_info();
    debug!(context = reason, ?info, "viewer_gpu_adapter_acquired");
    let limits = adapter.limits();
    debug!(
        context = reason,
        max_texture_dimension_2d = limits.max_texture_dimension_2d,
        max_buffer_size = limits.max_buffer_size,
        "viewer_gpu_limits"
    );
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("viewer-device"),
        // Timestamps for the KMS per-frame audit, where offered; nothing
        // else depends on them.
        required_features: adapter.features() & wgpu::Features::TIMESTAMP_QUERY,
        required_limits: limits.clone(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::default(),
    }))
    .map_err(|err| format!("failed to acquire GPU device: {err}"))?;
    debug!(context = reason, "viewer_gpu_device_ready");
    let caps = surface.get_capabilities(&adapter);
    let format = caps
        .formats
        .iter()
        .copied()
        .find(|f| f.is_srgb())
        .or_else(|| caps.formats.first().copied())
        .ok_or("surface reported no supported texture formats")?;
    // Prefer an opaque surface: the viewer always fills every pixel,
    // and an opaque fullscreen surface is what lets the compositor
    // skip blending — and, with matching formats, hand the buffer
    // straight to the display (direct scanout, no composite copy).
    let alpha_mode = caps
        .alpha_modes
        .iter()
        .copied()
        .find(|m| *m == wgpu::CompositeAlphaMode::Opaque)
        .or_else(|| caps.alpha_modes.first().copied())
        .ok_or("surface reported no alpha modes")?;
    debug!(
        context = reason,
        surface_width = size.width,
        surface_height = size.height,
        format = ?format,
        present_modes = ?caps.present_modes,
        "viewer_gpu_surface_caps"
    );
    let present_mode = choose_present_mode(
        std::env::var("PHOTOFRAME_PRESENT_MODE").ok().as_deref(),
        &caps.present_modes,
        present_default,
    );
    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width: size.width.max(1),
        height: size.height.max(1),
        present_mode,
        alpha_mode,
        view_formats: vec![],
        desired_maximum_frame_latency: 2,
    };
    // Resources for quad
    let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("viewer-uniforms"),
        size: std::mem::size_of::<TransitionUniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let petal_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("iris-petals"),
        size: std::mem::size_of::<IrisPetals>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("uniform-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let uniform_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("uniform-bind"),
        layout: &uniform_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: petal_buf.as_entire_binding(),
            },
        ],
    });
    let img_bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("image-bind-layout"),
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
        label: Some("linear-clamp"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("viewer-quad"),
        source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
            "shaders/viewer_quad.wgsl"
        ))),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline-layout"),
        bind_group_layouts: &[
            &uniform_layout,
            &img_bind_layout,
            &img_bind_layout,
            &img_bind_layout,
        ],
        push_constant_ranges: &[],
    });
    let iris_cover_pipeline_layout =
        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("iris-cover-pipeline-layout"),
            bind_group_layouts: &[&uniform_layout],
            push_constant_ranges: &[],
        });
    // The shading pass reads the cover texel at group 1.
    let iris_layer_pipeline_layout =
        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("iris-layer-pipeline-layout"),
            bind_group_layouts: &[&uniform_layout, &img_bind_layout],
            push_constant_ranges: &[],
        });
    // Both petal-layer passes write Rgba16Float: the layer because dark
    // petals band in 8-bit (see ensure_iris_layer), the cover for its
    // distances.
    let petal_pipeline = |label: &str, layout: &wgpu::PipelineLayout, entry_point: &str| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(entry_point),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba16Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview: None,
            cache: None,
        })
    };
    let iris_cover_pipeline = petal_pipeline(
        "iris-cover-pipeline",
        &iris_cover_pipeline_layout,
        "fs_iris_cover",
    );
    let iris_layer_pipeline = petal_pipeline(
        "iris-layer-pipeline",
        &iris_layer_pipeline_layout,
        "fs_iris_layer",
    );
    let eink_mask_pipeline = petal_pipeline(
        "eink-mask-pipeline",
        &iris_cover_pipeline_layout,
        "fs_eink_mask",
    );
    let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("blit-pipeline-layout"),
        bind_group_layouts: &[&uniform_layout, &img_bind_layout],
        push_constant_ranges: &[],
    });
    // The upscale of the intermediate (PHOTOFRAME_UPSCALE).
    let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("blit-pipeline"),
        layout: Some(&blit_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some(upscale_filter().entry_point()),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview: None,
        cache: None,
    });
    let canvas_mip_pipeline = |label: &str, entry_point: &str, constants: &[(&str, f64)]| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&blit_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(entry_point),
                targets: &[Some(wgpu::ColorTargetState {
                    format: CANVAS_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants,
                    ..Default::default()
                },
            }),
            multiview: None,
            cache: None,
        })
    };
    let mip_pipeline = canvas_mip_pipeline("canvas-mip-pipeline", "fs_blit", &[]);
    // PHOTOFRAME_MIP_SHARPEN, as the shader's MIP_SHARPEN.
    let mip_sharpen_pipeline = canvas_mip_pipeline(
        "canvas-mip-sharpen-pipeline",
        "fs_mip_sharpen",
        &[("MIP_SHARPEN", f64::from(mip_sharpen()))],
    );
    // PHOTOFRAME_PROBE=single-sample (diagnostic): see the
    // shader.
    let probe_constants = probe_pipeline_constants(probes());
    let probe_constants = probe_constants.as_slice();
    // One pipeline per DrawKind (see DrawKind for why), all on the same
    // layout and vertex stage.
    let quad_pipelines = QuadPipelines {
        pipelines: DrawKind::ALL
            .iter()
            .map(|kind| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(kind.entry_point()),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs_main"),
                        buffers: &[],
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    },
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some(kind.entry_point()),
                        targets: &[Some(wgpu::ColorTargetState {
                            format,
                            // Opaque: the shader composites letterbox
                            // regions over U.background itself. Skipping
                            // blending avoids a destination read on every
                            // pixel (significant at 4K) and keeps the
                            // surface scanout-friendly.
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                        compilation_options: wgpu::PipelineCompilationOptions {
                            constants: probe_constants,
                            ..Default::default()
                        },
                    }),
                    multiview: None,
                    cache: None,
                })
            })
            .collect(),
    };
    let make_plane = |label: &str, width: u32, height: u32, data: &[u8]| -> TexturePlane {
        let w = width.max(1);
        let h = height.max(1);
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
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * w),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &img_bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        TexturePlane {
            bind,
            w,
            h,
            _gpu_bytes: None,
        }
    };

    let blank_plane = make_plane("blank-texture", 1, 1, &[0, 0, 0, 255]);

    let greeting = GreetingScene::new(GreetingScreen::new(
        &device,
        &queue,
        format,
        cfg.greeting_screen.screen(),
    ));

    let sleep = SleepScene::new(GreetingScreen::new(
        &device,
        &queue,
        format,
        cfg.sleep_screen.screen(),
    ));

    let caption_overlay = (cfg.showcase.enabled && cfg.showcase.caption_enabled())
        .then(|| scenes::CaptionOverlay::new(&device, &queue, format));
    // Styled like the greeting screen (its colours, font and frame).
    let wifi_overlay = scenes::wifi_overlay::WifiOverlayScreen::new(
        &device,
        &queue,
        format,
        cfg.greeting_screen.screen(),
    );
    let pass_timer = PassTimer::new(&device, &queue);
    debug!(
        context = reason,
        available = pass_timer.is_some(),
        "viewer_gpu_pass_timing"
    );
    let gpu = GpuCtx {
        device,
        queue,
        surface,
        config,
        limits,
        uniform_buf,
        petal_buf,
        uniform_bind,
        img_bind_layout,
        sampler,
        quad_pipelines,
        blank_plane,
        iris_cover_pipeline,
        iris_layer_pipeline,
        iris_cover: None,
        iris_layer: None,
        eink_mask_pipeline,
        eink_mask: None,
        blit_pipeline,
        mip_pipeline,
        mip_sharpen_pipeline,
        half_target: None,
        pass_timer,
    };
    Ok(GpuParts {
        gpu,
        greeting,
        sleep,
        caption_overlay,
        wifi_overlay,
    })
}

struct App {
    from_loader: PhotoReceiver,
    to_manager_displayed: Sender<Displayed>,
    cancel: CancellationToken,
    /// `None` until the Wayland window exists; set once at startup on KMS.
    output: Option<Output>,
    gpu: Option<GpuCtx>,
    surface_configured: bool,
    surface_gate: SurfaceReadyGate,
    pending_scene_enter: bool,
    mode: Option<ViewerMode>,
    preload_count: usize,
    oversample: f32,
    max_upscale_factor: f32,
    matting: MattingConfig,
    mat_pipeline: MattingPipeline,
    mat_inflight: usize,
    ready_results: VecDeque<MatResult>,
    deferred_images: VecDeque<QueuedImage>,
    clear_color: wgpu::Color,
    rng: rand::rngs::ThreadRng,
    full_config: Arc<crate::config::Configuration>,
    surface_timeout_streak: u32,
    // Last surface size that we considered "configured" and stabilized
    configured_surface_size: Option<(u32, u32)>,
    /// The installed GPU's texture limit (bounds the canvas); `None` without
    /// a GPU.
    max_texture_dimension: Option<u32>,
    /// Portrait pairing's pairable rule, `None` with pairing off.
    pairing_rules: Option<crate::tasks::manager::pairing::PairingRules>,
    /// Shared with the loader: the canvas size it should decode towards
    /// (published once the surface is configured; see `publish_canvas`).
    decode_scaling: super::loader::DecodeScaling,
    /// Caption overlay for showcase mode; `None` when showcase is disabled.
    caption_overlay: Option<scenes::CaptionOverlay>,
    /// Frame cadence of the transition currently being presented.
    transition_frame_metric: Option<TransitionFrameMetric>,
    /// DRM/KMS: the transition frame just submitted (its completion flag
    /// and pass times), taken by the KMS loop for `pacing::FlipAudit`.
    /// Never set on Wayland.
    paced_submission: Option<PacedSubmission>,
    /// The Wi-Fi overlay lease and panel power, and so the layer on screen
    /// (`presentation.rs`).
    presentation: presentation::Presentation,
    /// The layer `sync_presentation` last applied.
    layer: presentation::Layer,
    /// Renders the Wi-Fi overlay; `None` without a GPU.
    wifi_overlay: Option<scenes::wifi_overlay::WifiOverlayScreen>,
    /// Frames handed to `present()`. On KMS only "has Mesa presented on
    /// this display yet" is read from it (`presentation::power_commit`);
    /// panel power never depends on it (wgpu hides present errors).
    frames_presented: u64,
    /// Wayland: where the winit thread publishes `wake::Published` after each
    /// event batch for the driver (`wake::drive_forwarding`). `None` on KMS,
    /// which waits in its own loop, and in tests.
    wake_published: Option<tokio::sync::watch::Sender<wake::Published>>,
    /// Wayland: user events handled so far; published with the plan so the
    /// driver knows the plan includes everything it forwarded.
    events_handled: u64,
    /// Wayland: a window event changed what the control step sees (a resize,
    /// a resume); the step runs at the end of the batch.
    tick_wanted: bool,
}

impl App {
    fn mode(&self) -> &ViewerMode {
        self.mode.as_ref().expect("viewer mode not initialized")
    }

    fn mode_mut(&mut self) -> &mut ViewerMode {
        self.mode.as_mut().expect("viewer mode not initialized")
    }

    fn active_surface(&self) -> Option<SurfaceState> {
        if !self.surface_configured {
            return None;
        }
        self.gpu
            .as_ref()
            .map(|gpu| SurfaceState::from_config(&gpu.config, &gpu.limits))
    }

    fn update_surface_ready(&mut self, width: u32, height: u32, report: SurfaceReport) -> bool {
        if let Some(ready) = self.surface_gate.update(width, height, report) {
            let report_label = match report {
                SurfaceReport::InitialConfig => "initial_config",
                SurfaceReport::ConfigureEvent => "configure_event",
            };
            debug!(
                surface_ready = ready,
                width,
                height,
                report = report_label,
                "viewer_surface_ready_state_changed"
            );
        }
        let ready = self.surface_gate.is_ready();
        // Detect stabilized size changes and purge stale queues/results
        if ready {
            let size_changed = match self.configured_surface_size {
                Some((w, h)) => w != width || h != height,
                None => true,
            };
            if size_changed {
                self.configured_surface_size = Some((width, height));
                self.on_surface_size_settled();
            }
        }
        self.surface_configured = ready;
        if !ready {
            self.pending_scene_enter = true;
        } else {
            self.publish_canvas();
        }
        ready
    }

    /// Publish the canvas the matting worker renders into (the surface times
    /// the oversample factor, within the texture limit); the loader sizes
    /// JPEG decodes against it, and portrait pairing judges pairability on
    /// it. Called wherever the surface becomes ready: the Wayland configure
    /// handshake (`update_surface_ready`) and KMS (`configure_fixed_surface`).
    fn publish_canvas(&self) {
        if let (true, Some((width, height)), Some(max_dim)) = (
            self.surface_configured,
            self.configured_surface_size,
            self.max_texture_dimension,
        ) {
            let (canvas_w, canvas_h) =
                compute_canvas_size(width.max(1), height.max(1), self.oversample, max_dim);
            self.decode_scaling.publish_canvas(canvas_w, canvas_h);
        }
    }

    /// KMS: apply the surface configuration once and mark it ready. The mode
    /// never changes for this process (a change is a restart, see
    /// `viewer/kms.rs`), so there is no configure handshake to wait for.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))] // the KMS viewer
    fn configure_fixed_surface(&mut self, size: PhysicalSize<u32>) {
        if let Some(gpu) = self.gpu.as_ref() {
            gpu.surface.configure(&gpu.device, &gpu.config);
        }
        let _ = self.surface_gate.set_ready(true);
        self.surface_gate.awaiting = false;
        self.surface_configured = true;
        self.configured_surface_size = Some((size.width, size.height));
        self.publish_canvas();
    }

    fn on_surface_size_settled(&mut self) {
        // Drop any precomputed canvases for the previous size and flush pending queue.
        self.ready_results.clear();
        if let Some(mode) = self.mode.as_mut() {
            let wake = mode.wake_mut();
            wake.set_next(None);
            wake.pending_mut().clear();
            wake.set_transition_state(None);
        }
        // We cannot cancel inflight matting; mismatched results will be dropped on upload.
    }

    fn with_active_scene<R>(
        &mut self,
        f: impl FnOnce(&mut dyn Scene, SceneContext<'_>) -> R,
    ) -> Option<R> {
        if !self.surface_configured {
            debug!("viewer_scene_waiting_for_surface_configuration");
            return None;
        }
        let viewport = self.output.as_ref().map(Output::viewport);
        let mut request_redraw = self.redraw_requester();
        let config = Arc::clone(&self.full_config);
        let mut notify_displayed = {
            let sender = self.to_manager_displayed.clone();
            move |displayed: Displayed| {
                let _ = sender.try_send(displayed);
            }
        };

        let mut mode = self.mode.take()?;
        let surface = self.active_surface();
        let mut bridge = MattingBridge {
            preload_count: self.preload_count,
            mat_inflight: &mut self.mat_inflight,
            deferred_images: &mut self.deferred_images,
            ready_results: &mut self.ready_results,
            from_loader: &mut self.from_loader,
            surface,
            matting: &self.matting,
            oversample: self.oversample,
            max_upscale_factor: self.max_upscale_factor,
            bar_ratio: self.full_config.playlist.pairing.bar_ratio,
            pairing: self.pairing_rules,
            mat_pipeline: &self.mat_pipeline,
        };
        let mut enqueue_matting = move |wake: &mut scenes::WakeScene| {
            bridge.queue_for_wake(wake);
        };
        let rng = &mut self.rng;

        let result = mode.with_scene_mut(
            viewport,
            config,
            &mut request_redraw,
            rng,
            &mut notify_displayed,
            &mut enqueue_matting,
            f,
        );
        self.mode = Some(mode);
        result
    }

    fn mode_kind(&self) -> ViewerModeKind {
        self.mode().kind()
    }

    /// The closure scenes call through `SceneContext::request_redraw`.
    fn redraw_requester(&self) -> Box<dyn FnMut()> {
        match &self.output {
            Some(Output::Window(window)) => {
                let window = Arc::clone(window);
                Box::new(move || window.request_redraw())
            }
            Some(Output::Kms {
                redraw_requested, ..
            }) => {
                let flag = Arc::clone(redraw_requested);
                Box::new(move || flag.store(true, Ordering::Relaxed))
            }
            None => Box::new(|| {}),
        }
    }

    fn set_mode(&mut self, target: ViewerModeKind) -> ViewerModeKind {
        let current = self.mode_kind();
        if current == target {
            return current;
        }

        let mut mode = self.mode.take().expect("viewer mode not initialized");

        self.dispatch_scene_hook(&mut mode, SceneHook::Exit);

        mode = mode.into_kind(target);

        self.dispatch_scene_hook(&mut mode, SceneHook::Enter);

        self.mode = Some(mode);
        current
    }

    fn dispatch_scene_hook(&mut self, mode: &mut ViewerMode, hook: SceneHook) {
        if !self.surface_configured {
            if matches!(hook, SceneHook::Enter) {
                self.pending_scene_enter = true;
            }
            debug!(hook = ?hook, "viewer_scene_hook_deferred_until_surface_configured");
            return;
        }
        let viewport = self.output.as_ref().map(Output::viewport);
        let mut request_redraw = self.redraw_requester();
        let config = Arc::clone(&self.full_config);
        let mut notify_displayed = {
            let sender = self.to_manager_displayed.clone();
            move |displayed: Displayed| {
                let _ = sender.try_send(displayed);
            }
        };
        let surface = self.active_surface();
        let mut bridge = MattingBridge {
            preload_count: self.preload_count,
            mat_inflight: &mut self.mat_inflight,
            deferred_images: &mut self.deferred_images,
            ready_results: &mut self.ready_results,
            from_loader: &mut self.from_loader,
            surface,
            matting: &self.matting,
            oversample: self.oversample,
            max_upscale_factor: self.max_upscale_factor,
            bar_ratio: self.full_config.playlist.pairing.bar_ratio,
            pairing: self.pairing_rules,
            mat_pipeline: &self.mat_pipeline,
        };
        let mut enqueue_matting = move |wake: &mut scenes::WakeScene| {
            bridge.queue_for_wake(wake);
        };
        let rng = &mut self.rng;
        let _ = mode.with_scene_mut(
            viewport,
            config,
            &mut request_redraw,
            rng,
            &mut notify_displayed,
            &mut enqueue_matting,
            |scene, ctx| {
                Self::run_scene_hook(scene, hook, ctx);
            },
        );
        if matches!(hook, SceneHook::Enter) {
            self.pending_scene_enter = false;
        }
    }

    fn run_scene_hook(scene: &mut dyn Scene, hook: SceneHook, ctx: SceneContext<'_>) {
        match hook {
            SceneHook::Enter => scene.enter(ctx),
            SceneHook::Exit => scene.exit(ctx),
        }
    }

    /// Wayland: create the window and its surface, then the shared GPU
    /// context. A failure exits the event loop.
    fn ensure_gpu_ready(&mut self, event_loop: &ActiveEventLoop, reason: &'static str) -> bool {
        let Some(window) = self.ensure_window(event_loop) else {
            debug!(context = reason, "viewer_gpu_ensure_no_window");
            return false;
        };
        let window_log = format!("{reason}_window_ready");
        self.log_event_loop_state(&window_log);

        if self.gpu.is_some() {
            debug!(context = reason, "viewer_gpu_ensure_already_initialized");
            return true;
        }

        // Honor wgpu's env overrides (notably WGPU_BACKEND=gl|vulkan) so
        // the rendering backend can be switched per deployment without a
        // rebuild. On the Pi the GL/EGL window-buffer path is the one the
        // desktop stack exercises daily and may be eligible for compositor
        // direct scanout where the v3dv Vulkan swapchain path is not.
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::from_env_or_default());
        debug!(context = reason, "viewer_gpu_instance_ready");
        let surface = match instance.create_surface(window.clone()) {
            Ok(surface) => {
                debug!(context = reason, "viewer_gpu_surface_created");
                surface
            }
            Err(err) => {
                warn!(
                    context = reason,
                    error = %err,
                    "failed to create surface; exiting viewer"
                );
                event_loop.exit();
                return false;
            }
        };
        let size = window.inner_size();
        let parts = match build_gpu(
            &instance,
            surface,
            size,
            PresentDefault::Wayland,
            &self.full_config,
            reason,
        ) {
            Ok(parts) => parts,
            Err(err) => {
                warn!(context = reason, error = %err, "failed to initialize GPU; exiting viewer");
                event_loop.exit();
                return false;
            }
        };
        self.output = Some(Output::Window(window));
        self.install_gpu(parts, size, reason);
        true
    }

    /// Bookkeeping shared by both outputs once a GPU context exists: the
    /// overlay scenes are rebuilt for the new device, and the slideshow
    /// restarts from an empty stage. The surface is not yet "configured";
    /// Wayland waits for the compositor's configure events
    /// (SurfaceReadyGate), KMS marks it ready itself (fixed mode).
    fn install_gpu(&mut self, parts: GpuParts, size: PhysicalSize<u32>, reason: &'static str) {
        let GpuParts {
            gpu,
            greeting,
            sleep,
            caption_overlay,
            mut wifi_overlay,
        } = parts;
        self.caption_overlay = caption_overlay;
        // A lease held across a GPU rebuild is drawn by the new renderer.
        if let Some(content) = self.presentation.overlay() {
            wifi_overlay.set_content(content);
        }
        self.wifi_overlay = Some(wifi_overlay);
        if let Some(mode) = self.mode.as_mut() {
            mode.set_overlays(Some(greeting), Some(sleep));
        }
        self.max_texture_dimension = Some(gpu.limits.max_texture_dimension_2d);
        self.gpu = Some(gpu);
        self.surface_gate.reset();
        self.surface_gate.arm_for_initial_report();
        self.update_surface_ready(size.width, size.height, SurfaceReport::InitialConfig);
        self.pending_scene_enter = true;
        debug!(context = reason, "viewer_gpu_surface_configuration_pending");
        let wake = self.mode_mut().wake_mut();
        wake.set_current(None);
        wake.set_next(None);
        wake.set_transition_state(None);
        wake.set_displayed_at(None);
        let ready_log = format!("{reason}_gpu_ready");
        self.log_event_loop_state(&ready_log);
    }

    fn reset_for_resume(&mut self) {
        self.mode_mut().wake_mut().reset();
        self.ready_results.clear();
        self.deferred_images.clear();
        if self.mode_kind() == ViewerModeKind::Sleep {
            self.mode_mut().wake_mut().take_redraw_needed();
            let _ = self.with_active_scene(|scene, ctx| {
                scene.enter(ctx);
            });
        } else {
            self.enter_greeting();
        }
        self.mat_inflight = 0;
        self.surface_gate.reset();
        self.surface_configured = false;
        self.pending_scene_enter = true;
        self.surface_timeout_streak = 0;
        self.log_event_loop_state("reset_for_resume");
    }

    fn log_event_loop_state(&self, context: &str) {
        if !tracing::level_enabled!(Level::DEBUG) {
            return;
        }
        let now = Instant::now();
        let mode = self.mode();
        let wake = mode.wake();
        let current_path = wake.current().map(|img| img.path.display().to_string());
        let next_path = wake.next().map(|img| img.path.display().to_string());
        let displayed_elapsed_ms = wake
            .displayed_at()
            .map(|instant| now.saturating_duration_since(instant).as_millis() as u64);
        let transition_kind = wake.transition_state().map(TransitionState::kind);
        let transition_progress = wake.transition_state().map(TransitionState::progress);
        let greeting_pending = mode
            .greeting()
            .map(GreetingScene::needs_redraw)
            .unwrap_or(false);
        let sleep_pending = mode.sleep().map(SleepScene::needs_redraw).unwrap_or(false);

        debug!(
            context = context,
            viewer_mode = ?self.mode_kind(),
            has_output = self.output.is_some(),
            has_gpu = self.gpu.is_some(),
            surface_configured = self.surface_configured,
            pending_queue_len = wake.pending().len(),
            ready_results_len = self.ready_results.len(),
            deferred_queue_len = self.deferred_images.len(),
            mat_inflight = self.mat_inflight,
            preload_target = self.preload_count,
            pending_redraw = wake.needs_redraw(),
            greeting_overlay_pending = greeting_pending,
            sleep_overlay_pending = sleep_pending,
            displayed_elapsed_ms,
            current_path = current_path.as_deref(),
            next_path = next_path.as_deref(),
            transition_kind = ?transition_kind,
            transition_progress,
            has_next_stage = wake.next().is_some(),
            has_current_stage = wake.current().is_some(),
            "viewer_event_loop_state"
        );
    }

    fn ensure_window(&mut self, event_loop: &ActiveEventLoop) -> Option<Arc<Window>> {
        if let Some(window) = self.output.as_ref().and_then(Output::window).cloned() {
            self.log_event_loop_state("ensure_window_cached");
            return Some(window);
        }

        let attrs = with_photo_app_id(Window::default_attributes().with_title("Photo Frame"));
        let window = match event_loop.create_window(attrs) {
            Ok(window) => Arc::new(window),
            Err(err) => {
                warn!(error = %err, "failed to create viewer window; exiting");
                event_loop.exit();
                return None;
            }
        };
        window.set_decorations(false);
        let fullscreen_monitor = window
            .current_monitor()
            .or_else(|| event_loop.primary_monitor());
        window.set_fullscreen(Some(match fullscreen_monitor {
            Some(m) => Fullscreen::Borderless(Some(m)),
            None => Fullscreen::Borderless(None),
        }));
        window.set_cursor_visible(false);
        self.output = Some(Output::Window(window.clone()));
        self.log_event_loop_state("ensure_window_created");
        Some(window)
    }

    fn teardown_gpu(&mut self) {
        let current_kind = self.mode_kind();
        {
            let mode = self.mode_mut();
            mode.wake_mut().reset();
            if current_kind == ViewerModeKind::Wake {
                mode.wake_mut().mark_redraw_needed();
            }
            mode.set_overlays(None, None);
        }
        self.wifi_overlay = None;
        self.gpu = None;
        self.max_texture_dimension = None;
        self.surface_gate.reset();
        self.surface_configured = false;
        self.pending_scene_enter = true;
        self.log_event_loop_state("teardown_gpu");
    }

    fn handle_control_command(&mut self, cmd: ViewerCommand) {
        debug!(command = ?cmd, "viewer_control_command");
        match cmd {
            ViewerCommand::SetState(ControlViewerState::Awake) => self.enter_wake(),
            ViewerCommand::SetState(ControlViewerState::Asleep) => self.enter_sleep(),
            ViewerCommand::ToggleState => match self.mode_kind() {
                ViewerModeKind::Sleep => self.enter_wake(),
                ViewerModeKind::Greeting | ViewerModeKind::Wake => self.enter_sleep(),
            },
            ViewerCommand::DisplayPower { reply, .. } => {
                // The KMS loop handles this itself (`kms.rs`); the control
                // task answers it on Wayland, so this is only a backstop.
                let _ = reply.send(config_model::control::Reply::error(
                    config_model::control::ErrorKind::Unsupported,
                    "the compositor owns display power on this stack",
                ));
            }
            ViewerCommand::ShowWifiOverlay(content) => self.show_wifi_overlay(*content),
            ViewerCommand::HideWifiOverlay => self.hide_wifi_overlay(),
        }
        self.sync_presentation();
    }

    /// One control step: matting results in, uploads, matting work out, and
    /// the active scene's tick (dwell, transition start/finish, redraw
    /// requests). Run per event and, while a transition animates or the
    /// surface is settling, every 4 ms on Wayland (`wake::drive_forwarding`);
    /// once per loop iteration on KMS. Cancellation is the driver's job.
    fn process_tick(&mut self) {
        self.log_event_loop_state("process_tick_start");
        // Keep surface configuration and readiness in sync even if winit
        // doesn't deliver an explicit resize event (Wayland/Sway can do this).
        self.refresh_surface_config_from_window();
        if self.surface_configured && self.pending_scene_enter {
            self.pending_scene_enter = false;
            let _ = self.with_active_scene(|scene, ctx| {
                scene.enter(ctx);
            });
        }
        self.sync_presentation();
        self.rearm_wifi_overlay();

        self.drain_mat_results();

        let mode_kind = self.mode_kind();
        if !matches!(mode_kind, ViewerModeKind::Sleep)
            && let Some(mut mode) = self.mode.take()
        {
            {
                let wake = mode.wake_mut();
                self.upload_ready_results_for_wake(wake);
                self.queue_mat_tasks_for_wake(wake);
            }
            self.mode = Some(mode);
        }

        let _ = self.with_active_scene(|scene, ctx| {
            scene.process_tick(ctx);
        });
        self.log_event_loop_state("process_tick_end");
    }

    fn refresh_surface_config_from_window(&mut self) {
        let (Some(window), Some(gpu)) = (
            self.output.as_ref().and_then(Output::window),
            self.gpu.as_mut(),
        ) else {
            return;
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            gpu.config.width = 0;
            gpu.config.height = 0;
            self.surface_gate.arm_for_initial_report();
            let _ = self.update_surface_ready(0, 0, SurfaceReport::ConfigureEvent);
            return;
        }
        if gpu.config.width != size.width || gpu.config.height != size.height {
            gpu.config.width = size.width;
            gpu.config.height = size.height;
            gpu.surface.configure(&gpu.device, &gpu.config);
        }
        let _ = self.update_surface_ready(size.width, size.height, SurfaceReport::ConfigureEvent);
    }

    /// One matting result handed over by the Wayland driver (it owns the
    /// channel); same bookkeeping as `drain_mat_results`.
    fn accept_forwarded_mat_result(&mut self, result: MatResult) {
        accept_mat_result(&mut self.mat_inflight, &mut self.ready_results, result);
    }

    /// Wayland: end of an event batch; see `wake::end_of_batch`. Publishes
    /// the plan for the state the viewer is now in, without which the driver
    /// would keep waiting on the state before the batch.
    fn publish_wake_state(&mut self) {
        if self.wake_published.is_none() || self.cancel.is_cancelled() {
            return;
        }
        let step_wanted = std::mem::take(&mut self.tick_wanted);
        let handled = self.events_handled;
        let published = wake::end_of_batch(self, step_wanted, handled, Instant::now);
        if let Some(tx) = self.wake_published.as_ref() {
            let _ = tx.send(published);
        }
    }

    fn drain_mat_results(&mut self) {
        while let Some(result) = self.mat_pipeline.try_recv() {
            accept_mat_result(&mut self.mat_inflight, &mut self.ready_results, result);
        }
    }

    fn upload_ready_results_for_wake(&mut self, wake: &mut scenes::WakeScene) {
        if self.gpu.is_none() {
            return;
        }
        // Defer non-priority uploads while a transition is animating:
        // pushing a multi-megapixel canvas through the queue contends
        // with transition rendering and shows up as multi-frame hitches.
        // Dwell time is long, so deferred uploads resume on the first
        // tick after the transition completes.
        if wake.transition_state().is_some() {
            let mut deferred = VecDeque::with_capacity(self.ready_results.len());
            let mut priority_now = VecDeque::new();
            for result in self.ready_results.drain(..) {
                if result.is_priority() {
                    priority_now.push_back(result);
                } else {
                    deferred.push_back(result);
                }
            }
            // Priority results (operator-requested) still upload
            // immediately, mip generation included, so at most one frame
            // of the animation can slip; everything else waits it out.
            self.ready_results = priority_now;
            if !self.ready_results.is_empty() {
                self.upload_results_now(wake);
            }
            self.ready_results = deferred;
            return;
        }
        self.upload_results_now(wake);
    }

    fn upload_results_now(&mut self, wake: &mut scenes::WakeScene) {
        let gpu = self.gpu.as_ref().unwrap();
        let expected = compute_canvas_size(
            gpu.config.width,
            gpu.config.height,
            self.oversample,
            gpu.limits.max_texture_dimension_2d,
        );

        while let Some(result) = self.ready_results.pop_front() {
            let path = result.path.clone();
            // A screen is priority iff any member is (B2).
            let priority_paths = result.priority_paths();
            let priority = !priority_paths.is_empty();
            // Discard canvases that were prepared for a different surface size.
            if result.canvas.width != expected.0 || result.canvas.height != expected.1 {
                debug!(
                    path = %path.display(),
                    canvas_w = result.canvas.width,
                    canvas_h = result.canvas.height,
                    expected_w = expected.0,
                    expected_h = expected.1,
                    "dropping_mismatched_canvas_after_resize"
                );
                continue;
            }
            if let Some(new_tex) = upload_mat_result(gpu, result) {
                if priority {
                    photo_set::stage_priority(wake, new_tex, &priority_paths);
                } else {
                    wake.pending_mut().push_back(new_tex);
                }
                debug!(
                    path = %path.display(),
                    priority,
                    depth = wake.pending().len(),
                    "queued_image"
                );
            }
        }
    }

    fn queue_mat_tasks_for_wake(&mut self, wake: &mut scenes::WakeScene) {
        // `queue_for_wake` defers work until the surface reports a usable size.
        // Once `WindowEvent::Resized` updates `self.gpu.config`, the next tick will
        // call this helper again and retry automatically.
        let surface = self.active_surface();
        let mut bridge = MattingBridge {
            preload_count: self.preload_count,
            mat_inflight: &mut self.mat_inflight,
            deferred_images: &mut self.deferred_images,
            ready_results: &mut self.ready_results,
            from_loader: &mut self.from_loader,
            surface,
            matting: &self.matting,
            oversample: self.oversample,
            max_upscale_factor: self.max_upscale_factor,
            bar_ratio: self.full_config.playlist.pairing.bar_ratio,
            pairing: self.pairing_rules,
            mat_pipeline: &self.mat_pipeline,
        };
        bridge.queue_for_wake(wake);
    }

    fn enter_sleep(&mut self) {
        if self.mode_kind() == ViewerModeKind::Sleep {
            return;
        }
        info!("viewer: entering sleep");
        self.mode_mut().wake_mut().take_redraw_needed();
        self.set_mode(ViewerModeKind::Sleep);
        self.log_event_loop_state("enter_sleep");
    }

    fn enter_wake(&mut self) {
        if self.mode_kind() == ViewerModeKind::Wake {
            return;
        }
        info!("viewer: entering wake");
        self.set_mode(ViewerModeKind::Wake);
        self.log_event_loop_state("enter_wake");
    }

    fn enter_greeting(&mut self) {
        if self.mode_kind() != ViewerModeKind::Greeting {
            info!("viewer: entering greeting");
        }
        self.mode_mut().wake_mut().take_redraw_needed();
        self.set_mode(ViewerModeKind::Greeting);
        self.log_event_loop_state("enter_greeting");
    }

    /// Count one acquire timeout; a streak of
    /// `MAX_CONSECUTIVE_SURFACE_TIMEOUTS` is an error for the driver.
    fn note_surface_timeout(&mut self) -> Result<(), RedrawError> {
        self.surface_timeout_streak = self.surface_timeout_streak.saturating_add(1);
        if self.surface_timeout_streak < MAX_CONSECUTIVE_SURFACE_TIMEOUTS {
            return Ok(());
        }
        warn!(
            streak = self.surface_timeout_streak,
            "viewer_surface_timeout_streak"
        );
        self.surface_timeout_streak = 0;
        Err(RedrawError::TimeoutStreak)
    }

    /// Wayland: rebuild the window's GPU context after a timeout streak.
    fn recover_from_surface_timeouts(&mut self, event_loop: &ActiveEventLoop) {
        warn!("viewer_surface_timeout_recovering");
        self.teardown_gpu();
        self.reset_for_resume();
        if self.ensure_gpu_ready(event_loop, "surface_timeout_recover") {
            debug!("viewer_surface_timeout_recovered");
        }
    }

    fn record_frame_presented(&mut self) {
        self.surface_timeout_streak = 0;
        self.frames_presented = self.frames_presented.saturating_add(1);
    }

    /// Render and present one frame of the active scene. Shared by the
    /// winit handler (RedrawRequested) and the KMS loop; each decides what
    /// a `RedrawError` means for its output.
    ///
    /// `paced` is `None` on Wayland and for KMS frames outside a transition
    /// (they are drawn for "now"). For a KMS transition frame it carries the
    /// frame clock; the frame is drawn for `pacing::frame_time`: the vblank
    /// it is meant for, or the moment it is rendered (after the acquire) if
    /// that is later.
    fn redraw(&mut self, paced: Option<PacedFrame>) -> Result<(), RedrawError> {
        let mode_kind = self.mode_kind();
        let Some(_) = self.gpu.as_ref() else {
            return Ok(());
        };
        if !self.surface_configured {
            debug!("viewer_redraw_waiting_for_surface_configuration");
            return Ok(());
        }
        // What is on screen (presentation.rs): nothing while the panel is
        // dark, the Wi-Fi overlay while its lease is live (not in Sleep),
        // the active scene otherwise.
        let draw_wifi_overlay = match self.presentation.layer(mode_kind, Instant::now()) {
            presentation::Layer::Nothing => return Ok(()),
            presentation::Layer::Overlay => true,
            presentation::Layer::Scene(_) => false,
        };
        if draw_wifi_overlay {
            let (Some(output), Some(screen)) = (self.output.as_ref(), self.wifi_overlay.as_mut())
            else {
                return Ok(());
            };
            screen.resize(output.size(), output.scale_factor());
            if !screen.needs_redraw() {
                return Ok(());
            }
        }
        let overlay_pending = match mode_kind {
            ViewerModeKind::Greeting => self
                .mode()
                .greeting()
                .map(GreetingScene::needs_redraw)
                .unwrap_or(false),
            ViewerModeKind::Sleep => self
                .mode()
                .sleep()
                .map(SleepScene::needs_redraw)
                .unwrap_or(false),
            ViewerModeKind::Wake => false,
        };
        let (pending_redraw, queue_depth, has_transition) = {
            let wake = self.mode().wake();
            (
                wake.needs_redraw(),
                wake.pending().len(),
                wake.transition_state().is_some(),
            )
        };
        debug!(
            viewer_mode = ?mode_kind,
            overlay_pending,
            pending_redraw,
            queue_depth,
            ready_results = self.ready_results.len(),
            mat_inflight = self.mat_inflight,
            has_transition,
            "viewer_window_redraw_requested"
        );
        if !draw_wifi_overlay
            && matches!(mode_kind, ViewerModeKind::Greeting | ViewerModeKind::Sleep)
            && !overlay_pending
        {
            return Ok(());
        }
        if !draw_wifi_overlay
            && matches!(mode_kind, ViewerModeKind::Greeting | ViewerModeKind::Sleep)
        {
            let Some(output) = self.output.as_ref() else {
                return Ok(());
            };
            let size = output.size();
            let scale_factor = output.scale_factor();
            let layout_ready = {
                let Some(mode) = self.mode.as_mut() else {
                    return Ok(());
                };
                match mode_kind {
                    ViewerModeKind::Greeting => {
                        if let Some(screen) = mode.greeting_mut() {
                            let message = self
                                .full_config
                                .greeting_screen
                                .screen()
                                .message_or_default()
                                .into_owned();
                            screen.resize(size, scale_factor);
                            screen.set_message(message);
                            screen.ensure_layout_ready()
                        } else {
                            false
                        }
                    }
                    ViewerModeKind::Sleep => {
                        if let Some(screen) = mode.sleep_mut() {
                            let message = self
                                .full_config
                                .sleep_screen
                                .screen()
                                .message_or_default()
                                .into_owned();
                            screen.resize(size, scale_factor);
                            screen.set_message(message);
                            screen.ensure_layout_ready()
                        } else {
                            false
                        }
                    }
                    ViewerModeKind::Wake => true,
                }
            };
            if !layout_ready {
                debug!(
                    viewer_mode = ?mode_kind,
                    width = size.width,
                    height = size.height,
                    "viewer_overlay_waiting_for_layout"
                );
                return Ok(());
            }
        }

        let mut frame = None;
        let mut encountered_timeout = false;
        let mut need_scene_enter = false;
        for attempt in 0..2 {
            let surface_result = {
                let Some(gpu) = self.gpu.as_mut() else {
                    return Ok(());
                };
                gpu.surface.get_current_texture()
            };
            match surface_result {
                Ok(current) => {
                    frame = Some(current);
                    break;
                }
                Err(err) => match err {
                    wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated => {
                        // DRM/KMS (v1.2): the mode is fixed for the process
                        // lifetime, so a lost or outdated swapchain means the
                        // connector or mode changed under us. Not handled: exit and
                        // let systemd restart the unit, which re-enumerates.
                        let window = match self.output.as_ref() {
                            Some(Output::Window(window)) => Arc::clone(window),
                            _ => {
                                return Err(RedrawError::Fatal(format!(
                                    "kms_surface_lost: {err:?} (connector or mode changed?)"
                                )));
                            }
                        };
                        let size = window.inner_size();
                        warn!(
                            attempt = attempt,
                            error = ?err,
                            width = size.width,
                            height = size.height,
                            "viewer_surface_reconfigure"
                        );
                        if size.width == 0 || size.height == 0 {
                            if let Some(gpu) = self.gpu.as_mut() {
                                gpu.config.width = 0;
                                gpu.config.height = 0;
                            }
                            self.surface_gate.arm_for_initial_report();
                            self.update_surface_ready(
                                size.width,
                                size.height,
                                SurfaceReport::ConfigureEvent,
                            );
                            break;
                        }
                        if let Some(gpu) = self.gpu.as_mut() {
                            gpu.config.width = size.width;
                            gpu.config.height = size.height;
                            gpu.surface.configure(&gpu.device, &gpu.config);
                        } else {
                            return Ok(());
                        }
                        let ready = self.update_surface_ready(
                            size.width,
                            size.height,
                            SurfaceReport::ConfigureEvent,
                        );
                        if ready && self.pending_scene_enter {
                            need_scene_enter = true;
                        }
                        if !ready {
                            break;
                        }
                        if attempt == 0 {
                            continue;
                        } else {
                            break;
                        }
                    }
                    wgpu::SurfaceError::Timeout => {
                        warn!("viewer_surface_timeout");
                        encountered_timeout = true;
                        if attempt == 0 {
                            continue;
                        } else {
                            break;
                        }
                    }
                    wgpu::SurfaceError::OutOfMemory => {
                        return Err(RedrawError::Fatal("viewer_surface_out_of_memory".into()));
                    }
                    wgpu::SurfaceError::Other => {
                        // Wayland shrugs this off and retries on the next redraw;
                        // on KMS there is no compositor to recover the surface, so
                        // it is treated like a lost one.
                        if self.output.as_ref().is_some_and(Output::is_kms) {
                            return Err(RedrawError::Fatal(format!("kms_surface_error: {err:?}")));
                        }
                        warn!(?err, "viewer_surface_error");
                        return Ok(());
                    }
                },
            }
        }
        if need_scene_enter {
            self.pending_scene_enter = false;
            let _ = self.with_active_scene(|scene, ctx| {
                scene.enter(ctx);
            });
            if !self.surface_configured {
                return Ok(());
            }
        }
        let Some(frame) = frame else {
            if encountered_timeout {
                // KMS: the request that brought us here has been consumed and
                // no frame was presented. Nothing else re-arms it for a
                // resting photo (there is no tick), so the screen would keep
                // the previous frame until the next dwell; ask again. Wayland
                // keeps its old behaviour (no retry): there a retry blocks
                // the winit thread and a streak escalates to a GPU rebuild.
                if self.output.as_ref().is_some_and(Output::is_kms) {
                    (self.redraw_requester())();
                }
                return self.note_surface_timeout();
            }
            return Ok(());
        };
        let (Some(gpu), Some(mode)) = (self.gpu.as_mut(), self.mode.as_mut()) else {
            return Ok(());
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("draw-encoder"),
            });

        if draw_wifi_overlay {
            // Drawn once per change, like Greeting and Sleep (no
            // pre_present_notify, for the same reason as below).
            let Some(screen) = self.wifi_overlay.as_mut() else {
                return Ok(());
            };
            encoder.push_debug_group("wifi-overlay");
            let rendered = screen.render(&mut encoder, &view);
            encoder.pop_debug_group();
            if !rendered {
                debug!("wifi_overlay_render_deferred");
                return Ok(());
            }
            gpu.queue.submit(Some(encoder.finish()));
            frame.present();
            screen.after_submit();
            self.record_frame_presented();
            return Ok(());
        }

        // Greeting and Sleep draw once on demand and deliberately
        // do not call pre_present_notify: a frame callback is only
        // ever answered after a commit, and these scenes may redraw
        // (re-layout, resize) without one, so gating them would
        // risk waiting on a callback that never comes. The only
        // callback they can inherit is from a wake frame presented
        // just before a mode switch, which the compositor answers
        // on its next repaint.
        match mode_kind {
            ViewerModeKind::Sleep => {
                let Some(screen) = mode.sleep_mut() else {
                    return Ok(());
                };
                encoder.push_debug_group("sleep-banner");
                let rendered = screen.render(&mut encoder, &view);
                encoder.pop_debug_group();

                if !rendered {
                    debug!("sleep_banner_render_deferred");
                    return Ok(());
                }

                gpu.queue.submit(Some(encoder.finish()));
                frame.present();
                screen.after_submit();
                self.record_frame_presented();
            }
            ViewerModeKind::Greeting => {
                let Some(screen) = mode.greeting_mut() else {
                    return Ok(());
                };
                encoder.push_debug_group("greeting-banner");
                let rendered = screen.render(&mut encoder, &view);
                encoder.pop_debug_group();

                if !rendered {
                    debug!("greeting_banner_render_deferred");
                    return Ok(());
                }

                gpu.queue.submit(Some(encoder.finish()));
                frame.present();
                screen.after_submit();
                self.record_frame_presented();
            }
            ViewerModeKind::Wake => {
                let wake = mode.wake_mut();
                // After the acquire: a frame is never drawn for a time before
                // it is rendered.
                let frame_time = paced.map(|p| pacing::frame_time(p.index_time, Instant::now()));
                if let (Some(p), Some(frame_time), Some(state)) =
                    (paced, frame_time, wake.transition_state_mut())
                {
                    state.anchor_once(p.anchor);
                    state.note_rendered(state.progress_at(frame_time));
                }
                encoder.push_debug_group("wake-draw");
                // Created up front: later code holds immutable
                // borrows of gpu for the bind groups.
                if let Some(kind) = wake.transition_state().map(|s| s.kind()) {
                    if transition_scale() > 1 {
                        gpu.ensure_half_target();
                    }
                    if kind == TransitionKind::Iris {
                        gpu.ensure_iris_layer();
                    }
                }
                if let Some(ActiveTransition::EInk { stripe_count, .. }) =
                    wake.transition_state().map(|s| s.variant())
                {
                    gpu.ensure_eink_mask((*stripe_count).max(1));
                }
                let screen_w = gpu.config.width as f32;
                let screen_h = gpu.config.height as f32;
                let mut uniforms = TransitionUniforms {
                    screen_size: [screen_w, screen_h],
                    progress: 0.0,
                    lod: 0.0,
                    current_xform: draw_params::plane_xform([0.0; 4], [screen_w, screen_h]),
                    next_xform: draw_params::plane_xform([0.0; 4], [screen_w, screen_h]),
                    params0: [0.0; 4],
                    params1: [0.0; 4],
                    params3: [0.0; 4],
                    background: [
                        self.clear_color.r as f32,
                        self.clear_color.g as f32,
                        self.clear_color.b as f32,
                        1.0,
                    ],
                };
                let mut petals = <IrisPetals as bytemuck::Zeroable>::zeroed();
                let mut draw_kind = DrawKind::Rest;
                // Where the planes are drawn, in screen pixels; turned into
                // texture-coordinate maps once the kind has had its say
                // (push moves them).
                let mut current_dest = [0.0f32; 4];
                let mut next_dest = [0.0f32; 4];
                let screen = [screen_w, screen_h];
                let mut current_bind = &gpu.blank_plane.bind;
                let mut next_bind = &gpu.blank_plane.bind;
                let mut have_current = false;
                let mut have_next = false;

                if let Some(cur) = wake.current() {
                    let rect = compute_cover_rect(
                        cur.plane.w,
                        cur.plane.h,
                        gpu.config.width,
                        gpu.config.height,
                    );
                    current_dest = rect_to_uniform(rect);
                    current_bind = &cur.plane.bind;
                    have_current = true;
                }
                if let Some(next) = wake.next() {
                    let rect = compute_cover_rect(
                        next.plane.w,
                        next.plane.h,
                        gpu.config.width,
                        gpu.config.height,
                    );
                    next_dest = rect_to_uniform(rect);
                    next_bind = &next.plane.bind;
                    have_next = true;
                }

                let active_transition = wake.transition_state().map(|state| state.kind());
                let mut should_draw_quad = false;
                let debug_bezier = std::env::var("PHOTOFRAME_DEBUG_BEZIER")
                    .map(|s| matches!(s.as_str(), "1" | "true" | "yes" | "on"))
                    .unwrap_or(false);

                if debug_bezier {
                    // Draw current image and overlay a debug quadratic Bezier stroke.
                    draw_kind = DrawKind::DebugBezier;
                    // Control points in UV for a gentle arc across center
                    uniforms.params0 = [0.15, 0.60, 0.50, 0.25]; // P0.xy, P1.xy
                    uniforms.params1[0] = 0.85; // P2.x
                    uniforms.params1[1] = 0.60; // P2.y
                    uniforms.params1[2] = 4.0; // stroke width px
                    // Red stroke by default
                    uniforms.params3 = [1.0, 0.1, 0.1, 1.0];
                    should_draw_quad = have_current;
                } else if let Some(state) = wake.transition_state() {
                    should_draw_quad = have_current || have_next;
                    let base_progress = match frame_time {
                        Some(frame_time) => state.progress_at(frame_time),
                        None => state.progress(),
                    };
                    let eased_progress =
                        base_progress * base_progress * (3.0 - 2.0 * base_progress);
                    uniforms.progress = eased_progress;
                    draw_kind = DrawKind::for_transition(state.kind());
                    match state.variant() {
                        ActiveTransition::Fade { through_black } => {
                            uniforms.params0[0] = if *through_black { 1.0 } else { 0.0 };
                        }
                        ActiveTransition::Wipe { normal, softness } => {
                            let normal = *normal;
                            let (min_proj, inv_span) =
                                compute_wipe_span(normal, screen_w, screen_h);
                            (uniforms.params0, uniforms.params1) = draw_params::wipe(
                                normal,
                                min_proj,
                                inv_span,
                                *softness,
                                uniforms.progress,
                                screen,
                            );
                        }
                        ActiveTransition::Push { direction } => {
                            let direction = *direction;
                            let diag = (screen_w * screen_w + screen_h * screen_h).sqrt();
                            (current_dest, next_dest) = draw_params::push(
                                current_dest,
                                next_dest,
                                [direction[0] * diag, direction[1] * diag],
                                uniforms.progress,
                            );
                        }
                        ActiveTransition::EInk {
                            flash_count,
                            reveal_portion,
                            stripe_count,
                            flash_color,
                            noise_seed,
                        } => {
                            let noise_seed = *noise_seed;
                            let flash_color = *flash_color;
                            uniforms.params0[0] = (*flash_count).min(6) as f32;
                            uniforms.params0[1] = *reveal_portion;
                            uniforms.params0[2] = (*stripe_count).max(1) as f32;
                            uniforms.params0[3] = noise_seed[0];
                            uniforms.params1[0] = noise_seed[1];
                            uniforms.params1[1] = flash_color[0].clamp(0.0, 1.0);
                            uniforms.params1[2] = flash_color[1].clamp(0.0, 1.0);
                            uniforms.params1[3] = flash_color[2].clamp(0.0, 1.0);
                        }
                        ActiveTransition::Dissolve { softness, scale } => {
                            uniforms.params0[0] = *softness;
                            uniforms.params0[1] = *scale;
                        }
                        ActiveTransition::RadialWipe {
                            softness,
                            shape_is_diamond,
                            center,
                        } => {
                            (uniforms.params0, uniforms.params1) = draw_params::radial_wipe(
                                *center,
                                *softness,
                                *shape_is_diamond,
                                uniforms.progress,
                                screen,
                            );
                        }
                        ActiveTransition::VenetianBlinds {
                            stripe_count,
                            softness,
                            vertical,
                        } => {
                            (uniforms.params0, uniforms.params1) = draw_params::venetian_blinds(
                                *stripe_count,
                                *softness,
                                *vertical,
                                uniforms.progress,
                            );
                        }
                        ActiveTransition::CrossfadeZoom {
                            zoom,
                            current_zooms_in,
                            next_zooms_in,
                        } => {
                            (uniforms.params1, uniforms.params3) = draw_params::crossfade_zoom(
                                current_dest,
                                next_dest,
                                *zoom,
                                *current_zooms_in,
                                *next_zooms_in,
                                uniforms.progress,
                                screen,
                            );
                        }
                        ActiveTransition::Iris {
                            blades,
                            color,
                            petal_sheen,
                            overlap_shadow,
                            min_aperture,
                            swirl,
                        } => {
                            // Solve the petal kinematics here so the
                            // per-pixel shader loop is transcendental-free.
                            // Geometry derivation: r_in circumscribes the
                            // screen, petals are annular arcs (band r_in..
                            // 2*r_in) with semicircular end caps, pivoting
                            // by `psi` about the center of the trailing cap.
                            let n = (*blades).clamp(1, 16) as usize;
                            let t = uniforms.progress;
                            let x = if t < 0.5 { t * 2.0 } else { 2.0 - t * 2.0 };
                            let f = x * x * (3.0 - 2.0 * x);
                            let r_in =
                                1.02 * 0.5 * (screen_w * screen_w + screen_h * screen_h).sqrt();
                            let r_mid = 1.5 * r_in;
                            let e = f * r_in * (1.0 - min_aperture);
                            let psi = 2.0 * (e / (2.0 * r_mid)).clamp(0.0, 1.0).asin();
                            let spin = swirl * psi;
                            let sigma = std::f32::consts::TAU / n as f32 + IRIS_EXTRA_WIDTH_RAD;
                            // Photos crossfade behind the petals around full close.
                            let swap = ((t - 0.42) / 0.16).clamp(0.0, 1.0);
                            let swap = swap * swap * (3.0 - 2.0 * swap);
                            uniforms.params0 = [n as f32, *petal_sheen, *overlap_shadow, swap];
                            // Inscribed aperture radius: pixels closer to
                            // center than this are provably petal-free.
                            uniforms.params1 = [r_in - e, color[0], color[1], color[2]];
                            // The layer's scale (how far its texels reach),
                            // the ring geometry, and the main pass's edge
                            // anti-aliasing half-width: 0.75 of its pixel,
                            // in screen pixels (iris frames always draw
                            // into the intermediate when there is one).
                            let edge_band = 0.75 * transition_scale().max(1) as f32;
                            uniforms.params3 =
                                [iris_layer_scale() as f32, r_mid, 0.5 * r_in, edge_band];
                            let (s_psi, c_psi) = psi.sin_cos();
                            for i in 0..n {
                                let ai = std::f32::consts::TAU * i as f32 / n as f32 + spin;
                                let (s_ai, c_ai) = ai.sin_cos();
                                let piv = [r_mid * c_ai, r_mid * s_ai];
                                // Annulus center after swinging about the pivot.
                                let center = [
                                    piv[0] - (c_psi * piv[0] - s_psi * piv[1]),
                                    piv[1] - (s_psi * piv[0] + c_psi * piv[1]),
                                ];
                                let trail = ai + psi;
                                let tip = trail + sigma;
                                // Directional sheen: cos of the petal's facing
                                // angle against a fixed light, constant across a
                                // petal. Baked raw here; the shader scales it by
                                // petal_sheen and fades it near full closure.
                                let facing = (ai + 0.5 * sigma + psi - 2.3).cos();
                                petals.petals_a[i] = [center[0], center[1], tip.cos(), tip.sin()];
                                petals.petals_b[i] = [trail.cos(), trail.sin(), facing, 0.0];
                            }
                        }
                    }
                } else if have_current {
                    should_draw_quad = true;
                }

                // A transition's final frame (drawn for progress 1) is its
                // end state, the next photo at rest, drawn natively with
                // fs_rest: the switch from the half-res intermediate to
                // native resolution happens on exactly one frame, and that
                // frame costs a resting frame, not the kind's shader at 4K.
                // (Previously every frame past *eased* progress 0.99 was
                // native, which smoothstep easing makes the last ~6% of a
                // transition: e-ink missed a refresh on each of them.) On
                // full-screen canvases every kind's progress-1 image is the
                // next photo; dissolve and venetian blinds, whose soft edges
                // never quite finish at progress 1, now reach it one frame
                // earlier instead of on the first dwell frame.
                let final_frame = active_transition.is_some()
                    && !debug_bezier
                    && uniforms.progress >= 1.0
                    && have_next;
                if final_frame {
                    draw_kind = DrawKind::Rest;
                    current_dest = next_dest;
                    current_bind = next_bind;
                    next_dest = [0.0; 4];
                    next_bind = &gpu.blank_plane.bind;
                    should_draw_quad = true;
                }
                if draw_kind == DrawKind::Rest && have_next && !final_frame {
                    // fs_rest draws the current plane only; a next photo
                    // staged at rest still counts for coverage, as it always
                    // has: a fade at progress 0 is exactly that.
                    draw_kind = DrawKind::Fade;
                    uniforms.progress = 0.0;
                    uniforms.params0 = [0.0; 4];
                }
                uniforms.current_xform = draw_params::plane_xform(current_dest, screen);
                uniforms.next_xform = draw_params::plane_xform(next_dest, screen);

                let mut timestamps = None;
                if should_draw_quad {
                    let iris_active = draw_kind == DrawKind::Iris;
                    // Transitions render into the reduced-resolution
                    // intermediate and get upsampled; resting photos and a
                    // transition's final frame render natively.
                    let transition_frame =
                        active_transition.is_some() && !debug_bezier && !final_frame;
                    let half_target = if transition_frame && transition_scale() > 1 {
                        gpu.half_target.as_ref()
                    } else {
                        None
                    };
                    // Into the intermediate the planes are read from the
                    // mip level that matches its resolution.
                    // Push moves its planes off the canvas grid, where the
                    // 2:1 level softens fine detail; it reads one real plane
                    // per pixel and holds 60 at level 0, so it stays exact
                    // there (Pi, 94094b0).
                    uniforms.lod = canvas_lod(half_target.is_some() && draw_kind != DrawKind::Push);
                    gpu.queue
                        .write_buffer(&gpu.uniform_buf, 0, bytemuck::bytes_of(&uniforms));
                    if iris_active {
                        gpu.queue
                            .write_buffer(&gpu.petal_buf, 0, bytemuck::bytes_of(&petals));
                    }
                    // Audited KMS frames time their passes on the GPU.
                    let timer = gpu
                        .pass_timer
                        .as_ref()
                        .filter(|_| paced.is_some_and(|p| p.time_passes))
                        .map(PassTimer::frame);
                    // A pass that renders a layer the main pass reads at
                    // group 3, timed with the layer pair: `begin` and `end`
                    // say whether it opens and/or closes it.
                    let layer_pass = |encoder: &mut wgpu::CommandEncoder,
                                      label: &'static str,
                                      target: &wgpu::TextureView,
                                      pipeline: &wgpu::RenderPipeline,
                                      input: Option<&wgpu::BindGroup>,
                                      begin: bool,
                                      end: bool| {
                        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some(label),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: target,
                                depth_slice: None,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                    store: wgpu::StoreOp::Store,
                                },
                            })],
                            depth_stencil_attachment: None,
                            occlusion_query_set: None,
                            timestamp_writes: timer
                                .as_ref()
                                .map(|t| t.writes(TimedPass::Layer, begin, end)),
                        });
                        pass.set_pipeline(pipeline);
                        pass.set_bind_group(0, &gpu.uniform_bind, &[]);
                        if let Some(input) = input {
                            pass.set_bind_group(1, input, &[]);
                        }
                        pass.draw(0..3, 0..1);
                    };
                    // Render the petals into the reduced-resolution
                    // layer first, in two passes (cover, then shading);
                    // the main pass composites it.
                    if iris_active
                        && let (Some(cover), Some(layer)) =
                            (gpu.iris_cover.as_ref(), gpu.iris_layer.as_ref())
                    {
                        layer_pass(
                            &mut encoder,
                            "iris-cover-pass",
                            &cover.view,
                            &gpu.iris_cover_pipeline,
                            None,
                            true,
                            false,
                        );
                        layer_pass(
                            &mut encoder,
                            "iris-layer-pass",
                            &layer.view,
                            &gpu.iris_layer_pipeline,
                            Some(&cover.bind),
                            false,
                            true,
                        );
                    }
                    // E-ink's per-cell reveal mask.
                    let eink_mask = gpu
                        .eink_mask
                        .as_ref()
                        .filter(|_| draw_kind == DrawKind::EInk);
                    if let Some(mask) = eink_mask {
                        layer_pass(
                            &mut encoder,
                            "eink-mask-pass",
                            &mask.view,
                            &gpu.eink_mask_pipeline,
                            None,
                            true,
                            true,
                        );
                    }
                    let petal_bind = match (gpu.iris_layer.as_ref(), eink_mask) {
                        (Some(layer), _) if iris_active => &layer.bind,
                        (_, Some(mask)) => &mask.bind,
                        _ => &gpu.blank_plane.bind,
                    };
                    {
                        let target_view = half_target.map(|t| &t.view).unwrap_or(&view);
                        let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("draw-pass"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: target_view,
                                depth_slice: None,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(self.clear_color),
                                    store: wgpu::StoreOp::Store,
                                },
                            })],
                            depth_stencil_attachment: None,
                            occlusion_query_set: None,
                            timestamp_writes: timer
                                .as_ref()
                                .map(|t| t.writes(TimedPass::Main, true, true)),
                        });
                        rpass.set_pipeline(gpu.quad_pipelines.get(draw_kind));
                        rpass.set_bind_group(0, &gpu.uniform_bind, &[]);
                        rpass.set_bind_group(1, current_bind, &[]);
                        rpass.set_bind_group(2, next_bind, &[]);
                        rpass.set_bind_group(3, petal_bind, &[]);
                        rpass.draw(0..3, 0..1);
                    }
                    if let Some(target) = half_target {
                        let mut bpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("upscale-pass"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: &view,
                                depth_slice: None,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(self.clear_color),
                                    store: wgpu::StoreOp::Store,
                                },
                            })],
                            depth_stencil_attachment: None,
                            occlusion_query_set: None,
                            timestamp_writes: timer
                                .as_ref()
                                .map(|t| t.writes(TimedPass::Upscale, true, true)),
                        });
                        bpass.set_pipeline(&gpu.blit_pipeline);
                        bpass.set_bind_group(0, &gpu.uniform_bind, &[]);
                        bpass.set_bind_group(1, &target.bind, &[]);
                        bpass.draw(0..3, 0..1);
                    }
                    timestamps = timer.map(|t| {
                        let readback = t.resolve(&gpu.device, &mut encoder);
                        (t, readback)
                    });
                }
                encoder.pop_debug_group();

                if let Some(cap) = self.caption_overlay.as_mut() {
                    // Show the most recent transition (kept after it
                    // finishes) and the current photo's mat.
                    let transition_kind = wake.last_transition_kind();
                    let mat_kind = wake.current().and_then(|img| img.mat_kind);
                    let pair = wake.current().is_some_and(|img| img.partner.is_some());
                    let text = scenes::showcase_caption(transition_kind, mat_kind, pair);
                    cap.set_text(text);
                    let surface_size =
                        winit::dpi::PhysicalSize::new(gpu.config.width, gpu.config.height);
                    cap.resize(surface_size);
                    cap.render(&mut encoder, &view);
                }

                gpu.queue.submit(Some(encoder.finish()));
                // KMS transition frames (the only paced ones): learn when the
                // GPU finished, for the late-frame audit, and what its passes
                // took. The map is requested first, so the poll that reports
                // the frame done has its timestamps too.
                if paced.is_some() {
                    let passes = timestamps.map(|(timer, readback)| timer.read(readback));
                    let done = Arc::new(AtomicBool::new(false));
                    let flag = Arc::clone(&done);
                    gpu.queue
                        .on_submitted_work_done(move || flag.store(true, Ordering::Release));
                    self.paced_submission = Some(PacedSubmission { done, passes });
                }
                // Wayland: frame-callback pacing; KMS: nothing (the FIFO
                // acquire bounds how far ahead the loop renders). See
                // Output::before_present.
                if let Some(output) = self.output.as_ref() {
                    output.before_present();
                }
                frame.present();
                if let Some(cap) = self.caption_overlay.as_mut() {
                    cap.after_submit();
                }
                wake.after_present();
                self.record_frame_presented();
                self.note_transition_frame(
                    active_transition,
                    frame_time.unwrap_or_else(Instant::now),
                );
            }
        }
        Ok(())
    }

    /// Track per-frame cadence while a transition plays; log a summary
    /// once it ends (or hands over to a different transition kind).
    ///
    /// `now` is the frame's clock: wall time at present on Wayland, the
    /// pacing vblank on DRM/KMS, so the metric reports the display's
    /// cadence rather than how quickly frames were queued.
    fn note_transition_frame(&mut self, active: Option<TransitionKind>, now: Instant) {
        let metrics = self.full_config.metrics;
        let kms = matches!(self.output, Some(Output::Kms { .. }));
        match (self.transition_frame_metric.take(), active) {
            (None, Some(kind)) => {
                self.transition_frame_metric = Some(TransitionFrameMetric::begin(kind, now, kms));
            }
            (Some(mut stats), Some(kind)) if stats.kind == kind => {
                stats.record(now);
                self.transition_frame_metric = Some(stats);
            }
            (Some(stats), Some(kind)) => {
                stats.log(metrics);
                self.transition_frame_metric = Some(TransitionFrameMetric::begin(kind, now, kms));
            }
            (Some(stats), None) => stats.log(metrics),
            (None, None) => {}
        }
    }
}

impl App {
    /// The viewer state shared by both outputs; no output or GPU yet.
    fn new(
        from_loader: PhotoReceiver,
        to_manager_displayed: Sender<Displayed>,
        cancel: CancellationToken,
        cfg: Arc<crate::config::Configuration>,
        decode_scaling: super::loader::DecodeScaling,
    ) -> Self {
        let worker_count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .max(1);
        let pipeline_capacity = cfg.viewer_preload_count.max(2);
        let mat_pipeline = MattingPipeline::new(worker_count, pipeline_capacity);
        let clear_color = cfg
            .matting
            .primary_option()
            .and_then(MattingOptions::fixed_color)
            .map(|color| wgpu::Color {
                // Config colors are sRGB; clear values for sRGB render targets
                // (and the in-shader background composite) are linear.
                r: srgb_to_linear((color[0] as f32) / 255.0) as f64,
                g: srgb_to_linear((color[1] as f32) / 255.0) as f64,
                b: srgb_to_linear((color[2] as f32) / 255.0) as f64,
                a: 1.0,
            })
            .unwrap_or(wgpu::Color::BLACK);
        let pairing_rules = crate::tasks::manager::pairing::PairingRules::from_config(&cfg);
        let initial_wake = scenes::WakeScene::new(
            cfg.global_photo_settings.dwell_ms,
            scenes::pair_dwell_ms(
                cfg.global_photo_settings.dwell_ms,
                cfg.playlist.pairing.dwell_multiplier,
            ),
            cfg.transition.clone(),
        );
        App {
            from_loader,
            to_manager_displayed,
            cancel,
            output: None,
            gpu: None,
            surface_configured: false,
            surface_gate: SurfaceReadyGate::default(),
            pending_scene_enter: true,
            mode: Some(ViewerMode::new(ViewerModeKind::Greeting, initial_wake)),
            preload_count: cfg.viewer_preload_count,
            oversample: cfg.global_photo_settings.oversample,
            max_upscale_factor: cfg.global_photo_settings.max_upscale_factor,
            matting: cfg.matting.clone(),
            mat_pipeline,
            mat_inflight: 0,
            ready_results: VecDeque::new(),
            deferred_images: VecDeque::new(),
            clear_color,
            rng: rand::rng(),
            full_config: cfg,
            surface_timeout_streak: 0,
            configured_surface_size: None,
            max_texture_dimension: None,
            pairing_rules,
            decode_scaling,
            caption_overlay: None,
            transition_frame_metric: None,
            paced_submission: None,
            presentation: presentation::Presentation::default(),
            layer: presentation::Layer::Scene(ViewerModeKind::Greeting),
            wifi_overlay: None,
            frames_presented: 0,
            wake_published: None,
            events_handled: 0,
            tick_wanted: false,
        }
    }

    /// Apply the layer the mode, the overlay lease and panel power select
    /// (`presentation.rs`): drop a lapsed lease, pause or resume the
    /// slideshow, and ask for a frame when what is on screen changes. Run
    /// after every control command and at the start of every control step.
    fn sync_presentation(&mut self) {
        let now = Instant::now();
        if self.presentation.expire_overlay(now) {
            info!("wifi_overlay_expired: no refresh within its lease; hiding it");
            if let Some(screen) = self.wifi_overlay.as_mut() {
                screen.clear();
            }
        }
        let layer = self.presentation.layer(self.mode_kind(), now);
        let visible = layer == presentation::Layer::Scene(ViewerModeKind::Wake);
        match self.mode_mut().wake_mut().set_visible(visible) {
            scenes::Visibility::Hidden { rewound } => {
                if rewound {
                    // The rewound transition never finished on screen.
                    self.transition_frame_metric = None;
                }
                info!(rewound, "slideshow_paused: not on screen");
            }
            scenes::Visibility::Revealed => info!("slideshow_resumed: dwell restarted"),
            scenes::Visibility::Unchanged => {}
        }
        if layer == self.layer {
            return;
        }
        debug!(previous = ?self.layer, ?layer, "viewer_layer_changed");
        self.layer = layer;
        match layer {
            presentation::Layer::Scene(_) => {
                let _ = self.with_active_scene(|scene, ctx| scene.handle_visibility(ctx, true));
            }
            presentation::Layer::Overlay => {
                if let Some(screen) = self.wifi_overlay.as_mut() {
                    screen.mark_redraw_needed();
                }
                (self.redraw_requester())();
            }
            presentation::Layer::Nothing => {}
        }
    }

    /// Wayland: an overlay frame that was not presented (an acquire
    /// timeout, a deferred render) is asked for again on the next control
    /// step, which the wake plan schedules while the overlay owes a frame
    /// (`presentation::layer_owes_frame`). Nothing else would: the covered
    /// slideshow requests no frames, and lease refreshes do not redraw. KMS re-arms its own request after
    /// an acquire timeout, and re-arming there would spin the loop on a
    /// render that cannot succeed.
    fn rearm_wifi_overlay(&self) {
        if self.layer == presentation::Layer::Overlay
            && let Some(Output::Window(window)) = self.output.as_ref()
            && self.wifi_overlay.as_ref().is_some_and(|s| s.needs_redraw())
        {
            window.request_redraw();
        }
    }

    /// `wifi-overlay-show`: set or extend the lease; new content is
    /// re-rendered, a refresh of the same content is not.
    fn show_wifi_overlay(&mut self, content: config_model::control::WifiOverlay) {
        let now = Instant::now();
        match self.presentation.show_overlay(content, now) {
            presentation::OverlayShown::Content => {
                let content = self.presentation.overlay().expect("just shown");
                info!(
                    ssid = %content.ssid,
                    lease_s = content.ttl().as_secs(),
                    "wifi_overlay_shown"
                );
                if let Some(screen) = self.wifi_overlay.as_mut() {
                    screen.set_content(content);
                }
                if self.layer == presentation::Layer::Overlay {
                    (self.redraw_requester())();
                }
            }
            presentation::OverlayShown::Refreshed => debug!("wifi_overlay_refreshed"),
        }
    }

    fn hide_wifi_overlay(&mut self) {
        if self.presentation.hide_overlay() {
            info!("wifi_overlay_hidden");
            if let Some(screen) = self.wifi_overlay.as_mut() {
                screen.clear();
            }
        }
    }

    /// The state `wake::plan_wake` decides from. See `wake.rs` for the
    /// model; the DRM/KMS loop and the Wayland driver both wait on it.
    fn wake_plan(&mut self, now: Instant) -> wake::WakePlan {
        let transition_just_finished = self.mode_mut().wake_mut().take_just_finalized();
        let kind = self.mode_kind();
        let wake = self.mode().wake();
        let redraw_requested = match &self.output {
            Some(Output::Kms {
                redraw_requested, ..
            }) => redraw_requested.load(Ordering::Relaxed),
            // winit owns the request on Wayland.
            _ => false,
        };
        let on_kms = matches!(self.output, Some(Output::Kms { .. }));
        let mode = self.mode();
        // Wayland retries a frame the layer on screen still owes (T5); under
        // the Wi-Fi overlay that is the overlay's, not the covered scene's.
        let overlay_pending = !on_kms
            && presentation::layer_owes_frame(
                self.layer,
                |kind| match kind {
                    ViewerModeKind::Greeting => mode.greeting().is_some_and(|g| g.needs_redraw()),
                    ViewerModeKind::Sleep => mode.sleep().is_some_and(|s| s.needs_redraw()),
                    ViewerModeKind::Wake => false,
                },
                self.wifi_overlay.as_ref().is_some_and(|s| s.needs_redraw()),
            );
        wake::plan_wake(
            wake::WakeInputs {
                // Paused while covered or dark (`sync_presentation`).
                slideshow_active: kind == ViewerModeKind::Wake && wake.visible(),
                sleeping: kind == ViewerModeKind::Sleep,
                // A dark panel presents nothing; the request stays armed for
                // the first frame after the wake.
                redraw_requested: redraw_requested && self.presentation.power().may_present(),
                transition_active: wake.transition_state().is_some(),
                transition_just_finished,
                displayed_at: wake.displayed_at(),
                dwell: wake.dwell(),
                surface_ready: self.surface_configured,
                pending: wake.pending().len(),
                in_flight: self.mat_inflight,
                // Counted in photos (`wake::screen_fits`).
                deferred: self.deferred_images.iter().map(QueuedImage::photos).sum(),
                deferred_head: self.deferred_images.front().map_or(0, QueuedImage::photos),
                largest_screen: largest_screen(self.pairing_rules.is_some()),
                preload: self.preload_count,
                overlay_expires_at: self.presentation.overlay_expires_at(),
                surface_poll: (!on_kms).then_some(SURFACE_POLL_INTERVAL),
                overlay_pending,
                // `maybe_start_transition`'s own condition.
                photo_staged: wake.current().is_some()
                    && (wake.next().is_some() || !wake.pending().is_empty()),
                lead_in: if on_kms {
                    TRANSITION_LEAD_IN
                } else {
                    Duration::ZERO
                },
                // Two refreshes: the most FIFO queues ahead of the panel.
                lead_in_drain: match &self.output {
                    Some(Output::Kms { refresh_period, .. }) => *refresh_period * 2,
                    _ => Duration::ZERO,
                },
            },
            now,
        )
    }
}

/// Window events after which queued work may have become runnable (a surface
/// that is now usable, a window that is visible again): the control step runs
/// at the end of the batch. Redraws do not.
fn window_event_wants_step(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::Resized(_)
            | WindowEvent::ScaleFactorChanged { .. }
            | WindowEvent::Occluded(false)
    )
}

impl wake::BatchHost for App {
    fn step(&mut self) {
        self.process_tick();
    }

    fn plan(&mut self, now: Instant) -> wake::WakePlan {
        self.wake_plan(now)
    }
}

impl ApplicationHandler<ViewerEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        debug!("viewer_app_resumed");
        self.reset_for_resume();
        self.log_event_loop_state("resumed_post_reset");

        let _ = self.ensure_gpu_ready(event_loop, "resumed");
        self.tick_wanted = true;
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        debug!("viewer_app_suspended");
        self.teardown_gpu();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.output.as_ref().and_then(Output::window).cloned() else {
            return;
        };
        if window.id() != window_id {
            return;
        }
        self.log_event_loop_state("window_event_pre");
        if window_event_wants_step(&event) {
            self.tick_wanted = true;
        }
        match event {
            WindowEvent::CloseRequested => {
                debug!("viewer_window_close_requested");
                event_loop.exit();
            }
            WindowEvent::Resized(new_size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    debug!(
                        width = new_size.width,
                        height = new_size.height,
                        "viewer_window_resized"
                    );
                    if new_size.width == 0 || new_size.height == 0 {
                        gpu.config.width = 0;
                        gpu.config.height = 0;
                        self.surface_gate.arm_for_initial_report();
                        self.update_surface_ready(
                            new_size.width,
                            new_size.height,
                            SurfaceReport::ConfigureEvent,
                        );
                        debug!("viewer_surface_configuration_waiting_for_non_zero_size");
                        return;
                    }
                    gpu.config.width = new_size.width;
                    gpu.config.height = new_size.height;
                    gpu.surface.configure(&gpu.device, &gpu.config);
                    let ready = self.update_surface_ready(
                        new_size.width,
                        new_size.height,
                        SurfaceReport::ConfigureEvent,
                    );
                    if ready && self.pending_scene_enter {
                        self.pending_scene_enter = false;
                        let _ = self.with_active_scene(|scene, ctx| {
                            scene.enter(ctx);
                        });
                    }
                    if ready {
                        if let Some(cap) = self.caption_overlay.as_mut() {
                            cap.resize(new_size);
                        }
                        let scale_factor = window.scale_factor();
                        let _ = self.with_active_scene(|scene, ctx| {
                            scene.handle_resize(ctx, new_size, scale_factor);
                        });
                    } else {
                        debug!(
                            width = new_size.width,
                            height = new_size.height,
                            "viewer_surface_configuration_waiting_for_expectation"
                        );
                    }
                } else {
                    self.surface_gate.reset();
                    self.surface_configured = false;
                    self.pending_scene_enter = true;
                }
            }
            WindowEvent::ScaleFactorChanged {
                mut inner_size_writer,
                ..
            } => {
                if let Some(gpu) = self.gpu.as_mut() {
                    let scale_factor = window.scale_factor();
                    let size = window.inner_size();
                    debug!(
                        scale_factor = scale_factor,
                        width = size.width,
                        height = size.height,
                        "viewer_window_scale_factor_changed"
                    );
                    let _ = inner_size_writer.request_inner_size(size);
                    if size.width == 0 || size.height == 0 {
                        gpu.config.width = 0;
                        gpu.config.height = 0;
                        self.surface_gate.arm_for_initial_report();
                        self.update_surface_ready(
                            size.width,
                            size.height,
                            SurfaceReport::ConfigureEvent,
                        );
                        debug!("viewer_surface_configuration_waiting_for_non_zero_size");
                        return;
                    }
                    gpu.config.width = size.width;
                    gpu.config.height = size.height;
                    gpu.surface.configure(&gpu.device, &gpu.config);
                    let ready = self.update_surface_ready(
                        size.width,
                        size.height,
                        SurfaceReport::ConfigureEvent,
                    );
                    if ready && self.pending_scene_enter {
                        self.pending_scene_enter = false;
                        let _ = self.with_active_scene(|scene, ctx| {
                            scene.enter(ctx);
                        });
                    }
                    if ready {
                        let _ = self.with_active_scene(|scene, ctx| {
                            scene.handle_scale_factor_changed(ctx, size, scale_factor);
                        });
                    } else {
                        debug!(
                            width = size.width,
                            height = size.height,
                            "viewer_surface_configuration_waiting_for_expectation"
                        );
                    }
                }
            }
            WindowEvent::Occluded(false) => {
                debug!("viewer_window_occluded_false");
                let _ = self.with_active_scene(|scene, ctx| {
                    scene.handle_visibility(ctx, true);
                });
            }
            WindowEvent::Occluded(true) => {
                debug!("viewer_window_occluded_true");
            }
            WindowEvent::RedrawRequested => match self.redraw(None) {
                Ok(()) => {}
                Err(RedrawError::TimeoutStreak) => self.recover_from_surface_timeouts(event_loop),
                Err(RedrawError::Fatal(reason)) => {
                    warn!(%reason, "viewer_redraw_failed; exiting viewer");
                    event_loop.exit();
                }
            },

            other => {
                debug!(event = ?other, "viewer_window_event_other");
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        self.log_event_loop_state("about_to_wait");
        let mode_kind = self.mode_kind();
        debug!(viewer_mode = ?mode_kind, "viewer_about_to_wait");
        let _ = self.with_active_scene(|scene, ctx| {
            scene.about_to_wait(ctx);
        });
        self.publish_wake_state();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: ViewerEvent) {
        self.events_handled = self.events_handled.wrapping_add(1);
        match event {
            ViewerEvent::Tick => {
                debug!("viewer_user_event_tick");
                if self.cancel.is_cancelled() {
                    event_loop.exit();
                    return;
                }
                self.process_tick();
            }
            ViewerEvent::Command(cmd) => {
                debug!(command = ?cmd, "viewer_user_event_command");
                self.handle_control_command(cmd);
                if self.cancel.is_cancelled() {
                    event_loop.exit();
                    return;
                }
                self.process_tick();
            }
            ViewerEvent::MatResult(result) => {
                debug!(path = %result.path.display(), "viewer_user_event_mat_result");
                self.accept_forwarded_mat_result(result);
                if self.cancel.is_cancelled() {
                    event_loop.exit();
                    return;
                }
                self.process_tick();
            }
            ViewerEvent::Photo(PhotoLoaded {
                prepared,
                priority,
                partner,
            }) => {
                debug!(path = %prepared.path.display(), "viewer_user_event_photo");
                // queue_for_wake (the step below) submits deferred photos
                // before reading the loader again, which on Wayland is the
                // driver's alone.
                self.deferred_images.push_back(QueuedImage {
                    image: prepared,
                    priority,
                    partner,
                });
                if self.cancel.is_cancelled() {
                    event_loop.exit();
                    return;
                }
                self.process_tick();
            }
            ViewerEvent::Cancelled => {
                debug!("viewer_user_event_cancelled");
                event_loop.exit();
            }
        }
    }
}

/// Which output the viewer presents to, chosen at launch by
/// `PHOTOFRAME_SURFACE` (`wayland` | `drm`; unset means `wayland`). The
/// systemd unit `photoframe-kms.service` sets `drm`; the greetd/sway kiosk
/// session leaves it unset. There is deliberately no config.yaml key: the
/// surface is a property of how the process is launched, and a second
/// source of truth could contradict the installed stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewerSurface {
    /// A fullscreen winit window under the Wayland compositor.
    Wayland,
    /// Direct DRM/KMS scanout with no compositor (Linux only).
    Drm,
}

impl ViewerSurface {
    pub const ENV: &'static str = "PHOTOFRAME_SURFACE";

    pub fn from_env() -> anyhow::Result<Self> {
        Self::parse(std::env::var(Self::ENV).ok().as_deref())
    }

    /// Unset or empty means Wayland. Anything unrecognised is an error
    /// rather than a silent Wayland fallback: under the KMS unit that would
    /// fail later and far less clearly (no compositor to connect to).
    pub fn parse(value: Option<&str>) -> anyhow::Result<Self> {
        match value.map(str::trim) {
            None | Some("") | Some("wayland") => Ok(Self::Wayland),
            Some("drm") => Ok(Self::Drm),
            Some(other) => Err(anyhow::anyhow!(
                "{}={other:?} is not a surface kind; use wayland or drm",
                Self::ENV
            )),
        }
    }
}

/// Whether the viewer can render this overlay: the protocol's limits, and
/// both QR payloads encode.
pub fn validate_wifi_overlay(overlay: &config_model::control::WifiOverlay) -> Result<(), String> {
    overlay.validate()?;
    for (name, payload) in [
        ("join-qr", &overlay.join_qr),
        ("portal-url", &overlay.portal_url),
    ] {
        scenes::wifi_overlay::qr_modules(payload).map_err(|err| format!("{name}: {err}"))?;
    }
    Ok(())
}

/// Run the viewer on DRM/KMS; see `kms.rs`.
#[cfg(target_os = "linux")]
pub use kms::run_kms;

#[cfg(not(target_os = "linux"))]
pub async fn run_kms(
    _from_loader: PhotoReceiver,
    _to_manager_displayed: Sender<Displayed>,
    _cancel: CancellationToken,
    _cfg: Arc<crate::config::Configuration>,
    _control: Receiver<ViewerCommand>,
    _decode_scaling: super::loader::DecodeScaling,
) -> anyhow::Result<()> {
    anyhow::bail!("PHOTOFRAME_SURFACE=drm is only available on Linux")
}

pub fn run_windowed(
    from_loader: PhotoReceiver,
    to_manager_displayed: Sender<Displayed>,
    cancel: CancellationToken,
    cfg: Arc<crate::config::Configuration>,
    mut control: Receiver<ViewerCommand>,
    decode_scaling: super::loader::DecodeScaling,
) -> anyhow::Result<()> {
    // winit permits only one EventLoop per process, even after a failed attempt,
    // so retrying in-process can never recover — it would just surface a confusing
    // "EventLoop can't be recreated" error. Build once and, on failure, return an
    // actionable message. A missing compositor (no WAYLAND_DISPLAY/DISPLAY) is the
    // usual cause: photoframe must run inside the kiosk's Wayland session, not from
    // a bare SSH shell.
    let event_loop = match EventLoop::<ViewerEvent>::with_user_event().build() {
        Ok(event_loop) => event_loop,
        Err(winit::error::EventLoopError::Os(os_err)) => {
            if cancel.is_cancelled() {
                info!("viewer initialization cancelled before compositor became available");
                return Ok(());
            }
            return Err(anyhow::anyhow!(os_err).context(
                "failed to initialize display compositor: no Wayland/X display is available. \
                 photoframe must run inside the kiosk's Wayland session \
                 (check WAYLAND_DISPLAY / DISPLAY); it cannot open a window from a bare SSH shell",
            ));
        }
        Err(other) => return Err(other.into()),
    };
    let proxy: EventLoopProxy<ViewerEvent> = event_loop.create_proxy();
    let control_cancel = cancel.clone();
    let (command_tx, command_rx) = tokio::sync::mpsc::channel(64);
    let forward_handle = {
        let tx = command_tx.clone();
        tokio::spawn(async move {
            while let Some(cmd) = control.recv().await {
                if tx.send(cmd).await.is_err() {
                    break;
                }
            }
        })
    };
    // The driver owns the loader and matting-result channels (it awaits them
    // and forwards what arrives); the app keeps closed stand-ins, which its
    // `try_recv` polls read as empty.
    let (_, loader_stand_in) = crate::tasks::photo_channel::photo_channel(1);
    let mut app = App::new(
        loader_stand_in,
        to_manager_displayed,
        cancel,
        cfg,
        decode_scaling,
    );
    let (_, mat_stand_in) = tokio::sync::mpsc::channel(1);
    let mat_results = std::mem::replace(&mut app.mat_pipeline.result_rx, mat_stand_in);
    let (published_tx, published_rx) = tokio::sync::watch::channel(wake::Published::idle());
    app.wake_published = Some(published_tx);
    let control_driver = tokio::spawn(drive_viewer_events(
        command_rx,
        control_cancel,
        proxy,
        mat_results,
        from_loader,
        published_rx,
    ));
    app.enter_greeting();
    event_loop.run_app(&mut app)?;

    drop(command_tx);
    control_driver.abort();
    forward_handle.abort();

    let wait_handles = async { (control_driver.await, forward_handle.await) };

    let (control_driver_result, forward_handle_result) =
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| block_on(wait_handles))
        } else {
            block_on(wait_handles)
        };

    if let Err(err) = control_driver_result {
        if err.is_cancelled() {
            debug!(error = %err, "viewer control driver cancelled while shutting down");
        } else {
            warn!(error = %err, "viewer control driver join error during shutdown");
        }
    }

    if let Err(err) = forward_handle_result {
        if err.is_cancelled() {
            debug!(error = %err, "viewer command forwarder cancelled while shutting down");
        } else {
            warn!(error = %err, "viewer command forwarder join error during shutdown");
        }
    }

    Ok(())
}

#[doc(hidden)]
#[allow(dead_code)]
pub mod testkit {
    //! Internal viewer harness helpers exposed for integration testing.
    use super::*;
    use std::time::{Duration, Instant};

    pub struct MattingQueueHarness {
        preload_count: usize,
        mat_inflight: usize,
        deferred_images: VecDeque<QueuedImage>,
        ready_results: VecDeque<MatResult>,
        from_loader_tx: PhotoSender,
        from_loader_rx: PhotoReceiver,
        mat_pipeline: MattingPipeline,
        wake: scenes::WakeScene,
        oversample: f32,
        max_upscale_factor: f32,
        matting: MattingConfig,
        pairing: Option<crate::tasks::manager::pairing::PairingRules>,
        surface_configured: bool,
        surface: Option<SurfaceState>,
    }

    pub struct ProcessedCanvas {
        pub path: std::path::PathBuf,
        pub width: u32,
        pub height: u32,
        pub priority: bool,
        /// A pair screen's second member and its priority.
        pub partner: Option<(std::path::PathBuf, bool)>,
    }

    impl MattingQueueHarness {
        pub fn new(
            preload_count: usize,
            oversample: f32,
            matting: MattingConfig,
            dwell_ms: u64,
            transition_cfg: TransitionConfig,
        ) -> Self {
            let (from_loader_tx, from_loader_rx) = crate::tasks::photo_channel::photo_channel(8);
            Self {
                preload_count,
                mat_inflight: 0,
                deferred_images: VecDeque::new(),
                ready_results: VecDeque::new(),
                from_loader_tx,
                from_loader_rx,
                mat_pipeline: MattingPipeline::new(1, preload_count.max(2)),
                wake: scenes::WakeScene::new(dwell_ms, dwell_ms, transition_cfg),
                oversample,
                max_upscale_factor: 1.0,
                // The rule as the frame would apply it with these mats and
                // pairing enabled at its defaults.
                pairing: crate::tasks::manager::pairing::PairingRules::from_parts(
                    &crate::config::PairingOptions {
                        enabled: true,
                        ..Default::default()
                    },
                    &matting,
                    1.0,
                ),
                matting,
                surface_configured: false,
                surface: None,
            }
        }

        pub fn loader_sender(&self) -> PhotoSender {
            self.from_loader_tx.clone()
        }

        pub fn update_surface_state(&mut self, surface: Option<(u32, u32, u32)>) {
            self.surface = surface.map(|(width, height, max_texture_dimension)| SurfaceState {
                width,
                height,
                max_texture_dimension,
            });
        }

        pub fn set_surface_configured(&mut self, configured: bool) {
            self.surface_configured = configured;
        }

        pub fn push_deferred(&mut self, image: PreparedImageCpu, priority: bool) {
            self.deferred_images.push_back(QueuedImage {
                image,
                priority,
                partner: None,
            });
        }

        /// Queue a pair screen (portrait pairing) as the loader would send it.
        pub fn push_deferred_pair(
            &mut self,
            (image, priority): (PreparedImageCpu, bool),
            (partner, partner_priority): (PreparedImageCpu, bool),
        ) {
            self.deferred_images.push_back(QueuedImage {
                image,
                priority,
                partner: Some(LoadedPartner {
                    prepared: partner,
                    priority: partner_priority,
                }),
            });
        }

        /// Paths of the queued (not yet matted) screens, members in order.
        pub fn deferred_screens(&self) -> Vec<Vec<std::path::PathBuf>> {
            self.deferred_images
                .iter()
                .map(|queued| {
                    std::iter::once(queued.image.path.clone())
                        .chain(queued.partner.as_ref().map(|p| p.prepared.path.clone()))
                        .collect()
                })
                .collect()
        }

        pub fn queue_once(&mut self) {
            let mut bridge = MattingBridge {
                preload_count: self.preload_count,
                mat_inflight: &mut self.mat_inflight,
                deferred_images: &mut self.deferred_images,
                ready_results: &mut self.ready_results,
                from_loader: &mut self.from_loader_rx,
                surface: surface_state_for_queue(self.surface_configured, self.surface),
                matting: &self.matting,
                oversample: self.oversample,
                max_upscale_factor: self.max_upscale_factor,
                bar_ratio: 0.618,
                pairing: self.pairing,
                mat_pipeline: &self.mat_pipeline,
            };
            bridge.queue_for_wake(&mut self.wake);
        }

        pub fn drain_pipeline(&mut self) {
            while let Some(result) = self.mat_pipeline.try_recv() {
                self.mat_inflight = self.mat_inflight.saturating_sub(result.photos);
                if !result.is_acknowledgement() {
                    self.ready_results.push_back(result);
                }
            }
        }

        pub async fn wait_for_ready_results(&mut self, timeout: Duration) {
            let mut deadline = Instant::now() + timeout;
            let mut extended = false;
            loop {
                self.drain_pipeline();
                if !self.ready_results.is_empty() || self.mat_inflight == 0 {
                    break;
                }
                let now = Instant::now();
                if now >= deadline {
                    if extended {
                        break;
                    }
                    extended = true;
                    deadline = now + timeout;
                    continue;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }

        pub fn take_ready_canvases(&mut self) -> Vec<ProcessedCanvas> {
            self.ready_results
                .drain(..)
                .map(|result| ProcessedCanvas {
                    path: result.path,
                    width: result.canvas.width,
                    height: result.canvas.height,
                    priority: result.priority,
                    partner: result.partner.map(|member| (member.path, member.priority)),
                })
                .collect()
        }

        pub fn deferred_queue_len(&self) -> usize {
            self.deferred_images.len()
        }

        /// Screens handed to the matting pipeline whose result has not been drained yet.
        pub fn matting_inflight(&self) -> usize {
            self.mat_inflight
        }
    }

    pub fn compute_canvas_size_for_test(
        screen_w: u32,
        screen_h: u32,
        oversample: f32,
        max_dim: u32,
    ) -> (u32, u32) {
        compute_canvas_size(screen_w, screen_h, oversample, max_dim)
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::MattingQueueHarness;
    use super::*;
    use crate::config::TransitionConfig;
    use image::{Rgba, RgbaImage};
    use std::collections::VecDeque;
    use std::path::PathBuf;

    fn make_gradient(width: u32, height: u32) -> RgbaImage {
        image::ImageBuffer::from_fn(width, height, |x, y| {
            let r = ((x * 255) / width.max(1)).min(255) as u8;
            let g = ((y * 255) / height.max(1)).min(255) as u8;
            Rgba([r, g, 0, 255])
        })
    }

    /// `wake::plan_wake`'s `want_photo` must say "take a photo from the
    /// loader" exactly when `MattingBridge::queue_for_wake` would read the
    /// loader itself; otherwise the KMS loop either sleeps while the bridge
    /// wants photos, or drains the loader past its backpressure. Checked
    /// against the real bridge over every queue state it can see without a
    /// GPU (pending uploads need textures, so `pending` stays 0 here).
    #[test]
    fn want_photo_matches_the_bridge_gate() {
        const PRELOAD: usize = 3;
        let cfg = crate::config::TransitionConfig::default();
        let matting = MattingConfig::default();
        let now = Instant::now();
        let rules = crate::tasks::manager::pairing::PairingRules {
            probability: 1.0,
            bar_ratio: 0.618,
            max_upscale: 1.0,
            m_min: 0.06,
            m_max: 0.06,
            bevel_max: 0.0,
            lookahead: 1,
        };
        // Pairing off and on; with it on, the deferred screens are pairs
        // (two photos each, review M4).
        for pairs in [false, true] {
            for transition in [false, true] {
                for surface_ready in [false, true] {
                    for in_flight in 0..=PRELOAD + 1 {
                        for deferred in 0..=2usize {
                            // Fresh per case: the bridge only ever sees a task
                            // queue no fuller than its own in-flight count.
                            let pipeline = MattingPipeline::new(1, PRELOAD);
                            let mut wake = scenes::WakeScene::new(8_000, 8_000, cfg.clone());
                            if transition {
                                let selected = cfg.primary_selected().expect("default transition");
                                wake.set_transition_state(Some(TransitionState::new(
                                    selected,
                                    now,
                                    &mut rand::rng(),
                                )));
                            }
                            let (tx, mut rx) = crate::tasks::photo_channel::photo_channel(4);
                            tx.try_send(PhotoLoaded {
                                prepared: PreparedImageCpu {
                                    path: "loader.jpg".into(),
                                    width: 4,
                                    height: 4,
                                    source_size: (4, 4),
                                    pixels: vec![0; 64],
                                },
                                priority: false,
                                partner: None,
                            })
                            .unwrap();
                            let photo = |name: String| PreparedImageCpu {
                                path: name.into(),
                                width: 4,
                                height: 4,
                                source_size: (4, 4),
                                pixels: vec![0; 64],
                            };
                            let mut deferred_images: VecDeque<QueuedImage> = (0..deferred)
                                .map(|i| QueuedImage {
                                    image: photo(format!("deferred-{i}.jpg")),
                                    priority: false,
                                    partner: pairs.then(|| LoadedPartner {
                                        prepared: photo(format!("partner-{i}.jpg")),
                                        priority: false,
                                    }),
                                })
                                .collect();
                            let deferred_photos: usize =
                                deferred_images.iter().map(QueuedImage::photos).sum();
                            let deferred_head =
                                deferred_images.front().map_or(0, QueuedImage::photos);
                            let mut mat_inflight = in_flight;
                            let mut ready_results = VecDeque::new();
                            let mut bridge = MattingBridge {
                                preload_count: PRELOAD,
                                mat_inflight: &mut mat_inflight,
                                deferred_images: &mut deferred_images,
                                ready_results: &mut ready_results,
                                from_loader: &mut rx,
                                surface: surface_state_for_queue(
                                    surface_ready,
                                    Some(SurfaceState::new(64, 64, 4096)),
                                ),
                                matting: &matting,
                                oversample: 1.0,
                                max_upscale_factor: 1.0,
                                bar_ratio: 0.618,
                                pairing: pairs.then_some(rules),
                                mat_pipeline: &pipeline,
                            };
                            bridge.queue_for_wake(&mut wake);
                            let bridge_read_loader = rx.is_empty();

                            let plan = wake::plan_wake(
                                wake::WakeInputs {
                                    slideshow_active: true,
                                    sleeping: false,
                                    redraw_requested: false,
                                    transition_active: transition,
                                    transition_just_finished: false,
                                    displayed_at: None,
                                    dwell: Duration::from_secs(8),
                                    surface_ready,
                                    pending: 0,
                                    in_flight,
                                    deferred: deferred_photos,
                                    deferred_head,
                                    largest_screen: largest_screen(pairs),
                                    preload: PRELOAD,
                                    overlay_expires_at: None,
                                    surface_poll: None,
                                    overlay_pending: false,
                                    photo_staged: false,
                                    lead_in: Duration::ZERO,
                                    lead_in_drain: Duration::ZERO,
                                },
                                now,
                            );
                            assert_eq!(
                                plan.want_photo, bridge_read_loader,
                                "pairs={pairs} transition={transition} surface_ready={surface_ready} \
                             in_flight={in_flight} deferred={deferred}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// Review M4: the viewer's preload counts decoded photos. Pairs never
    /// take it past `viewer-preload-count` photos; single photos fill it
    /// exactly as before.
    #[test]
    fn preload_counts_photos_so_pairs_never_hold_more_than_solos() {
        const PRELOAD: usize = 3;
        let cfg: crate::config::Configuration = serde_yaml::from_str(
            "photo-library-path: /photos\nmatting:\n  selection: fixed\n  active:\n    - kind: fixed-color\n      minimum-mat-percentage: 6\n",
        )
        .unwrap();
        let cfg = cfg.validated().unwrap();
        let rules = crate::tasks::manager::pairing::PairingRules::from_parts(
            &crate::config::PairingOptions {
                enabled: true,
                ..Default::default()
            },
            &cfg.matting,
            1.0,
        );
        // Pairable on a 320 x 180 canvas: full height 158.4 px, aspect <= 0.87.
        let photo = |name: String, w: u32| PreparedImageCpu {
            path: name.into(),
            width: w,
            height: 160,
            source_size: (w, 160),
            pixels: vec![200; (w * 160 * 4) as usize],
        };
        for (pairing, pairs, expected_photos) in
            [(false, false, 3), (true, true, 2), (true, false, 2)]
        {
            let pipeline = MattingPipeline::new(1, 8);
            let mut wake = scenes::WakeScene::new(8_000, 8_000, TransitionConfig::default());
            let (tx, mut rx) = crate::tasks::photo_channel::photo_channel(8);
            for i in 0..4 {
                tx.try_send(PhotoLoaded {
                    prepared: photo(format!("a{i}.jpg"), 120),
                    priority: false,
                    partner: pairs.then(|| LoadedPartner {
                        prepared: photo(format!("b{i}.jpg"), 96),
                        priority: false,
                    }),
                })
                .unwrap();
            }
            let sent = rx.photos();
            let mut deferred_images = VecDeque::new();
            let mut ready_results = VecDeque::new();
            let mut mat_inflight = 0;
            let mut bridge = MattingBridge {
                preload_count: PRELOAD,
                mat_inflight: &mut mat_inflight,
                deferred_images: &mut deferred_images,
                ready_results: &mut ready_results,
                from_loader: &mut rx,
                surface: Some(SurfaceState::new(320, 180, 4096)),
                matting: &cfg.matting,
                oversample: 1.0,
                max_upscale_factor: 1.0,
                bar_ratio: 0.618,
                pairing: if pairing { rules } else { None },
                mat_pipeline: &pipeline,
            };
            bridge.queue_for_wake(&mut wake);
            let held: usize = mat_inflight
                + deferred_images
                    .iter()
                    .map(QueuedImage::photos)
                    .sum::<usize>();
            assert!(
                held <= PRELOAD,
                "pairing={pairing} pairs={pairs}: {held} photos"
            );
            assert_eq!(held, expected_photos, "pairing={pairing} pairs={pairs}");
            assert_eq!(
                rx.photos(),
                sent - held,
                "the rest stays in the loader channel"
            );
        }
    }

    /// A matting task that produces no canvas still gives back its photos:
    /// a pair whose members cannot be read must not hold two places in the
    /// preload for good (with pairing at preload 3 that would stall it).
    #[test]
    fn a_failed_matting_task_gives_back_its_photos() {
        let cfg: crate::config::Configuration = serde_yaml::from_str(
            "photo-library-path: /photos\nmatting:\n  selection: fixed\n  active:\n    - kind: fixed-color\n      minimum-mat-percentage: 6\n",
        )
        .unwrap();
        let cfg = cfg.validated().unwrap();
        let rules = crate::tasks::manager::pairing::PairingRules::from_parts(
            &crate::config::PairingOptions {
                enabled: true,
                ..Default::default()
            },
            &cfg.matting,
            1.0,
        );
        // Pairable on 320 x 180; `readable: false` leaves no pixels behind
        // the claimed size, which `decoded_member` cannot read.
        let photo = |name: &str, w: u32, readable: bool| PreparedImageCpu {
            path: name.into(),
            width: w,
            height: 160,
            source_size: (w, 160),
            pixels: if readable {
                vec![200; (w * 160 * 4) as usize]
            } else {
                Vec::new()
            },
        };
        // (first readable, partner readable) -> canvases expected.
        for (first_ok, second_ok, canvases) in
            [(false, false, 0), (true, false, 1), (false, true, 1)]
        {
            let pipeline = MattingPipeline::new(1, 4);
            let mut wake = scenes::WakeScene::new(8_000, 8_000, TransitionConfig::default());
            let (_tx, mut rx) = crate::tasks::photo_channel::photo_channel(4);
            let mut deferred_images = VecDeque::from([QueuedImage {
                image: photo("a.jpg", 120, first_ok),
                priority: false,
                partner: Some(LoadedPartner {
                    prepared: photo("b.jpg", 96, second_ok),
                    priority: false,
                }),
            }]);
            let mut ready_results = VecDeque::new();
            let mut mat_inflight = 0;
            MattingBridge {
                preload_count: 3,
                mat_inflight: &mut mat_inflight,
                deferred_images: &mut deferred_images,
                ready_results: &mut ready_results,
                from_loader: &mut rx,
                surface: Some(SurfaceState::new(320, 180, 4096)),
                matting: &cfg.matting,
                oversample: 1.0,
                max_upscale_factor: 1.0,
                bar_ratio: 0.618,
                pairing: rules,
                mat_pipeline: &pipeline,
            }
            .queue_for_wake(&mut wake);
            assert_eq!(mat_inflight, 2, "the pair went to matting as two photos");
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut pipeline = pipeline;
            while mat_inflight > 0 && Instant::now() < deadline {
                match pipeline.try_recv() {
                    Some(result) => {
                        accept_mat_result(&mut mat_inflight, &mut ready_results, result)
                    }
                    None => std::thread::sleep(Duration::from_millis(5)),
                }
            }
            assert_eq!(
                mat_inflight, 0,
                "first={first_ok} second={second_ok}: photos leaked"
            );
            assert_eq!(
                ready_results.len(),
                canvases,
                "first={first_ok} second={second_ok}"
            );
            assert!(
                ready_results
                    .iter()
                    .all(|r| r.partner.is_none() && r.canvas.width > 0)
            );
        }
    }

    #[test]
    fn a_forwarded_matting_result_is_accounted_like_a_polled_one() {
        let mut inflight = 2usize;
        let mut ready = VecDeque::new();
        let result = |name: &str| MatResult {
            path: name.into(),
            canvas: ImagePlane {
                width: 1,
                height: 1,
                pixels: vec![0; 4],
            },
            priority: false,
            partner: None,
            windows: Vec::new(),
            mat_kind: None,
            photos: 1,
        };
        accept_mat_result(&mut inflight, &mut ready, result("a.jpg"));
        assert_eq!((inflight, ready.len()), (1, 1));
        accept_mat_result(&mut inflight, &mut ready, result("b.jpg"));
        // After a resume zeroed the count, stragglers do not underflow it.
        accept_mat_result(&mut inflight, &mut ready, result("c.jpg"));
        assert_eq!((inflight, ready.len()), (0, 3));
    }

    #[test]
    fn window_events_that_free_queued_work_ask_for_a_step() {
        use winit::dpi::PhysicalSize;
        assert!(window_event_wants_step(&WindowEvent::Resized(
            PhysicalSize::new(800, 600)
        )));
        assert!(window_event_wants_step(&WindowEvent::Occluded(false)));
        assert!(!window_event_wants_step(&WindowEvent::Occluded(true)));
        assert!(!window_event_wants_step(&WindowEvent::RedrawRequested));
    }

    #[test]
    fn a_kms_transition_ends_when_its_last_frame_is_rendered_not_by_wall_time() {
        let cfg = crate::config::TransitionConfig::default();
        let selected = cfg.primary_selected().expect("default transition");
        let created = Instant::now();
        let mut state = TransitionState::new(selected, created, &mut rand::rng());
        // Wayland (never anchored): wall time decides; a fresh one is running.
        assert!(!state.is_complete());
        // KMS: frames render ahead of the wall clock, so the frame drawn for
        // progress 1 may be rendered before the duration has passed on the
        // wall; that frame ends the transition.
        let anchor = created + Duration::from_millis(10);
        state.anchor_once(anchor);
        state.note_rendered(state.progress_at(anchor + state.duration / 2));
        assert!(!state.is_complete());
        state.note_rendered(state.progress_at(anchor + state.duration));
        assert!(state.is_complete());
        assert!(state.is_anchored());
    }

    #[test]
    fn kms_frame_clock_reports_the_display_cadence() {
        // Frames paced on consecutive vblanks of a 60 Hz panel, the first
        // one arriving at an arbitrary phase after the dwell ended (the
        // transition was created 7 ms before it).
        let period = Duration::from_nanos(16_666_667);
        let created = Instant::now();
        let vblank0 = created + Duration::from_millis(7);
        let cfg = crate::config::TransitionConfig::default();
        let selected = cfg.primary_selected().expect("default transition");
        let mut state = TransitionState::new(selected, created, &mut rand::rng());
        let duration = state.duration.as_secs_f32();
        // Frames up to (not including) the one that completes the transition.
        let count = (state.duration.as_secs_f64() / period.as_secs_f64()).floor() as u32;
        let frames: Vec<Instant> = (0..count).map(|k| vblank0 + period * k).collect();

        let mut metric: Option<TransitionFrameMetric> = None;
        let mut progress = Vec::new();
        for &t in &frames {
            state.anchor_once(t);
            progress.push(state.progress_at(t));
            match metric.as_mut() {
                None => metric = Some(TransitionFrameMetric::begin(state.kind(), t, true)),
                Some(m) => m.record(t),
            }
        }
        // The first frame shows the resting photo; every step is one refresh.
        assert_eq!(progress[0], 0.0);
        let step = period.as_secs_f32() / duration;
        for pair in progress.windows(2) {
            assert!(((pair[1] - pair[0]) - step).abs() < 1e-4, "{pair:?}");
        }
        let m = metric.unwrap();
        let ms = period.as_secs_f32() * 1000.0;
        assert!(
            (m.best_frame_ms - ms).abs() < 0.01,
            "best {}",
            m.best_frame_ms
        );
        assert!(
            (m.worst_frame_ms - ms).abs() < 0.01,
            "worst {}",
            m.worst_frame_ms
        );
        // Re-anchoring is one-shot: a later frame time does not restart it.
        let last = *frames.last().unwrap();
        state.anchor_once(last + period);
        assert!(state.progress_at(last) > 0.0);
    }

    #[test]
    fn mip_sharpen_parses_a_clamped_finite_number() {
        assert_eq!(parse_mip_sharpen("1"), Some((1.0, false)));
        assert_eq!(parse_mip_sharpen(" 0.5 "), Some((0.5, false)));
        assert_eq!(parse_mip_sharpen("0"), Some((0.0, false)));
        assert_eq!(parse_mip_sharpen("3"), Some((MIP_SHARPEN_MAX, true)));
        assert_eq!(parse_mip_sharpen("-1"), Some((0.0, true)));
        for bad in ["", "sharp", "NaN", "inf"] {
            assert_eq!(parse_mip_sharpen(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn upscale_filters_parse_to_their_entry_points() {
        assert_eq!(UpscaleFilter::DEFAULT, UpscaleFilter::Bilinear);
        for (value, entry) in [
            ("bilinear", "fs_blit"),
            (" bicubic ", "fs_upscale_bicubic"),
            ("bicubic-9", "fs_upscale_bicubic9"),
        ] {
            assert_eq!(
                UpscaleFilter::parse(value).map(UpscaleFilter::entry_point),
                Some(entry)
            );
        }
        assert_eq!(UpscaleFilter::parse("lanczos"), None);
    }

    #[test]
    fn probes_parse_a_comma_list() {
        assert_eq!(parse_probes(""), (Probes::default(), vec![]));
        let (probes, unknown) = parse_probes("single-sample");
        assert!(probes.single_sample && unknown.is_empty());
        let (probes, unknown) = parse_probes(" single-sample ,bogus,");
        assert!(probes.single_sample);
        assert_eq!(unknown, vec!["bogus"]);
    }

    #[test]
    fn pass_times_read_the_timestamp_pairs() {
        let ms = Duration::from_millis;
        // A 1 ns tick: 4 ms main, 1.5 ms upscale, no layer.
        let times = pass_times(
            [Some((1_000, 4_001_000)), Some((4_100_000, 5_600_000)), None],
            1.0,
        )
        .unwrap();
        assert_eq!((times.draw, times.main), (ms(4), ms(4)));
        assert_eq!(times.upscale, Some(Duration::from_micros(1_500)));
        assert_eq!(times.layer, None);
        // With a layer, draw runs from the layer's start to the main end.
        let iris = pass_times(
            [Some((3_000_000, 5_000_000)), None, Some((0, 2_500_000))],
            1.0,
        )
        .unwrap();
        assert_eq!(iris.layer, Some(Duration::from_micros(2_500)));
        assert_eq!((iris.main, iris.draw), (ms(2), ms(5)));
        assert_eq!(iris.upscale, None);
        // Ticks of another length are scaled.
        let scaled = pass_times([Some((0, 1_000)), None, None], 52.08).unwrap();
        assert_eq!(scaled.draw, Duration::from_nanos(52_080));
        // A pair that runs backwards is no measurement; a bad layer pair
        // leaves draw = main.
        assert_eq!(pass_times([Some((10, 5)), None, None], 1.0), None);
        let bad_layer = pass_times([Some((10, 20)), None, Some((5, 0))], 1.0).unwrap();
        assert_eq!(
            (bad_layer.layer, bad_layer.draw),
            (None, Duration::from_nanos(10))
        );
    }

    /// The timer's writes, resolve, copy and mapped read on a real device
    /// (skipped where the adapter has no timestamp queries), over more
    /// frames than the timer has slots: frames drawn into the intermediate
    /// yield both pass times, native ones the draw time only. Metal now and
    /// then leaves a pass's end sample at 0 (Apple silicon, a few frames in a
    /// few hundred); `pass_times` rejects that pair and the
    /// viewer logs `-`, so the test asserts on what was read, not on every
    /// frame.
    #[test]
    fn pass_timer_reads_back_both_passes() {
        let Some((_gpu, device, queue)) =
            crate::gpu::test_device_with("pass-timer-test", wgpu::Features::TIMESTAMP_QUERY)
        else {
            return;
        };
        let Some(timer) = PassTimer::new(&device, &queue) else {
            eprintln!("no TIMESTAMP_QUERY on this adapter; skipped");
            return;
        };
        // Two targets, as in the viewer (intermediate, then the surface).
        let target = || {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("pass-timer-target"),
                size: wgpu::Extent3d {
                    width: 64,
                    height: 64,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
        };
        let views =
            [target(), target()].map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()));
        // Each pass draws a triangle, as the viewer's do (Metal does not
        // sample the end of a pass that only clears).
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pass-timer-test"),
            source: wgpu::ShaderSource::Wgsl(
                "@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
                     let p = vec2<f32>(f32(i & 1u) * 4.0 - 1.0, f32(i >> 1u) * 4.0 - 1.0);
                     return vec4<f32>(p, 0.0, 1.0);
                 }
                 @fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }"
                    .into(),
            ),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pass-timer-test"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())],
            }),
            multiview: None,
            cache: None,
        });
        let frame = |upscaled: bool| {
            let timer = timer.frame();
            let mut encoder =
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            let mut pass = |view, writes| {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    occlusion_query_set: None,
                    timestamp_writes: writes,
                });
                pass.set_pipeline(&pipeline);
                pass.draw(0..3, 0..1);
            };
            // A petal layer drawn as two passes, as the viewer does.
            pass(&views[1], Some(timer.writes(TimedPass::Layer, true, false)));
            pass(&views[1], Some(timer.writes(TimedPass::Layer, false, true)));
            pass(&views[0], Some(timer.writes(TimedPass::Main, true, true)));
            if upscaled {
                pass(
                    &views[1],
                    Some(timer.writes(TimedPass::Upscale, true, true)),
                );
            }
            let readback = timer.resolve(&device, &mut encoder);
            queue.submit(Some(encoder.finish()));
            let cell = timer.read(readback);
            let _ = device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(5)),
            });
            cell.get().copied()
        };
        let (mut upscaled, mut native) = (Vec::new(), Vec::new());
        for _ in 0..2 * PassTimer::SLOTS {
            upscaled.extend(frame(true));
            native.extend(frame(false));
        }
        assert!(
            upscaled.iter().filter(|t| t.upscale.is_some()).count() > upscaled.len() / 2,
            "upscale times read back: {upscaled:?}"
        );
        assert!(
            native.len() > PassTimer::SLOTS as usize,
            "draw times read back: {native:?}"
        );
        assert!(native.iter().all(|t| t.upscale.is_none()));
        assert!(
            native
                .iter()
                .chain(&upscaled)
                .filter(|t| t.layer.is_some())
                .count()
                > native.len(),
            "layer times read back"
        );
    }

    #[test]
    fn canvas_bytes_count_every_level() {
        let one = 3840u64 * 2160 * 4;
        assert_eq!(rgba8_bytes(3840, 2160, 1), one);
        // Level 1 adds a quarter: +25% at scale 2.
        assert_eq!(rgba8_bytes(3840, 2160, 2), one + one / 4);
        assert_eq!(rgba8_bytes(3840, 2160, 3), one + one / 4 + one / 16);
    }

    #[test]
    fn canvases_carry_the_levels_the_transition_scale_reads() {
        assert_eq!(canvas_mip_levels(3840, 2160, 1), 1);
        assert_eq!(canvas_mip_levels(3840, 2160, 2), 2);
        assert_eq!(canvas_mip_levels(3840, 2160, 3), 3);
        assert_eq!(canvas_mip_levels(3840, 2160, 4), 3);
        // Never more levels than the canvas can halve.
        assert_eq!(canvas_mip_levels(3, 2, 4), 2);
        assert_eq!(canvas_mip_levels(1, 1, 4), 1);
    }

    #[test]
    fn every_transition_kind_has_its_own_pipeline() {
        let mut seen = std::collections::HashSet::new();
        for &kind in TransitionKind::ALL {
            let draw = DrawKind::for_transition(kind);
            assert!(DrawKind::ALL.contains(&draw), "{kind:?} -> {draw:?}");
            assert!(
                seen.insert(draw),
                "{kind:?} shares {draw:?} with another kind"
            );
            assert_ne!(draw, DrawKind::Rest);
            assert_ne!(draw, DrawKind::DebugBezier);
        }
        // DrawKind::ALL lists each kind once, in index order.
        for (i, kind) in DrawKind::ALL.iter().enumerate() {
            assert_eq!(kind.index(), i);
        }
    }

    /// Each per-kind fragment entry point exists and compiles on its own
    /// (WGSL -> naga IR -> SPIR-V for that entry point alone), the same unit
    /// the driver compiles per pipeline.
    #[test]
    fn every_draw_kind_entry_point_compiles_alone() {
        let source = include_str!("shaders/viewer_quad.wgsl");
        let module = naga::front::wgsl::parse_str(source)
            .unwrap_or_else(|e| panic!("viewer_quad.wgsl: {}", e.emit_to_string(source)));
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("viewer_quad.wgsl validates");
        let mut entry_points: Vec<&str> = DrawKind::ALL.iter().map(|k| k.entry_point()).collect();
        entry_points.extend([
            "fs_iris_cover",
            "fs_iris_layer",
            "fs_eink_mask",
            "fs_blit",
            "fs_upscale_bicubic",
            "fs_upscale_bicubic9",
            "fs_mip_sharpen",
        ]);
        for name in entry_points {
            let ep = module
                .entry_points
                .iter()
                .find(|ep| ep.name == name)
                .unwrap_or_else(|| panic!("viewer_quad.wgsl has no entry point {name}"));
            assert_eq!(ep.stage, naga::ShaderStage::Fragment, "{name}");
            // Resolve the pipeline constants as wgpu does at pipeline
            // creation, for every combination of the compiled-in probes.
            for single_sample in [false, true] {
                let probe = Probes { single_sample };
                let mut constants = naga::back::PipelineConstants::default();
                for (name, value) in probe_pipeline_constants(probe) {
                    constants.insert(name.to_string(), value);
                }
                let (resolved, resolved_info) = naga::back::pipeline_constants::process_overrides(
                    &module,
                    &info,
                    Some((naga::ShaderStage::Fragment, name)),
                    &constants,
                )
                .unwrap_or_else(|e| panic!("{name}: overrides {probe:?}: {e:?}"));
                let pipeline = naga::back::spv::PipelineOptions {
                    shader_stage: naga::ShaderStage::Fragment,
                    entry_point: name.to_string(),
                };
                let words = naga::back::spv::write_vec(
                    &resolved,
                    &resolved_info,
                    &naga::back::spv::Options::default(),
                    Some(&pipeline),
                )
                .unwrap_or_else(|e| panic!("{name} ({probe:?}) does not compile to SPIR-V: {e:?}"));
                assert!(!words.is_empty(), "{name}");
            }
        }
    }

    #[test]
    fn surface_defaults_to_wayland_and_rejects_unknown_values() {
        assert_eq!(ViewerSurface::parse(None).unwrap(), ViewerSurface::Wayland);
        assert_eq!(
            ViewerSurface::parse(Some("")).unwrap(),
            ViewerSurface::Wayland
        );
        assert_eq!(
            ViewerSurface::parse(Some("wayland")).unwrap(),
            ViewerSurface::Wayland
        );
        assert_eq!(
            ViewerSurface::parse(Some("drm")).unwrap(),
            ViewerSurface::Drm
        );
        assert_eq!(
            ViewerSurface::parse(Some(" drm\n")).unwrap(),
            ViewerSurface::Drm
        );
        for bad in ["kms", "DRM", "x11", "wayland,drm"] {
            assert!(ViewerSurface::parse(Some(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn present_mode_defaults_per_surface_kind() {
        use wgpu::PresentMode::*;
        let all = [Fifo, Mailbox, Immediate];
        assert_eq!(choose_present_mode(None, &all, PresentDefault::Kms), Fifo);
        assert_eq!(
            choose_present_mode(None, &all, PresentDefault::Wayland),
            Mailbox
        );
        assert_eq!(
            choose_present_mode(None, &[Fifo], PresentDefault::Wayland),
            AutoVsync
        );
    }

    #[test]
    fn present_mode_override_is_honoured_when_supported() {
        use wgpu::PresentMode::*;
        let all = [Fifo, Mailbox, Immediate];
        for (env, want) in [
            ("fifo", Fifo),
            ("mailbox", Mailbox),
            ("immediate", Immediate),
        ] {
            assert_eq!(
                choose_present_mode(Some(env), &all, PresentDefault::Kms),
                want
            );
            assert_eq!(
                choose_present_mode(Some(env), &all, PresentDefault::Wayland),
                want
            );
        }
    }

    #[test]
    fn present_mode_falls_back_to_the_surface_default() {
        use wgpu::PresentMode::*;
        // Unsupported request.
        assert_eq!(
            choose_present_mode(Some("mailbox"), &[Fifo], PresentDefault::Kms),
            Fifo
        );
        assert_eq!(
            choose_present_mode(Some("immediate"), &[Fifo, Mailbox], PresentDefault::Wayland),
            Mailbox
        );
        // Unknown value.
        assert_eq!(
            choose_present_mode(Some("vsync"), &[Fifo, Mailbox], PresentDefault::Kms),
            Fifo
        );
    }

    #[test]
    fn scale_cover_matches_canvas_dimensions() {
        let src = make_gradient(400, 300);
        let canvas = scale_image_to_cover_canvas(&src, 1920, 1080, 4096);
        assert_eq!(canvas.dimensions(), (1920, 1080));
        let center = canvas.get_pixel(960, 540);
        assert_eq!(center[3], 255);
    }

    #[test]
    fn scale_cover_respects_max_texture_limit() {
        let src = make_gradient(1000, 400);
        let canvas = scale_image_to_cover_canvas(&src, 1920, 1080, 2000);
        assert_eq!(canvas.dimensions(), (1920, 1080));
        let top_left = canvas.get_pixel(0, 0);
        let bottom_right = canvas.get_pixel(1919, 1079);
        assert!(top_left[3] == 255 && bottom_right[3] == 255);
    }

    #[test]
    fn compute_canvas_size_preserves_aspect_ratio_when_limited() {
        let (w, h) = compute_canvas_size(3840, 2160, 1.25, 4096);
        assert_eq!(w, 4096);
        assert_eq!(h, 2304);
        assert!(w <= 4096 && h <= 4096);

        fn gcd(mut a: u32, mut b: u32) -> u32 {
            while b != 0 {
                let r = a % b;
                a = b;
                b = r;
            }
            a
        }

        let divisor = gcd(w, h);
        assert_eq!(w / divisor, 16);
        assert_eq!(h / divisor, 9);
    }

    #[test]
    fn resize_with_margin_respects_bounds() {
        let (w, h) = resize_to_fit_with_margin(1920, 1080, 4000, 1000, 0.1, 1.5);
        assert!(w <= 1920 && h <= 1080);
        assert!(w > 0 && h > 0);
    }

    use crate::tasks::loader::DecodedPhoto;
    use crate::tasks::loader::scaled_decode_tests::{Flavor, photo_jpeg, psnr_rgb, synthetic_jpeg};

    fn mat_canvas(
        photo: &DecodedPhoto,
        path: &std::path::Path,
        screen: (u32, u32),
        oversample: f32,
        kind: crate::config::MattingKind,
        fill_screen: bool,
    ) -> RgbaImage {
        use crate::config::MattingOptions;
        let matting = MattingOptions::default_for_showcase(kind, 6.0, None).expect("mat option");
        let task = MatTask {
            image: PreparedImageCpu {
                path: path.to_path_buf(),
                width: photo.image.width(),
                height: photo.image.height(),
                source_size: photo.source_size,
                pixels: photo.image.as_raw().clone(),
            },
            params: MatParams {
                screen_w: screen.0,
                screen_h: screen.1,
                oversample,
                max_dim: 4096,
                max_upscale_factor: 2.0,
                matting: matting.clone(),
                fill_screen,
                bar_ratio: 0.618,
            },
            priority: false,
            partner: None,
            mat_kind: Some(matting.caption()),
        };
        let c = process_mat_task(task).expect("matted canvas").canvas;
        RgbaImage::from_raw(c.width, c.height, c.pixels).unwrap()
    }

    /// The canvas produced from a DCT-scaled decode must match the one from a
    /// full decode, for every mat that draws the photo or samples it. Textured,
    /// non-multiple-of-8 sources so that a stretched or mis-sized buffer shows.
    #[test]
    fn matted_canvas_from_scaled_decode_matches_full_decode() {
        use crate::config::MattingKind;
        use crate::tasks::loader::{CoverNeed, DecodeTarget, decode_rgba8_apply_exif};

        let (screen_w, screen_h) = (240u32, 135u32);
        let dir = tempfile::tempdir().unwrap();
        for (orientation, oversample) in [(1u16, 1.0f32), (6, 1.0), (3, 1.25), (8, 0.75)] {
            for flavor in [Flavor::Baseline420, Flavor::Progressive420] {
                let path = dir.path().join(format!("photo-{orientation}.jpg"));
                std::fs::write(&path, photo_jpeg(499, 375, orientation, flavor)).unwrap();
                let full = decode_rgba8_apply_exif(&path, None).unwrap();
                let (canvas_w, canvas_h) =
                    compute_canvas_size(screen_w, screen_h, oversample, 4096);

                for kind in [
                    MattingKind::Studio,
                    MattingKind::Blur,
                    MattingKind::DropShadow,
                ] {
                    for fill_screen in [false, true] {
                        // `cover` is what `fill-when-fits` being configured asks
                        // for; 0.125 is the default blur sample scale.
                        let target = DecodeTarget {
                            canvas_w,
                            canvas_h,
                            cover: if fill_screen {
                                CoverNeed::Always
                            } else {
                                CoverNeed::Never
                            },
                            bg_sample_scale: Some(0.125),
                        };
                        let scaled = decode_rgba8_apply_exif(&path, Some(target)).unwrap();
                        assert_eq!(scaled.source_size, full.source_size);
                        if !fill_screen {
                            assert!(
                                scaled.image.width() < full.image.width(),
                                "orientation {orientation}: expected a reduced decode"
                            );
                        }
                        let ctx = format!(
                            "{flavor:?} {kind:?} fill={fill_screen} o{orientation} x{oversample}"
                        );
                        let screen = (screen_w, screen_h);
                        let a = mat_canvas(&full, &path, screen, oversample, kind, fill_screen);
                        let b = mat_canvas(&scaled, &path, screen, oversample, kind, fill_screen);
                        assert_eq!(a.dimensions(), b.dimensions(), "{ctx}");
                        let psnr = psnr_rgb(&a, &b);
                        // Grain on every pixel makes this far harsher than a photo
                        // (real files measure 45-53 dB); a stretched or shifted
                        // buffer lands several dB below these bounds.
                        let floor = if fill_screen { 36.0 } else { 44.0 };
                        assert!(psnr > floor, "{ctx}: PSNR {psnr:.1} dB");
                    }
                }
            }
        }
    }

    /// A reduced decode planned for a small canvas must not be upscaled when
    /// the canvas has since grown: the worker re-decodes in full.
    #[test]
    fn matting_worker_redecodes_when_canvas_outgrows_reduced_decode() {
        use crate::config::MattingKind;
        use crate::tasks::loader::{CoverNeed, DecodeTarget, decode_rgba8_apply_exif};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.jpg");
        std::fs::write(&path, photo_jpeg(997, 749, 1, Flavor::Baseline420)).unwrap();
        let small = DecodeTarget {
            canvas_w: 240,
            canvas_h: 135,
            cover: CoverNeed::Never,
            bg_sample_scale: None,
        };
        let reduced = decode_rgba8_apply_exif(&path, Some(small)).unwrap();
        let full = decode_rgba8_apply_exif(&path, None).unwrap();
        assert!(reduced.image.width() < 997);

        // The surface settles larger: 960x540 now needs more than 249x187.
        for kind in [MattingKind::Studio, MattingKind::Blur] {
            let from_reduced = mat_canvas(&reduced, &path, (960, 540), 1.0, kind, false);
            let from_full = mat_canvas(&full, &path, (960, 540), 1.0, kind, false);
            assert_eq!(from_reduced.as_raw(), from_full.as_raw(), "{kind:?}");
        }

        // Still sufficient for the canvas it was planned for: left alone.
        let a = mat_canvas(&reduced, &path, (240, 135), 1.0, MattingKind::Studio, false);
        let b = mat_canvas(&full, &path, (240, 135), 1.0, MattingKind::Studio, false);
        assert!(psnr_rgb(&a, &b) > 34.0);
    }

    /// `fill-when-fits` must judge the original photo, not the reduced decode,
    /// including when the canvas is smaller than the screen (oversample < 1).
    #[test]
    fn fill_when_fits_uses_source_size_of_reduced_decode() {
        use crate::config::MattingConfig;
        use crate::tasks::loader::{CoverNeed, DecodeTarget, decode_rgba8_apply_exif};

        let matting: MattingConfig = serde_yaml::from_str(
            "fill-when-fits:\n  maximum-crop-percentage: 5.0\n  skip-matting-probability: 1.0\n\
             selection: fixed\nactive:\n  - kind: fixed-color\n    colors:\n      - [0, 0, 0]\n",
        )
        .unwrap();
        assert!(matting.fill_when_fits().is_some());

        // 16:9 photo, 1920x1080 screen, canvas halved by oversample 0.5.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.jpg");
        std::fs::write(&path, synthetic_jpeg(2000, 1125, 1)).unwrap();
        let target = DecodeTarget {
            canvas_w: 960,
            canvas_h: 540,
            cover: CoverNeed::Always,
            bg_sample_scale: None,
        };
        let photo = decode_rgba8_apply_exif(&path, Some(target)).unwrap();
        assert!(photo.image.width() < 2000, "expected a reduced decode");
        assert_eq!(photo.source_size, (2000, 1125));

        let mut mat_inflight = 0usize;
        let mut deferred_images = VecDeque::new();
        deferred_images.push_back(QueuedImage {
            image: PreparedImageCpu {
                path: path.clone(),
                width: photo.image.width(),
                height: photo.image.height(),
                source_size: photo.source_size,
                pixels: photo.image.as_raw().clone(),
            },
            priority: false,
            partner: None,
        });
        let mut ready_results = VecDeque::new();
        let (tx, mut rx) = crate::tasks::photo_channel::photo_channel(1);
        drop(tx);
        let mut mat_pipeline = MattingPipeline::new(1, 2);
        let mut bridge = MattingBridge {
            preload_count: 1,
            mat_inflight: &mut mat_inflight,
            deferred_images: &mut deferred_images,
            ready_results: &mut ready_results,
            from_loader: &mut rx,
            surface: Some(SurfaceState::new(1920, 1080, 4096)),
            matting: &matting,
            oversample: 0.5,
            // 1.0: the photo (2000 px) fills the 1920 px screen without upscaling,
            // but its 960 px reduced decode would look like it needs 2x.
            max_upscale_factor: 1.0,
            bar_ratio: 0.618,
            pairing: None,
            mat_pipeline: &mat_pipeline,
        };
        let mut wake = scenes::WakeScene::new(5_000, 5_000, TransitionConfig::default());
        bridge.queue_for_wake(&mut wake);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let result = loop {
            if let Some(result) = mat_pipeline.try_recv() {
                break result;
            }
            assert!(std::time::Instant::now() < deadline, "matting timed out");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert!(result.mat_kind.is_none(), "photo should have been filled");
    }

    #[test]
    fn matting_bridge_defers_until_surface_configured() {
        let mut mat_inflight = 0usize;
        let mut deferred_images = VecDeque::new();
        let prepared = PreparedImageCpu {
            path: PathBuf::from("/tmp/photo.jpg"),
            width: 100,
            height: 50,
            source_size: (100, 50),
            pixels: vec![0; (100 * 50 * 4) as usize],
        };
        deferred_images.push_back(QueuedImage {
            image: prepared,
            priority: false,
            partner: None,
        });
        let mut ready_results = VecDeque::new();
        let (tx, mut rx) = crate::tasks::photo_channel::photo_channel(1);
        drop(tx);
        let matting = MattingConfig::default();
        let mat_pipeline = MattingPipeline::new(1, 2);
        let mut bridge = MattingBridge {
            preload_count: 1,
            mat_inflight: &mut mat_inflight,
            deferred_images: &mut deferred_images,
            ready_results: &mut ready_results,
            from_loader: &mut rx,
            surface: Some(SurfaceState::new(1, 1, 4096)),
            matting: &matting,
            oversample: 1.0,
            max_upscale_factor: 1.0,
            bar_ratio: 0.618,
            pairing: None,
            mat_pipeline: &mat_pipeline,
        };
        let mut wake = scenes::WakeScene::new(5_000, 5_000, TransitionConfig::default());

        bridge.queue_for_wake(&mut wake);

        assert_eq!(mat_inflight, 0);
        assert!(wake.pending().is_empty());
        assert_eq!(deferred_images.len(), 1);
    }

    #[test]
    fn matting_queue_starts_after_initial_surface_config() {
        let mut harness = MattingQueueHarness::new(
            1,
            1.0,
            MattingConfig::default(),
            5_000,
            TransitionConfig::default(),
        );
        harness.update_surface_state(Some((3840, 2160, 4096)));
        harness.set_surface_configured(true);

        let gradient = make_gradient(800, 600).into_raw();
        harness.push_deferred(
            PreparedImageCpu {
                path: PathBuf::from("/tmp/photo.jpg"),
                width: 800,
                height: 600,
                source_size: (800, 600),
                pixels: gradient,
            },
            false,
        );

        harness.queue_once();

        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime for test");
        runtime.block_on(harness.wait_for_ready_results(std::time::Duration::from_secs(2)));
        harness.drain_pipeline();
        let canvases = harness.take_ready_canvases();
        assert!(
            !canvases.is_empty(),
            "expected matting to start after initial config"
        );
        assert_eq!(harness.deferred_queue_len(), 0);
    }
}
