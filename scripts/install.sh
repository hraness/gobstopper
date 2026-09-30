#!/bin/sh
# Install Gobstopper on macOS (Apple silicon) or Linux (x86_64, arm64).
#
#   curl -fsSL https://gobstopper.sh/install.sh | sh
#   curl -fsSL https://gobstopper.sh/install.sh | GOBSTOPPER_VERSION=<version> sh
#
# Downloads gobstopper-<version>-<platform>.tar.gz from the GitHub Release,
# checks it against the release's .sha256 file, and installs
# ~/.local/bin/gobstopper. Nothing runs as root.
#
# Options (environment):
#   GOBSTOPPER_VERSION         exact version, MAJOR.MINOR.PATCH (default: the latest release)
#   GOBSTOPPER_INSTALL_PREFIX  install into <prefix>/bin (default: ~/.local)
# Source: https://github.com/hraness/gobstopper/blob/main/scripts/install.sh
#
# Everything is inside main(), so a partial download runs nothing.

main() {
  set -eu
  repository="hraness/gobstopper"
  source_help="build from source instead: cargo install --git https://github.com/$repository --locked gobstopper"

  command -v curl >/dev/null 2>&1 || fail "curl is required"
  command -v tar >/dev/null 2>&1 || fail "tar is required"

  os=$(uname -s)
  arch=$(uname -m)
  case "$os/$arch" in
    Darwin/arm64 | Darwin/aarch64) platform=darwin-aarch64 ;;
    Linux/x86_64 | Linux/amd64) platform=linux-x86_64 ;;
    Linux/aarch64 | Linux/arm64) platform=linux-aarch64 ;;
    *) fail "there is no release build for $os $arch; $source_help" ;;
  esac

  base_url=
  # Tests serve a locally built archive from loopback; nothing else may
  # replace the GitHub Release as the source.
  if [ -n "${GOBSTOPPER_RELEASE_BASE_URL:-}" ]; then
    printf '%s\n' "$GOBSTOPPER_RELEASE_BASE_URL" | LC_ALL=C grep -Eq '^http://127\.0\.0\.1:[0-9]{1,5}$' \
      || fail "GOBSTOPPER_RELEASE_BASE_URL may only name a loopback test server"
    [ -n "${GOBSTOPPER_VERSION:-}" ] || fail "GOBSTOPPER_RELEASE_BASE_URL needs GOBSTOPPER_VERSION"
    base_url=$GOBSTOPPER_RELEASE_BASE_URL
  fi

  version="${GOBSTOPPER_VERSION:-}"
  if [ -z "$version" ]; then
    # The latest release page redirects to .../releases/tag/v<version>.
    latest=$(curl -fsSLI --proto '=https' --tlsv1.2 --connect-timeout 15 --max-time 60 \
      -o /dev/null -w '%{url_effective}' "https://github.com/$repository/releases/latest") \
      || fail "could not find the latest release; set GOBSTOPPER_VERSION"
    version="${latest##*/}"
  fi
  version="${version#v}"
  printf '%s\n' "$version" | LC_ALL=C grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' \
    || fail "GOBSTOPPER_VERSION must be an exact release version, MAJOR.MINOR.PATCH (got '$version')"
  [ -n "$base_url" ] || base_url="https://github.com/$repository/releases/download/v$version"

  [ -n "${HOME:-}" ] || [ -n "${GOBSTOPPER_INSTALL_PREFIX:-}" ] \
    || fail "HOME is not set; set GOBSTOPPER_INSTALL_PREFIX to choose where gobstopper goes"
  prefix="${GOBSTOPPER_INSTALL_PREFIX:-$HOME/.local}"
  case "$prefix" in
    /*) ;;
    *) fail "GOBSTOPPER_INSTALL_PREFIX must be an absolute path" ;;
  esac
  bin="$prefix/bin"

  if command -v sha256sum >/dev/null 2>&1; then
    sha256() { sha256sum "$1" | cut -d ' ' -f 1; }
  elif command -v shasum >/dev/null 2>&1; then
    sha256() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
  else
    fail "sha256sum or shasum is required"
  fi

  temporary=$(mktemp -d "${TMPDIR:-/tmp}/gobstopper-install.XXXXXX")
  temporary=$(cd "$temporary" && pwd -P)
  trap 'rm -rf "$temporary"' EXIT
  trap 'exit 1' HUP INT TERM

  asset="gobstopper-$version-$platform.tar.gz"
  echo "Installing gobstopper $version for $os $arch"
  download "$base_url/$asset" "$temporary/$asset" \
    || fail "could not download $asset; v$version may have no build for $os $arch (see https://github.com/$repository/releases/tag/v$version)"
  download "$base_url/$asset.sha256" "$temporary/$asset.sha256" \
    || fail "could not download $asset.sha256"

  expected=$(cut -d ' ' -f 1 < "$temporary/$asset.sha256" | head -n 1)
  printf '%s\n' "$expected" | LC_ALL=C grep -Eq '^[0-9a-f]{64}$' || fail "$asset.sha256 is not a SHA-256 checksum"
  actual=$(sha256 "$temporary/$asset")
  [ "$actual" = "$expected" ] || fail "checksum mismatch for $asset (expected $expected, got $actual)"

  # The archive holds exactly one regular file, gobstopper.
  members=$(tar_list -tzf "$temporary/$asset") || fail "$asset is not a readable archive"
  [ "$members" = gobstopper ] || fail "$asset must contain only gobstopper"
  types=$(tar_list -tvzf "$temporary/$asset") || fail "$asset is not a readable archive"
  [ "$(printf '%s\n' "$types" | wc -l | tr -d '[:space:]')" = 1 ] || fail "$asset must contain one entry"
  case "$types" in -*) ;; *) fail "$asset must contain a regular file named gobstopper" ;; esac
  mkdir "$temporary/extract"
  candidate="$temporary/extract/gobstopper"
  tar -xzOf "$temporary/$asset" gobstopper > "$candidate"
  [ -f "$candidate" ] && [ ! -L "$candidate" ] || fail "$asset must contain a regular file named gobstopper"
  chmod 0755 "$candidate"
  if [ "$platform" = darwin-aarch64 ] && ! historical_unsigned_release "$version"; then
    verify_macos_release_signature "$candidate"
  fi
  reported=$("$candidate" --version 2>&1) || fail "the downloaded gobstopper does not run on this system: $reported"
  [ "$reported" = "gobstopper $version" ] || fail "the downloaded binary reports '$reported', expected 'gobstopper $version'"

  case "$base_url" in
    http://127.0.0.1:*) native_transaction=no ;;
    *) if historical_unsigned_release "$version"; then native_transaction=no; else native_transaction=yes; fi ;;
  esac
  if [ "$native_transaction" = yes ]; then
    # This verified candidate independently checks canonical immutable release
    # metadata and its own bytes, then owns the lock, replacement and receipt.
    # The loopback fixture path never claims public release ownership.
    if [ -n "${GOBSTOPPER_VERSION:-}" ]; then
      "$candidate" __install-release --archive "$temporary/$asset" --checksum "$temporary/$asset.sha256" --prefix "$prefix" --pinned
    else
      "$candidate" __install-release --archive "$temporary/$asset" --checksum "$temporary/$asset.sha256" --prefix "$prefix"
    fi
  else
    mkdir -p "$bin"
    [ ! -L "$bin/gobstopper" ] || fail "$bin/gobstopper is a symlink; remove it first"
    [ ! -e "$bin/.hraness-cli-update-gobstopper" ] \
      || fail "this installation uses native update coordination; use gobstopper update or install a modern official release"
    staged="$bin/.gobstopper-install.$$"
    cp "$candidate" "$staged"
    chmod 0755 "$staged"
    mv -f "$staged" "$bin/gobstopper"
  fi

  echo "Installed $bin/gobstopper ($actual)"
  case ":${PATH:-}:" in
    *":$bin:"*) ;;
    *)
      echo
      echo "$bin is not on your PATH. Add it to your shell profile:"
      echo "  export PATH=\"$bin:\$PATH\""
      ;;
  esac
  echo
  echo "Next: gobstopper detect"
}

download() {
  case "$1" in
    https://*) curl -fsSL --proto '=https' --tlsv1.2 --connect-timeout 15 --max-time 300 -o "$2" "$1" ;;
    *) curl -fsSL --connect-timeout 15 --max-time 300 -o "$2" "$1" ;;
  esac
}

fail() {
  printf 'gobstopper install: %s\n' "$*" >&2
  exit 1
}

# Mac releases before 0.7.6 predate Developer ID signing.
historical_unsigned_release() {
  printf '%s\n' "$1" | awk -F . '{ exit !($1 == 0 && ($2 < 7 || ($2 == 7 && $3 < 6))) }'
}

verify_macos_release_signature() {
  apple_team_id='8AAP53VTW3'
  apple_identifier='dev.hraness.gobstopper'
  printf '%s\n' "$apple_team_id" | LC_ALL=C grep -Eq '^[A-Z0-9]{10}$' || fail "release Apple Developer Team ID is not configured"
  [ -x /usr/bin/codesign ] || fail "macOS codesign is required to verify this release"
  requirement="anchor apple generic and identifier \"$apple_identifier\" and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"$apple_team_id\""
  /usr/bin/codesign --verify --strict --all-architectures --test-requirement "=$requirement" "$1" \
    || fail "release does not have the required Apple Developer ID signature"
}

tar_list() {
  if listed=$(LC_ALL=C tar --options '!mac-ext' "$1" "$2" 2>/dev/null); then
    printf '%s\n' "$listed"
  else
    LC_ALL=C tar "$1" "$2"
  fi
}

main "$@"
