#!/usr/bin/env bash
# Adversarial generated-site inventory tests, no Cargo or browser needed.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INVENTORY="${NODECRAFT_INVENTORY_TEST_SOURCE:-$HERE/inventory.sh}"
TEMP="$(mktemp -d "${TMPDIR:-/tmp}/nodecraft-web-inventory.XXXXXX")"
trap 'rm -rf "$TEMP"' EXIT
SITE="$TEMP/site"
fixture() {
  rm -rf "$SITE"; mkdir -p "$SITE"
  printf '<html></html>' > "$SITE/index.html"
  for name in nodecraft-web-abcdef_bg.wasm nodecraft-web-abcdef.js nodecraft-mesh-worker_bg.wasm nodecraft-mesh-worker.js; do printf 'fixture' > "$SITE/$name"; done
  printf './nodecraft-mesh-worker.js ./nodecraft-mesh-worker_bg.wasm' > "$SITE/nodecraft-mesh-worker_loader.js"
}
accept() { ( . "$INVENTORY" ) >/dev/null 2>&1; }
reject() { if accept; then echo "FAIL: $1 unexpectedly accepted" >&2; exit 1; fi; }
fixture; accept
rm "$SITE/nodecraft-mesh-worker_loader.js"; reject 'missing worker shim'
fixture; rm "$SITE/nodecraft-mesh-worker_bg.wasm"; reject 'missing worker Wasm'
fixture; printf 'extra' > "$SITE/old.wasm"; reject 'stale extra Wasm'
fixture; printf 'old' > "$SITE/nodecraft-web-old.js"; reject 'stale extra main wrapper'
fixture; printf 'old' > "$SITE/worker.rs"; reject 'source in package'
fixture; printf './workers/workers/other.js' > "$SITE/nodecraft-mesh-worker_loader.js"; reject 'broken generated relative wrapper'
fixture; printf '<script integrity="old" src="x"></script>' > "$SITE/index.html"; reject 'stale integrity'
fixture; mkdir -p "$SITE/snippets/valid"; printf 'generated' > "$SITE/snippets/valid/example.js"; accept
printf 'web generated-inventory tests: 9 passed\n'
