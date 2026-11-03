#!/usr/bin/env bash
# Exact generated main + trusted worker inventory. Sourced after Trunk has built the site.
if [ -n "$(find "$SITE" ! -type d ! -type f -print -quit)" ]; then
  echo "error: generated site contains a link or special file" >&2; exit 1
fi
shopt -s nullglob
wasm=("$SITE"/*.wasm)
main=("$SITE"/nodecraft-web-*_bg.wasm)
shopt -u nullglob
if [ "${#wasm[@]}" -ne 2 ] || [ "${#main[@]}" -ne 1 ] || [ ! -f "$SITE/nodecraft-mesh-worker_bg.wasm" ]; then
  echo "error: expected exactly main and mesh-worker Wasm artifacts" >&2; exit 1
fi
MAIN_STEM="${main[0]%_bg.wasm}"
for expected in "$MAIN_STEM.js" "$SITE/nodecraft-mesh-worker.js" "$SITE/nodecraft-mesh-worker_loader.js"; do
  if [ ! -f "$expected" ]; then echo "error: missing generated loader/wrapper: $expected" >&2; exit 1; fi
done
if ! grep -Fq "from './nodecraft-mesh-worker.js'" "$SITE/nodecraft-mesh-worker_loader.js" ||
   ! grep -Fq "new URL('nodecraft-mesh-worker_bg.wasm', import.meta.url)" "$SITE/nodecraft-mesh-worker.js"; then
  echo "error: worker shim/wrapper do not resolve their adjacent generated artifacts" >&2; exit 1
fi
if ! grep -Fq "./${MAIN_STEM##*/}.js" "$SITE/index.html" ||
   ! grep -Fq "./${main[0]##*/}" "$SITE/index.html"; then
  echo "error: main HTML does not reference its generated wrapper and Wasm" >&2; exit 1
fi
# The icon is unchanged by packaging: preserve and verify its generated SRI. Only the
# generated main/worker Wasm are rewritten, so every other integrity attribute is refused.
html="$(cat "$SITE/index.html")"
icon_tag="$(grep -oE '<link[[:space:]][^>]*rel="icon"[^>]*>' "$SITE/index.html" || true)"
case "$icon_tag" in ''|*$'\n'*) echo "error: expected one generated icon reference" >&2; exit 1 ;; esac
icon_path_pattern='href="\./([[:alnum:]_.-]+\.png)"'
icon_hash_pattern='integrity="sha384-([A-Za-z0-9+/]{64})"'
if [[ ! "$icon_tag" =~ $icon_path_pattern ]]; then echo "error: icon must reference an adjacent PNG" >&2; exit 1; fi
icon_name="${BASH_REMATCH[1]}"
if [[ ! "$icon_tag" =~ $icon_hash_pattern ]]; then echo "error: icon integrity is missing or unsupported" >&2; exit 1; fi
icon_expected="${BASH_REMATCH[1]}"
if [ ! -f "$SITE/$icon_name" ]; then echo "error: referenced icon is missing" >&2; exit 1; fi
if ! command -v openssl >/dev/null 2>&1; then echo "error: openssl is required to verify unchanged icon integrity" >&2; exit 1; fi
icon_actual="$(openssl dgst -sha384 -binary "$SITE/$icon_name" | openssl base64 -A)"
if [ "$icon_actual" != "$icon_expected" ]; then echo "error: icon integrity does not match its referenced asset" >&2; exit 1; fi
without_icon="${html/"$icon_tag"/}"
if grep -Eq 'integrity[[:space:]]*=' <<< "$without_icon"; then
  echo "error: stale integrity attributes are incompatible with post-Trunk wasm-opt" >&2; exit 1
fi
# wasm-bindgen emits snippet paths as quoted imports in its adjacent wrappers. Keep
# their original relative layout, and do not pick up unrelated files in snippets/.
snippets="$({ grep -hoE "'\./snippets/[[:alnum:]_./-]+\.js'" "$MAIN_STEM.js" "$SITE/nodecraft-mesh-worker.js" || true; } | sed "s@^'\./@@;s@'\$@@" | sort -u)"
while IFS= read -r snippet; do
  [ -n "$snippet" ] || continue
  case "/$snippet/" in *'/../'*|*'/./'*|*'//'*) echo "error: invalid snippet path: $snippet" >&2; exit 1 ;; esac
  if [ ! -f "$SITE/$snippet" ]; then echo "error: missing generated snippet: $snippet" >&2; exit 1; fi
done <<< "$snippets"
while IFS= read -r -d '' f; do
  relative="${f#"$SITE"/}"
  case "$relative" in
    index.html|"${MAIN_STEM##*/}.js"|"${main[0]##*/}"|nodecraft-mesh-worker.js|nodecraft-mesh-worker_bg.wasm|nodecraft-mesh-worker_loader.js|"$icon_name"|README.md|LICENSE-MIT|LICENSE-APACHE|NOTICE|ATTRIBUTION.md) ;;
    snippets/*.js) grep -Fxq -- "$relative" <<< "$snippets" || { echo "error: unreferenced snippet: $relative" >&2; exit 1; } ;;
    *) echo "error: unexpected site artifact: $relative" >&2; exit 1 ;;
  esac
done < <(find "$SITE" -type f -print0)
