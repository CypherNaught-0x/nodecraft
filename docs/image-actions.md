# Image actions: CPU reference, version 1

`nodecraft_actions::image` supplies pure, fallible SDR image operations and typed parameter data.
They have stable library action IDs, not yet registered engine commands. Live graph recomputation
and command/undo integration belong to #88; GPU implementations compare with this reference in
#89. Shared descriptor generation belongs to #162. This document is the normative action-owned
inventory that that contract must express; it is not a second schema implementation.

## Inputs and entry points

`Raster` is immutable, tightly packed RGBA8 with straight alpha and sRGB RGB channels. It has
private dimensions and a private `Arc<Vec<u8>>`; clones share storage without copying pixels.
No mutable backing-buffer access is exposed. `new` admits an owned vector's actual capacity and
its 64-byte Arc/header allowance. `copy_from` reserves the borrowed source extent, a new copy and
that allowance before allocating. Backing-owner capacity outside a borrowed slice is unknown
and remains caller-owned. Exact byte count and nonzero dimensions are mandatory.

`ImageRequest::execute` borrows one `Raster` in the source role. `ImageInputs.overlay` must be
present exactly for `image.overlay`; base and overlay roles cannot be swapped by connection order.
`execute_views` accepts checked borrowed `RasterView`s, preflights the entire operation before
copying either input, and keeps the original slice extents charged through normalization and
execution. `execute_decoded` accepts borrowed `media::image::DynamicImage`s, checks their actual
buffer capacities and supports 8/16-bit gray, gray-alpha, RGB and RGBA. Normalization explicitly
quantizes 16-bit channels to 8-bit. Float/HDR images are unsupported in version 1.

`execute_encoded` probes before decoding, using action-derived media limits. It validates request
parameters, displayed dimensions, pixels, color model, output geometry, work and scratch before
pixel decode. Decoder reservations include expanded channels of at least 8 bits, a conservative
decoded-size orientation copy, RGBA normalization, the other retained input, and conversion lifetime
overlap. The public media probe does not expose orientation, so that copy cannot be omitted.
Header probing constructs a decoder under its initial allocation limit; it is not an allocation-
free parsing promise. Corrupt compressed data can still fail during the subsequent capped decode.
Caller-owned encoded buffers stay charged until the adapter returns. Decoder errors map to bounded
categories, with no codec message or filename echo: `InputLimit`, `DecodeLimit`, `UnsupportedFormat`
or `Decode`. Action-only size/working/work failures retain their own structured categories.

`from_json_bytes` rejects envelopes above 65,536 bytes before deserialization. Typed requests have
no arbitrary recursive JSON values or embedded pixel payloads. Direct serde users must apply the
same transport cap themselves. Programmatically constructed parameters still undergo the same
execution validation; finite values are required before casts, geometry or kernel construction.

All operations return one `ImageResult::Image`, except duplication returns one ordered
`ImageResult::Images` collection of exactly the requested count. Outputs are immutable; inputs
are unchanged. No action fans out paid jobs or performs provider calls. Connected/local precedence
belongs to the future adapter; the CPU library receives already resolved inputs.

## Color and alpha

Decode sRGB channels to linear light with the standard piecewise sRGB transfer. Alpha is linear
coverage. Color grade and tint formulas use unassociated linear RGB. Sampling, convolution and
source-over use premultiplied linear RGBA, then unpremultiply and encode once per stage. Clamp final SDR
channels to [0, 1], round to nearest byte; if quantized alpha is zero, newly computed RGB is zero.
Luminance is `0.2126 R + 0.7152 G + 0.0722 B`. The media pipeline currently assumes sRGB rather
than applying ICC profiles; this API does not claim HDR or color-profile fidelity.

Admitted identity paths and exact byte copies/permutations preserve every stored byte, including
hidden RGB at alpha zero and low-alpha colors. Computed operations may canonicalize hidden RGB.
A shortcut never bypasses active parameter validation or requested background compositing.

