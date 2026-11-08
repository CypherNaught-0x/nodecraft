#!/bin/sh
# Sign local macOS dev builds of NodeCraft with a stable identity, so the keychain's
# "Always Allow" survives rebuilds (docs/development.md, "macOS: keychain prompts on rebuilds").
#
# As a Cargo runner, it signs `nodecraft` and `nodecraft-cli` before running them and runs every
# other binary (tests, xtask, other projects) unchanged:
#
#   [target.aarch64-apple-darwin]
#   runner = "/path/to/NodeCraft/scripts/macos/dev-sign.sh"
#
# Or sign a build you launch directly: dev-sign.sh --sign target/debug/nodecraft
#
# The identity is the code-signing certificate named by NODECRAFT_DEV_SIGN_IDENTITY (default
# "NodeCraft Dev"). Without it, or off macOS, nothing is signed and builds behave as before.
set -eu

sign_only=false
if [ "${1:-}" = "--sign" ]; then
    sign_only=true
    shift
fi
if [ "$#" -lt 1 ]; then
    echo "usage: dev-sign.sh [--sign] <binary> [args...]" >&2
    exit 2
fi
bin=$1
shift

identity=${NODECRAFT_DEV_SIGN_IDENTITY:-NodeCraft Dev}
case $(basename "$bin") in
    nodecraft) id=io.github.cyphernaught0x.nodecraft ;;
    nodecraft-cli) id=io.github.cyphernaught0x.nodecraft-cli ;;
    *) id= ;;
esac

if [ -n "$id" ] && [ "$(uname -s)" = Darwin ]; then
    # Already signed as this identifier since the last build: the linker's ad-hoc signature
    # carries a hashed identifier instead.
    if ! codesign -dv "$bin" 2>&1 | grep -qx "Identifier=$id"; then
        if security find-identity -p codesigning 2>/dev/null | grep -q "\"$identity\""; then
            codesign --force --sign "$identity" --identifier "$id" "$bin" 2>/dev/null ||
                echo "dev-sign: couldn't sign $bin with \"$identity\"; the keychain will ask again" >&2
        elif $sign_only; then
            echo "dev-sign: no code-signing identity \"$identity\"; see docs/development.md" >&2
        fi
    fi
fi

if $sign_only; then
    exit 0
fi
exec "$bin" "$@"
