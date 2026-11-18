#!/usr/bin/env bash
# Build dist/CalLink-<version>-macos.dmg: a universal (Apple Silicon + Intel) CalLink.app.
# Run on macOS with Xcode command line tools and both Rust targets:
#   rustup target add aarch64-apple-darwin x86_64-apple-darwin
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
WORK="$ROOT/target/macos"
APP="$WORK/CalLink.app"
DMG="$ROOT/dist/CalLink-$VERSION-macos.dmg"
export MACOSX_DEPLOYMENT_TARGET=11.0

for t in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release --target "$t" --manifest-path "$ROOT/Cargo.toml" --bins
done

rm -rf "$WORK" && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
for bin in callink callink-cli; do
    lipo -create -output "$APP/Contents/MacOS/$bin" \
        "$ROOT/target/aarch64-apple-darwin/release/$bin" "$ROOT/target/x86_64-apple-darwin/release/$bin"
done
sed "s/@VERSION@/$VERSION/g" "$ROOT/packaging/macos/Info.plist" > "$APP/Contents/Info.plist"

# icon: an .iconset of scaled PNGs → .icns
ICONSET="$WORK/callink.iconset" && mkdir -p "$ICONSET"
for s in 16 32 128 256; do
    sips -z $s $s "$ROOT/packaging/callink.png" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
    d=$((s * 2)); [ $d -le 256 ] && sips -z $d $d "$ROOT/packaging/callink.png" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/callink.icns"

# lipo invalidates the linker's ad-hoc signatures; Apple Silicon needs one to run at all
codesign --force --deep --sign - "$APP"

mkdir -p "$ROOT/dist" "$WORK/dmg"
cp -R "$APP" "$WORK/dmg/" && ln -sf /Applications "$WORK/dmg/Applications"
cp "$ROOT/README.md" "$WORK/dmg/README.md"
rm -f "$DMG"
hdiutil create -volname "CalLink" -srcfolder "$WORK/dmg" -ov -format UDZO "$DMG" >/dev/null
echo "built $DMG"
