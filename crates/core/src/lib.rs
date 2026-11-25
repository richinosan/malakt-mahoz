//! malakt-mahoz のデータとロジック。UI には依存しない。
//!
//! - `Store`: SQLite への保存と読み出し
//! - `archive`: zip へのエクスポート / インポート
//! - `attachment`: 添付ファイル（重複なし・圧縮つきで保存）
//! - `bulk`: 複数のタスクをまとめて変える操作と、一覧での選択
//! - `defaults`: ワークスペースごとのタスク追加時の既定値と基準日
//! - `links`: 1 つのタスクを複数のワークスペースに表示する
//! - `notice`: 通知（期限が今日、ほかの人からの変更）
//! - `remind`: OS のリマインダーとの連携（何を作り・直し・消すかの判断）
//! - `ticket`: 完了チケット（画面で発行し、コマンドで完了報告する）
//! - `lock`: アプリロック
//! - `mention`: コメントの `@名前`
//! - `view`: 一覧の絞り込みや並べ替え

#![cfg_attr(test, allow(clippy::cloned_ref_to_slice_refs))]

pub mod archive;
pub mod attachment;
pub mod bulk;
pub mod defaults;
pub mod error;
pub mod links;
pub mod lock;
pub mod mention;
pub mod model;
pub mod notice;
pub mod panes;
pub mod remind;
mod schema;
pub mod store;
pub mod tasklink;
pub mod text;
pub mod ticket;
pub mod view;

pub use error::{Error, Result};
pub use model::*;
pub use store::{Store, Usage};
