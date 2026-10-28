# 3D model files

Import a model file to place a **3D** source card, then connect **Convert 3D** to make a new
file. Choose its **Format** and run the card. The source stays available in its original
format; conversion results belong to the card's history and are saved in the project.

| Format | Import and preview | Convert 3D output |
|---|---|---|
| GLB | Binary glTF 2.0 | GLB, with geometry, normals and vertex colours |
| glTF | JSON with embedded resources; external files are refused | Choose GLB for conversion |
| OBJ | Geometry and supported vertex colours | OBJ, with geometry, normals and vertex colours |
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
PLY, USDZ, the browser decode worker and retained material/UV rendering remain separate work.
