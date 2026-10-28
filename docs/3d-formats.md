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
USDZ, the browser decode worker and retained material/UV rendering remain separate work.

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
