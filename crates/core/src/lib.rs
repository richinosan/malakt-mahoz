//! malakt-mahoz のデータとロジック。UI には依存しない。
//!
//! - `Store`: SQLite への保存と読み出し
//! - `actor`: 誰が変えたか（自分・AI・外のエージェントなど）
//! - `alert`: 期限の通知のタイミング（何分前・何日前に知らせるか）
//! - `archive`: zip へのエクスポート / インポート
//! - `attachment`: 添付ファイル（重複なし・圧縮つきで保存）
//! - `bulk`: 複数のタスクをまとめて変える操作と、一覧での選択
//! - `diff`: 文字列の差分（変更履歴の赤と緑）
//! - `done_notice`: ほかの人が完了にしたときに知らせるか（役・担当者・ワークスペースで選ぶ）
//! - `defaults`: ワークスペースごとのタスク追加時の既定値と基準日
//! - `history`: 変更履歴を、1 回の変更ごとの差分と、コメント欄に挟む出来事にする
//! - `links`: 1 つのタスクを複数のワークスペースに表示する
//! - `notice`: 通知（期限が近い、ほかの人からの変更）
//! - `remind`: OS のリマインダーとの連携（何を作り・直し・消すかの判断）
//! - `rules`: AI のルール（グローバルとワークスペースごと）
//! - `ticket`: 完了チケット（画面で発行し、コマンドで完了報告する）
//! - `undo`: 元に戻す・やり直し（上限つきの記録）
//! - `lock`: アプリロック
//! - `markdown`: 本文の Markdown を画面に出す形にそろえる（改行をそのまま見せる）
//! - `mention`: コメントの `@名前`
//! - `view`: 一覧の絞り込みや並べ替え
//! - `when`: 期限や開始の入力（いろいろな書き方）を読む

#![cfg_attr(test, allow(clippy::cloned_ref_to_slice_refs))]

pub mod actor;
pub mod alert;
pub mod archive;
pub mod attachment;
pub mod bulk;
pub mod defaults;
pub mod diff;
pub mod done_notice;
pub mod error;
pub mod history;
pub mod icon;
pub mod links;
pub mod lock;
pub mod markdown;
pub mod mention;
pub mod model;
pub mod notice;
pub mod panes;
pub mod remind;
pub mod rules;
mod schema;
pub mod store;
pub mod tasklink;
pub mod text;
pub mod ticket;
pub mod undo;
pub mod view;
pub mod weblink;
pub mod when;
pub mod wslink;

pub use actor::Actor;
pub use error::{Error, Result};
pub use model::*;
pub use store::{Store, Usage};
