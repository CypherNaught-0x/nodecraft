## 2026-10-28 · Bounded static USDA/USDZ triangle profile (#1019)

Add an original bidirectional USDZ profile through the existing Mesh, Convert 3D, viewer,
history/export and project storage routes. Keep #569 open: this is not general USD/USDC,
retained UV/material rendering or the browser worker. No external USD runtime, C++ or unsafe
code is added. Apple USD Tools are independent local test tools, never product dependencies.

Use one unencrypted STORE ZIP32 entry containing USDA 1.0. Its data starts on a 64-byte
boundary; a bounded zero-filled 0x1986 padding extra field provides alignment. Check both
record authorities, CRC, names, offsets, sizes, ordinary-file attributes and exact end.
Reject ZIP64, descriptors, compression, multiple files, paths, symlinks and unknown extras.
Use the registered model/vnd.usdz+zip MIME type and an explicit .usdz export mapping. Keep
binary STL's arbitrary-header recognition ahead of USDZ recognition. No filesystem extraction
or resource loading occurs. Reuse the existing pinned permissive crc32fast checksum crate.

The closed USDA grammar contains one Mesh/defaultPrim with Y-up/unit-meter metadata,
rightHanded orientation, subdivisionScheme none, indexed triangle counts/indices, float32
positions/unit vertex normals and vertex displayColor/displayOpacity. All fields are required;
metadata and properties may be reordered. Reject all other schema/composition, transforms,
resources, time samples, interpolation and USDC rather than silently dropping content.
The unit/axis declaration labels unscaled coordinates; it does not infer source physical units.
Linear unassociated RGBA is retained, including transparent RGB. Existing opaque rendering,
GLB's opaque material, PLY's quantization and OBJ/STL fidelity limits remain unchanged.

First parse the entire bounded container/grammar using borrowed ASCII tokens and constant
metadata. Validate counts, finite numbers, attribute lengths, index ranges and derived bounds
before any owned geometry. Then fallibly reserve exactly the admitted indexed arrays, fill
them and validate every triangle area before adoption. Peak geometry is 40V+12T bytes, at
most 208,000,000 bytes under the unchanged default 4M/4M geometry caps. Encoded input/output
remain capped at 256 MiB, and callers can only lower limits. Lines/tokens/metadata are bounded;
grammar depth is fixed and there is no recursive scene graph. Count the full layer formatting
and 142-byte writer package overhead before output allocation; Convert 3D still passes 16 MiB.

Use f64 differences/cross products for facet validation and normal normalization. Improve
shared Bounds center/radius intermediates to f64 without changing their public signatures
or other formats' admission policy. Add checked_sphere for this profile's explicit rejection
of a nonfinite or float32-unrepresentable preview sphere; do not clamp an incorrect bound or
redesign the renderer. Large finite representable bounds no longer overflow only because an
intermediate f32 square/sum did. An extreme unrepresentable span remains refused by USDZ.

Primary references checked 2026-10-28: [USDZ specification](https://openusd.org/release/spec_usdz.html),
[Mesh schema](https://openusd.org/release/api/class_usd_geom_mesh.html),
[Gprim schema](https://openusd.org/release/api/class_usd_geom_gprim.html),
and [IANA registration](https://www.iana.org/assignments/media-types/model/vnd.usdz+zip).
Implementation and fixtures are original. Independent input is authored as USDA, packaged and
checked by installed Apple USD Tools 0.25.11, with version/commands/hashes beside the fixture.
Generated output requires independent tool validation as well as self-roundtrip tests.