Source-over is `Ao = As + Ab (1 - As)` and premultiplied `Co = Cs + Cb (1 - As)`. Opacity scales
source alpha and premultiplied RGB together. Transparent saturated neighbors cannot add colored
fringes to interpolation. Fixed sample ordering makes serial/rayon results identical on one target;
reference fixtures permit at most two channel units across floating/trigonometric target paths.
Exact integer permutations remain byte-exact. Browser runtime parity is distinct from wasm compilation.

## Normative parameter contract

Each field below has stable ID `image.<operation>.<field path>`, omitting the wire `options`
wrapper. Nested records use their exact typed field paths, such as `image.key.matcher.tolerance`.
Selector `kind` values below are stable wire strings; display labels may use normal capitalization.
Numbers are finite `f64`; dimensions/counts are `u32`, offsets are `i32`. Suggested UI steps are
0.01 for fractions, 1 for integer pixels/counts and 1 degree for angles. Steps do not quantize
otherwise valid programmatic values. Colors are `{r,g,b,a}` integer bytes, RGB encoded sRGB.
Points are `{x,y}` finite numbers in [0, 1]. Target/duotone colors require alpha 255.

All active fields are wire-required. Missing and null do not mean a default, zero, false or auto.
The defaults below are valid creation values, not serde omission behavior. Unknown request,
option, conditional-mode fields and enum tags reject at this library boundary. Project-version
unknown-data preservation/migration remains #162/format responsibility. Tagged modes persist only their selected typed record; inactive variant drafts are view state.
Required settings in that record remain present and validated even when they do not currently
change the math (for example the exact-resize anchor outside cover mode). Hidden controls cannot
bypass validation.
There is no random seed, implicit coercion, dynamic preset lookup or callback/shader parameter.

All effective active settings, ordered input identities, CPU algorithm version and color rules
belong in future cache/staleness snapshots. Dimensions and copy count also determine output shape
and admission. Document/UI/CLI/MCP adapters must share #162's validation and mutation path rather
than treating this library inventory as already generated controls.

### Grade, filters, tint and keying

| Action and fields | Creation values and finite bounds | Meaning |
|---|---|---|
| `image.grade`: `warmth`, `brightness`, `contrast`, `saturation` | 0 in [-1,1]; 0 in [-4,4] stops; 1 in [0,4]; 1 in [0,4] | Warmth, exposure, contrast, saturation in that order |
| `image.filter`: `preset`, `intensity` | `{kind:"none"}`; 1 in [0,1] | Presets `none`, `grayscale`, `sepia`, `noir`, `duotone` |
| `filter.preset.shadow`, `.highlight` | Opaque black/white | Only the `duotone` variant, required RGBA colors |
| `image.tint`: `color`, `intensity`, `blend` | White; 0 in [0,1]; `normal` | Blend strength multiplied by tint alpha; preserve source alpha |
| `image.key`: `matcher`, `mode` | Green matcher; `remove_matched` | Alternative `keep_matched` |
| `image.replace_color`: `matcher`, `replacement`, `replace_alpha` | Green matcher; opaque black; false | Replacement alpha changes only when explicitly enabled |
| `matcher.target`, `.tolerance`, `.softness` | Opaque green; 0.1 in [0,1]; 0.1 in [0,1] | Same bounded selection math in key, replacement and target-color blur |

Warmth multiplies red by `2^(warmth/2)` and blue by `2^(-warmth/2)`, leaving green unchanged;
this is not a Kelvin white-balance claim. Exposure multiplies all channels by `2^brightness`.
Contrast computes `(RGB - .18) * contrast + .18`. Saturation computes `Y + saturation*(RGB-Y)`.
Clamp after the complete grade. Grayscale returns Y in all channels; sepia returns
`min(1, (1.15Y, Y, .8Y))`; noir returns grayscale of `clamp((Y-.18)*1.8+.18)`; duotone linearly
ramps between its opaque endpoints using Y. Intensity mixes original and preset RGB, preserving
alpha. These are original fixed NodeCraft definitions, not imported LUTs or another app's look.

