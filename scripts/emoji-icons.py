#!/usr/bin/env python3
"""絵文字のアイコン（crates/app/assets/icons/emoji.tsv.gz）を作る。

    npm pack @iconify-json/fluent-emoji-high-contrast emojibase-data
    で取った 2 つを展開し、その package フォルダを順に渡す:
    scripts/emoji-icons.py <fluent-emoji-high-contrast/package> <emojibase-data/package>

1 行に 1 アイコン。「名前、カテゴリ、タグ（日本語、空白区切り）、SVG の中身」をタブで区切る。
"""
import gzip
import json
import pathlib
import sys

icons_dir, emojibase_dir = map(pathlib.Path, sys.argv[1:3])
out = pathlib.Path(__file__).parent.parent / "crates/app/assets/icons/emoji.tsv.gz"

CATEGORIES = {
    "Animals & Nature": "nature",
    "Food & Drink": "food",
    "Travel & Places": "travel",
    "Activities": "activity",
    "Objects": "object",
}

icons = json.loads((icons_dir / "icons.json").read_text())["icons"]
categories = json.loads((icons_dir / "metadata.json").read_text())["categories"]
chars = json.loads((icons_dir / "chars.json").read_text())


def key(hexcode):
    return "-".join(p for p in hexcode.lower().split("-") if p != "fe0f")


japanese = {}
for emoji in json.loads((emojibase_dir / "ja/data.json").read_text()):
    japanese[key(emoji["hexcode"])] = (emoji.get("order", 10**6), [emoji["label"], *emoji.get("tags", [])])
by_name = {name: japanese[key(code)] for code, name in chars.items() if key(code) in japanese}

rows = []
for category, short in CATEGORIES.items():
    for name in categories[category]:
        full = f"emoji-{name}"
        if len(full) > 40 or name not in icons:
            continue
        order, words = by_name.get(name, (10**6, []))
        tags = " ".join(dict.fromkeys(w.replace("\t", " ") for w in words))
        rows.append((order, full, short, tags, icons[name]["body"]))
rows.sort()
text = "".join(f"{name}\t{short}\t{tags}\t{body}\n" for _, name, short, tags, body in rows)
with open(out, "wb") as file:
    with gzip.GzipFile(fileobj=file, mode="wb", compresslevel=9, mtime=0, filename="") as zipped:
        zipped.write(text.encode())
print(len(rows), "icons,", out.stat().st_size, "bytes")
