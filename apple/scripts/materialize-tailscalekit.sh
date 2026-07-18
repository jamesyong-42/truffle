#!/usr/bin/env bash
# Materialize the exact TailscaleKit binary consumed by Package.swift.
set -euo pipefail

REVISION="5e89501def80a6579ca5d0f9a02f336be62b8f2e"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SOURCE="${TAILSCALE_SOURCE_DIR:-$ROOT/.vendor/libtailscale}"
DESTINATION="$ROOT/Vendor/TailscaleKit.xcframework"
DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
export DEVELOPER_DIR
export PATH="$DEVELOPER_DIR/usr/bin:$PATH"

if [[ ! -d "$DEVELOPER_DIR" ]]; then
  echo "error: full Xcode is required; set DEVELOPER_DIR" >&2
  exit 1
fi

if [[ ! -d "$SOURCE/.git" ]]; then
  mkdir -p "$(dirname "$SOURCE")"
  git init "$SOURCE"
  git -C "$SOURCE" remote add origin https://github.com/tailscale/libtailscale.git
  git -C "$SOURCE" fetch --depth 1 origin "$REVISION"
  git -C "$SOURCE" checkout --detach FETCH_HEAD
fi

ACTUAL_REVISION="$(git -C "$SOURCE" rev-parse HEAD)"
if [[ "$ACTUAL_REVISION" != "$REVISION" ]]; then
  echo "error: libtailscale is $ACTUAL_REVISION; expected $REVISION" >&2
  exit 1
fi
if [[ -n "$(git -C "$SOURCE" status --porcelain)" ]]; then
  echo "error: libtailscale source has local modifications" >&2
  exit 1
fi

echo "Building TailscaleKit from $REVISION"
make -C "$SOURCE/swift" ios-fat
ARTIFACT="$SOURCE/swift/build/Build/Products/Release-iphonefat/TailscaleKit.xcframework"
if [[ ! -d "$ARTIFACT" ]]; then
  echo "error: build did not produce $ARTIFACT" >&2
  exit 1
fi

mkdir -p "$ROOT/Vendor"
ditto "$ARTIFACT" "$DESTINATION"
cp "$SOURCE/LICENSE" "$ROOT/Vendor/TAILSCALE-LICENSE"

echo "Materialized $DESTINATION"
find "$DESTINATION" -type f -maxdepth 5 -print | sort | xargs shasum -a 256

