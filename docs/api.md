# API の定義（TypeSpec）

アプリの中の AI、MCP のサーバー（[docs/mcp.md](mcp.md)）、コマンド（`malakt-mahoz call`）が使うツールは、[TypeSpec](https://typespec.io/) で `api/` に 1 か所で定義しています。名前・説明・引数と、書き込みか、確認が要るかなどの印は、ここに書いたものがそのまま使われます。

```sh
malakt-mahoz api                    # ツールの一覧（JSON）を出す
malakt-mahoz api tsp <フォルダー>     # 入っている版の API の定義（*.tsp）を書き出す
```

どちらも、入っているアプリの版のものです。ネットワークは使いません。

## ファイル

| ファイル | 中身 |
| --- | --- |
| `api/main.tsp` | 入口。下の 3 つを読む |
| `api/data.tsp` | `Malakt.Data`: データベースの操作。アプリの中の AI、MCP、コマンドのすべてに出す |
| `api/app.tsp` | `Malakt.App`: 画面と設定の操作。アプリの中の AI にだけ出す |
| `api/mcp.tsp` | `Malakt.Mcp`: MCP のサーバーだけが持つ操作（スタートアップ、`undo` / `redo`） |
| `api/lib.tsp`・`api/lib.js` | ツールに付ける印 |
| `api/build.mjs` | `*.tsp` から `api/generated/tools.json` を作る |
| `api/generated/tools.json` | ツールの一覧。アプリはこれを実行ファイルに埋め込んで読む（`crates/ai/src/api.rs`） |

## 印

| 印 | 意味 |
| --- | --- |
| `@writes` | データベースを書き換える。1 回の呼び出しが、元に戻す 1 つの段になる。読み取りのみのアクセスキーでは呼べない |
| `@confirm` | アプリの中の AI が呼ぶときに、実行の前に確認を出す。中身によって確認を出すもの（上書きになるエクスポートなど）はアプリのコードで決める |
| `@inAppOnly` | アプリの中の AI にだけ渡す。MCP とコマンドには出さない |
| `@view` | 画面を動かす。メインのウィンドウで実行する |
| `@session` | AI の設定を変える。チャットで実行する |
| `@localFiles` | ユーザーの PC のファイルを読む。プラグインからは呼べない |
| `@closed` | （モデルに付ける）書いていない項目を受け付けない |

ツールの引数は、どれも書いていない項目を受け付けません。説明（`/** */`）は、行を区切りなしでつなぎます。日本語の文は、文の終わりで折り返して書きます。

## 変えるとき

1. `api/*.tsp` を書き換える
2. `mise run api` で `api/generated/tools.json` を作り直す
3. 新しいツールなら、実装を `crates/ai/src/tools.rs`（データベース）か `crates/app/src/ai_tools.rs`（画面と設定）に足す

`mise run verify`（pre-commit フック）は、`tools.json` が `*.tsp` と合っているかを確かめます。定義だけあって実装がないツールと、`*.tsp` に書いた候補や上限の数がアプリのコードとずれているところは、テストで見つかります。Node は `mise` で入り、使うのは定義を変えるときだけです。アプリのビルドには要りません。
