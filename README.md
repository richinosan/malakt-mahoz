# malakt-mahoz

オフラインで使うタスク管理ツールです。Rust と [gpui](https://www.gpui.rs/) で書かれていて、macOS・Windows・Linux で動きます。データはすべて手元の SQLite に保存し、通信はしません。

## できること

- **そのまま使える**: 起動してすぐ、何も設定せずにタスクを足せます。ワークスペースも担当者も、あとから作れば十分です
- **ワークスペース**: タスクをまとめる単位です。ワークスペースの中にワークスペースを入れ子にできます。どこにも入れない「未所属」のタスクも、そのまま置いておけます
- **タスク**: 状態（未着手・進行中・完了・中止）、優先度、開始日、期限、進捗、カテゴリ、サブタスク、説明、コメント（ピン留めつき）、変更履歴
- **担当者**: 自分のほか、アカウントを持たない人も名前だけで登録して割り当てられます
- **一覧**: 未完了 / 完了のタブ、検索、状態・担当・優先度・カテゴリでの絞り込み、並べ替え、まとめて選んでの一括変更
- **ホーム**: 期限が 30 日以内のタスクと、期限が過ぎたタスクを全ワークスペースから集めて表示します
- **マルチウィンドウ**: ウィンドウを何枚でも開けます（`Ctrl/⌘ + N`）。どこかで変えた内容は、すべてのウィンドウにすぐ出ます。ウィンドウの大きさは自由で、狭くするとカード表示に切り替わります
- **ロック**: パスフレーズでアプリをロックできます（`Ctrl/⌘ + L` で今すぐロック）。保存するのは Argon2id のハッシュだけです
- **エクスポート / インポート**: すべてのデータを zip 1 つに書き出し、別の端末で取り込めます。同じものは上書き、ないものは追加になります

## 使い始める

[mise](https://mise.jdx.dev/) でツールとタスクを管理しています。

```sh
mise install        # Rust を入れる
mise run dev        # 開発用に起動する
mise run start      # リリースビルドで起動する
mise run build      # リリース用にビルドする（target/release/malakt-mahoz）
```

Linux では、ビルドに次のライブラリが要ります（Ubuntu の例）。

```sh
sudo apt-get install clang pkg-config libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev \
  libwayland-dev libvulkan-dev libfontconfig-dev libfreetype-dev libssl-dev libasound2-dev libzstd-dev
```

データベースは OS のデータ用フォルダの `malakt-mahoz/data.sqlite3` に作ります。環境変数 `MALAKT_MAHOZ_DB` で場所を変えられます。

## 構成

| クレート | 役割 |
| --- | --- |
| `crates/core`（`malakt-core`） | データとロジック。SQLite への保存、zip の読み書き、ロック、絞り込みと並べ替え。画面に依存しません |
| `crates/app`（`malakt-mahoz`） | gpui の画面。状態は `state.rs`、描画は `render.rs` |

画面に依存する部分を `app` に閉じ込めているので、`core` はそのままほかの画面からも使えます。

### 速さのために

- 一覧は見えている行だけを描きます（仮想リスト）
- 絞り込みと並べ替えは読み込み済みのデータに対して行い、キーを打つたびにデータベースを読みません
- SQLite は WAL で開き、よく使う問い合わせはプリペアドステートメントをキャッシュします。担当者やカテゴリはタスクごとに問い合わせず、まとめて読みます
- `mise run bench` で、1 万件のタスクでの一覧・検索・エクスポートの時間を測れます。CI でも毎回実行して結果を残します

### エクスポートの形式

zip の中身は次のとおりです。OS が違っても同じ結果になるようにしています。

```
manifest.json          形式の名前とバージョン、件数
data/workspaces.json
data/people.json
data/tasks.json
data/comments.json
data/changes.json
```

- zip の中のファイル名は ASCII だけです。タイトルなどの文字列はファイル名に使いません（文字コードの違い、Windows の予約名、パス区切りの違いを避けるため）
- 中身は UTF-8（BOM なし）の JSON で、文字列は NFC、改行は LF にそろえます
- 読むときは、BOM・CRLF・NFD（macOS のファイル名由来の濁点が分かれた形）が混ざっていても受け入れ、正規化して取り込みます
- 取り込みは 1 つのトランザクションで行い、途中で失敗したら何も変えません

## テスト

```sh
mise run test       # ユニットテストとシナリオテスト
mise run lint       # 書式と clippy
mise run mutants    # ミューテーションテスト（cargo-mutants が必要）
```

- **ユニットテスト**: 各モジュールの中にあります
- **シナリオテスト**: `crates/core/tests/scenario.rs` は、作成からエクスポート・インポートまでの流れを通します。`crates/app/tests/ui.rs` は、ヘッドレスのウィンドウを実際に開き、クリックと入力で操作して確かめます
- **CI**: プルリクエストごとに、Linux・macOS・Windows で全部のテストを実行します
- **ミューテーションテスト**: 週 1 回、cargo-mutants を実行し、生き残ったミュータントを分析してテストを足すプルリクエストを作ります（GitHub Actions の外で、定期実行のタスクとして動かしています）
- **AI シナリオテスト**: 週 1 回、まだ確かめられていない使い方を AI が探し、シナリオテストを足します。不具合を見つけたら issue にします（同じく定期実行のタスクです）

## リリース

1. `Cargo.toml` の `[workspace.package]` の `version` を上げ（`Cargo.lock` も更新）、そのプルリクエストを作ります
2. CI が通ったらマージします
3. main に入ると「リリース」が動き、3 つの OS でビルドして GitHub Releases に公開します。同じバージョンのリリースがすでにあれば何もしません

## Android と iOS

gpui が公式に対応しているのは macOS・Windows・Linux です。そのため、まずデスクトップを完成させています。`core` は画面から独立しているので、gpui のモバイル対応が進んだ段階で画面を載せられる構成にしてあります。

## ライセンス

Apache-2.0
