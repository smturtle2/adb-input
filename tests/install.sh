#!/bin/sh
# SPDX-License-Identifier: EUPL-1.2
set -eu
root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/adb-input-tests.XXXXXX")
trap 'rm -rf "$test_root"' 0 HUP INT TERM
fail() { echo "FAIL: $*" >&2; exit 1; }
fixture=$test_root/fixture
mkdir -p "$fixture" "$test_root/bin" "$test_root/mock"
cat > "$fixture/adb-input" <<'EOF'
#!/bin/sh
echo "adb-input 9.9.9"
EOF
chmod 755 "$fixture/adb-input"
printf 'license\n' > "$fixture/LICENSE"
printf 'third party\n' > "$fixture/THIRD_PARTY.md"
tar -czf "$fixture/adb-input-linux-x86_64.tar.gz" -C "$fixture" adb-input LICENSE THIRD_PARTY.md
sha256sum "$fixture/adb-input-linux-x86_64.tar.gz" | sed 's#  .*adb-input-linux-x86_64.tar.gz#  adb-input-linux-x86_64.tar.gz#' > "$fixture/adb-input-linux-x86_64.tar.gz.sha256"
cat > "$test_root/mock/curl" <<'EOF'
#!/bin/sh
set -eu
out=; url=
while [ "$#" -gt 0 ]; do
  case "$1" in -o) out=$2; shift 2 ;; -*) shift ;; *) url=$1; shift ;; esac
done
cp "$ADB_INPUT_TEST_FIXTURE/${url##*/}" "$out"
EOF
cat > "$test_root/mock/uname" <<'EOF'
#!/bin/sh
case "$1" in -s) echo Linux ;; -m) echo x86_64 ;; esac
EOF
chmod 755 "$test_root/mock/curl" "$test_root/mock/uname"
run_install() {
  ADB_INPUT_TEST_FIXTURE=$fixture PATH="$test_root/mock:$PATH" ADB_INPUT_BIN_DIR="$test_root/bin" ADB_INPUT_DATA_DIR="$test_root/data" ADB_INPUT_RELEASE_BASE=file://unused ADB_INPUT_VERSION=v1.2.3 sh "$root/install.sh" >/dev/null
}
run_install
[ -f "$test_root/bin/adb-input" ] || fail "missing binary"
[ -f "$test_root/data/LICENSE" ] || fail "missing LICENSE"
[ -x "$test_root/bin/adb-input" ] || fail "binary is not executable"
if ADB_INPUT_REPOSITORY='bad/repo/extra' sh "$root/install.sh" 2>/dev/null; then fail "invalid repository was accepted"; fi
if ADB_INPUT_VERSION='v1.2.3/evil' sh "$root/install.sh" 2>/dev/null; then fail "invalid version was accepted"; fi
printf 'old binary\n' > "$test_root/bin/adb-input"
run_install
grep -q 'adb-input 9.9.9' "$test_root/bin/adb-input" || fail "update did not replace binary"
old=$(cat "$test_root/bin/adb-input")
printf '%064d  adb-input-linux-x86_64.tar.gz\n' 0 > "$fixture/adb-input-linux-x86_64.tar.gz.sha256"
if run_install 2>/dev/null; then fail "corrupt checksum was accepted"; fi
[ "$(cat "$test_root/bin/adb-input")" = "$old" ] || fail "previous binary was not preserved"
bad=$test_root/bad
mkdir "$bad"
cp "$fixture/LICENSE" "$bad/LICENSE"; cp "$fixture/THIRD_PARTY.md" "$bad/THIRD_PARTY.md"
ln -s LICENSE "$bad/adb-input"
tar -czf "$bad/adb-input-linux-x86_64.tar.gz" -C "$bad" adb-input LICENSE THIRD_PARTY.md
sha256sum "$bad/adb-input-linux-x86_64.tar.gz" | sed 's#  .*adb-input-linux-x86_64.tar.gz#  adb-input-linux-x86_64.tar.gz#' > "$bad/adb-input-linux-x86_64.tar.gz.sha256"
if ADB_INPUT_TEST_FIXTURE=$bad PATH="$test_root/mock:$PATH" ADB_INPUT_BIN_DIR="$test_root/bin" ADB_INPUT_DATA_DIR="$test_root/data" ADB_INPUT_RELEASE_BASE=file://unused ADB_INPUT_VERSION=v1.2.3 sh "$root/install.sh" 2>/dev/null; then fail "symlink archive member was accepted"; fi
cat > "$test_root/mock/uname" <<'EOF'
#!/bin/sh
case "$1" in -s) echo Darwin ;; -m) echo x86_64 ;; esac
EOF
if ADB_INPUT_TEST_FIXTURE=$fixture PATH="$test_root/mock:$PATH" ADB_INPUT_BIN_DIR="$test_root/bin" ADB_INPUT_DATA_DIR="$test_root/data" sh "$root/install.sh" 2>/dev/null; then fail "unsupported platform was accepted"; fi
echo "install tests passed"
