#!/bin/sh
set -eu

SAUERTEIG_ROOT=$(CDPATH= cd -P "$(dirname "$0")/.." && pwd)
cd "$SAUERTEIG_ROOT"
. "$SAUERTEIG_ROOT/scripts/env.sh"

if ! command -v cargo >/dev/null 2>&1; then
  echo "Rust is required. Install it from https://rustup.rs, then see README.md." >&2
  exit 1
fi
if ! command -v wasm-bindgen >/dev/null 2>&1; then
  echo "Install the matching browser build tool: cargo install wasm-bindgen-cli --version 0.2.100 --locked" >&2
  exit 1
fi
if [ "$(wasm-bindgen --version)" != "wasm-bindgen 0.2.100" ]; then
  echo "This project needs wasm-bindgen-cli 0.2.100 to match its Rust dependency." >&2
  echo "Run: cargo install wasm-bindgen-cli --version 0.2.100 --locked --force" >&2
  exit 1
fi

cargo build --release --locked -p sauerteig-client --target wasm32-unknown-unknown --lib
sauerteig_target_dir=${CARGO_TARGET_DIR:-target}
mkdir -p web/dist/pkg
wasm-bindgen \
  --target web \
  --out-dir web/dist/pkg \
  --out-name sauerteig \
  "$sauerteig_target_dir/wasm32-unknown-unknown/release/sauerteig_client.wasm"
cp web/index.html web/style.css web/dist/
cp LICENSE web/dist/LICENSE.txt
cp THIRD_PARTY_NOTICES.md web/dist/THIRD_PARTY_NOTICES.txt
mkdir -p web/dist/asset-provenance
cp assets/materials/manifest.json web/dist/asset-provenance/materials.json
cp assets/props/manifest.json web/dist/asset-provenance/props.json
echo "Browser build ready in web/dist. Start it with: cargo run -p sauerteig-server"
