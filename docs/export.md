# エクスポートの形式

zip の中身は次のとおりです。OS が違っても同じ結果になるようにしています。

```
manifest.json          形式の名前とバージョン、件数
data/workspaces.json
data/people.json
data/tasks.json
data/comments.json
data/changes.json
data/task_defaults.json     ワークスペースごとのタスク追加時の既定値
data/attachments.json       添付の名前と大きさ
data/repeats.json           タスクに付けた繰り返しと AI への指示
data/schedules.json         定時タスク（雛形と次に作る日時）
data/schedule_files.json    定時タスクの雛形の添付（中身は files/）
files/<SHA-256>             添付の中身（同じ中身は 1 つだけ）
```

- zip の中のファイル名は ASCII だけです。タイトルなどの文字列はファイル名に使いません（文字コードの違い、Windows の予約名、パス区切りの違いを避けるため）
- 中身は UTF-8（BOM なし）の JSON で、文字列は NFC、改行は LF にそろえます
- 読むときは、BOM・CRLF・NFD（macOS のファイル名由来の濁点が分かれた形）が混ざっていても受け入れ、正規化して取り込みます
- 取り込みは 1 つのトランザクションで行い、途中で失敗したら何も変えません。添付の中身は、ハッシュと大きさが合うものだけを取り込みます
- 添付や既定値がまだなかった頃の書き出しも、そのまま読めます
- 通知と完了チケット、繰り返しの AI の待ち行列、CalDAV のアカウントと同期の控え（[docs/caldav.md](caldav.md)）は、その端末だけのものなので書き出しません
