#!/bin/sh
# Install Gobstopper on macOS (Apple silicon) or Linux (x86_64, arm64).
#
#   curl -fsSL https://gobstopper.sh/install.sh | sh
#   curl -fsSL https://gobstopper.sh/install.sh | GOBSTOPPER_VERSION=<version> sh
#
# Downloads gobstopper-<version>-<platform>.tar.gz from the GitHub Release,
# checks it against the release's .sha256 file, and installs
# ~/.local/bin/gobstopper. Nothing runs as root.
# Update-enabled releases require authenticated GitHub CLI (gh).
#
# It also installs aicharts beside it for local usage history across your
# agents (https://aicharts.io/usage), checked against the digest pinned below,
# and on a first install turns that history and aicharts' daily verified
# self-update check on. Everything stays on this computer; nothing is
# uploaded.
#
# Options (environment):
#   GOBSTOPPER_VERSION         exact version, MAJOR.MINOR.PATCH (default: the latest release)
#   GOBSTOPPER_INSTALL_PREFIX  install into <prefix>/bin (default: ~/.local)
#   GOBSTOPPER_AICHARTS=no     skip aicharts
#   GOBSTOPPER_USAGE_HISTORY=no  install aicharts but leave usage history off
#   GOBSTOPPER_AICHARTS_UPDATE=no  install aicharts but leave daily updates off
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
  first_install=yes
  [ ! -e "$bin/gobstopper" ] || first_install=no

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
    *) if supports_native_update "$version"; then native_transaction=yes; else native_transaction=no; fi ;;
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
  install_aicharts "$bin" "$platform" "$first_install"
  echo
  echo "Next: gobstopper detect"
}

# The aicharts release this installer adds, with its reviewed archive digests.
AICHARTS_VERSION=0.3.1
AICHARTS_SHA256_DARWIN_AARCH64=e79a19b0b174845c939e2472b4dbf3e5738b6bf867bd16aba2daa86be6f049b6
AICHARTS_SHA256_LINUX_X86_64=c2a8acf56019565668bbcf84884503428d857ab5c54fecec85ae145644f83559

# install_aicharts BIN PLATFORM FIRST_INSTALL installs or upgrades the pinned
# aicharts in BIN, leaves an aicharts installed elsewhere or a newer one alone,
# and on a first install turns on local usage history and aicharts' daily
# self-update check. A failure here only warns: gobstopper is already
# installed.
install_aicharts() {
  case "${GOBSTOPPER_AICHARTS:-yes}" in no | 0 | false | off) return 0 ;; esac
  case "$2" in
    darwin-aarch64) aicharts_target=aarch64-apple-darwin aicharts_sha256=$AICHARTS_SHA256_DARWIN_AARCH64 ;;
    linux-x86_64) aicharts_target=x86_64-unknown-linux-gnu aicharts_sha256=$AICHARTS_SHA256_LINUX_X86_64 ;;
    *) echo "aicharts has no release for this platform yet, so usage history is not installed"; return 0 ;;
  esac
  aicharts_base="https://github.com/hraness/aicharts/releases/download/cli-v$AICHARTS_VERSION"
  if [ -n "${GOBSTOPPER_RELEASE_BASE_URL:-}" ]; then
    # A loopback fixture install never reaches GitHub; tests opt in here.
    [ -n "${GOBSTOPPER_AICHARTS_BASE_URL:-}" ] || return 0
    printf '%s\n' "$GOBSTOPPER_AICHARTS_BASE_URL" | LC_ALL=C grep -Eq '^http://127\.0\.0\.1:[0-9]{1,5}$' \
      || { warn "GOBSTOPPER_AICHARTS_BASE_URL may only name a loopback test server"; return 0; }
    aicharts_base=$GOBSTOPPER_AICHARTS_BASE_URL
    aicharts_sha256=${GOBSTOPPER_AICHARTS_SHA256:-$aicharts_sha256}
  fi
  aicharts="$1/aicharts"
  aicharts_new=yes
  [ ! -e "$aicharts" ] || aicharts_new=no
  elsewhere=$(command -v aicharts 2>/dev/null || true)
  if [ -n "$elsewhere" ] && [ "$elsewhere" != "$aicharts" ]; then
    echo "Using $elsewhere for local usage history"
    aicharts=$elsewhere
    aicharts_new=no
  else
    current=$("$aicharts" --version 2>/dev/null | sed -n 's/^aicharts \([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\).*/\1/p' || true)
    if [ -z "$current" ] || version_older "$current" "$AICHARTS_VERSION"; then
      fetch_aicharts "$1" "$2" || return 0
    fi
  fi
  [ "$3" = yes ] && [ "$aicharts_new" = yes ] || return 0
  case "${GOBSTOPPER_USAGE_HISTORY:-yes}" in
    no | 0 | false | off) ;;
    *) enable_aicharts_history "$aicharts" ;;
  esac
  case "${GOBSTOPPER_AICHARTS_UPDATE:-yes}" in
    no | 0 | false | off) ;;
    *) enable_aicharts_updates "$aicharts" ;;
  esac
}

# enable_aicharts_history AICHARTS turns local history on unless it has ever
# been turned on before.
enable_aicharts_history() {
  status=$(HRANESS_SUPPORT_AUDIENCE=off "$1" history status --json 2>/dev/null) || return 0
  case "$status" in *'"collecting":"off"'*) ;; *) return 0 ;; esac
  case "$status" in *'"record":null'*) ;; *) return 0 ;; esac
  if HRANESS_SUPPORT_AUDIENCE=off "$1" history enable >/dev/null 2>&1; then
    echo
    echo "Local usage history is on: aicharts records your agents' daily token totals"
    echo "on this computer and never uploads them."
    echo "  See them:  aicharts history report"
    echo "  Turn off:  aicharts history disable"
  else
    warn "could not turn on local usage history; run: aicharts history enable"
  fi
}

