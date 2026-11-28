//! API の定義。ツールの名前・説明・引数と、書き込みか・確認が要るかなどの印は、
//! `api/*.tsp`（TypeSpec）に 1 か所で書き、`mise run api` で作った `api/generated/tools.json` を
//! 実行ファイルに埋め込んで読む。アプリの中の AI、MCP のサーバー、コマンドは、どれもここから読む。

use std::sync::LazyLock;

use serde_json::Value;

use crate::wire::ToolDef;

/// ツールの組。どこに出すかが組で決まる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// データベースの操作。アプリの中の AI、MCP、コマンドのすべてに出す
    Data,
    /// 画面と設定の操作。アプリの中の AI にだけ出す
    App,
    /// MCP のサーバーだけが持つ操作
    Mcp,
}

/// ツール 1 つの定義。
#[derive(Debug)]
pub struct Tool {
    pub name: &'static str,
    pub group: Group,
    pub description: &'static str,
    /// 引数の JSON Schema
    pub schema: Value,
    /// データベースを書き換える。1 回の呼び出しが、元に戻す 1 つの段になる
    pub writes: bool,
    /// アプリの中の AI が呼ぶときに、実行の前にユーザーの確認を出す
    pub confirm: bool,
    /// アプリの中の AI にだけ渡す（MCP とコマンドには出さない）
    pub in_app_only: bool,
    /// 画面を動かす（メインのウィンドウで実行する）
    pub view: bool,
    /// AI の設定を変える（チャットで実行する）
    pub session: bool,
}

/// ツールの一覧（`malakt-mahoz api` が出す JSON）。
pub const SOURCE: &str = include_str!("../../../api/generated/tools.json");

/// API の定義のファイル（`malakt-mahoz api tsp` が書き出す）。`npm install` のあと
/// `npx tsp compile main.tsp` で読める。
pub const FILES: &[(&str, &str)] = &[
    ("main.tsp", include_str!("../../../api/main.tsp")),
    ("lib.tsp", include_str!("../../../api/lib.tsp")),
    ("lib.js", include_str!("../../../api/lib.js")),
    ("data.tsp", include_str!("../../../api/data.tsp")),
    ("app.tsp", include_str!("../../../api/app.tsp")),
    ("mcp.tsp", include_str!("../../../api/mcp.tsp")),
    ("package.json", include_str!("../../../api/package.json")),
];

static TOOLS: LazyLock<Vec<Tool>> =
    // 埋め込んだファイルは build.mjs が作り、テスト（`the_definitions_read`）で読めることを確かめている
    LazyLock::new(|| parse(SOURCE).expect("api/generated/tools.json を読めません"));

fn leak(text: &str) -> &'static str {
    // 起動中に 1 回だけ読み、終わるまで使い続ける
    Box::leak(text.to_owned().into_boxed_str())
}

fn parse(text: &str) -> Result<Vec<Tool>, String> {
    let root: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let list = root
        .get("tools")
        .and_then(Value::as_array)
        .ok_or("tools がありません")?;
    list.iter()
        .map(|entry| {
            let text = |key: &str| {
                entry
                    .get(key)
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("{key} がありません: {entry}"))
            };
            let mark = |key: &str| entry.get(key).and_then(Value::as_bool) == Some(true);
            let group = match text("group")? {
                "data" => Group::Data,
                "app" => Group::App,
                "mcp" => Group::Mcp,
                other => return Err(format!("組 {other} はありません")),
            };
            Ok(Tool {
                name: leak(text("name")?),
                group,
                description: leak(text("description")?),
                schema: entry
                    .get("input_schema")
                    .cloned()
                    .ok_or_else(|| format!("input_schema がありません: {entry}"))?,
                writes: mark("writes"),
                confirm: mark("confirm"),
                in_app_only: mark("in_app_only"),
                view: mark("view"),
                session: mark("session"),
            })
        })
        .collect()
}

/// すべてのツール。定義に書いた順。
pub fn tools() -> &'static [Tool] {
    &TOOLS
}

/// 組の中のツール 1 つ。
pub fn tool(group: Group, name: &str) -> Option<&'static Tool> {
    tools().iter().find(|t| t.group == group && t.name == name)
}

/// 組のツールを、AI に渡す形で。
pub fn definitions(group: Group) -> Vec<ToolDef> {
    tools()
        .iter()
        .filter(|t| t.group == group)
        .map(|t| ToolDef {
            name: t.name,
            description: t.description,
            schema: t.schema.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_definitions_read() {
        let tools = parse(SOURCE).unwrap();
        assert!(tools.iter().any(|t| t.group == Group::Data));
        assert!(tools.iter().any(|t| t.group == Group::App));
        assert!(tools.iter().any(|t| t.group == Group::Mcp));
        for t in &tools {
            assert!(!t.description.is_empty(), "{}", t.name);
            assert_eq!(t.schema["type"], "object", "{}", t.name);
            assert_eq!(t.schema["additionalProperties"], false, "{}", t.name);
        }
    }

    #[test]
    fn broken_definitions_are_reported() {
        assert!(parse("{").is_err());
        assert!(parse(r#"{"tools": [{"name": "x"}]}"#).is_err());
        let wrong = r#"{"tools": [{"name": "x", "group": "other", "description": "d", "input_schema": {}}]}"#;
        assert!(parse(wrong).unwrap_err().contains("other"));
    }
}
