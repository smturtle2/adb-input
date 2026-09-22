#!/bin/sh
# SPDX-License-Identifier: EUPL-1.2
set -eu
die() { echo "adb-input installer: $*" >&2; exit 1; }
repository=${ADB_INPUT_REPOSITORY:-smturtle2/adb-input}
case "$repository" in
  *[!A-Za-z0-9_.\/-]*|*/*/*|/*|*/|"") die "ADB_INPUT_REPOSITORY must be owner/repository" ;;
  */*) : ;;
  *) die "ADB_INPUT_REPOSITORY must be owner/repository" ;;
esac
version=${ADB_INPUT_VERSION:-latest}
if [ "$version" != latest ]; then
  if ! awk 'BEGIN { exit(ARGV[1] ~ /^v[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$/ ? 0 : 1) }' "$version"; then
    die "ADB_INPUT_VERSION must be latest or vX.Y.Z with an optional prerelease"
  fi
fi
os=$(uname -s 2>/dev/null || true); arch=$(uname -m 2>/dev/null || true)
[ "$os" = Linux ] || die "unsupported operating system: $os (Linux is required)"
case "$arch" in x86_64|amd64) asset_arch=x86_64 ;; aarch64|arm64) asset_arch=aarch64 ;; *) die "unsupported architecture: $arch (x86_64 or aarch64 is required)" ;; esac
asset=adb-input-linux-$asset_arch.tar.gz
base=${ADB_INPUT_RELEASE_BASE:-https://github.com/$repository/releases}
case "$version" in latest) url=$base/latest/download/$asset ;; *) url=$base/download/$version/$asset ;; esac
checksum_url=$url.sha256
bin_dir=${ADB_INPUT_BIN_DIR:-$HOME/.local/bin}; data_dir=${ADB_INPUT_DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/adb-input}
[ -n "$bin_dir" ] || die "ADB_INPUT_BIN_DIR must not be empty"; [ -n "$data_dir" ] || die "ADB_INPUT_DATA_DIR must not be empty"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/adb-input-install.XXXXXX") || die "cannot create temporary directory"
new_binary=; new_license=; new_third_party=
cleanup_staged() { rm -f "$new_binary" "$new_license" "$new_third_party"; }
trap 'exit 1' HUP INT TERM
trap 'cleanup_staged; rm -rf "$tmp"' 0
archive=$tmp/$asset; checksum=$tmp/$asset.sha256
command -v curl >/dev/null 2>&1 || die "curl is required"
curl -fsSL "$url" -o "$archive" || die "failed to download $url"
curl -fsSL "$checksum_url" -o "$checksum" || die "failed to download $checksum_url"
[ -s "$archive" ] || die "downloaded archive is empty"; [ -s "$checksum" ] || die "downloaded checksum is empty"
expected=$(awk 'NF { print $1; exit }' "$checksum")
case "$expected" in ''|*[!0123456789abcdefABCDEF]*) die "invalid checksum file" ;; esac
[ "${#expected}" -eq 64 ] || die "invalid checksum file"
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$archive" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "$archive" | awk '{print $1}')
else
  die "sha256sum or shasum is required"
fi
[ "$actual" = "$expected" ] || die "checksum verification failed"
mkdir "$tmp/unpacked"
tar -tvzf "$archive" > "$tmp/tar.list" || die "failed to inspect archive"
awk '$1 !~ /^-[-rwxst]+$/ || ($NF != "adb-input" && $NF != "LICENSE" && $NF != "THIRD_PARTY.md") { exit 1 }' "$tmp/tar.list" || die "archive contains unexpected or nonregular members"
tar -xzf "$archive" -C "$tmp/unpacked" || die "failed to extract archive"
[ -f "$tmp/unpacked/adb-input" ] || die "archive does not contain adb-input"
[ -f "$tmp/unpacked/LICENSE" ] || die "archive does not contain LICENSE"
[ -f "$tmp/unpacked/THIRD_PARTY.md" ] || die "archive does not contain THIRD_PARTY.md"
chmod 755 "$tmp/unpacked/adb-input" || die "cannot prepare executable"
"$tmp/unpacked/adb-input" --version >/dev/null 2>&1 || die "downloaded executable failed --version"
mkdir -p "$bin_dir" "$data_dir" || die "cannot create install directories"
new_binary=$(mktemp "$bin_dir/.adb-input.tmp.XXXXXX") || die "cannot stage executable"
new_license=$(mktemp "$data_dir/.LICENSE.tmp.XXXXXX") || die "cannot stage LICENSE"
new_third_party=$(mktemp "$data_dir/.THIRD_PARTY.md.tmp.XXXXXX") || die "cannot stage THIRD_PARTY.md"
cp "$tmp/unpacked/adb-input" "$new_binary"; chmod 755 "$new_binary"
cp "$tmp/unpacked/LICENSE" "$new_license"; cp "$tmp/unpacked/THIRD_PARTY.md" "$new_third_party"
mv "$new_license" "$data_dir/LICENSE" || die "cannot install LICENSE"
mv "$new_third_party" "$data_dir/THIRD_PARTY.md" || die "cannot install THIRD_PARTY.md"
mv "$new_binary" "$bin_dir/adb-input" || die "cannot install executable"
new_binary=; new_license=; new_third_party=
echo "Installed adb-input to $bin_dir/adb-input"
