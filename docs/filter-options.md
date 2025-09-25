# Photo Filter Options

This note documents a short list of photo filters that can be applied to a frame-ready
image after decoding and scaling but before it is sent to the display renderer. The
filters below favor deterministic, well-understood operations that can be implemented
with the existing `image` + `palette` ecosystem and should behave reliably without
per-filter tuning.

## Placement in the pipeline

1. Load and decode image bytes into an RGB(A) buffer.
2. Scale/crop to the frame's target dimensions.
3. Apply the selected filter (if any) to the linearized pixel data.
4. Convert back to display gamma and hand the buffer to the renderer.

All filters assume we work on floating-point linear RGB data to avoid compounding
quantization artifacts. When source data arrives in sRGB, convert to linear space before
processing and back to sRGB for display.

## Selection guidelines

The filters below were chosen to cover a mix of "nostalgic" and "clean" looks, while
keeping the algorithms:

- Deterministic: no stochastic grain unless it derives from a seeded generator.
- Efficient: all operations are either per-pixel transforms or involve small, separable
  convolutions.
- Portable: built only from primitives already available in `image`, `palette`, or
  easily expressible SIMD kernels.

## Candidate filters

### 1. Classic Sepia ("Vintage")
- **Look**: Warm, muted highlights with soft contrast reminiscent of early film prints.
- **Algorithm**:
  1. Convert to linear RGB.
  2. Apply a fixed 3x3 color transform matrix `[[0.393, 0.769, 0.189], ...]` to generate
     the sepia toning.
  3. Apply a gentle S-curve tone map (e.g., cubic Bezier) to compress highlights and lift
     shadows slightly.
  4. Optionally add a subtle vignette via a radial falloff mask multiplied into the RGB
     channels.
- **Notes**: Clamp after the matrix multiply; tone curve keeps values in-range.

### 2. Polaroid Fade
- **Look**: Cool shadows, warm highlights, lowered contrast, slight vignette.
- **Algorithm**:
  1. Convert to LAB; shift `b*` channel by `-3` in shadows and `+5` in highlights using a
     luminance-dependent blend to avoid color banding.
  2. Apply a matte curve: remap luminance `L*` with a low-contrast S-curve (lift blacks to
     ~10% and compress highlights to ~92%).
  3. Multiply by a vignette mask with smooth falloff (e.g., power of cosine).
  4. Convert back to RGB.
- **Notes**: LAB manipulations maintain perceptual uniformity; vignette mask is reusable.

### 3. Kodachrome Punch
- **Look**: Saturated mid-tones, crisp contrast inspired by Kodachrome 64 film.
- **Algorithm**:
  1. Convert to linear RGB.
  2. Increase mid-tone contrast with a tone curve derived from a parametric curve
     (e.g., Filmic `contrast=1.2, shoulder=0.22, toe=0.3`).
  3. Boost saturation by 10–15% in HSV/HSL space, but clamp luminance to avoid clipping.
  4. Apply a subtle local sharpening: unsharp mask using a separable Gaussian kernel
     (`sigma ≈ 1.0`, amount 0.5).
- **Notes**: Keep sharpening optional on already-sharp sources by thresholding the mask.

### 4. High-Key Black & White
- **Look**: Clean monochrome with bright mid-tones, minimal color cast.
- **Algorithm**:
  1. Convert to linear RGB.
  2. Map to luminance using Rec.709 coefficients (0.2126, 0.7152, 0.0722).
  3. Apply a high-key tone curve: raise shadows to ~15%, maintain highlights at 100%, and
     add a mild S-curve for contrast.
  4. Optionally add subtle simulated grain using blue-noise tiled texture seeded by the
     image hash for determinism.
- **Notes**: Grain overlay can be skipped if performance constrained.

### 5. Matte Portrait
- **Look**: Soft contrast, warm skin preservation, gentle roll-off in highlights.
- **Algorithm**:
  1. Convert to linear RGB, then to YCbCr.
  2. Apply a lift-gamma-gain curve on the Y channel with parameters (lift 0.08, gamma 0.95,
     gain 0.95) to introduce matte feel.
  3. Blend 20% of the original image back in to retain texture.
  4. For skin tones (Cb, Cr within preset range), slightly reduce desaturation to keep
     natural colors.
- **Notes**: Skin detection uses a simple, deterministic chroma box filter.

### 6. Lomo Pop
- **Look**: Deep vignette, high saturation, shifted cyan-magenta balance.
- **Algorithm**:
  1. Convert to HSL.
  2. Increase saturation by 25% and shift hue by +5° for mid-tones.
  3. Apply a strong vignette (radial mask exponent ~2.5) and lift the blue channel within
     the outer 20% radius to create the signature color cast.
  4. Apply a contrast curve that deepens shadows while protecting highlights.
- **Notes**: Hue adjustments are bounded to avoid wrap-around artifacts.

### 7. Cross-Process Sim
- **Look**: Mimics color cross-processing (E6 film in C41 chemicals) with green shadows and
  yellow highlights.
- **Algorithm**:
  1. Convert to LAB.
  2. Apply a custom tone curve to `L*` that boosts highlights and crushes shadows.
  3. Modify `a*` and `b*` using piecewise-linear mappings: shift `a*` negative in shadows,
     positive in highlights; shift `b*` positive across the range.
  4. Convert back to RGB and clamp.
- **Notes**: Piecewise-linear curves can be precomputed as 256-entry LUTs for speed.

### 8. Clean Boost
- **Look**: Slightly enhanced clarity without stylization; useful default enhancement.
- **Algorithm**:
  1. Perform white balance normalization using gray-world or simple channel gains.
  2. Apply a mild local contrast enhancement via bilateral filter (radius 2, sigma 30).
  3. Boost saturation by 5% and apply a neutral tone curve to protect highlights.
- **Notes**: Bilateral filter preserves edges while lifting local contrast; ensure the
     implementation is separable or approximated for speed.

## Implementation notes

- **Parameterization**: expose per-filter intensity (0–100%) to allow blending with the
  original image: `output = lerp(original, filtered, amount)`.
- **Performance**: precompute LUTs for tone curves and chroma shifts; reuse vignette masks
  per output resolution.
- **Testing**: add golden-image tests covering each filter at full intensity to guard
  against regressions.
- **Extensibility**: the filters all compose from shared primitives (color matrix, LUT,
  tone curve, convolution, vignette). Keep these helpers in a common module so new filters
  can leverage them without code duplication.

