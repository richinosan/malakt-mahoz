//! AI チャットの中身。画面には依存しない。
//!
//! API キーを設定して送信するまで、通信はしない。
//!
//! - `provider`: 対応しているサービスと、送り先の決め方
//! - `wire`: 各 API の形式への変換（リクエストの組み立てと、返事の読み取り）
//! - `api`: ツールの定義（`api/*.tsp` から作った一覧）
//! - `files`: チャットに添えるファイルの見分け方、AI への渡し方、置き場所
//! - `tools`: AI がタスクを読み書きするためのツール
//! - `github`: GitHub のリンクの検出と、取得のためのコマンド
//! - `watch`: GitHub の定期チェックで、issue とプルリクエストの進展を読む
//! - `slack`: Slack のリンクの検出と、取得のためのコマンド
//! - `shell`: コマンドの実行と強制終了
//! - `keys`: API キーの保存（OS のキーチェーン）
//! - `http`: HTTPS の呼び出し

pub mod ansi;
pub mod api;
pub mod files;
pub mod github;
pub mod http;
pub mod keys;
pub mod provider;
pub mod shell;
pub mod slack;
pub mod tools;
pub mod watch;
pub mod wire;

pub use provider::{Effort, Provider};
pub use wire::{Reply, ToolCall, Turn, Wire};

/// AI に渡す文字列の上限。長い出力は末尾を残して切る。
pub const MAX_TOOL_OUTPUT_CHARS: usize = 20_000;

/// 文字の途中で切らずに、末尾の `max` 文字を残す。
pub fn clip_tail(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let tail: String = text.chars().skip(count - max).collect();
    format!("（長いので先頭の {} 文字を省略）\n{tail}", count - max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_keeps_the_tail_on_char_boundaries() {
        assert_eq!(clip_tail("あいう", 3), "あいう");
        let clipped = clip_tail("あいうえお", 2);
        assert!(clipped.ends_with("えお"));
        assert!(clipped.contains("3 文字を省略"));
    }
}