# enable_aicharts_updates AICHARTS turns aicharts' daily verified self-update
# check on when this aicharts supports it. An aicharts released before
# `aicharts update` existed fails its status probe and is left alone; so is a
# scheduler already on, unsupported, or owned by another install.
enable_aicharts_updates() {
  status=$(HRANESS_SUPPORT_AUDIENCE=off "$1" update status --json 2>/dev/null) || return 0
  case "$status" in
    *'"scheduler":"on"'* | *'"scheduler":"not-ours"'* | *'"scheduler":"unsupported"'*) return 0 ;;
  esac
  if HRANESS_SUPPORT_AUDIENCE=off "$1" update enable >/dev/null 2>&1; then
    echo
    echo "Daily aicharts updates are on: it checks GitHub once a day and installs"
    echo "a new release only after verifying it. Nothing is uploaded."
    echo "  Turn off:  aicharts update disable"
  else
    warn "could not turn on daily aicharts updates; run: aicharts update enable"
  fi
}

# fetch_aicharts BIN PLATFORM downloads, checks and installs the pinned build.
fetch_aicharts() {
  aicharts_root="aicharts-$AICHARTS_VERSION-$aicharts_target"
  aicharts_asset="$aicharts_root.tar.gz"
  download "$aicharts_base/$aicharts_asset" "$temporary/$aicharts_asset" \
    || { warn "could not download $aicharts_asset; usage history is not installed"; return 1; }
  aicharts_actual=$(sha256 "$temporary/$aicharts_asset")
  [ "$aicharts_actual" = "$aicharts_sha256" ] \
    || { warn "checksum mismatch for $aicharts_asset (expected $aicharts_sha256, got $aicharts_actual); usage history is not installed"; return 1; }
  mkdir "$temporary/aicharts"
  tar -xzf "$temporary/$aicharts_asset" -C "$temporary/aicharts" "$aicharts_root/bin/aicharts" 2>/dev/null \
    || { warn "$aicharts_asset has no bin/aicharts; usage history is not installed"; return 1; }
  aicharts_candidate="$temporary/aicharts/$aicharts_root/bin/aicharts"
  [ -f "$aicharts_candidate" ] && [ ! -L "$aicharts_candidate" ] \
    || { warn "$aicharts_asset must contain a regular bin/aicharts"; return 1; }
  if [ "$2" = darwin-aarch64 ] && ! macos_signature_ok "$aicharts_candidate" dev.hraness.aicharts; then
    warn "aicharts does not have the required Apple Developer ID signature; usage history is not installed"
    return 1
  fi
  chmod 0755 "$aicharts_candidate"
  case "$("$aicharts_candidate" --version 2>/dev/null)" in
    "aicharts $AICHARTS_VERSION" | "aicharts $AICHARTS_VERSION "*) ;;
    *) warn "the downloaded aicharts does not report version $AICHARTS_VERSION"; return 1 ;;
  esac
  mkdir -p "$1"
  [ ! -L "$1/aicharts" ] || { warn "$1/aicharts is a symlink; leaving it alone"; return 1; }
  cp "$aicharts_candidate" "$1/.aicharts-install.$$"
  chmod 0755 "$1/.aicharts-install.$$"
  mv -f "$1/.aicharts-install.$$" "$1/aicharts"
  echo "Installed $1/aicharts $AICHARTS_VERSION for local usage history ($aicharts_actual)"
}

# version_older A B succeeds when MAJOR.MINOR.PATCH A sorts before B.
version_older() {
  printf '%s %s\n' "$1" "$2" | awk '{ split($1, a, "."); split($2, b, "."); for (i = 1; i <= 3; i++) { if (a[i] + 0 < b[i] + 0) exit 0; if (a[i] + 0 > b[i] + 0) exit 1 } exit 1 }'
}

warn() {
  printf 'gobstopper install: %s\n' "$*" >&2
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

# Self-update begins with the signed 0.8.2 release. Older signed binaries do
# not implement the private installation transaction.
supports_native_update() {
  printf '%s\n' "$1" | awk -F . '{ exit !($1 > 0 || ($1 == 0 && ($2 > 8 || ($2 == 8 && $3 >= 2)))) }'
}

# Mac releases before 0.7.6 predate Developer ID signing.
historical_unsigned_release() {
  printf '%s\n' "$1" | awk -F . '{ exit !($1 == 0 && ($2 < 7 || ($2 == 7 && $3 < 6))) }'
}

verify_macos_release_signature() {
  apple_identifier='dev.hraness.gobstopper'
  [ -x /usr/bin/codesign ] || fail "macOS codesign is required to verify this release"
  macos_signature_ok "$1" "$apple_identifier" \
    || fail "release does not have the required Apple Developer ID signature"
}

# macos_signature_ok BINARY IDENTIFIER checks a Developer ID signature from
# the Hraness team for IDENTIFIER, offline.
macos_signature_ok() {
  apple_team_id='8AAP53VTW3'
  printf '%s\n' "$apple_team_id" | LC_ALL=C grep -Eq '^[A-Z0-9]{10}$' || fail "release Apple Developer Team ID is not configured"
  [ -x /usr/bin/codesign ] || return 1
  requirement="anchor apple generic and identifier \"$2\" and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"$apple_team_id\""
  /usr/bin/codesign --verify --strict --all-architectures --test-requirement "=$requirement" "$1"
}

tar_list() {
  if listed=$(LC_ALL=C tar --options '!mac-ext' "$1" "$2" 2>/dev/null); then
    printf '%s\n' "$listed"
  else
    LC_ALL=C tar "$1" "$2"
  fi
}

main "$@"
