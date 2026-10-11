#!/bin/sh
# Builds the C interface library (timecode-tools-ffi) for the given Rust
# targets into target/ffi-dist/<target>/, with a SHA256SUMS file.
#
#   scripts/build-ffi.sh                         # what this host can build
#   scripts/build-ffi.sh aarch64-linux-android   # specific targets
#
# Android targets need the Android NDK (cargo-ndk). iOS targets need Xcode.
set -eu
cd "$(dirname "$0")/.."

if [ "$#" -eq 0 ]; then
  case "$(uname)" in
    Darwin) set -- aarch64-apple-darwin x86_64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios ;;
    Linux) set -- "$(rustc -vV | sed -n 's/^host: //p')" ;;
    *) set -- "$(rustc -vV | sed -n 's/^host: //p')" ;;
  esac
fi

# The minimum OS versions Flutter supports.
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-10.15}"
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-13.0}"

for target in "$@"; do
  case "$target" in
    *-android*)
      cargo ndk --platform 21 --target "$target" build -p timecode-tools-ffi --release ;;
    *)
      cargo build -p timecode-tools-ffi --release --target "$target" ;;
  esac
  case "$target" in
    *-apple-*) lib=libtimecode_tools_ffi.dylib ;;
    *-windows-*) lib=timecode_tools_ffi.dll ;;
    *) lib=libtimecode_tools_ffi.so ;;
  esac
  out="target/ffi-dist/$target"
  rm -rf "$out"
  mkdir -p "$out"
  cp "target/$target/release/$lib" "$out/"
  case "$target" in
    *-apple-*)
      # A stable install name; Flutter's build hook renames and signs it.
      install_name_tool -id "@rpath/$lib" "$out/$lib" ;;
  esac
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$out" && sha256sum "$lib" > SHA256SUMS)
  else
    (cd "$out" && shasum -a 256 "$lib" > SHA256SUMS)
  fi
  echo "$out/$lib"
done