Tint modes are `normal` (tint), `multiply` (s*t), `screen` (1-(1-s)*(1-t)), `overlay` (2*s*t for
s<=.5, otherwise 1-2*(1-s)*(1-t)), `darken` (min), `lighten` (max) and `difference` (abs(s-t)).
Mix with original by intensity times tint alpha, preserving original image alpha.

Matching uses linear RGB distance `sqrt(((R-Rt)^2+(G-Gt)^2+(B-Bt)^2)/3)`. Coverage is 1 at or
below tolerance, 0 at or above min(1, tolerance+softness), linear between; softness 0 is an
inclusive hard threshold, tolerance 1 matches all colors. Remove/keep multiplies original alpha
by 1-coverage/coverage. Replacement mixes unassociated RGB by coverage; preserve original alpha
unless `replace_alpha`, when it also mixes toward replacement alpha. No implicit despill,
inpainting or content synthesis is promised.

### Geometry, overlay and duplication

Coordinates start at the upper left, x right and y down; pixel centers are (x+.5, y+.5).
Angles are clockwise in displayed-image degrees. Aspect sizing uses integer rational cross-products,
not float ratio round trips. Crop the maximal fitting rectangle using floor, or pad the smallest
containing canvas using ceil, at least one pixel. Aspect error from integer sizing is at most one
pixel. Anchors distribute removed/added extent with nearest rounding, ties toward positive extent.

| Action and fields | Creation values and bounds | Conditional semantics |
|---|---|---|
| `image.aspect`: `ratio`, `mode`, `anchor` | `{kind:"source"}`, `{kind:"crop"}`, center | Ratio kinds `source`, `square`, `landscape4_3`, `portrait3_4`, `landscape16_9`, `portrait9_16`, `custom` |
| `aspect.ratio.numerator`, `.denominator` | 1 each, integers 1..16384 | Custom only; equivalent/source rational ratios preserve dimensions exactly |
| `aspect.mode.padding` | `{kind:"solid",color:transparent black}` | Pad mode only; crop has no padding fields |
| `aspect.mode.padding.color` | Transparent black | Solid only |
| `aspect.mode.padding.sigma` | 8; 0 or [1/256,64] pixels | `blurred` only: cover-resize then Gaussian background, preserving its alpha |
| `image.rotate`: `angle`, `expand`, `background`, `resampling` | 0 in [-360,360], true, transparent black, `bilinear` | `nearest` also allowed; area rejected |
| `image.flip`: `axis` | `horizontal` | `horizontal`, `vertical`, `both` |
| `image.crop`: tagged options `full`, `pixels`, `normalized` | `full` | Pixel x/y >=0 and width/height 1..16384; normalized x/y [0,1], positive width/height <=1, fully in bounds |
| `image.resize`: `size`, `resampling` | `{kind:"scale",factor:1}`, `bilinear` | Scale factor [1/64,64]; `nearest`, `bilinear`, `area` |
| `resize.size.width`, `.height`, `.fit`, `.anchor` | 1024 each, `stretch`, center | Exact size only: positive dimensions <=16384; `stretch`, `contain`, `cover` |
| `image.overlay`: `anchor`, `offset_x`, `offset_y`, `size`, `opacity`, `resampling` | `{kind:"center"}`, 0/0, relative width .25, 1, `bilinear` | Named roles source/base + overlay; fixed base canvas, outside content clips |
| `overlay.anchor` | `center` | Also `top_left`, `top`, `top_right`, `left`, `right`, `bottom_left`, `bottom`, `bottom_right`, or `custom` with `point` |
| `overlay.size` | `{kind:"relative_width",fraction:.25}` | Fraction [1/1024,4] of base width, or `source`, or `pixels` with width 256, range 1..16384; preserve overlay ratio |
| `overlay.offset_x`, `.offset_y`, `.opacity` | 0 pixels, 0 pixels, 1 | Offsets [-16384,16384], opacity [0,1] |
| `image.duplicate`: `count` | 2, integers 1..256 | Ordered shared immutable references; count zero is invalid |

