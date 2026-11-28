//! プラグインのマニフェスト（`plugin.toml`）。
//!
//! ```toml
//! id = "task-markdown"
//! name = "Markdown でコピー"
//! version = "1.0.0"
//! api = 1
//! permissions = ["read"]
//!
//! [[commands]]
//! id = "copy"
//! title = "Markdown でコピー"
//! on = "task"
//! ```

use serde::Deserialize;

/// このアプリが読める API の版。マニフェストの `api` がこれと同じものだけを読む。
pub const API_VERSION: u32 = 1;

/// マニフェストのファイル名。
pub const FILE: &str = "plugin.toml";

/// `wasm` を省いたときのファイル名。
pub const DEFAULT_WASM: &str = "plugin.wasm";

/// プラグインに許すこと。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    /// `list_` / `get_` で始まるツール（読むだけ）
    Read,
    /// データベースを書き換えるツール
    Write,
}

impl Permission {
    pub fn code(self) -> &'static str {
        match self {
            Permission::Read => "read",
            Permission::Write => "write",
        }
    }

    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "read" => Some(Permission::Read),
            "write" => Some(Permission::Write),
            _ => None,
        }
    }

    /// 承認のモーダルに出す文。
    pub fn label(self) -> &'static str {
        match self {
            Permission::Read => "タスク・ワークスペース・担当者・コメントを読む",
            Permission::Write => {
                "タスク・ワークスペース・担当者・コメントを作る・書き換える・削除する"
            }
        }
    }
}

/// コマンドを出す場所。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Place {
    /// タスクの右クリックのメニュー。呼ぶときに `task_id` を渡す
    Task,
    /// ワークスペースの右クリックのメニュー。呼ぶときに `workspace_id` を渡す
    Workspace,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub id: String,
    /// メニューに出す名前
    pub title: String,
    pub on: Place,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// プラグインの ID。英小文字・数字・`-`。承認はこの ID で覚える
    pub id: String,
    pub name: String,
    pub version: String,
    /// 使う API の版
    pub api: u32,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub permissions: Vec<Permission>,
    /// WASM のファイル（マニフェストのフォルダーからの相対）
    #[serde(default = "default_wasm")]
    pub wasm: String,
    #[serde(default)]
    pub commands: Vec<Command>,
}

fn default_wasm() -> String {
    DEFAULT_WASM.into()
}

const MAX_ID_CHARS: usize = 64;
const MAX_NAME_CHARS: usize = 60;

fn is_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_ID_CHARS
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !text.starts_with('-')
}

impl Manifest {
    /// `plugin.toml` の中身を読む。正しくないところは、全部を並べて返す。
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let mut manifest: Manifest = toml::from_str(text).map_err(|e| e.message().to_owned())?;
        manifest.permissions.sort_unstable();
        manifest.permissions.dedup();
        let mut problems = Vec::new();
        if !is_id(&manifest.id) {
            problems.push(format!(
                "id「{}」は英小文字・数字・- の {MAX_ID_CHARS} 文字までにしてください",
                manifest.id
            ));
        }
        let name = manifest.name.trim();
        if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
            problems.push(format!("name は 1〜{MAX_NAME_CHARS} 文字にしてください"));
        }
        if manifest.api != API_VERSION {
            problems.push(format!(
                "api {} には対応していません（このアプリは {API_VERSION}）",
                manifest.api
            ));
        }
        let wasm = std::path::Path::new(&manifest.wasm);
        if wasm.is_absolute()
            || wasm
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            problems.push(format!(
                "wasm「{}」は、plugin.toml のフォルダーの中のファイルにしてください",
                manifest.wasm
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for command in &manifest.commands {
            if !is_id(&command.id) {
                problems.push(format!(
                    "コマンドの id「{}」は英小文字・数字・- にしてください",
                    command.id
                ));
            } else if !seen.insert(command.id.as_str()) {
                problems.push(format!("コマンドの id「{}」が 2 つあります", command.id));
            }
            let title = command.title.trim();
            if title.is_empty() || title.chars().count() > MAX_NAME_CHARS {
                problems.push(format!(
                    "コマンド「{}」の title は 1〜{MAX_NAME_CHARS} 文字にしてください",
                    command.id
                ));
            }
        }
        if problems.is_empty() {
            Ok(manifest)
        } else {
            Err(problems.join("。"))
        }
    }

    pub fn allows(&self, permission: Permission) -> bool {
        self.permissions.contains(&permission)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
id = "task-markdown"
name = "Markdown でコピー"
version = "1.0.0"
api = 1
permissions = ["read", "read"]

[[commands]]
id = "copy"
title = "Markdown でコピー"
on = "task"
"#;

    #[test]
    fn a_manifest_reads() {
        let m = Manifest::parse(GOOD).unwrap();
        assert_eq!(m.id, "task-markdown");
        assert_eq!(m.permissions, [Permission::Read]);
        assert_eq!(m.wasm, DEFAULT_WASM);
        assert_eq!(m.commands[0].on, Place::Task);
        assert!(m.allows(Permission::Read));
        assert!(!m.allows(Permission::Write));
    }

    #[test]
    fn every_problem_is_listed() {
        let bad = r#"
id = "Task Markdown"
name = "x"
version = "1"
api = 2
wasm = "../x.wasm"

[[commands]]
id = ""
title = "x"
on = "task"
"#;
        let error = Manifest::parse(bad).unwrap_err();
        for part in ["Task Markdown", "api 2", "../x.wasm", "コマンドの id「」"] {
            assert!(error.contains(part), "{part}: {error}");
        }
    }

    #[test]
    fn unknown_fields_and_places_are_errors() {
        assert!(Manifest::parse(&format!("{GOOD}\n[extra]\nx = 1\n")).is_err());
        assert!(Manifest::parse(&GOOD.replace("on = \"task\"", "on = \"menu\"")).is_err());
        assert!(Manifest::parse(&GOOD.replace("\"read\", \"read\"", "\"net\"")).is_err());
    }
}
