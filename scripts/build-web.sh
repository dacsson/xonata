#!/usr/bin/env bash
set -euo pipefail
if [[ ! -f Cargo.lock ]]; then
  cargo generate-lockfile
fi
cargo build --locked --release --target wasm32-unknown-unknown -p xonata-web
wasm-bindgen --target web --out-dir web/pkg target/wasm32-unknown-unknown/release/xonata_web.wasm
cp crates/app/assets/fonts/LICENSE web/font-license.txt
echo 'Serve web/ over HTTP; open index.html or embed-example.html.'
