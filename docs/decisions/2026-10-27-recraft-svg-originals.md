## 2026-10-27 · Recraft and bounded SVG originals (#35)

Use the existing fal queue for Recraft V4.1 raster and vector generation. Both are Image
outputs; `image/svg+xml` identifies the vector original without changing the graph modality
or project-file version. Quotes use the dated standard per-image rates ($0.035/$0.08), one
image per job. The fal schemas do not expose the native API's seed or image-count controls.

An SVG asset retains its exact bytes, hash and typed MIME through history, extraction,
save/reopen and original export. Import and provider results pass the same L0 SVG admission;
project/reference verification rechecks that authority. SVG is never relabelled PNG and
does not claim stored raster bit-depth/colour-channel metadata. Raster-only tools require the
explicit **Rasterize SVG** local action, which emits an ordinary PNG through the run host.

Admission caps encoded bytes at 16 MiB, XML nodes at 65,536, depth at 64, attributes at 64
per element/262,144 total and parsed path/point/dash entries at 250,000. Intrinsic geometry
is bounded to 16,384 per axis and 16,777,216 pixels; caller limits may lower it. Logical
document work is capped at 128 MiB. These are logical operation limits, not allocator/RSS
claims. Reference expansion is counted separately with cycle/depth limits before rendering.

Use exact resvg/usvg 0.45.1 with default features disabled and permissive dependencies.
Parse XML with DTD disabled and feed `Tree::from_xmltree`; the convenience string parser
enables DTDs. Install deny resource resolvers: the renderer's default string resolver can
read local files even with no resources directory. Reject scripts, events, animation,
foreign content, processing instructions, external/font resources and CSS escape/at-rule
tricks in the retained original. No system fonts or resource files are read.

Native previews render directly at cached size buckets on the existing worker. Preview
work has narrower node/path/pixel-pass and scratch budgets. Unsupported static text/fonts,
stylesheets, filters, patterns and embedded images retain their safe original and display
an explanatory unavailable state. Rasterization also refuses them rather than silently omit
their visual content. This initial policy does not promise complete SVG 2 rendering.
Browser builds retain/export SVG but do not render it on the frame thread; a small encoded
file can still carry expensive vector work. Existing raster previews remain available.

Reuse fal's canonical saved-task routes, no ambiguous-create replay and credential-free
bounded downloads. Recraft delivery additionally admits the exact documented falserverless
Google Storage bucket; it does not widen other models' or style-weight origins. Live paid
generation is opt-in and was not used as validation. See [provider setup](../providers.md#recraft-through-fal)
for the supported model subset and rates.
