#!/bin/sh
# Compiles tests/c/smoke.c against the header and the host build of the
# library, runs it, and checks the C layout against the Rust one.
set -eu
cd "$(dirname "$0")/.."
cargo build -p timecode-tools-ffi --release --quiet
out="target/c-abi"
mkdir -p "$out"
cc -std=c99 -Wall -Wextra -Werror -o "$out/smoke" crates/timecode-tools-ffi/tests/c/smoke.c \
  -I crates/timecode-tools-ffi/include -L target/release -ltimecode_tools_ffi
case "$(uname)" in
  Darwin) DYLD_LIBRARY_PATH=target/release "$out/smoke" > "$out/c.txt" ;;
  *) LD_LIBRARY_PATH=target/release "$out/smoke" > "$out/c.txt" ;;
esac
cat "$out/c.txt"
grep '^layout' "$out/c.txt" > "$out/c-layout.txt"
TT_LAYOUT_OUT="$PWD/$out/rust-layout.txt" cargo test -p timecode-tools-ffi --release --quiet --test layout >/dev/null
diff "$out/c-layout.txt" "$out/rust-layout.txt"
echo "C and Rust layouts match"
