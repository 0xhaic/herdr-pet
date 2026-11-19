#!/usr/bin/env bash
# 组装 dist/herdr-pet.app。用法：scripts/bundle.sh [target-triple]
# 不传 target 时使用 rustc 主机三元组；CI 传入 arm64 目标。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
APP="$ROOT/dist/herdr-pet.app"

if ! rustup target list --installed | grep -qx "$TARGET"; then
    rustup target add "$TARGET"
fi
cargo build --release --manifest-path "$ROOT/Cargo.toml" --target "$TARGET"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/target/$TARGET/release/herdr-pet" "$APP/Contents/MacOS/herdr-pet"
cp "$ROOT/packaging/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"
sed "s/@VERSION@/$VERSION/g" "$ROOT/packaging/Info.plist" > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist" > /dev/null
# ad-hoc 签名：Apple Silicon 要求 arm64 可执行文件带签名；不含 Developer ID/公证。
codesign --force --sign - "$APP"

echo "built $APP ($TARGET, version $VERSION)"
