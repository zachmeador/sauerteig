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

# Each build is a fresh bundle, so nothing carries over from an older copy,
# such as the quarantine flag of one that was downloaded or sent by AirDrop.
sauerteig_app="$SAUERTEIG_ROOT/.local/Sauerteig.app"
staging="$SAUERTEIG_ROOT/.local/Sauerteig.app.next"
rm -rf "$staging"
mkdir -p "$staging/Contents/MacOS"
mkdir -p "$staging/Contents/Resources/asset-provenance"
cp LICENSE THIRD_PARTY_NOTICES.md "$staging/Contents/Resources/"
cp assets/materials/manifest.json "$staging/Contents/Resources/asset-provenance/materials.json"
cp assets/props/manifest.json "$staging/Contents/Resources/asset-provenance/props.json"
cp "${CARGO_TARGET_DIR:-target}/release/sauerteig-client" "$staging/Contents/MacOS/sauerteig-client"
chmod +x "$staging/Contents/MacOS/sauerteig-client"
cat > "$staging/Contents/Info.plist" <<'PLIST'
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
  <key>NSLocalNetworkUsageDescription</key>
  <string>Sauerteig connects to worlds hosted on your local network.</string>
  <key>NSPrincipalClass</key>
  <string>NSApplication</string>
  <key>LSApplicationCategoryType</key>
  <string>public.app-category.action-games</string>
</dict>
</plist>
PLIST

# An ad hoc signature covers the bundle's Info.plist and resources. The linker
# signs only the executable, and macOS calls such a bundle damaged once it is
# quarantined. Ad hoc signing needs no Apple developer account.
codesign --force --sign - "$staging"
rm -rf "$sauerteig_app"
mv "$staging" "$sauerteig_app"

echo "Local development app ready: $sauerteig_app"
echo "The app is signed ad hoc, not by a registered developer. Open it to create a world or join a server."