Solid padding is a full canvas background with unchanged subject pixels source-over it, including
through subject transparency. Blurred padding does the same with its cover-scaled/blurred source
background and retained alpha. Version 1 materializes RGBA8 after cover resize and again after
Gaussian blur, then composites and encodes the final output; those two intermediate quantizations
are part of the reference semantics. Tilt-shift and target-color blur instead fuse Gaussian
filtering and their post-mix before encoding. A no-growth pad still composites; it is not
automatically identity.
Rotation source-overs the requested background everywhere, including through source transparency.
Zero/quarter-turn raw shortcuts require a transparent background; opaque backgrounds still apply.
Expansion uses pixel-edge bounds, with exact quarter-turn trigonometry to avoid phantom rows.

Normalized crop starts at floor(origin*size) and ends at ceil((origin+extent)*size); empty or
out-of-bounds rectangles reject. Resize rounds resulting dimensions nearest, minimum one. Contain
returns an image within the requested box, not padding; cover crops to the box using the anchor;
stretch changes ratio. Anchor is present/validated in the exact-size record; only cover uses it.
Nearest copies original bytes. Bilinear and area use premultiplied linear RGBA. Area integrates
pixel coverage on reduced axes and uses bilinear weights on enlarged axes, independently for
mixed-axis resize; it does not round intermediate samples to bytes. Overlay uses the same rules.
Watermark means overlaying a second image; no text/font rendering or file naming heuristic.

### Complete blur inventory

`image.blur.options.kind` selects exactly one of eight families. All controls below are active
and required only in that tagged variant. Gaussian sigma accepts exactly zero for identity or
[1/256,64] pixels. Positive sigmas/falloffs have explicit lower bounds so tiny f64/subnormal inputs
cannot become zero when narrowed or squared. Ratios for exponential weights are evaluated in f64;
center taps are explicit. Invalid controls reject even when another control makes the effect identity.

| Kind | Valid creation values and limits | CPU definition |
|---|---|---|
| `gaussian` | sigma 2 | Separable Gaussian; radius ceil(3*sigma), at most 192; 385 taps |
| `box` | radius 2, integer [0,128] | Uniform square; horizontal/vertical running sums |
| `motion` | length 8 in [0,256] pixels, angle 0 in [-360,360], samples 17 in [1,129] | Uniform line centered on each pixel; endpoints included; one sample means center |
| `radial` | mode `spin`, center (.5,.5), amount 0 in [0,1], samples 17 in [1,129] | `spin` averages angles -90*amount..90*amount; `zoom` averages radial factors 1-amount..1; one sample uses midpoint |
| `bilateral` | radius 3 in [0,8], spatial_sigma 2 in [1/256,8], range_sigma .1 in [1/65536,1] | Spatial Gaussian times original unassociated-linear-RGB guide Gaussian, at most 289 neighbors; transparent guide RGB is zero |
| `bokeh` | radius 4 in [0,64] pixels, samples 25 in [1,129], aperture `disk` | Deterministic uniform disk/`hexagon` samples, finite lens-style approximation |
| `tilt_shift` | sigma 4, focus (.5,.5), angle 0 in [-180,180], band_width .25 in [0,1], falloff .1 in [1/65536,1] | Gaussian mixed outside a focused band, perpendicular distance in shorter-side units |
| `target_color` | sigma 2, green matcher, selection `matched` | Mix original/Gaussian in premultiplied space using original-color selection; `unmatched` inverts it |

