#!/usr/bin/env bash
# Build and package NodeCraft for the browser (preview, #99):
#
#   $DIST/nodecraft-web-<version>.zip   a static site: index.html, the JS loader wasm-bindgen
#                                       generates, the .wasm and the licence files. Serve the
#                                       unzipped folder from any static host (any path).
#
# Usage: packaging/web/package.sh [--skip-build] [--serve]
#
# Needs: the wasm32-unknown-unknown target, trunk and wasm-bindgen-cli at the version in
# Cargo.lock (trunk runs offline, so it never downloads tools), zip. Optional: wasm-opt
# (binaryen) shrinks the .wasm further; without it the script warns and ships it as built.
# --serve instead runs `trunk serve` on http://127.0.0.1:8080: a debug-profile development
# build (no wasm-opt, no zip) that rebuilds on change; --skip-build does not apply to it.
#
# Every file must fit the per-file cap of common static hosts (MAX_FILE_BYTES, 25 MiB:
# Cloudflare Pages; GitHub Pages and Netlify allow more). The script fails if one does not.

set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/apps/nodecraft-web"
# trunk runs in $HERE, so relative paths (AGENTS.md suggests `target/agent-<name>`) would
# resolve there instead of where the caller meant them.
absolute() { case "$1" in /*) printf '%s' "$1" ;; *) printf '%s/%s' "$PWD" "$1" ;; esac; }
CARGO_TARGET_DIR="$(absolute "$CARGO_TARGET_DIR")"
DIST="$(absolute "$DIST")"
export CARGO_TARGET_DIR DIST
MAX_FILE_BYTES="${MAX_FILE_BYTES:-26214400}"

SKIP_BUILD=0
SERVE=0
while [ $# -gt 0 ]; do
  case "$1" in
    --skip-build) SKIP_BUILD=1; shift ;;
    --serve) SERVE=1; shift ;;
    -h | --help) sed -n '2,16p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

BASENAME="nodecraft-web-$VERSION"
SITE="$CARGO_TARGET_DIR/web-package/$BASENAME"
echo "==> NodeCraft $VERSION for the web"

if [ "$SERVE" = 1 ]; then
  (cd "$HERE" && exec trunk serve --offline --locked --dist "$CARGO_TARGET_DIR/web-serve" --address 127.0.0.1 --port 8080)
  exit 0
fi

if [ "$SKIP_BUILD" = 0 ]; then
  rm -rf "$SITE"
  # A relative public URL lets the site live under any path (a project page, a subfolder).
  (cd "$HERE" && trunk build --offline --locked --release --dist "$SITE" --public-url ./)
fi
if [ ! -f "$SITE/index.html" ]; then
  echo "error: no build in $SITE (run without --skip-build)" >&2
  exit 1
fi

shopt -s nullglob
wasm=("$SITE"/*.wasm)
shopt -u nullglob
if [ "${#wasm[@]}" -ne 1 ]; then
  echo "error: expected one .wasm in $SITE, found ${#wasm[@]}" >&2
  exit 1
fi
if [ "$SKIP_BUILD" = 1 ]; then
  echo "wasm-opt: skipped (--skip-build reuses the already optimised site)"
elif command -v wasm-opt >/dev/null; then
  before="$(wc -c <"${wasm[0]}" | tr -d ' ')"
  # The features rustc enables by default for wasm32, so wasm-opt accepts the module.
  wasm-opt -Oz --enable-bulk-memory --enable-mutable-globals --enable-nontrapping-float-to-int \
    --enable-sign-ext --enable-reference-types --enable-multivalue \
    -o "${wasm[0]}.opt" "${wasm[0]}"
  mv "${wasm[0]}.opt" "${wasm[0]}"
  echo "wasm-opt: $before -> $(wc -c <"${wasm[0]}" | tr -d ' ') bytes"
else
  warn "wasm-opt (binaryen) not found: the .wasm ships unoptimised by it"
fi

copy_docs "$SITE"
# A failed run below must not leave an earlier zip looking like this build's.
ZIP="$DIST/$BASENAME.zip"
rm -f "$ZIP"

# Every file under the host cap, or the site can't be deployed as is.
too_big=0
while IFS= read -r -d '' f; do
  size="$(wc -c <"$f" | tr -d ' ')"
  if [ "$size" -gt "$MAX_FILE_BYTES" ]; then
    echo "error: ${f#"$SITE"/} is $size bytes, over the $MAX_FILE_BYTES-byte per-file host cap" >&2
    too_big=1
  fi
done < <(find "$SITE" -type f -print0)
if [ "$too_big" = 1 ]; then exit 1; fi

ZIP="$DIST/$BASENAME.zip"
(cd "$(dirname "$SITE")" && zip -q -r -X "$ZIP" "$BASENAME")
echo "==> $ZIP ($(wc -c <"$ZIP" | tr -d ' ') bytes)"
find "$SITE" -type f -exec wc -c {} + | sort -n
