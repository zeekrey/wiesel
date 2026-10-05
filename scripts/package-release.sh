#!/usr/bin/env bash
# Native build, an installable DMG, and a ZIP preserving the app signature.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd -P)"
cd "$root"
version="$(knope get-version)"
build_number="$(git rev-list --count HEAD)"
case "$(uname -m)" in
    arm64) arch=arm64 ;;
    x86_64) arch=x86_64 ;;
    *) echo 'Unsupported release architecture' >&2; exit 1 ;;
esac
# No development identity/configuration is expected on a clean CI runner.
WIESEL_SIGNING_IDENTITY=- bash scripts/build-app.sh --release
output="$root/dist/release"
mkdir -p "$output"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
cp -R dist/Wiesel.app "$stage/Wiesel.app"
mkdir -p "$stage/Wiesel.app/Contents/Resources"
cp -R licenses "$stage/Wiesel.app/Contents/Resources/"
# Use a positive, increasing build number without another versioning system.
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $build_number" "$stage/Wiesel.app/Contents/Info.plist"
# Re-sign after updating metadata and adding license notices.
codesign --force --sign - "$stage/Wiesel.app"
codesign --verify --strict "$stage/Wiesel.app"
ln -s /Applications "$stage/Applications"
base="Wiesel-$version-macos-$arch"
hdiutil create -volname Wiesel -srcfolder "$stage" -ov -format UDZO "$output/$base.dmg"
ditto -c -k --sequesterRsrc --keepParent "$stage/Wiesel.app" "$output/$base.zip"
(cd "$output"; shasum -a 256 "$base.dmg" "$base.zip" > "$base.sha256")
