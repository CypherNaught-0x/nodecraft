#!/usr/bin/env bash
# Exact generated main + trusted worker inventory. Sourced after Trunk has built the site.
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
if grep -Eq 'integrity[[:space:]]*=' "$SITE/index.html"; then
  echo "error: stale integrity attributes are incompatible with post-Trunk wasm-opt" >&2; exit 1
fi
while IFS= read -r -d '' f; do
  relative="${f#"$SITE"/}"
  case "$relative" in
    index.html|nodecraft-web-*.js|nodecraft-web-*_bg.wasm|nodecraft-mesh-worker.js|nodecraft-mesh-worker_bg.wasm|nodecraft-mesh-worker_loader.js|snippets/*.js|*.png|README.md|LICENSE-MIT|LICENSE-APACHE|NOTICE|ATTRIBUTION.md) ;;
    *) echo "error: unexpected site artifact: $relative" >&2; exit 1 ;;
  esac
  case "$relative" in
    nodecraft-web-*.js) [ "$f" = "$MAIN_STEM.js" ] || { echo "error: extra main wrapper" >&2; exit 1; } ;;
    nodecraft-web-*_bg.wasm) [ "$f" = "${main[0]}" ] || { echo "error: extra main Wasm" >&2; exit 1; } ;;
  esac
done < <(find "$SITE" -type f -print0)
