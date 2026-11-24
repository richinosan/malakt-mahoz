//! malakt-mahoz のデータとロジック。UI には依存しない。
//!
//! - `Store`: SQLite への保存と読み出し
//! - `archive`: zip へのエクスポート / インポート
//! - `lock`: アプリロック
//! - `mention`: コメントの `@名前`
//! - `view`: 一覧の絞り込みや並べ替え

#![cfg_attr(test, allow(clippy::cloned_ref_to_slice_refs))]

pub mod archive;
pub mod error;
pub mod lock;
pub mod mention;
pub mod model;
mod schema;
pub mod store;
pub mod text;
pub mod view;

pub use error::{Error, Result};
pub use model::*;
pub use store::{Store, Usage};
