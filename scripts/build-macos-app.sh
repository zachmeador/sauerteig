#!/bin/sh
set -eu

SAUERTEIG_ROOT=$(CDPATH= cd -P "$(dirname "$0")/.." && pwd)
cd "$SAUERTEIG_ROOT"
. "$SAUERTEIG_ROOT/scripts/env.sh"

if [ "$(uname -s)" != "Darwin" ]; then
  echo "Build the macOS app on a Mac. Other platforms can run the native client with cargo run -p sauerteig-client." >&2
  exit 1
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "Rust is required. Install it from https://rustup.rs, then see README.md." >&2
  exit 1
fi

cargo build --release --locked -p sauerteig-client --bin sauerteig-client

sauerteig_app="$SAUERTEIG_ROOT/.local/Sauerteig.app"
mkdir -p "$sauerteig_app/Contents/MacOS"
mkdir -p "$sauerteig_app/Contents/Resources"
rm -rf "$sauerteig_app/Contents/Resources/web"
cp LICENSE THIRD_PARTY_NOTICES.md "$sauerteig_app/Contents/Resources/"
mkdir -p "$sauerteig_app/Contents/Resources/asset-provenance"
cp assets/materials/manifest.json "$sauerteig_app/Contents/Resources/asset-provenance/materials.json"
cp assets/props/manifest.json "$sauerteig_app/Contents/Resources/asset-provenance/props.json"
cp "${CARGO_TARGET_DIR:-target}/release/sauerteig-client" "$sauerteig_app/Contents/MacOS/sauerteig-client.next"
mv "$sauerteig_app/Contents/MacOS/sauerteig-client.next" "$sauerteig_app/Contents/MacOS/sauerteig-client"
chmod +x "$sauerteig_app/Contents/MacOS/sauerteig-client"
cat > "$sauerteig_app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key>
  <string>org.sauerteig.game</string>
  <key>CFBundleExecutable</key>
  <string>sauerteig-client</string>
  <key>CFBundleName</key>
  <string>Sauerteig</string>
  <key>CFBundleDisplayName</key>
  <string>Sauerteig</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>CFBundleVersion</key>
  <string>1</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSPrincipalClass</key>
  <string>NSApplication</string>
  <key>LSApplicationCategoryType</key>
  <string>public.app-category.action-games</string>
</dict>
</plist>
PLIST

echo "Local development app ready: $sauerteig_app"
echo "The app is unsigned. Open it to continue, create a world, or join a server."
