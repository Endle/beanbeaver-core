#!/usr/bin/env bash
# Host Swift/UniFFI contract check; does not modify or launch a mobile app.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$repo_root"
export CARGO_TARGET_DIR="$repo_root/target"
smoke_dir="$(mktemp -d /tmp/bb-gift-ffi.XXXXXX)"
cargo build -p bb-receipt-ffi --lib --bin uniffi-bindgen
target/debug/uniffi-bindgen generate --library target/debug/libbb_receipt_ffi.dylib \
  --language swift --out-dir "$smoke_dir"
swiftc -parse-as-library -module-cache-path "$smoke_dir/cache" \
  -I "$smoke_dir" -Xcc "-fmodule-map-file=$smoke_dir/bb_receipt_ffiFFI.modulemap" \
  "$smoke_dir/bb_receipt_ffi.swift" crates/ffi/tests/gift_card_smoke.swift \
  -L target/debug -lbb_receipt_ffi -Xlinker -rpath -Xlinker "$repo_root/target/debug" \
  -o "$smoke_dir/smoke"
"$smoke_dir/smoke"
echo "Bindings and executable: $smoke_dir"
