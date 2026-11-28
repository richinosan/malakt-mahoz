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

ツールの定義は `api/*.tsp`（TypeSpec）にあり、`malakt-mahoz api` で JSON の一覧を出せます（[docs/api.md](api.md)）。

| ツール | すること |
| --- | --- |
| `list_workspaces` / `list_people` | ワークスペース、担当者の一覧 |
| `list_tasks` | タスクの一覧（ワークスペース、文字で絞り込み。完了・中止は `include_done`） |
| `get_task` | タスク 1 件と、コメント、サブタスク、添付、変更履歴 |
| `create_task` / `update_task` / `move_task` / `delete_task` | タスクを作る、書き換える、移す、削除する。`assignees` で担当者と役（RACI）をまとめて決められる。URL は `urls`（全部を置き換える）、`add_urls`、`remove_urls` |
| `set_assignee` | タスクの担当者 1 人の役（R 実行責任者・A 説明責任者・C 相談先・I 報告先）を決める。担当でない人は担当に加え、役を空にすると担当から外す |
| `set_assignee_done` | 担当者 1 人の「自分の分」の完了を付ける・外す（ほかの人の分も）。全員そろうとタスクが完了になり、完了したタスクで外すと進行中に戻る |
| `link_task` / `reorder_tasks` | タスクをほかのワークスペースにも出す・外す、手動の順に並べる |
| `add_comment` | コメントを書く（書き手は自分。返信とレビューコメントも）。書くのは、あとでタスクを読む人に要る事実だけ。作業をしたことだけを伝える文と、していないことは書かない |
| `update_comment` / `delete_comment` | コメントを書き直す・ピン留めする・解決済みにする、削除する |
| `add_attachment` / `delete_attachment` | ファイルを添付する、添付を削除する |
| `issue_ticket` / `revoke_tickets` | 完了チケットを発行する、取り消す |
| `create_workspace` / `update_workspace` / `delete_workspace` | ワークスペースを作る、名前を変える・移す・並べ替える、削除する |
| `get_workspace_settings` / `update_workspace_settings` | ワークスペースの基準日、タスクを追加したときに入る値、ターミナルのフォルダー |
| `list_ai_rules` / `create_ai_rule` / `update_ai_rule` / `delete_ai_rule` | AI のルール（グローバルと、ワークスペースごと）を読む、足す、変える、削除する。サーバーの説明で、有効なルールに従うよう伝えている |
| `create_person` / `update_person` / `delete_person` | 担当者を作る、名前・メモ・使うワークスペースを変える、削除する。`create_person` に `workspace_id` を渡すと、そのワークスペースの担当者になる |
| `list_categories` / `list_notices` / `mark_notices_read` | カテゴリの一覧、通知の一覧、通知を既読にする |
| `get_due_alerts` / `set_due_alerts` | 期限をいつ知らせるか（全体の標準と、タスクごとの上書き） |
| `get_done_notices` / `set_done_notices` | ほかの人が完了を報告したときに知らせるか（オン・オフ、知らせるとき、役、知らせない担当者・ワークスペース） |
| `set_repeat` | タスクに繰り返し（毎日・毎週◯曜・毎月◯日・毎年、何日・何週間ごと、次の回を作る時刻）を付ける・変える・外す。`ai_instruction` で、回を作ったときにアプリの AI に頼むことを書く（[docs/repeat.md](repeat.md)）。今の繰り返しは `get_task` の `repeat` |
| `list_schedules` / `create_schedule` / `update_schedule` / `delete_schedule` | 定時タスク（決まった時刻に雛形から新しいタスクを作る。ワークスペースごとと未所属）を読む、足す、変える、削除する。雛形にはタスクと同じ項目（URL・状態・優先度・開始日・期限・通知・進捗・予想工数・所要時間の基準・担当と役・カテゴリ・同期先・親タスク・説明・サブタスク）を入れられる。`ai_instruction` で、作ったタスクでアプリの AI に頼むことを書く（[docs/schedule.md](schedule.md)） |
| `add_schedule_attachment` / `delete_schedule_attachment` | 定時タスクの雛形にファイルを添付する、外す。作ったタスクに同じファイルが添付される |
| `get_time_stats` / `set_time_basis` | 所要時間の統計（全体かワークスペース、期間、担当者ごと・ワークスペースごと）を読む、所要時間の基準（開始と完了）を全体の標準かワークスペースに決める。タスクの予想工数と基準は `create_task` / `update_task` の `estimate` / `time_basis`、所要時間は `get_task` の `time`（[docs/timing.md](timing.md)） |
| `get_pomodoro` / `pomodoro` / `list_work_logs` | ポモドーロタイマーの状態を読む、始める・止める・設定を変える、作業の記録を読む（[docs/pomodoro.md](pomodoro.md)） |
| `get_autostart` / `set_autostart` | PC にログインしたときにアプリを起動するか（スタートアップ）を読む、変える。OS の登録を書き換えるので `undo` では戻らない（[docs/startup.md](startup.md)） |
| `get_on_top` / `set_on_top` | アプリのウィンドウ（`task` はメインのウィンドウ、`pomodoro`、`chat`、`shell`）を常に最前面に表示しているかを読む、切り替える（`{"on": true, "window": "pomodoro"}`）。切り替えはデータベースに書き、開いているアプリが 2 秒以内に当てる。`undo` では戻らない |
| `undo` / `redo` | このサーバーからの変更を 1 つ元に戻す、やり直す |

