## 2026-10-28 · Bounded ASCII PLY triangle/RGBA profile (#1011)

Add an original PLY 1.0 ASCII reader and writer through the existing Mesh, Convert 3D,
import/viewer, history, export and native storage seams. This implements only the PLY
portion of #569. USDZ, browser decode workers and retained UV/material rendering stay open.
Keep `.ply` canonical. Use `model/ply` as a NodeCraft convention, explicitly not an IANA
registration. Preserve binary STL recognition before PLY magic recognition so arbitrary
STL headers, including `ply` followed by a newline, retain their existing meaning.

Require one ordered vertex element containing float32 positions, nonzero normals and
uint8 RGBA, followed by one triangle face-index list. Emit traditional float/uchar/int type
names; read their float32/uint8/int32 aliases and vertex_index/vertex_indices spellings.
Refuse all other elements/properties, binary profiles and polygons. This deliberately narrow
profile avoids silently discarding data or guessing semantics. It does not support general
PLY interchange. Bound comments, header/line/token sizes and all counts. Check every body
value and index and the exact end of input before fallible owned geometry allocation.

Normalize finite nonzero normals with f64 intermediates. Interpret RGB as sRGB8, converted
from/to the existing linear mesh channels, and alpha as unassociated linear coverage8.
Round channels independently to the nearest integer and retain transparent RGB. This is
our profile's convention, not a claim that PLY universally standardizes colour space.
Refuse non-finite coordinates/normals and invalid channels. No unit, texture, material or
transparency-rendering semantics are invented. The existing opaque viewer and GLB material
remain unchanged; OBJ still drops alpha. These limits are documented in 3d-formats.md.

Count the complete formatted output before output allocation; use the same formatting in
both passes. Enforce default model geometry and 256 MiB encoded limits even if a caller
supplies larger limits. Convert 3D supplies its unchanged 16 MiB action-result allowance.
No new mutation route, storage privilege or external resource resolution is introduced.

Primary references checked 2026-10-28: [original format overview](https://sites.cc.gatech.edu/projects/large_models/ply.html),
`PLY_FILES.txt` from [Greg Turk's author-linked archive](https://sites.cc.gatech.edu/home/turk/ply.tar.gz),
and the [IANA registry](https://www.iana.org/assignments/media-types).
Only the archive's documentation and license were read; implementation and byte fixtures
are original. No third-party code, example mesh or vendor assets are copied.
