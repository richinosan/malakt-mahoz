//! Slack の API の答え（slack-cli の `api` の出力）を、取り込む中身にする。
//!
//! - リマインダー: `reminders.list`
//! - Slack リスト: `slackLists.items.list`（`include_list_metadata` で列の名前と種類も受け取る）
//!
//! 担当者は Slack のユーザー ID で返るので、名前は呼ぶ側が `users.info` で調べて渡す。

use std::collections::HashMap;

use serde_json::Value;

use super::{status_of, Batch, Item, Source};
use crate::{Error, Result};

/// 出力から JSON の所を読む。シェルの設定が何か書いていても、`{` から `}` までを読む。
pub fn json_of(output: &str) -> Result<Value> {
    let json = match (output.find('{'), output.rfind('}')) {
        (Some(start), Some(end)) if start < end => &output[start..=end],
        _ => return Err(Error::Invalid("Slack CLI の出力を読めませんでした".into())),
    };
    let value: Value = serde_json::from_str(json)
        .map_err(|_| Error::Invalid("Slack CLI の出力を読めませんでした".into()))?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        let reason = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("理由なし");
        return Err(Error::Invalid(format!("Slack が断りました: {reason}")));
    }
    Ok(value)
}

fn text(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_default()
}

/// 秒の UNIX 時刻を、この PC の日時にする。
fn local(secs: i64) -> Option<String> {
    let dt = chrono::DateTime::from_timestamp(secs, 0)?.with_timezone(&chrono::Local);
    Some(dt.format("%Y-%m-%d %H:%M").to_string())
}

/// `reminders.list` の答え。
pub fn reminders(value: &Value) -> Batch {
    let list = value
        .get("reminders")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let items = list
        .iter()
        .filter_map(|r| {
            let title = crate::text::normalize_line(&text(r, "text"));
            if title.is_empty() {
                return None;
            }
            let done = r.get("complete_ts").and_then(Value::as_i64).unwrap_or(0) > 0;
            Some(Item {
                key: text(r, "id"),
                title,
                status: if done {
                    crate::Status::Completed
                } else {
                    crate::Status::NeedsAction
                },
                due: r.get("time").and_then(Value::as_i64).and_then(local),
                ..Default::default()
            })
        })
        .collect();
    Batch {
        source: Source::SlackReminder,
        host: None,
        items,
    }
}

/// Slack リストの URL（`https://<ワークスペース>.slack.com/lists/<チーム>/<リスト>`）から、
/// リストの ID とホスト名。ID だけ（`F…`）も受け付ける。
pub fn list_target(input: &str) -> Option<(String, Option<String>)> {
    let input = input.trim();
    let is_id = |s: &str| {
        s.len() >= 9
            && s.starts_with('F')
            && s.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    };
    if is_id(input) {
        return Some((input.to_owned(), None));
    }
    let host = super::host_of(input).filter(|h| h.ends_with(".slack.com"))?;
    let path = input.split_once("://")?.1;
    let mut parts = path.split(['/', '?', '#']).skip(1);
    if parts.next()? != "lists" {
        return None;
    }
    let _team = parts.next()?;
    let id = parts.next()?;
    is_id(id).then(|| (id.to_owned(), Some(host)))
}

/// リストの列。
struct Column {
    id: String,
    kind: String,
    name: String,
    primary: bool,
}