- 担当者は ID でも名前でも指定できます。登録されていない名前は、タスクのワークスペースの担当者としてその場で登録します（どこにも入っていないタスクでは、すべてのワークスペースの担当者）
- 変更は、開いているアプリの画面に数秒で出ます
- 1 回の呼び出しが、元に戻す 1 段になります。記録はサーバーのメモリの中だけにあり、サーバーが終わると消えます（アプリの「元に戻す」と同じ仕組みで、段の数と大きさの上限も同じです）
- アプリの `Ctrl/⌘ + Z` が戻すのは、アプリの中での操作です。サーバーからの変更は、サーバーの `undo` で戻します

## アクセスキー

エージェントごとにアクセスキーを渡して、できることを分けられます。キーはアプリの設定の「セキュリティ」の「アクセスキー」で発行し、環境変数 `MALAKT_MAHOZ_KEY` で渡します。

| キーごとに選べること | 標準 |
| --- | --- |
| 読み書き / 読み取りのみ（`list_` と `get_` で始まるツールだけ） | 読み書き |
| ロックを解除している間だけ / ロック中も使える | ロックを解除している間だけ |

- キーの文字列は、発行したときにだけ表示されます。データベースにはハッシュと最後の 4 文字だけを置きます
- 発行したキーの行から、キーそのもの、Claude Code に登録するコマンド、MCP の設定（JSON）をコピーできます
- 失効させたキーや、できることを変えたキーは、動いているサーバーにもすぐ効きます（呼び出しのたびに確かめます）
- 「ロック中も使える」をオンにするときは、ロックが有効ならパスキーの確認が出ます
- キーの一覧で、キーにマウスを乗せると、発行した日時と最後に使った日時が出ます
- アプリのターミナルの MCP に登録するボタン（プラグ）のメニューの「アクセスキーを設定」からも開けます

### 期限と時間帯

キーの行の時計のボタンで、そのキーを使える期間と時間帯を決められます。どれもその PC の時刻で見ます。

| 項目 | 書き方 |
| --- | --- |
| 開始 | 日時（`12/1`、`12/1 9:00`、`明日` など、タスクの期限と同じ書き方）。日付だけならその日の始まりから |
| 期限 | 開始と同じ書き方。日付だけならその日の終わりまで。砂時計のボタンで「1 時間」「今日の終わりまで」「7 日」「30 日」「90 日」も選べる |
| 時間帯 | 曜日と時刻（`9:00〜18:00`）。いくつでも足せて、どれかに入っていれば使える。終わりが始まりより前なら日をまたぐ（`22:00〜6:00` は、その曜日の夜から翌朝まで） |

期間か時間帯の外では、MCP は理由を添えたエラー（「アクセスキー「Claude Code」を使える時間帯は 平日 9:00〜18:00 です。」など）を返し、コマンドは同じ文を出して終了コード 3 で終わります。呼び出しのたびに確かめるので、動いているサーバーも期限や時間帯の外になった次の呼び出しから使えなくなります。今使えないキーには、一覧の名前の横に印が出ます。

```sh
claude mcp add --scope user malakt-mahoz -e MALAKT_MAHOZ_KEY=<キー> -- /path/to/malakt-mahoz mcp
codex mcp add malakt-mahoz --env MALAKT_MAHOZ_KEY=<キー> -- /path/to/malakt-mahoz mcp
MALAKT_MAHOZ_KEY=<キー> malakt-mahoz tasks
```

Cursor などの `mcpServers` の形の設定では、`env` に `MALAKT_MAHOZ_KEY` を書きます。`MALAKT_MAHOZ_KEY` を入れた環境で `malakt-mahoz mcp config` を呼ぶと、キーの入った設定が出ます。

### キーなしの接続

キーを渡さずにつないできたものの扱いは、設定の「アクセスキー」の「キーなしの接続」で選びます。

| 選べるもの | できること |
| --- | --- |
| 読み書き（標準） | アクセスキーを入れる前と同じ。ロックを解除している間だけ、すべてのツール |
| 読み取りのみ | ロックを解除している間だけ、読み取りのツール |
| 使えない | 何もできない。アクセスキーが要る |

アプリのターミナルが自動で入れる登録（[アプリのターミナルから](#アプリのターミナルから)）は、キーなしの接続です。「使えない」にしたときは、そのエージェントにもキーを付けて登録し直します。Cursor の `~/.cursor/mcp.json` に書いたキーは、アプリのターミナルが登録を書き直すときもそのまま残ります。

## ロック

ロックを有効にしているときは、アプリでロックを解除している間だけ読み書きできます（ロック中も使えるアクセスキーを除く）。アプリをロックする、または閉じると、動いているサーバーもすぐ読めなくなり、「ロックされています」と返します。解除すると、サーバーを起動し直さずに続けられます。

アプリは解除している間、その時刻を 1 分ごとにデータベースに書き、サーバーは 150 秒以内の時刻があるときだけ動きます。アプリが異常終了したときは、150 秒たつと読めなくなります。

MCP のサーバーはアプリが動いている間だけ動くので、アプリを閉じている間に使えるのは、ロック中も使えるアクセスキーを渡したコマンド（`malakt-mahoz tasks` など）だけです。

## コマンド

MCP を持たないエージェントやスクリプトからは、同じツールをコマンドで 1 回ずつ呼べます。結果は JSON を 1 行で出し、終了コードは成功が 0、失敗が 1、使い方の誤りが 2、ロックやアクセスキー（期限・時間帯・読み取りのみを含む）で使えないときが 3 です。アクセスキーとロックの扱いは MCP と同じです。

```sh
malakt-mahoz workspaces
malakt-mahoz people
malakt-mahoz tasks --workspace <ID> --query 告知 --all
malakt-mahoz task <ID>
malakt-mahoz call create_task '{"title":"告知を出す","due":"2026-12-01"}'
malakt-mahoz call add_comment '{"task_id":"<ID>","body":"下書きができた"}'
```

コマンドは 1 回ごとに終わるので、`undo` は使えません。