All use clamped-edge normalized nonnegative weights and premultiplied linear RGBA. Radius/sigma,
length or amount zero, where defined, is identity. Bokeh sample zero is the center; for i=1..N-1,
angle is i*pi*(3-sqrt(5)), radial fraction sqrt((i-.5)/(N-1)); hexagon scales that radius by
cos(pi/6)/cos(angle mod (pi/3)-pi/6). Equal weights, no random state. Bilateral does not modify its
guide during traversal. Tilt band is centered at focus, no blur inside band_width/2, then a
linear ramp to full blur across falloff. Target-color masks are not recomputed from blurred pixels.

## Limits, CPU execution and validation

Defaults are hard ceilings; `ImageLimits` can only lower them. Per image: edge 16384, pixels
16,777,216. At most two named inputs, aggregate input capacity 128 MiB. Encoded inputs are limited
to 64 MiB each. Aggregate logical output bytes are 256 MiB, count 256. Total per-operation working
storage is 256 MiB, including normalization copies, outputs, Arc/collection allowance, coefficient
buffers and all row-ring/column scratch. Source owners not visible through a borrowed view remain
outside that extent-based bound; separate executors/caller-retained outputs need host admission.
This is not an allocator-exact or global process-memory guarantee.

Work is capped at 1,000,000,000 sample/neighbor visits, counting bilinear's four taps, separable
passes and compound stages. Preflight precedes output allocation. Gaussian and box use bounded
row rings rather than full-frame float copies; box uses running sums. Worker callbacks allocate no
per-pixel buffers; all concurrent scratch is reserved together. Duplicate shares physical storage
but charges N times logical RGBA bytes plus collection metadata before creating references.
A too-expensive request returns `WorkLimit`; it never silently substitutes a cheaper algorithm.
Errors return no partial output. Actual allocated output capacity is checked, not assumed from
`shrink_to_fit`. Small-budget tests prove preflight using allocation counters rather than huge images.

`CpuExecutor::parallel(1..=8)` is native-only and fallibly creates a private rayon pool; failure is
`Executor`. Callers can choose explicit serial fallback. No implicit global pool or image feature
spawns threads. Operations borrow `&mut CpuExecutor` synchronously; kernels parallelize disjoint
pixels with fixed within-pixel sample order. Wasm compiles the same serial kernels without rayon.
Thread-pool lifetime and aggregate concurrent executor/output storage belong to the host.

Fixtures are original authored arrays and `crates/actions/tests/fixtures/image_requests.json`.
Analytic tests cover color/alpha, exact bytes, rational geometry, mixed-axis area, background
composition, direct box/Gaussian oracles, all blur families, capacities, subnormal rejection,
request syntax and adapters. Native serial/rayon tests compare exact bytes. The ignored
`image::tests::write_contact_sheet` writes a public-safe original review image under `/tmp`, not
into the repository; fixture creation needs no downloaded assets or attribution from another app.


## GPU library backend (#89)

`nodecraft_gpu::image::GpuExecutor` executes the same 13 typed request families, including all
eight blur variants, on wgpu. It accepts the canonical immutable `Raster`/`ImageInputs` and
returns the same `ImageResult`. This is a usable headless library backend; engine command/live
recompute integration remains #88. The GPU API does not currently add encoded/decoded/view
normalization adapters. Media decoding and SDR normalization remain CPU responsibilities.

`request()` acquires a fresh compatible adapter and matching device/queue together.
`request_with_options(GpuOptions)` selects enabled backends, power preference and whether a
fallback adapter is required, while still requesting its own fresh adapter. Required limits and
features are checked before device creation. Arbitrary externally supplied adapters/pairs are
deliberately unsupported: caller history and pair identity cannot be proven at this boundary. The executor exposes cloned device/queue handles for
hosts that share the created pair. Import of an existing eframe-owned pair is not implemented.
No adapter, unsupported limits, device failure, mapping failure and admission refusal are errors;
there is no silent CPU fallback. Identity and duplicate results intentionally share CPU storage.

