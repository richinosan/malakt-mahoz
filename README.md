# Malakt Mahoz

[![CI](https://github.com/richinosan/malakt-mahoz/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/richinosan/malakt-mahoz/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/richinosan/malakt-mahoz)](https://github.com/richinosan/malakt-mahoz/releases/latest)
[![License](https://img.shields.io/github/license/richinosan/malakt-mahoz)](LICENSE)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Linux-blue)

オフラインで使うタスク管理ツールです。Rust と [gpui](https://www.gpui.rs/) で作っていて、macOS・Windows・Linux で動きます。

<picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/list-dark.webp"><img alt="タスクの一覧" src="docs/images/list-light.webp"></picture>
<sub>タスクの一覧</sub>

<table>
<tr><td><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/home-dark.webp"><img alt="ホーム" src="docs/images/home-light.webp"></picture><br><sub>ホーム</sub></td><td><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/detail-dark.webp"><img alt="タスクの詳細" src="docs/images/detail-light.webp"></picture><br><sub>タスクの詳細</sub></td></tr>
<tr><td><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/date-dark.webp"><img alt="開始と期限の入力" src="docs/images/date-light.webp"></picture><br><sub>開始と期限の入力</sub></td><td><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/panes-dark.webp"><img alt="ペイン" src="docs/images/panes-light.webp"></picture><br><sub>ペイン</sub></td></tr>
<tr><td><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/stats-dark.webp"><img alt="統計" src="docs/images/stats-light.webp"></picture><br><sub>統計</sub></td><td><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/ai-dark.webp"><img alt="AI bot とターミナル" src="docs/images/ai-light.webp"></picture><br><sub>AI bot とターミナル</sub></td></tr>
<tr><td><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/browser-dark.webp"><img alt="アプリ内ブラウザのダウンロード" src="docs/images/browser-light.webp"></picture><br><sub>アプリ内ブラウザのダウンロード</sub></td><td><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/settings-dark.webp"><img alt="設定" src="docs/images/settings-light.webp"></picture><br><sub>設定</sub></td></tr>
<tr><td colspan="2" align="center"><picture><source media="(prefers-color-scheme: dark)" srcset="docs/images/pomodoro-dark.webp"><img alt="ポモドーロ（別ウィンドウ）" src="docs/images/pomodoro-light.webp" width="320"></picture><br><sub>ポモドーロ（別ウィンドウ）</sub></td></tr>
</table>

## 機能

- **ワークスペース**: 入れ子にできます。タスクはどこにも入れずに置くこともできます
- **タスク**: 状態、優先度、URL（複数。タイトルの下に並び、押すとアプリ内ブラウザで開く）、開始と期限（「明日」「11/27 14時」のように入力。一覧では「11/24～11/27」か「あと 3 日」で表示）、進捗、カテゴリ、サブタスク、説明（Markdown）、変更履歴（誰がいつ作り、何をどう変えたか。自分・AI・外のエージェントを分けて残す）
- **予想工数と所要時間**: 予想工数は「1日3時間」のように月・日・時間・分を組み合わせて決めます。所要時間はどこから数えてどこで終わりにするか（作った時・担当が付いた時・進行中にした時・開始日 → 完了・自分の分の完了）を全体・ワークスペース・タスクで選べます。統計はタスク・ワークスペース・全体で（[docs/timing.md](docs/timing.md)）
- **担当者**: 名前だけで登録でき、役（RACI）を付けられます。複数人のときは 1 人ずつ完了にできます
- **コメント**: Markdown、ピン留め、`@名前`、完了報告、説明の文章に付けるレビューコメント
- **添付ファイル**: 1 ファイル 10 MiB、1 タスク 500 件まで
- **タスク追加時の既定値**: ワークスペースごとに担当者・期限・優先度・カテゴリ・説明の雛形
- **一覧**: 検索、絞り込み、並べ替え、複数選択での一括変更
- **ホーム**: 期限が 30 日以内のタスクと期限が過ぎたタスク
- **繰り返し**: 毎日・平日・毎週◯曜・毎月◯日・毎年と、何日・何週間ごと。時刻が来ると次の回のタスクを作ります。回を作ったときに AI に頼むこと（報告の下書きなど）も書けます（[docs/repeat.md](docs/repeat.md)）
- **定時タスク**: ワークスペースごとに、決まった時刻が来るたびに雛形（タスクと同じ項目と AI への指示）から新しいタスクを作ります（[docs/schedule.md](docs/schedule.md)）
- **通知**: 期限が近いタスク（何分前・何日前に知らせるかを選べます。[docs/notices.md](docs/notices.md)）、ほかの人からの完了報告、タイマーとポモドーロの終わり
- **ペイン**: 画面を分けて、配置をプリセットとして保存
- **カスタマイズ**: テーマ 13 個、レイアウト、密度、背景の画像、書体の優先順（画面と等幅）、色。自分のテーマを保存
- **マルチウィンドウ**: `Ctrl/⌘ + N`。どのウィンドウも、上のバーの右クリックの「常に最前面に表示」で最前面に出せます。出している間はバーの右にピンが出て、押すと元に戻ります。ウィンドウごとに覚えて、次に開いたときも最前面に出ます（Linux は X11 のセッションだけ）
- **ポモドーロ**（別ウィンドウ）: タスクを選んで作業と休憩を測り、作業時間をタスクに残す。作業や休憩の始まりに OS で再生中の音楽を再生・一時停止できる（[docs/pomodoro.md](docs/pomodoro.md)）
- **タイマー**（別ウィンドウ）: カウントダウンとストップウォッチ。カウントダウンが 0 になると通知が出る（[docs/timer.md](docs/timer.md)）
- **ロック**: パスキー（Windows Hello、Touch ID、fprintd）。`Ctrl + Shift + L`（macOS は `⇧⌘L`）か、上のバーの右クリック。操作がないときに自動でロックする時間を選べます。ロック中も OS の通知は届きます（[docs/lock.md](docs/lock.md)）
- **エクスポート / インポート**: 全データを zip 1 つに書き出し、別の端末で取り込み
- **リマインダーとの連携**（標準はオフ）: macOS の「リマインダー」との同期、Windows の通知の予約（[docs/reminders.md](docs/reminders.md)）
- **スタートアップ**（標準はオフ）: PC にログインしたときに起動（[docs/startup.md](docs/startup.md)）
- **CalDAV との同期**（標準はオフ）: Fastmail・Nextcloud などのサーバーのタスクを、ワークスペースと両方向に同期（[docs/caldav.md](docs/caldav.md)）
- **自動アップデート**: GitHub Releases の新しい版に入れ替え（[docs/update.md](docs/update.md)）
- **完了チケット**: コマンドからタスクを完了にする（[docs/tickets.md](docs/tickets.md)）
- **AI bot**: `Ctrl/⌘ + J`（[docs/ai.md](docs/ai.md)）
- **ターミナル**: アプリの中で WSL や zsh を開き、Claude Code などをそのまま動かす（[docs/terminal.md](docs/terminal.md)）
- **MCP サーバー**: Claude Code などのエージェントからタスクを読み書き。アクセスキーで、エージェントごとに読み取りのみ・ロック中も使えるかを選べる。キーなしで使える条件（時間帯・ロック中）も決められ、HTTP のポートでも開ける（[docs/mcp.md](docs/mcp.md)）
- **プラグイン**: WASM のプラグインで、タスクやワークスペースの右クリックにコマンドを足す。有効にするのは、許すことを確かめて承認したものだけ（[docs/plugins.md](docs/plugins.md)）

## 起動とビルド

[mise](https://mise.jdx.dev/) を使います。

```sh
mise install        # Rust を入れる
mise run dev        # 開発用に起動する
mise run start      # リリースビルドで起動する
mise run build      # target/release/malakt-mahoz を作る
```

Linux では次のライブラリが必要です（Ubuntu の例）。

```sh
sudo apt-get install clang pkg-config libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev \
  libwayland-dev libvulkan-dev libfontconfig-dev libfreetype-dev libssl-dev libasound2-dev libzstd-dev \
  libwebkit2gtk-4.1-dev libgtk-3-dev
```

データベースは OS のデータ用フォルダの `malakt-mahoz/data.sqlite3` です。環境変数 `MALAKT_MAHOZ_DB` で場所を変えられます。

## テスト

```sh
mise run test       # ユニットテストとシナリオテスト
mise run lint       # 書式と clippy
mise run verify     # 書式、clippy、テスト
mise run hooks      # コミットの前に verify を走らせるフックを入れる
mise run bench      # 1 万件のタスクでの一覧・検索・エクスポートの時間
mise run mutants    # ミューテーションテスト（cargo-mutants が必要）
mise run api        # API の定義（api/*.tsp）からツールの一覧を作り直す
```

## ドキュメント

- [繰り返しのタスク](docs/repeat.md)
- [定時タスク](docs/schedule.md)
- [完了チケット](docs/tickets.md)
- [AI bot](docs/ai.md)
- [ターミナル](docs/terminal.md)
- [Claude Code などから使う（MCP）](docs/mcp.md)
- [API の定義（TypeSpec）](docs/api.md)
- [リマインダーとの連携](docs/reminders.md)
- [スタートアップ](docs/startup.md)
- [CalDAV との同期](docs/caldav.md)
- [ポモドーロ](docs/pomodoro.md)
- [タイマー](docs/timer.md)
- [自動アップデート](docs/update.md)
- [エクスポートの形式](docs/export.md)
- [設計（構成、速さ、ディスクの使い方、モバイル）](docs/design.md)
- [テストとリリース](docs/testing.md)

## ライセンス

Apache-2.0
