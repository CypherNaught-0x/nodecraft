# 3D model files

Import a model file to place a **3D** source card, then connect **Convert 3D** to make a new
file. Choose its **Format** and run the card. The source stays available in its original
format; conversion results belong to the card's history and are saved in the project.

| Format | Import and preview | Convert 3D output |
|---|---|---|
| GLB | Binary glTF 2.0 | GLB, with geometry, normals and vertex colours |
| glTF | JSON with embedded resources; external files are refused | Choose GLB for conversion |
| OBJ | Geometry and supported vertex colours | OBJ, with geometry, normals and vertex colours |
| ASCII PLY | NodeCraft triangle profile, with normals and RGBA vertex colours | **PLY (ASCII RGBA)** |
| Binary STL | Triangle geometry, shown with flat normals and a neutral colour | **STL (geometry only)** |
| USDZ | NodeCraft static USDA triangle profile, with vertex normals and linear RGBA | **USDZ (static RGBA)** |

Saving an original result keeps its original bytes. Conversion reads the supported mesh
representation: embedded base-colour textures are sampled into vertex colours. Fine texture
detail, retained UVs and material structures require the remaining work in #569.

## Binary STL

STL keeps the triangle surface and coordinate values. It carries no unit label, colours,
materials, textures or per-vertex smoothing. Facet normals are recomputed from vertex winding;
the reader does the same. Zero-area triangles and non-finite numbers are refused. This is
geometry exchange, without a watertightness or print-preparation check.

NodeCraft reads and writes the binary profile with zero facet attributes. Colour extensions
and ASCII STL are refused with a named error; save ASCII files as binary STL before import.
Recognition checks the complete binary byte extent, without relying on the filename or a
particular 80-byte header. A binary header beginning with `solid` is valid.

The reader checks the complete file length and every facet before allocating geometry. STL
has three decoded vertices per triangle, including when the source shared vertices. Both
the writer and reader enforce the default decoded geometry limits: four million vertices
and four million triangles, with a 256 MiB encoded-file ceiling. Caller limits can reduce
these allowances. The Convert 3D action keeps its **16 MiB result limit** and checks the
predicted STL size before allocating its output. Larger conversion output needs a separate
storage integration; large generated originals remain available to save in their own format.

Binary STL results can be previewed, converted back to GLB/OBJ, saved, reopened and imported
as new 3D sources. Their filename extension is `.stl` and media type is `model/stl`.

