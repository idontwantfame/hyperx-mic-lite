#!/usr/bin/env bash
# Build "HyperX Mic Lite.app" from a release build, using only tools that ship with macOS
# and Xcode (sips, iconutil, lipo, plutil, codesign, ditto).
#
# Usage: scripts/bundle-macos.sh [--universal] [--zip]
#   --universal  build for Apple silicon and Intel and merge them with lipo
#                (needs `rustup target add aarch64-apple-darwin x86_64-apple-darwin`)
#   --zip        also write dist/hyperx-mic-lite-v<version>-macos-<arch>.zip
#
# The version comes from HYPERX_BUILD_VERSION when set (as in the release workflow),
# otherwise from Cargo.toml with any -SNAPSHOT suffix dropped.
set -euo pipefail

readonly APP_NAME="HyperX Mic Lite"
readonly BUNDLE_ID="local.hyperx-mic-lite"
readonly EXECUTABLE="hyperx-mic-lite"
readonly MIN_MACOS="11.0"

universal=false
zip=false
for arg in "$@"; do
    case "$arg" in
        --universal) universal=true ;;
        --zip) zip=true ;;
        *)
            echo "Unknown argument: $arg" >&2
            echo "Usage: $0 [--universal] [--zip]" >&2
            exit 2
            ;;
    esac
done

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

version="${HYPERX_BUILD_VERSION:-$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)}"
version="${version%%-*}"
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Cannot derive a numeric bundle version from '$version'." >&2
    exit 1
fi

dist="$root/dist"
app="$dist/$APP_NAME.app"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

if $universal; then
    arch_label="universal"
    targets=(aarch64-apple-darwin x86_64-apple-darwin)
    binaries=()
    for target in "${targets[@]}"; do
        cargo build --release --locked --target "$target"
        binaries+=("target/$target/release/$EXECUTABLE")
    done
    binary="$work/$EXECUTABLE"
    lipo -create -output "$binary" "${binaries[@]}"
else
    arch_label="$(uname -m)"
    cargo build --release --locked
    binary="target/release/$EXECUTABLE"
fi

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/$EXECUTABLE"

# The .ico tops out at 256 px, so the iconset stops there rather than upscaling.
iconset="$work/AppIcon.iconset"
mkdir -p "$iconset"
sips -s format png resources/hyperx-mic-lite.ico --out "$work/icon.png" >/dev/null
for size in 16 32 128 256; do
    sips -z "$size" "$size" "$work/icon.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
done
for size in 16 32 128; do
    double=$((size * 2))
    sips -z "$double" "$double" "$work/icon.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>$APP_NAME</string>
    <key>CFBundleExecutable</key>
    <string>$EXECUTABLE</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$version</string>
    <key>CFBundleVersion</key>
    <string>$version</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.utilities</string>
    <key>LSMinimumSystemVersion</key>
    <string>$MIN_MACOS</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>HyperX Mic Lite reads the QuadCast S input level for the level meter and the VU Meter lighting effect.</string>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist" >/dev/null

# Ad-hoc signature: enough for macOS to run the bundle and to remember its microphone
# permission. It is not notarized, so a downloaded copy has to be opened once with
# right-click > Open (or have its quarantine attribute removed).
codesign --force --sign - --identifier "$BUNDLE_ID" "$app"

echo "Built $app ($version, $arch_label)"

if $zip; then
    archive="$dist/$EXECUTABLE-v$version-macos-$arch_label.zip"
    rm -f "$archive"
    ditto -c -k --keepParent "$app" "$archive"
    echo "Packed $archive"
fi
