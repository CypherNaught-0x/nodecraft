# Original USDZ fixture

`original-indexed.usda` was authored for #1019 from the public OpenUSD Mesh/Gprim/USDZ
documentation. It is an asymmetric shared-index square with four vertices, two triangles,
unit normals and four distinct linear RGBA values. No vendor code or asset is copied.
Its deliberately reordered metadata/properties test semantic field admission.

Installed independent tools report **Apple USD Tools (0.25.11)** on 2026-10-28.
The original source was named `/tmp/nodecraft1019-indexed.usda` when packaged. From `/tmp`:

```sh
usdchecker --strict nodecraft1019-indexed.usda
usdzip --checkCompliance nodecraft1019-indexed.usdz nodecraft1019-indexed.usda
usdchecker --strict nodecraft1019-indexed.usdz
```

All checks passed. The checked source/package are preserved under `original-indexed` names.
The package still contains its original safe root `nodecraft1019-indexed.usda` name.

| File | SHA-256 |
|---|---|
| original-indexed.usda | 43cf42614c004b2aa0ef9b62d028c7ce06f4ba2370f71324c8505d19ef3ba0be |
| original-indexed.usdz | bba6dd27416dbb25f9f6a0eba8ff9f087da60ca981cda738e0b784353478f112 |

The NodeCraft writer is separately exercised against these same installed independent tools;
its archive is not the source of this independent reader fixture. The tools are not bundled.

The ignored `usdz1019_independent_output_samples` test wrote `indexed.usdz` and
`reencoded-independent.usdz` from the NodeCraft encoder. Both are deterministic and have
SHA-256 `8261a3ff877ff39ce7be582e98b1762ab9f586f7fc94f66dfd30101ce59efdea`.
Apple USD Tools 0.25.11 accepted both with `usdchecker --strict`; `usdcat --loadOnly`
accepted `indexed.usdz`, and `usdtree` reported its single root Mesh. These generated proof
outputs are intentionally not committed.