This implements the STL portion of [#569](https://github.com/CypherNaught-0x/nodecraft/issues/569).
Broader USD profiles, the browser decode worker and retained material/UV rendering remain separate work.

## ASCII PLY

**PLY (ASCII RGBA)** writes PLY 1.0 with shared float32 positions, normalized float32 vertex
normals and four unsigned 8-bit colour channels. Import and preview read this same bounded
profile; results can be converted to GLB or OBJ, saved as their original bytes, reopened and
imported as new 3D sources. The canonical filename extension is `.ply`. NodeCraft uses
`model/ply` internally; this is **not an IANA-registered media type**.

The header must declare `vertex` then `face`, with properties in this order:
`float x/y/z/nx/ny/nz`, `uchar red/green/blue/alpha`, then one
`property list uchar int vertex_indices`. Each face must contain exactly three valid indices.
The reader also accepts the equivalent scalar spellings `float32`, `uint8`, `int32`, and the
singular `vertex_index`. Every property is required: binary PLY, reordered/missing/unknown
properties, extra elements, polygons and trailing data are refused. Comments are supported
only in the header. LF, CRLF and CR input line endings are accepted; output uses LF.

This is **NodeCraft's colour convention**, not a universal PLY colour-space guarantee:
RGB channels encode the mesh's linear RGB as sRGB, rounded to the nearest 8-bit value.
Alpha is unassociated linear coverage, independently rounded to 8 bits (at most half an
8-bit step of rounding error). Fully transparent RGB is retained, never premultiplied or
discarded. Reading converts sRGB to linear RGB and alpha to `value / 255`. Finite nonzero
normals are normalized with f64 intermediates. Non-finite coordinates/normals, zero normals
and colours outside 0–1 are refused on writing; input colours must be integers 0–255.
Coordinate values carry no physical unit label.

PLY retains the RGBA **channel data**. The current viewer is opaque, and the GLB converter
stores RGBA under an opaque material; neither promises visible transparency. OBJ conversion
keeps RGB and drops alpha. Textures, UVs, material structures and transparent-material
rendering remain separate #569 work, as does the existing texture-to-vertex-colour bake.

Before allocating geometry, the reader validates the entire header and body, counts, numeric
values, indices and exact end of input. It permits at most 64 header lines / 16 KiB, 1024 bytes
per line and 64 bytes per numeric/declaration token. Encoded files are limited to 256 MiB,
with four million vertices and four million triangles; caller limits may lower these caps.
The writer formats into a byte counter before allocating output. Convert 3D retains its
**16 MiB result limit**, including the complete formatted header, numbers and line endings.
No filesystem references or external resources are followed.

This implements [#1011](https://github.com/CypherNaught-0x/nodecraft/issues/1011), the PLY
portion of [#569](https://github.com/CypherNaught-0x/nodecraft/issues/569). Format sources checked
2026-10-28: [Greg Turk's original description](https://sites.cc.gatech.edu/projects/large_models/ply.html),
the `PLY_FILES.txt` documentation in the original author's
[PLY archive](https://sites.cc.gatech.edu/home/turk/ply.tar.gz), and the
[IANA media-type registry](https://www.iana.org/assignments/media-types).
Implementation and test geometry are original; no third-party decoder or assets are included.

## Browser previews

Browser 3D cards decode and prepare their GPU buffers on a dedicated Rust/Wasm worker. The
original remains in the asset store for saving, history and export. Preview availability does
not change what a conversion writes. The browser keeps its existing 16 MiB per-original and
48 MiB total resident-storage limits, with independent preview bounds: 600,000 decoded
vertices, 300,000 triangles and 900,000 unique wireframe edges. A file within 16 MiB can still
exceed those decoded bounds. Extra edges omit wireframe, as on desktop.

The browser profile admits up to four 1-megapixel base-color textures for vertex-color baking;
large textures fall back to the material/vertex color under the existing decoder policy. This
is a preview resource bound, not retained-texture or transparent-material rendering.

A missing, incompatible or timed-out worker produces a named preview failure while the rest
of the canvas remains editable. Reload retries the worker. Models no longer decode inline on
the browser UI thread. Main-thread transfer/validation is chunked; the final admitted GPU
upload still takes time proportional to model size. See the
[browser worker decision](decisions/2026-10-28-browser-mesh-worker.md) for ownership and resource
accounting, deployment and conformance coverage.

## Static USDZ

**USDZ (static RGBA)** reads and writes one self-contained USDA 1.0 Mesh layer in a USDZ
package. Shared vertices, triangle winding, float32 positions, normalized vertex normals
and linear unassociated float32 RGBA are retained. The extension is `.usdz` and the registered
media type is `model/vnd.usdz+zip`. Original imports and saved history results keep their
exact original bytes. Conversion makes a separate result and leaves its source unchanged.

This is a deliberately bounded profile, not a general USD scene loader. It requires one
root Mesh matching `defaultPrim`, `upAxis = "Y"`, `metersPerUnit = 1`, right-handed winding
and `subdivisionScheme = "none"`. The writer labels the existing unscaled coordinates with
this convention; it does not infer the source's physical units or transform its axes. Every
face must have exactly three indices. Required `points`, `faceVertexCounts`,
`faceVertexIndices`, `normals`, `primvars:displayColor` and `primvars:displayOpacity` fields
can appear in any order, with vertex interpolation for normals and colour/opacity. Metadata
can also be reordered. Numeric tuples and arrays accept ordinary whitespace, commas and
line comments. Unknown/duplicate fields, nonvertex interpolation, polygons, subdivision,
time samples, transforms, material/texture references, composition arcs and USDC are refused.
Save a compatible static mesh with the required attributes before importing it here.
LF, CRLF and CR input line endings are accepted; output uses LF. CRLF counts as one line
for the metadata limit, and comments end at any supported line ending.

RGB and alpha retain the existing Mesh's linear channel data without quantization or
premultiplication, including fully transparent RGB. The viewer and existing GLB material
remain opaque; this profile does not add transparent-material rendering. OBJ conversion
drops alpha, PLY rounds to its documented 8-bit convention, and STL drops colour. Retained
UVs, materials, textures and general USD scenes remain separate work under #569.

The package must contain exactly one safe root `.usda` entry, unencrypted and uncompressed
(STORE), with its data aligned to 64 bytes. Local/central records must agree; sizes, names,
offsets, CRC, padding and the exact archive end are validated. ZIP64, descriptors, additional
entries, path traversal, symlinks, unknown extra fields, comments and trailing bytes are
refused. No archive entry is extracted and no file/network reference is resolved.

The reader checks the complete container and closed grammar with borrowed tokens before
allocating geometry. Counts, numeric values, index ranges and derived bounds pass first;
then four fallibly allocated indexed arrays are filled and every triangle's nonzero area
is checked before adopting the mesh. Their total size is `40 × vertices + 12 × triangles`
bytes, at most 208,000,000 bytes with the default four-million vertex/triangle limits. Input
is at most 256 MiB; caller limits only lower these caps. Lines are at most 1024 ASCII bytes,
tokens 64 bytes, and layer metadata 16 KiB / 64 lines. Grammar nesting is fixed, with no
recursive scene traversal. Non-finite/unrepresentable numbers, zero normals, zero-area
faces and unrepresentable preview spheres are errors. Bounds use f64 intermediates to avoid
overflow when their final float32 sphere remains representable; other formats share that
arithmetic improvement, without changing the renderer or their admission policy.
USDZ refuses the entire mesh if even one triangle has zero area. A mesh that converts to
GLB, OBJ or PLY can therefore need its degenerate triangles removed before USDZ conversion.

A finite interchange mesh can still exceed the current preview camera's float32 numeric
range. Such a model reports **3D preview unavailable at this coordinate range** before
GPU upload. Its valid geometry, original coordinates and bytes remain available for saving
and conversion; they are never rescaled to make a preview. The same check guards the current
orbit before drawing, so Reset remains available if an extreme zoom makes a previously
usable view unavailable; capture buttons return when the view is usable again. This
capability check does not redesign the camera or shaders.
The shared photo, view-sheet and turntable executors also check their planned cameras before
GPU/encoder work and return an actionable coordinate-range error. CLI/MCP callers receive
that same refusal, rather than an empty image or video. Original 3D conversion stays available.

The writer validates geometry and counts the complete formatted layer plus ZIP headers,
alignment and footer before output allocation. Convert 3D retains its **16 MiB result cap**.
Larger original assets retain the existing project storage rules; USDZ adds no storage bypass.

This implements [#1019](https://github.com/CypherNaught-0x/nodecraft/issues/1019), one part of
[#569](https://github.com/CypherNaught-0x/nodecraft/issues/569). Primary sources checked
2026-10-28: [OpenUSD USDZ specification](https://openusd.org/release/spec_usdz.html),
[Mesh schema](https://openusd.org/release/api/class_usd_geom_mesh.html),
[Gprim colour/opacity schema](https://openusd.org/release/api/class_usd_geom_gprim.html),
and [IANA media type](https://www.iana.org/assignments/media-types/model/vnd.usdz+zip).
Original indexed test input was packaged and checked by Apple USD Tools 0.25.11; fixture
provenance is recorded beside it. General USD/USDC and retained material rendering remain open.
