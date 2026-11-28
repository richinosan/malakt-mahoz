//! 見本のプラグイン: タスク（とサブタスク）や、ワークスペースの未完了のタスクを、
//! Markdown のチェックリストにしてクリップボードに入れる。

use extism_pdk::*;
use serde::Deserialize;
use serde_json::{json, Value};

#[host_fn]
extern "ExtismHost" {
    fn call_tool(request: String) -> String;
}

#[derive(Deserialize)]
struct Request {
    command: String,
    task_id: Option<String>,
    workspace_id: Option<String>,
}

/// アプリのツールを呼ぶ。返事の `ok` を返し、`error` ならそのまま失敗にする。
fn tool(name: &str, args: Value) -> Result<Value, Error> {
    let request = json!({"name": name, "args": args}).to_string();
    let reply: Value = serde_json::from_str(&unsafe { call_tool(request)? })?;
    match reply.get("error").and_then(Value::as_str) {
        Some(error) => Err(Error::msg(error.to_owned())),
        None => Ok(reply["ok"].clone()),
    }
}

fn done(task: &Value) -> bool {
    matches!(task["status"].as_str(), Some("completed" | "cancelled"))
}

fn line(task: &Value, indent: &str) -> String {
    let mark = if done(task) { "x" } else { " " };
    let title = task["title"].as_str().unwrap_or_default();
    match task["due"].as_str().filter(|d| !d.is_empty()) {
        Some(due) => format!("{indent}- [{mark}] {title}（期限 {due}）"),
        None => format!("{indent}- [{mark}] {title}"),
    }
}

#[plugin_fn]
pub fn command(input: String) -> FnResult<String> {
    let request: Request = serde_json::from_str(&input)?;
    let (text, count) = match request.command.as_str() {
        "copy" => {
            let id = request.task_id.ok_or_else(|| Error::msg("タスクがありません"))?;
            let task = tool("get_task", json!({"id": id}))?;
            let mut lines = vec![line(&task, "")];
            for sub in task["subtasks"].as_array().into_iter().flatten() {
                lines.push(line(sub, "  "));
            }
            let count = lines.len();
            (lines.join("\n"), count)
        }
        "copy-workspace" => {
            let id = request
                .workspace_id
                .ok_or_else(|| Error::msg("ワークスペースがありません"))?;
            let found = tool("list_tasks", json!({"workspace_id": id}))?;
            let lines: Vec<String> = found["tasks"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|t| line(t, ""))
                .collect();
            let count = lines.len();
            (lines.join("\n"), count)
        }
        other => return Err(Error::msg(format!("知らないコマンドです: {other}")).into()),
    };
    Ok(json!({"copy": text, "notice": format!("{count} 行をコピーしました")}).to_string())
}
