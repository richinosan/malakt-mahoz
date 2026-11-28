# プラグイン

プラグインで、タスクやワークスペースの右クリックのメニューにコマンドを足せます。プラグインは [WebAssembly](https://webassembly.org/)（WASM）で、[Extism](https://extism.org/) の上で動きます。アプリの操作には、MCP やアプリの中の AI と同じツール（[docs/api.md](api.md)）を使います。

## 置く場所

フォルダーの直下に、プラグインごとのフォルダー（`plugin.toml` と WASM のファイル）を置きます。

| フォルダー | 変え方 |
| --- | --- |
| `~/.malakt-mahoz/plugins`（標準） | 設定の「プラグイン」でオフにできる。データベースのフォルダーとは別 |
| 設定で足したフォルダー | 設定の「プラグイン」の「＋」。いくつでも足せる |
| 環境変数 `MALAKT_MAHOZ_PLUGINS` のフォルダー | OS のパスの区切り（macOS・Linux は `:`、Windows は `;`）で複数 |

上の表の順に読み、同じ ID のプラグインがあれば先に見つけたものを使います。

```
~/.malakt-mahoz/plugins/
  task-markdown/
    plugin.toml
    plugin.wasm
```

## 有効にする

見つけたプラグインは、最初は無効です。設定の「プラグイン」でスイッチを押すと、プラグインに許すこと（権限）とコマンドの場所が出ます。「有効にする」を押すと有効になります。ロックが有効なら、パスキーでも確かめます。

有効にしたときの `plugin.toml` と WASM の中身（ハッシュ）を覚えます。そのあとでファイルが変わったら「変更あり」になり、もう一度有効にするまで動きません。

## plugin.toml

```toml
id = "task-markdown"          # 英小文字・数字・-（64 文字まで）。承認はこの ID で覚える
name = "Markdown でコピー"     # 1〜60 文字
version = "1.0.0"
api = 1                       # 使う API の版。今は 1
description = "タスクとサブタスクを Markdown のチェックリストにしてコピーする"
permissions = ["read"]        # read・write
wasm = "plugin.wasm"          # 省くと plugin.wasm。plugin.toml のフォルダーの中のファイル

[[commands]]
id = "copy"                   # 英小文字・数字・-
title = "Markdown でコピー"    # メニューに出す名前
on = "task"                   # task（タスクの右クリック）・workspace（ワークスペースの右クリック）
```

書いていない項目があると読み込みません。設定の「プラグイン」に、どのファイルの何が正しくないかが出ます。

| 権限 | 呼べるツール |
| --- | --- |
| `read` | `list_` と `get_` で始まるもの |
| `write` | それ以外（作る・書き換える・削除する） |

PC のファイルを読むツール（`add_attachment`、`add_schedule_attachment`）と、アプリの中の AI だけのツール（シェル、GitHub、Slack）は呼べません。プラグインは、ファイルとネットワークにも触れられません。WASI は付けますが、フォルダーを渡さず、ネットワークも許しません（使えるのは時計と乱数など）。HTTP の関数は付けません。

## 中身（ABI）

WASM は関数 `command` を出します。

- 入力（JSON）: `{"command": "<コマンドの id>", "task_id": "…"}`（`on = "task"`）か `{"command": "…", "workspace_id": "…"}`（`on = "workspace"`）
- 出力（JSON）: `{"notice": "画面に出す知らせ", "copy": "クリップボードに入れる文字"}`。どちらも省けます。空の出力でもかまいません。ほかの項目があると失敗にします

アプリの操作は、ホストの関数 `call_tool`（名前空間 `extism:host/user`）で行います。

- 入力: `{"name": "get_task", "args": {"id": "…"}}`
- 返事: `{"ok": <ツールの結果>}` か `{"error": "<文>"}`

ツールの名前と引数は `malakt-mahoz api` で出せます。書き込みは、ツールの呼び出し 1 回ずつ元に戻す段になり（`Ctrl/⌘ + Z`）、変更履歴にはプラグインの名前が残ります。

1 回のコマンドは 5 秒、メモリ 64 MiB までで、超えたら止めます。失敗したら、画面にプラグインの名前とコマンドと理由が出ます。

## 作り方

Extism の PDK がある言語で作れます。見本は Rust です（[examples/plugins/task-markdown](../examples/plugins/task-markdown)）。

```sh
mise run plugin-example     # 見本をビルドして plugin.wasm を作り直す
```

| 言語 | PDK | WASM にするもの |
| --- | --- | --- |
| Rust | [extism-pdk](https://github.com/extism/rust-pdk) | `cargo build --target wasm32-unknown-unknown` |
| Go | [go-pdk](https://github.com/extism/go-pdk) | [TinyGo](https://tinygo.org/)（`tinygo build -target wasip1 -buildmode=c-shared`）。ふつうの Go（1.24 以降、`GOOS=wasip1 go build -buildmode=c-shared`）でも作れるが、ファイルが大きくなる |
| JavaScript | [js-pdk](https://github.com/extism/js-pdk) | `extism-js` |
| ほか | Zig、C、AssemblyScript、.NET、Haskell | 各 PDK の手順 |

## アプリの大きさ

Extism（Wasmtime）を入れたことで、アプリの実行ファイルは約 15 MB 大きくなりました。コンパイルしたプラグインのキャッシュはディスクに置きません。

## AI から

アプリの中の AI は `list_plugins`、`run_plugin_command`、`set_plugin_enabled`、`plugin_folder`、`set_plugin_default_folder` を使えます。有効にするときは承認の画面を出し、ユーザーが「有効にする」を押したときだけ有効になります。
