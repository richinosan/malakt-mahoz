//! プラグイン。画面には依存しない。
//!
//! プラグインは、フォルダーに `plugin.toml`（マニフェスト）と WASM を置いたもの。
//! アプリのメニューにコマンドを足し、コマンドが呼ばれたら WASM の `command` を動かす。
//! WASM からアプリを操作するのは、MCP と同じツール（api/data.tsp）を呼ぶ `call_tool` だけ。
//!
//! - `manifest`: `plugin.toml` の形と確かめ方
//! - `discover`: フォルダーを合わせて読み、ID の重なりと読めないものを出す
//! - `run`: WASM の実行（Extism。ファイル・ネットワークなし、時間とメモリの上限つき）

pub mod discover;
pub mod manifest;
pub mod run;

pub use discover::{
    default_dir, discover, folders, Discovery, Folder, Found, Problem, Source, ENV_DIRS,
};
pub use manifest::{Command, Manifest, Permission, Place};
pub use run::{run_command, CallTool, Outcome, Request};