Execution is async. Native hosts acquire a cloneable device-only `poll_handle()` **before**
borrowing the executor in `execute`, then drive its nonblocking poll from their event loop.
Polling the shared original device is an alternative. The native `image_actions` example shows
a ten-second completion deadline and one-millisecond timer schedule, without a background thread
or busy loop. The library never sleeps or blocks waiting for GPU progress. Browser WebGPU uses
its event loop; compiling wasm does not prove browser runtime parity. No WebGL compute claim is
made. A dropped execution future retains its single pending submission in the executor; another
operation is refused until completion. Device loss and failed initial upload poison the executor;
recreate it explicitly rather than repeatedly submitting to a dead device. Ordinary admission
failures do not poison it. Mapping cleanup and synchronous create/submit/map calls
are scoped so driver validation errors cannot reach wgpu's default panic handler.

This is a **hybrid** implementation. Shared action-owned f64 preparation preserves matcher hard
thresholds, final quantized Key/Replace alpha, rational geometry and nearest sample decisions.
The GPU performs color transforms, composition, sampling and convolution. Optional decision
planes are explicitly charged; no CPU-rendered final operation is uploaded under a GPU label.
Computed transparent pixels have zero RGB; admitted raw permutations preserve hidden RGB.
Arbitrary nearest rotation is computed; only the reference's eligible quarter-turn shortcut is
raw. Blurred padding keeps the reference's two intermediate RGBA8 quantizations.

Common input/output/work validation runs before proportional allocation. Extra CPU preparation,
LE packing, GPU input/output/readback, decision planes, stage controls and bounded scratch share
`ImageLimits`. Defaults remain hard maxima. Separable effects choose the largest output stripe
height in 1..=128 that fits the **whole** memory/device/work plan, including global halos and
repeated horizontal work. At most 128 allocation-free candidates are checked. Horizontal floats
retain the CPU reference's f32 stage; box sums restart at each stripe's global origin. Blurred
padding stores an immutable packed cover stage, writes quantized blur into the output, then
composites only after all stripes finish. Later halos never read already-blurred cover pixels.
Area resampling uses bounded hierarchical block sums rather than one million-term f32 chain.

The GPU's footprint can exceed CPU row-ring storage. Full packed buffers, device binding limits,
and minimum halos can legitimately refuse CPU-admitted requests; no promise that every maximum
CPU-sized image fits every GPU is made. The bound covers logical requested buffers and visible
host capacities, not allocator pages, driver bookkeeping, GPU register storage or global memory
across multiple executors/caller-retained results. Shared CPU scratch is conservatively reserved
in addition to GPU scratch. Counters distinguish actual preparation/packing, encoded dispatches
and completed readbacks; an encoded invalid submission is not proof of completed GPU work.

Exact paths/decisions and output shape are tested exactly. Established opaque/well-conditioned
fixtures retain maximum straight-RGBA byte error two. Only explicitly marked adversarial samples
with `min(CPU alpha, GPU alpha) <= 2` may instead use maximum alpha error `2/255` and maximum
per-channel **premultiplied linear RGB** error `2/255`, calculated from returned bytes. Their
straight-channel maxima are reported. This exception never waives hard decisions, raw-byte
contracts or computed zero-alpha canonicalization. These are tested tolerances, not a universal
precision proof across f32 backends. There is no HDR/ICC or speedup claim.

Actual-device tests are explicitly ignored in ordinary CI: run
`cargo test -p nodecraft-gpu actual_gpu_ -- --ignored --nocapture` on a compatible adapter.
Unavailable hardware fails that run, rather than passing as GPU validation. Pure admission and
stripe arithmetic tests run normally. Record adapter/backend, real dispatch/readback counts,
observed errors and platforms actually executed with any result report.
