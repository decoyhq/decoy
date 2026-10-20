#!/bin/sh
# Install the decoy binary from GitHub Releases.
#
#   curl -fsSL https://raw.githubusercontent.com/decoyhq/decoy/main/scripts/install.sh | sh
#   curl -fsSL ... | sh -s -- 0.1.0-alpha.6     # a specific version
#
# Env: DECOY_VERSION (same as the argument), DECOY_INSTALL_DIR (default ~/.decoy/bin),
# GITHUB_TOKEN (optional, lifts the GitHub API rate limit when resolving the latest version).
set -eu

repo="decoyhq/decoy"
version="${1:-${DECOY_VERSION:-latest}}"
install_dir="${DECOY_INSTALL_DIR:-$HOME/.decoy/bin}"

err() {
  echo "decoy-install: $*" >&2
  exit 1
}

command -v curl > /dev/null || err "curl is required"
command -v tar > /dev/null || err "tar is required"

case "$(uname -s)" in
  Linux) os=unknown-linux-musl ;;
  Darwin) os=apple-darwin ;;
  *) err "unsupported OS $(uname -s); on Windows use install.ps1" ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  arm64 | aarch64) arch=aarch64 ;;
  *) err "unsupported architecture $(uname -m)" ;;
esac
# A shell running under Rosetta reports x86_64; prefer the native build.
if [ "$os" = apple-darwin ] && [ "$arch" = x86_64 ] &&
  [ "$(sysctl -n sysctl.proc_translated 2> /dev/null || echo 0)" = 1 ]; then
  arch=aarch64
fi
target="$arch-$os"

if [ "$version" = latest ]; then
  # The release marked Latest, the same one `decoy upgrade` installs.
  set -- -fsSL -H "Accept: application/vnd.github+json"
  [ -n "${GITHUB_TOKEN:-}" ] && set -- "$@" -H "Authorization: Bearer $GITHUB_TOKEN"
  version="$(curl "$@" "https://api.github.com/repos/$repo/releases/latest" |
    sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -n 1)"
  [ -n "$version" ] || err "could not resolve the latest version"
fi
version="${version#v}"

name="decoy-$version-$target"
base="https://github.com/$repo/releases/download/v$version"
mkdir -p "$install_dir"
# Stage next to the target so the final mv is a same-filesystem rename, never a partial copy.
tmp="$(mktemp -d "$install_dir/.decoy-install.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

echo "Installing decoy $version ($target)"
curl -fsSL -o "$tmp/$name.tar.gz" "$base/$name.tar.gz" ||
  err "download failed: $base/$name.tar.gz"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || err "download failed: $base/SHA256SUMS"

expected="$(awk -v f="$name.tar.gz" '$2 == f { print $1 }' "$tmp/SHA256SUMS")"
[ -n "$expected" ] || err "$name.tar.gz is not listed in SHA256SUMS"
if command -v sha256sum > /dev/null; then
  actual="$(sha256sum "$tmp/$name.tar.gz" | cut -d' ' -f1)"
else
  actual="$(shasum -a 256 "$tmp/$name.tar.gz" | cut -d' ' -f1)"
fi
[ "$actual" = "$expected" ] || err "checksum mismatch for $name.tar.gz"

tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
chmod +x "$tmp/$name/decoy"
mv -f "$tmp/$name/decoy" "$install_dir/decoy"

echo "Installed $("$install_dir/decoy" --version) to $install_dir/decoy"
case ":$PATH:" in
  *":$install_dir:"*) ;;
  *)
    echo
    echo "Add decoy to your PATH, e.g. in ~/.bashrc or ~/.zshrc:"
    echo "  export PATH=\"$install_dir:\$PATH\""
    ;;
esac