fn columns(value: &Value) -> Vec<Column> {
    let schema = value
        .pointer("/list_metadata/schema")
        .or_else(|| value.pointer("/list/schema"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    schema
        .iter()
        .map(|c| Column {
            id: text(c, "id"),
            kind: text(c, "type"),
            name: text(c, "name").to_lowercase(),
            primary: c
                .get("is_primary_column")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
        .collect()
}

/// 1 つのセルの、文字としての値。
fn cell_text(field: &Value) -> String {
    let t = text(field, "text");
    if !t.is_empty() {
        return t;
    }
    match field.get("value") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    }
}

fn strings(field: &Value, key: &str) -> Vec<String> {
    field
        .get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// リストの項目の中の、担当者のユーザー ID（名前を調べるため）。
pub fn list_users(value: &Value) -> Vec<String> {
    let mut users = Vec::new();
    for item in value
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        for f in item
            .get("fields")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            for u in strings(f, "user") {
                if !users.contains(&u) {
                    users.push(u);
                }
            }
        }
    }
    users
}

/// `users.info` の答えから、出す名前。
pub fn user_name(value: &Value) -> Option<String> {
    let user = value.get("user")?;
    [
        "/profile/display_name",
        "/profile/real_name",
        "/real_name",
        "/name",
    ]
    .iter()
    .filter_map(|p| user.pointer(p).and_then(Value::as_str))
    .map(str::trim)
    .find(|n| !n.is_empty())
    .map(str::to_owned)
}

/// `slackLists.items.list` の答え。`names` はユーザー ID と名前、`list_url` は項目の URL の元。
pub fn list_items(value: &Value, names: &HashMap<String, String>, list_url: Option<&str>) -> Batch {
    let cols = columns(value);
    let col = |id: &str| cols.iter().find(|c| c.id == id);
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut out = Vec::new();
    for item in items {
        let fields = item
            .get("fields")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut it = Item {
            key: text(item, "id"),
            ..Default::default()
        };
        let mut first_text = String::new();
        for f in fields {
            let id = {
                let c = text(f, "column_id");
                if c.is_empty() {
                    text(f, "key")
                } else {
                    c
                }
            };
            let key = text(f, "key");
            let c = col(&id);
            let kind = c.map(|c| c.kind.as_str()).unwrap_or_default();
            let name = c.map(|c| c.name.as_str()).unwrap_or_default();
            let value_text = cell_text(f);
            let primary = c.is_some_and(|c| c.primary) || key == "name";
            if primary && it.title.is_empty() {
                it.title = crate::text::normalize_line(&value_text);
            } else if key == "todo_completed" || kind == "todo_completed" {
                if f.get("checkbox").and_then(Value::as_bool) == Some(true)
                    || f.get("value").and_then(Value::as_bool) == Some(true)
                {
                    it.status = crate::Status::Completed;
                }
            } else if kind == "user" || key == "todo_assignee" || !strings(f, "user").is_empty() {
                for u in strings(f, "user") {
                    let n = names.get(&u).cloned().unwrap_or(u);
                    if !it.assignees.contains(&n) {
                        it.assignees.push(n);
                    }
                }
            } else if kind == "date" || key == "todo_due_date" || !strings(f, "date").is_empty() {
                let d = strings(f, "date").into_iter().next().unwrap_or(value_text);
                if let Ok(Some(d)) = super::date_of(&d) {
                    if name.contains("start") || name.contains("開始") {
                        it.start = Some(d);
                    } else {
                        it.due = Some(d);
                    }
                }
            } else if kind == "select" || !strings(f, "select").is_empty() {
                let v = strings(f, "select").join(", ");
                let v = if v.is_empty() { value_text } else { v };
                if name.contains("status") || name.contains("状態") || name.contains("ステータス")
                {
                    it.status = status_of(&v);
                } else if name.contains("priority") || name.contains("優先") {
                    it.priority = super::priority_of(&v);
                } else {
                    it.categories.extend(super::split_tags(&v));
                }
            } else if kind == "checkbox" {
                if f.get("checkbox").and_then(Value::as_bool) == Some(true) {
                    it.status = crate::Status::Completed;
                }
            } else if first_text.is_empty() && !value_text.trim().is_empty() {
                first_text = value_text;
            } else if !value_text.trim().is_empty() {
                it.description.push_str(&value_text);
                it.description.push('\n');
            }
        }
        if it.title.is_empty() {
            it.title = crate::text::normalize_line(&first_text);
        } else if !first_text.is_empty() {
            it.description = format!("{first_text}\n{}", it.description);
        }
        it.description = it.description.trim().to_owned();
        if it.title.is_empty() {
            continue;
        }
        if let Some(base) = list_url {
            if !it.key.is_empty() {
                it.urls.push(format!("{base}?record_id={}", it.key));
            }
        }
        out.push(it);
    }
    Batch {
        source: Source::SlackList,
        host: None,
        items: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminders_become_tasks() {
        let v = json_of(
            r#"warn: x
            {"ok":true,"reminders":[
              {"id":"Rm1","text":"請求書を送る","time":1700000000,"complete_ts":0,"recurring":false},
              {"id":"Rm2","text":"済んだもの","time":1700000000,"complete_ts":1700000100},
              {"id":"Rm3","text":"  "}
            ]}"#,
        )
        .unwrap();
        let b = reminders(&v);
        assert_eq!(b.items.len(), 2);
        assert_eq!(b.items[0].key, "Rm1");
        assert_eq!(b.items[0].title, "請求書を送る");
        assert!(b.items[0].due.as_deref().unwrap().starts_with("2023-11-1"));
        assert_eq!(b.items[1].status, crate::Status::Completed);
        assert!(json_of(r#"{"ok":false,"error":"method_deprecated"}"#)
            .unwrap_err()
            .to_string()
            .contains("method_deprecated"));
    }

    #[test]
    fn list_urls_are_read() {
        assert_eq!(
            list_target("https://acme.slack.com/lists/T0123/F0ABCDEFGH?view=1"),
            Some(("F0ABCDEFGH".into(), Some("acme.slack.com".into())))
        );
        assert_eq!(list_target("F0ABCDEFGH"), Some(("F0ABCDEFGH".into(), None)));
        assert_eq!(list_target("https://evil.com/lists/T0/F0ABCDEFGH"), None);
        assert_eq!(list_target("F0"), None);
    }

    #[test]
    fn list_items_become_tasks() {
        let v = json_of(
            r#"{"ok":true,
              "list_metadata":{"schema":[
                {"id":"Col1","name":"Name","type":"text","is_primary_column":true},
                {"id":"Col2","name":"Assignee","type":"user"},
                {"id":"Col3","name":"Due date","type":"date"},
                {"id":"Col4","name":"Status","type":"select"},
                {"id":"Col5","name":"Notes","type":"text"}
              ]},
              "items":[
                {"id":"Rec1","fields":[
                  {"column_id":"Col1","key":"name","text":"資料を作る"},
                  {"column_id":"Col2","user":["U1","U2"]},
                  {"column_id":"Col3","date":["2025-01-31"]},
                  {"column_id":"Col4","select":["Done"]},
                  {"column_id":"Col5","text":"第 3 章まで"}
                ]},
                {"id":"Rec2","fields":[{"column_id":"Col5","text":""}]}
              ]}"#,
        )
        .unwrap();
        assert_eq!(list_users(&v), vec!["U1", "U2"]);
        let names = HashMap::from([("U1".to_owned(), "山田".to_owned())]);
        let b = list_items(
            &v,
            &names,
            Some("https://acme.slack.com/lists/T0/F0ABCDEFGH"),
        );
        assert_eq!(b.items.len(), 1);
        let it = &b.items[0];
        assert_eq!(it.title, "資料を作る");
        assert_eq!(it.assignees, vec!["山田", "U2"]);
        assert_eq!(it.due.as_deref(), Some("2025-01-31"));
        assert_eq!(it.status, crate::Status::Completed);
        assert_eq!(it.description, "第 3 章まで");
        assert_eq!(
            it.urls,
            vec!["https://acme.slack.com/lists/T0/F0ABCDEFGH?record_id=Rec1"]
        );
    }
}
