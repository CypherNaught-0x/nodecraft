## 2026-10-28 · Bounded binary STL geometry for Convert 3D (#569)

Add an original binary STL reader and writer to standalone media, then use the existing
Convert 3D action, import, viewer, history, save/reopen and original-result export paths.
Keep `model/stl` and `.stl` canonical. This is one portion of #569; it does not complete the
parent's PLY/USDZ, browser-worker or retained material/UV requirements.

The supported profile is an 80-byte header, a little-endian 32-bit facet count, and exactly
50 bytes per facet: twelve IEEE single-precision values and zero attribute bytes. Accept
arbitrary binary headers, including `solid`, without trusting extensions. Recognize an
ordinary ASCII STL header sufficiently to report that the ASCII profile is unsupported.
Refuse unknown facet attributes/colour extensions, trailing or truncated bytes, empty
geometry, non-finite numbers and zero-area facets.

STL is geometry only. Preserve coordinate values and winding, without inventing physical
units. Recompute flat normals with f64 intermediates so finite f32 coordinates do not
overflow/underflow the cross product. Reading supplies neutral vertex colours and expands
each facet to three vertices. Result metadata reports those output counts, rather than the
shared source's counts. No watertightness, topology repair or manufacturing validity is claimed.

Validate counts and every facet before allocating decoded geometry; use fallible reserves.
Writing admits the checked `84 + 50 * facets` extent and all facet geometry before its output
allocation. Generated files must fit the default model byte/decoded-vertex/triangle limits,
even when a caller supplies a larger output allowance. Convert 3D additionally supplies the
existing 16 MiB action-result allowance. No global asset or conversion cap is raised, and
the existing texture-to-vertex-colour decision is unchanged.

Sources checked 2026-10-28: the [IANA registration](https://www.iana.org/assignments/media-types/model/stl)
defines the canonical type and extension and references the 1989 3D Systems specification.
[Marshall Burns's format documentation](https://www.fabbers.com/tech/STL_Format) describes
the facet representation, winding, lack of scale metadata and zero attribute-byte convention.
Implementation and fixtures are original; no third-party decoder, assets or diagrams are used.
