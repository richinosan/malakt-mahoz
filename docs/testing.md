# テストとリリース

## テスト

```sh
mise run test       # ユニットテストとシナリオテスト
mise run test-buttons  # 画面のボタンを全部押すテスト（重いので手で回す）
mise run lint       # 書式と clippy
mise run verify     # 書式、clippy、テストをまとめて（コミットの前の確認）
mise run hooks      # コミットの前に verify を走らせるフックを入れる
mise run mutants    # ミューテーションテスト（cargo-mutants が必要）
```

- **ユニットテスト**: 各モジュールの中にあります
- **シナリオテスト**: `crates/core/tests/scenario.rs` は、作成からエクスポート・インポートまでの流れを通します。`crates/app/tests/ui.rs` は、ヘッドレスのウィンドウを実際に開き、クリックと入力で操作して確かめます。`crates/app/tests/ai.rs` は、AI の返事だけを差し替えて、チャット・確認・シェル・別ウィンドウを同じように操作して確かめます。`crates/app/tests/term.rs` と `crates/term/tests/terminal.rs` は、本物のシェルを起こして、ターミナルをキーとマウスで操作して確かめます
- **押しても何も起きないボタン**: ボタンは `crate::press::button` で作ります。押したときの処理（`on_click` か `pop_menu`）を付けるまで画面に置けない型なので、処理のないボタンはビルドが通りません。`Button::new` を直接呼ぶと clippy が止めます（`clippy.toml`）。画面のボタンを実際に全部押す `crates/app/tests/buttons.rs` は重いので、`mise run test` と CI からは外していて、`mise run test-buttons` で手で回します
- **コミットの前の確認**: `mise run hooks` を 1 回実行すると、コミットのたびに書式・clippy・全部のテスト（`mise run verify`）が走り、通らなければコミットされません
- **CI**: main に入ったものを、Linux・macOS・Windows で全部のテストにかけます。プルリクエストでは動きません。ジョブには、ふだんかかる時間の 1.5〜2 倍のタイムアウトがあります
- **ミューテーションテスト**: 週 1 回、cargo-mutants を実行し、生き残ったミュータントを分析してテストを足すプルリクエストを作ります（GitHub Actions の外で、定期実行のタスクとして動かしています）
- **AI シナリオテスト**: 週 1 回、まだ確かめられていない使い方を AI が探し、シナリオテストを足します。不具合を見つけたら issue にします（同じく定期実行のタスクです）

## リリース

1. `Cargo.toml` の `[workspace.package]` の `version` を上げ（`Cargo.lock` も更新）、そのプルリクエストを作ります
2. コミットの前の確認（`mise run verify`）が通っていれば、そのままマージします
3. main に入ると「リリース」が動き、3 つの OS でビルドして GitHub Releases に公開します。同じバージョンのリリースがすでにあれば何もしません
4. 公開されると、入れてあるアプリが自動で新しい版になります。自動アップデートは、リリースのファイルの名前（`malakt-mahoz-v<版>-<OS>.tar.gz` / `.zip`）と `SHA256SUMS` を頼りにしているので、`scripts/package.sh` とリリースのワークフローで名前を変えるときは `crates/update` も合わせます
