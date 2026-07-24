#!/usr/bin/env bash
# Add static arm64 ffmpeg/ffprobe to the dx-built APK as native libs, then
# re-align and re-sign. dx wipes the generated project's jniLibs dir on every
# build, so the binaries are appended to the finished APK instead — with
# android:extractNativeLibs="true" (dx's template default) the installer
# extracts them exec-able into the app's nativeLibraryDir, where
# engine::tool_bin() finds them as lib{ffmpeg,ffprobe}.so.
#
# Binaries in packaging/android/prebuilt/ (gitignored, ~40 MB) MUST be
# NDK-built static bionic executables — a desktop glibc-static build dies with
# seccomp SIGSYS inside an app sandbox. Rebuild recipe (x264 + ffmpeg, both
# with the NDK's aarch64-linux-android28-clang):
#   x264:   ./configure --host=aarch64-linux-android --enable-static --enable-pic --disable-cli
#   ffmpeg: ./configure --target-os=android --arch=aarch64 --enable-cross-compile \
#             --enable-gpl --enable-libx264 --extra-ldflags="-L<x264>/lib -static" \
#             --disable-{doc,ffplay,avdevice,vulkan,iconv,xlib,libxcb}
# (GPL, same license as this app.)
#
# Optional hardware encode: add --enable-jni --enable-mediacodec and drop
# -static (libmediandk.so only exists on-device, so the binary must link
# dynamically; bionic + libmediandk are guaranteed present). engine.rs probes
# `ffmpeg -encoders` for h264_mediacodec at runtime and uses it automatically,
# falling back to libx264 per-export if the device's codec balks.
#
# Usage: packaging/android/bundle-ffmpeg.sh <in.apk> <out.apk>
set -euo pipefail

IN=$(realpath "$1")
OUT=$(realpath -m "$2")
HERE=$(cd "$(dirname "$0")" && pwd)
PREBUILT="$HERE/prebuilt"
SDK="${ANDROID_HOME:-$HOME/Android/Sdk}"
BT=$(ls -d "$SDK"/build-tools/* | sort -V | tail -1)
KS="${KEYSTORE:-$HOME/.android/debug.keystore}"
KS_PASS="${KEYSTORE_PASS:-android}"
KS_ALIAS="${KEYSTORE_ALIAS:-androiddebugkey}"

for f in ffmpeg ffprobe; do
    [ -f "$PREBUILT/$f" ] || { echo "missing $PREBUILT/$f — see header for the download"; exit 1; }
done

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/lib/arm64-v8a"
cp "$PREBUILT/ffmpeg" "$WORK/lib/arm64-v8a/libffmpeg.so"
cp "$PREBUILT/ffprobe" "$WORK/lib/arm64-v8a/libffprobe.so"

cp "$IN" "$WORK/app.apk"
(cd "$WORK" && zip -q app.apk lib/arm64-v8a/libffmpeg.so lib/arm64-v8a/libffprobe.so)
"$BT/zipalign" -f -p 4 "$WORK/app.apk" "$WORK/aligned.apk"
"$BT/apksigner" sign --ks "$KS" --ks-pass "pass:$KS_PASS" --ks-key-alias "$KS_ALIAS" \
    --out "$OUT" "$WORK/aligned.apk"
echo "wrote $OUT"
