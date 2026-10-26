## 2026-10-26 · Tabler outline control icons (#955)

Use **Tabler outline** for application controls. Its 24-unit view box, two-unit rounded strokes
and small, standalone SVGs fit the quiet canvas and can be scaled without adding a font.
The initial set serves Search, Comments, API keys, Budgets and the project Open/Save actions.
Only SVGs used by controls are vendored, unchanged, from commit
[`a4ce1404bc6d24d3c365afe7b258d6bf6f48d62d`](https://github.com/tabler/tabler-icons/tree/a4ce1404bc6d24d3c365afe7b258d6bf6f48d62d/icons/outline).
Keep the upstream MIT license beside them and list each added file in
[ATTRIBUTION.md](../../ATTRIBUTION.md).

The compared candidates are all usable permissive libraries:

| Library | License | Fit for this change |
| --- | --- | --- |
| [Tabler outline](https://github.com/tabler/tabler-icons/blob/a4ce1404bc6d24d3c365afe7b258d6bf6f48d62d/LICENSE) | MIT | One outline style and uniform geometry for the six controls needed now; chosen. |
| [Lucide](https://github.com/lucide-icons/lucide/blob/main/LICENSE) | ISC, with MIT notices for Feather-derived icons | Also suitable; keeping one MIT-licensed catalog makes the initial attribution simpler. |
| [Phosphor](https://github.com/phosphor-icons/core/blob/main/LICENSE) | MIT | Also suitable; its multiple weights are unnecessary for these outline controls. |

Reuse the already-pinned `craft_ui::icons::SvgIconSet` for the finite embedded catalog. Do not
fork its preparation code here. NodeCraft owns the enum, assets, labels and theme tokens.
`egui_extras`, with only its SVG feature, installs the synchronous embedded-SVG loader on native
and WebAssembly contexts. There is no file/HTTP loader or system-font lookup. Rasterization uses
egui's requested pixel size, so UI zoom and display density do not stretch a fixed bitmap.

SVG preparation substitutes a white mask once; visible colour is the `text` token applied at
draw time. `icon_size`, `icon_large_size` and `icon_stroke` are the shared size/stroke policy;
both themes use the same stroke. The six source names produce stable, application-specific URIs.
The loader retires unused size variants, and the source catalog never grows at runtime.

Prefer icons beside action text. The icon-only helper always supplies a Button accessibility
label and a tooltip; an empty caller label falls back to the icon's action name. This change
introduces the common path and initial controls. The coherent application menu and remaining
panel migration are tracked by #956 and #959.
