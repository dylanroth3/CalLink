#!/usr/bin/env bash
# Build dist/CalLink-<version>-x86_64.AppImage.
#
# An AppImage is the AppImage "type 2" runtime (a static ELF) with a squashfs image of the
# AppDir appended. We use packaging/runtime-x86_64 (the AppImage project's MIT-licensed
# static runtime, with the update-info and signature sections blanked) and mksquashfs, so
# no appimagetool is needed.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RUNTIME="${RUNTIME:-$ROOT/packaging/runtime-x86_64}"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
OUT="$ROOT/dist/CalLink-$VERSION-x86_64.AppImage"
WORK="$ROOT/target/appimage"
APPDIR="$WORK/CalLink.AppDir"

command -v mksquashfs >/dev/null || { echo "mksquashfs not found (install squashfs-tools)" >&2; exit 1; }
[ -f "$RUNTIME" ] || { echo "runtime not found: $RUNTIME" >&2; exit 1; }

cargo build --release -j 2 --manifest-path "$ROOT/Cargo.toml" --bins

rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" "$APPDIR/usr/share/icons/hicolor/256x256/apps"
install -m755 "$ROOT/target/release/callink" "$ROOT/target/release/callink-cli" "$APPDIR/usr/bin/"
strip "$APPDIR/usr/bin/"* 2>/dev/null || true
install -m755 "$ROOT/packaging/AppRun" "$APPDIR/AppRun"
install -m644 "$ROOT/packaging/callink.desktop" "$APPDIR/callink.desktop"
install -m644 "$ROOT/packaging/callink.desktop" "$APPDIR/usr/share/applications/callink.desktop"
install -m644 "$ROOT/packaging/callink.png" "$APPDIR/callink.png"
install -m644 "$ROOT/packaging/callink.png" "$APPDIR/usr/share/icons/hicolor/256x256/apps/callink.png"
ln -sf callink.png "$APPDIR/.DirIcon"

rm -f "$WORK/image.squashfs"
mksquashfs "$APPDIR" "$WORK/image.squashfs" -root-owned -noappend -comp zstd -quiet

mkdir -p "$ROOT/dist"
# write beside it and rename, so a copy that is currently running can be replaced
cat "$RUNTIME" "$WORK/image.squashfs" > "$OUT.tmp"
chmod +x "$OUT.tmp"
mv -f "$OUT.tmp" "$OUT"
echo "built $OUT ($(du -h "$OUT" | cut -f1))"
