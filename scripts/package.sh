#!/usr/bin/env bash
# 配布用のアーカイブを dist/ に作る。
#   scripts/package.sh <バージョン> <名前> <実行ファイル>
# 名前が macos-* なら .app、linux-* ならメニュー用のファイルとアイコンつき、windows-* なら zip にする。
set -euo pipefail

version=$1
name=$2
binary=$3

root=$(cd "$(dirname "$0")/.." && pwd)
assets="$root/crates/app/assets"
dir="malakt-mahoz-v$version-$name"
out="$root/dist/$dir"

rm -rf "$out"
mkdir -p "$out"
cp "$root/README.md" "$root/LICENSE" "$out/"
cp "$assets/fonts/OFL.txt" "$out/LICENSE-LINESeedJP.txt"
cp "$assets/icons/LICENSE" "$out/LICENSE-Lucide.txt"

case "$name" in
  macos-*)
    app="$out/Malakt Mahoz.app/Contents"
    mkdir -p "$app/MacOS" "$app/Resources"
    cp "$binary" "$app/MacOS/malakt-mahoz"
    cp "$assets/icon/icon.icns" "$app/Resources/icon.icns"
    sed "s/@VERSION@/$version/g" "$root/packaging/Info.plist" > "$app/Info.plist"
    # 署名のないアプリは Apple シリコンで起動できないので、その場かぎりの署名を付ける
    if command -v codesign >/dev/null; then
      codesign --force --deep --sign - "$out/Malakt Mahoz.app"
    fi
    tar -C "$root/dist" -czf "$root/dist/$dir.tar.gz" "$dir"
    ;;
  linux-*)
    cp "$binary" "$out/malakt-mahoz"
    cp "$root/packaging/malakt-mahoz.desktop" "$out/malakt-mahoz.desktop"
    cp "$assets/icon/icon.png" "$out/io.github.richinosan.malakt-mahoz.png"
    cp "$root/packaging/install.sh" "$out/install.sh"
    chmod +x "$out/install.sh" "$out/malakt-mahoz"
    tar -C "$root/dist" -czf "$root/dist/$dir.tar.gz" "$dir"
    ;;
  windows-*)
    cp "$binary" "$out/malakt-mahoz.exe"
    (cd "$root/dist" && 7z a "$dir.zip" "$dir" >/dev/null)
    ;;
  *)
    echo "知らない名前です: $name" >&2
    exit 1
    ;;
esac

rm -rf "$out"
ls -l "$root/dist"
