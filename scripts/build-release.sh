#!/bin/sh
# Build the gobstopper release binary for this host and package it as
# artifacts/gobstopper-<version>-<platform>.tar.gz plus <archive>.sha256
# ("<sha256>  <archive>", so `shasum -a 256 -c` reads it). The archive holds
# exactly one regular file, gobstopper; scripts/install.sh refuses anything
# else. GOBSTOPPER_VERSION (default: Cargo.toml) must match the binary.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
: "${CARGO:=cargo}"

if [ -n "${GOBSTOPPER_VERSION:-}" ]; then
  version="${GOBSTOPPER_VERSION#v}"
else
  version=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)
fi
printf '%s\n' "$version" | LC_ALL=C grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' \
  || { echo "error: version must be MAJOR.MINOR.PATCH (got '$version')" >&2; exit 1; }

case "$(uname -s)/$(uname -m)" in
  Darwin/arm64 | Darwin/aarch64) platform=darwin-aarch64 ;;
  Linux/x86_64) platform=linux-x86_64 ;;
  Linux/aarch64 | Linux/arm64) platform=linux-aarch64 ;;
  *) echo "error: no release platform for $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac

cd "$root"
"$CARGO" build --release --locked -p gobstopper
binary="${CARGO_TARGET_DIR:-$root/target}/release/gobstopper"
reported=$("$binary" --version)
[ "$reported" = "gobstopper $version" ] \
  || { echo "error: binary reports '$reported', expected 'gobstopper $version'" >&2; exit 1; }

if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum "$1" | cut -d ' ' -f 1; }
else
  sha256() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
fi

name="gobstopper-$version-$platform.tar.gz"
# Only the isolated Developer ID job may create the final Mac release name.
if [ "$platform" = darwin-aarch64 ]; then
  name="gobstopper-$version-$platform.unsigned.tar.gz"
fi
mkdir -p artifacts
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
install -m 0755 "$binary" "$work/gobstopper"
# One member, no AppleDouble companions or directory entries.
COPYFILE_DISABLE=1 tar --format=ustar -czf "artifacts/$name" -C "$work" gobstopper
printf '%s  %s\n' "$(sha256 "artifacts/$name")" "$name" > "artifacts/$name.sha256"

# Re-admit the packaged bytes the way the installer will.
[ "$(tar -tzf "artifacts/$name")" = gobstopper ] \
  || { echo "error: $name must contain only gobstopper" >&2; exit 1; }
mkdir "$work/admit"
tar -xzf "artifacts/$name" -C "$work/admit"
[ "$("$work/admit/gobstopper" --version)" = "gobstopper $version" ] \
  || { echo "error: the packaged binary does not report gobstopper $version" >&2; exit 1; }
(cd artifacts && if command -v sha256sum >/dev/null 2>&1; then sha256sum -c "$name.sha256"; else shasum -a 256 -c "$name.sha256"; fi)
echo "archive=artifacts/$name"
