#!/usr/bin/env bash
# 自分のユーザーだけに入れる。アプリの一覧に、アイコンつきで出るようになる。
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
data=${XDG_DATA_HOME:-$HOME/.local/share}
bin=$HOME/.local/bin

install -Dm755 "$here/malakt-mahoz" "$bin/malakt-mahoz"
install -Dm644 "$here/io.github.richinosan.malakt-mahoz.png" \
  "$data/icons/hicolor/256x256/apps/io.github.richinosan.malakt-mahoz.png"
install -Dm644 "$here/malakt-mahoz.desktop" \
  "$data/applications/io.github.richinosan.malakt-mahoz.desktop"
sed -i "s|^Exec=.*|Exec=$bin/malakt-mahoz|" \
  "$data/applications/io.github.richinosan.malakt-mahoz.desktop"
echo "$bin/malakt-mahoz に入れました。"
