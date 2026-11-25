# Claude Code などからタスクを読み書きする（MCP）

ターミナルで動く Claude Code などのエージェントから、タスク、ワークスペース、担当者、コメントを読み書きできます。アプリの実行ファイルが MCP サーバーになります。標準入出力でやり取りするだけで、ネットワークもポートも使いません。

```sh
malakt-mahoz mcp
```

## 登録する

Claude Code には、1 回だけ次のコマンドで登録します。どのフォルダーで開いた claude からも使えます。

```sh
claude mcp add --scope user malakt-mahoz -- /path/to/malakt-mahoz mcp
```

| エージェント | 登録のしかた |
| --- | --- |
| Claude Code | 上のコマンド。フォルダーごとにするなら、`malakt-mahoz mcp config` の出力を `.mcp.json` に置くか、`claude --mcp-config <ファイル>` で渡す |
| Codex CLI | `codex mcp add malakt-mahoz -- /path/to/malakt-mahoz mcp` |
| Cursor | `malakt-mahoz mcp config` の出力を `~/.cursor/mcp.json` に置く |
| そのほか | `mcpServers` の形の設定を読むものには、`malakt-mahoz mcp config` の出力を渡す。MCP を持たないものは、下のコマンドを使う |

### WSL から Windows のアプリを使う

WSL の中のエージェントには、Windows の実行ファイルをそのまま登録します。サーバーは Windows の側で動き、Windows のアプリと同じデータを読み書きします。

```sh
claude mcp add --scope user malakt-mahoz -- "/mnt/c/<malakt-mahoz.exe を置いたフォルダー>/malakt-mahoz.exe" mcp
```

WSL の中から SQLite のファイル（`/mnt/c/...`）を直接開く方法は使いません。

### アプリのターミナルから

アプリの中でターミナルを開くと、そのシェル（WSL の中を含む）の Claude Code と Cursor に、上の登録が自動で入ります。手で登録する必要はありません（[docs/terminal.md](terminal.md)）。

ターミナルのタブの右のプラグのボタンからも登録できます。Claude Code と Codex は、見ているシェルで有効なコマンドが入力の行に入ります。Cursor はその場で `~/.cursor/mcp.json` に書き足します。

### アプリを閉じると、サーバーも終わる

`malakt-mahoz mcp` のサーバーは、Claude Code や Cursor が起動します。アプリが裏で起動しておくものではありません。

サーバーは、アプリが動いている間だけ動きます。アプリを閉じると、動いているサーバーは 1 秒以内に終わります。アプリが動いていないときに起動したサーバーは、すぐに終わります。サーバーが残って実行ファイルをつかんだままになると、新しい版に入れ替えられないためです。

アプリを起動し直したあとは、エージェントの側でつなぎ直します（Claude Code は `/mcp`、Cursor は設定の MCP の一覧）。`malakt-mahoz tasks` などのコマンドは、アプリが動いていなくても使えます。

### Windows のコマンドの出力

Windows のアプリは、起動のときに黒い窓を出さないように作ってあります。PowerShell やコマンドプロンプトから `malakt-mahoz.exe mcp config` のように呼ぶと、出力はそのターミナルに出ます。シェルはアプリの終わりを待たないので、出力は次のプロンプトのあとに出ます。ファイルに書くときは `malakt-mahoz.exe mcp config > mcp.json` のようにリダイレクトします。

### アプリのシェルから

アプリのシェルで起動したコマンドには、次の環境変数が渡ります。

| 環境変数 | 中身 |
| --- | --- |
| `MALAKT_MAHOZ_BIN` | アプリの実行ファイルの場所 |
| `MALAKT_MAHOZ_DB` | データベースの場所 |

## ツール

| ツール | すること |
| --- | --- |
| `list_workspaces` / `list_people` | ワークスペース、担当者の一覧 |
| `list_tasks` | タスクの一覧（ワークスペース、文字で絞り込み。完了・中止は `include_done`） |
| `get_task` | タスク 1 件と、コメント、サブタスク |
| `create_task` / `update_task` / `move_task` / `delete_task` | タスクを作る、書き換える、移す、削除する |
| `add_comment` | コメントを書く（書き手は自分）。書くのは、あとでタスクを読む人に要る事実だけ。作業をしたことだけを伝える文と、していないことは書かない |
| `create_workspace` / `create_person` | ワークスペース、担当者を作る |
| `undo` / `redo` | このサーバーからの変更を 1 つ元に戻す、やり直す |

- 変更は、開いているアプリの画面に数秒で出ます
- 1 回の呼び出しが、元に戻す 1 段になります。記録はサーバーのメモリの中だけにあり、サーバーが終わると消えます（アプリの「元に戻す」と同じ仕組みで、段の数と大きさの上限も同じです）
- アプリの `Ctrl/⌘ + Z` が戻すのは、アプリの中での操作です。サーバーからの変更は、サーバーの `undo` で戻します

## ロック

ロックを有効にしているときは、アプリでロックを解除している間だけ読み書きできます。アプリをロックする、または閉じると、動いているサーバーもすぐ読めなくなり、「ロックされています」と返します。解除すると、サーバーを起動し直さずに続けられます。

アプリは解除している間、その時刻を 1 分ごとにデータベースに書き、サーバーは 150 秒以内の時刻があるときだけ動きます。アプリが異常終了したときは、150 秒たつと読めなくなります。

## コマンド

MCP を持たないエージェントやスクリプトからは、同じツールをコマンドで 1 回ずつ呼べます。結果は JSON を 1 行で出し、終了コードは成功が 0、失敗が 1、使い方の誤りが 2 です。ロックの扱いは MCP と同じです。

```sh
malakt-mahoz workspaces
malakt-mahoz people
malakt-mahoz tasks --workspace <ID> --query 告知 --all
malakt-mahoz task <ID>
malakt-mahoz call create_task '{"title":"告知を出す","due":"2026-12-01"}'
malakt-mahoz call add_comment '{"task_id":"<ID>","body":"下書きができた"}'
```

コマンドは 1 回ごとに終わるので、`undo` は使えません。
