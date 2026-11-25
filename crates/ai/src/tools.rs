//! AI がタスクを読み書きするためのツール。

use malakt_core::view::Priority;
use malakt_core::{Scope, Status, Store, Task, TaskDraft, TaskFilter};
use serde_json::{json, Value};

use crate::wire::ToolDef;

/// GitHub のリンクの中身を取りに行くツール。実行は画面の側（シェルに出す）。
pub const GITHUB_FETCH: &str = "github_fetch";
/// Slack のリンクの中身を取りに行くツール。実行は画面の側（シェルに出す）。
pub const SLACK_FETCH: &str = "slack_fetch";
/// シェルでコマンドを実行するツール。実行は画面の側（毎回、確認を出す）。
pub const RUN_SHELL: &str = "run_shell";

const LIST_LIMIT: usize = 200;

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn task_fields() -> Value {
    json!({
        "title": {"type": "string"},
        "description": {"type": "string"},
        "status": {"type": "string", "enum": ["needs_action", "in_process", "completed", "cancelled"],
                   "description": "未着手 / 進行中 / 完了 / 中止"},
        "priority": {"type": "string", "enum": ["none", "high", "mid", "low"]},
        "start": {"type": ["string", "null"], "description": "開始日 YYYY-MM-DD。時刻つきなら YYYY-MM-DD HH:MM。null でなくす"},
        "due": {"type": ["string", "null"], "description": "期限 YYYY-MM-DD。時刻つきなら YYYY-MM-DD HH:MM。null でなくす"},
        "percent": {"type": "integer", "minimum": 0, "maximum": 100, "description": "進捗"},
        "parent_id": {"type": ["string", "null"], "description": "親のタスクの ID（サブタスクにする）"},
        "assignee_ids": {"type": "array", "items": {"type": "string"}, "description": "担当者の ID"},
        "categories": {"type": "array", "items": {"type": "string"}},
    })
}

fn with(mut base: Value, extra: Value) -> Value {
    if let (Some(base), Value::Object(extra)) = (base.as_object_mut(), extra) {
        base.extend(extra);
    }
    base
}

/// AI に見せるツールの一覧。
pub fn definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "list_workspaces",
            description: "ワークスペース（タスクをまとめる単位。入れ子にできる）の一覧を返す。",
            schema: object(json!({}), &[]),
        },
        ToolDef {
            name: "list_people",
            description: "担当者の一覧を返す。",
            schema: object(json!({}), &[]),
        },
        ToolDef {
            name: "list_tasks",
            description: "タスクの一覧を返す。workspace_id を省くと全部、\"unfiled\" でどのワークスペースにも入っていないもの。",
            schema: object(
                json!({
                    "workspace_id": {"type": "string"},
                    "query": {"type": "string", "description": "タイトルと説明の部分一致"},
                    "include_done": {"type": "boolean", "description": "完了・中止も含める（標準は含めない）"},
                }),
                &[],
            ),
        },
        ToolDef {
            name: "get_task",
            description: "タスク 1 件の内容と、コメント、サブタスクを返す。",
            schema: object(json!({"id": {"type": "string"}}), &["id"]),
        },
        ToolDef {
            name: "create_task",
            description: "タスクを作る。workspace_id を省くと、どのワークスペースにも入れない。",
            schema: object(
                with(task_fields(), json!({"workspace_id": {"type": "string"}})),
                &["title"],
            ),
        },
        ToolDef {
            name: "update_task",
            description: "タスクを書き換える。渡した項目だけが変わる。",
            schema: object(with(task_fields(), json!({"id": {"type": "string"}})), &["id"]),
        },
        ToolDef {
            name: "move_task",
            description: "タスクを別のワークスペースへ移す。workspace_id が null なら、どこにも入れない。",
            schema: object(
                json!({"id": {"type": "string"}, "workspace_id": {"type": ["string", "null"]}}),
                &["id", "workspace_id"],
            ),
        },
        ToolDef {
            name: "delete_task",
            description: "タスクを削除する。元に戻せないので、実行の前にユーザーに確認が出る。",
            schema: object(json!({"id": {"type": "string"}}), &["id"]),
        },
        ToolDef {
            name: "add_comment",
            description: "タスクにコメントを書く。",
            schema: object(
                json!({"task_id": {"type": "string"}, "body": {"type": "string"}}),
                &["task_id", "body"],
            ),
        },
        ToolDef {
            name: "create_workspace",
            description: "ワークスペースを作る。parent_id を渡すと、その中に作る。",
            schema: object(
                json!({"name": {"type": "string"}, "parent_id": {"type": "string"}}),
                &["name"],
            ),
        },
        ToolDef {
            name: "create_person",
            description: "担当者を登録する。",
            schema: object(json!({"name": {"type": "string"}}), &["name"]),
        },
        ToolDef {
            name: GITHUB_FETCH,
            description: "GitHub のリンク（issue、プルリクエスト、リポジトリ、ファイル、コミット）の中身を gh コマンドで取ってくる。",
            schema: object(json!({"url": {"type": "string"}}), &["url"]),
        },
        ToolDef {
            name: SLACK_FETCH,
            description: "Slack のメッセージへのリンク（https://〜.slack.com/archives/…）のメッセージと、そのスレッドの返信を Slack CLI で取ってくる。",
            schema: object(json!({"url": {"type": "string"}}), &["url"]),
        },
        ToolDef {
            name: RUN_SHELL,
            description: "ユーザーの PC のシェル（Windows は cmd.exe、ほかは sh）でコマンドを 1 つ実行し、出力を返す。実行の前にユーザーに確認が出る。",
            schema: object(json!({"command": {"type": "string"}}), &["command"]),
        },
    ]
}

/// 実行の前にユーザーの確認が要るツールか。
pub fn needs_confirmation(name: &str) -> bool {
    matches!(name, "delete_task" | RUN_SHELL)
}

/// 確認に出す文。
pub fn confirmation_text(store: &Store, name: &str, args: &Value) -> String {
    match name {
        "delete_task" => {
            let id = args.get("id").and_then(Value::as_str).unwrap_or_default();
            match store.task(id) {
                Ok(task) => format!("タスク「{}」を削除します。元に戻せません。", task.title),
                Err(_) => format!("タスク（ID: {id}）を削除します。元に戻せません。"),
            }
        }
        RUN_SHELL => {
            let command = args
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default();
            format!("次のコマンドを実行します。\n{command}")
        }
        other => format!("{other} を実行します。"),
    }
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} が必要です"))
}

fn optional<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// 渡された項目だけを下書きに書き込む。
fn apply_fields(draft: &mut TaskDraft, args: &Value) -> Result<(), String> {
    if let Some(v) = optional(args, "title") {
        draft.title = v.to_owned();
    }
    if let Some(v) = optional(args, "description") {
        draft.description = v.to_owned();
    }
    if let Some(v) = optional(args, "status") {
        draft.status = Status::parse(v).ok_or_else(|| format!("status が正しくありません: {v}"))?;
    }
    if let Some(v) = optional(args, "priority") {
        draft.priority = match v {
            "none" => Priority::None,
            "high" => Priority::High,
            "mid" => Priority::Mid,
            "low" => Priority::Low,
            other => return Err(format!("priority が正しくありません: {other}")),
        }
        .value();
    }
    // 日付と親は、null か空の文字列で「なし」にできる
    // 日付は「2026-12-01T18:00」や「明日 14時」のような書き方も読む
    let now = malakt_core::view::now();
    for (key, slot) in [
        ("start", &mut draft.start),
        ("due", &mut draft.due),
        ("parent_id", &mut draft.parent_id),
    ] {
        match args.get(key) {
            Some(Value::Null) => *slot = None,
            Some(Value::String(s)) if s.is_empty() => *slot = None,
            Some(Value::String(s)) if key != "parent_id" => {
                *slot = Some(malakt_core::when::resolve(s, &now, slot.as_deref()))
            }
            Some(Value::String(s)) => *slot = Some(s.clone()),
            Some(other) => return Err(format!("{key} が正しくありません: {other}")),
            None => {}
        }
    }
    if let Some(v) = args.get("percent") {
        let n = v
            .as_u64()
            .filter(|n| *n <= 100)
            .ok_or_else(|| format!("percent が正しくありません: {v}"))?;
        draft.percent = n as u8;
    }
    for (key, slot) in [
        ("assignee_ids", &mut draft.assignee_ids),
        ("categories", &mut draft.categories),
    ] {
        if let Some(v) = args.get(key) {
            *slot = v
                .as_array()
                .and_then(|a| a.iter().map(|s| s.as_str().map(str::to_owned)).collect())
                .ok_or_else(|| format!("{key} が正しくありません: {v}"))?;
        }
    }
    Ok(())
}

fn priority_name(value: u8) -> &'static str {
    match Priority::of(value) {
        Priority::None => "none",
        Priority::High => "high",
        Priority::Mid => "mid",
        Priority::Low => "low",
    }
}

fn task_json(task: &Task) -> Value {
    json!({
        "id": task.id,
        "title": task.title,
        "description": task.description,
        "status": task.status.as_str(),
        "priority": priority_name(task.priority),
        "start": task.start,
        "due": task.due,
        "percent": task.percent,
        "workspace_id": task.workspace_id,
        "parent_id": task.parent_id,
        "assignee_ids": task.assignee_ids,
        "categories": task.categories,
    })
}

/// 一覧では、説明を省いて短くする。
fn task_row(task: &Task) -> Value {
    let mut row = task_json(task);
    if let Some(map) = row.as_object_mut() {
        map.remove("description");
        map.remove("start");
        map.remove("percent");
    }
    row
}

/// データベースに対するツールを実行する。返すのは AI に渡す文字列。
///
/// `author_id` は、コメントの書き手にする担当者（自分）。
pub fn execute(
    store: &mut Store,
    author_id: Option<&str>,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    let core = |e: malakt_core::Error| e.to_string();
    let value = match name {
        "list_workspaces" => store
            .workspaces()
            .map_err(core)?
            .iter()
            .map(|w| json!({"id": w.id, "name": w.name, "parent_id": w.parent_id}))
            .collect(),
        "list_people" => store
            .people()
            .map_err(core)?
            .iter()
            .map(|p| json!({"id": p.id, "name": p.name, "is_self": p.is_self}))
            .collect(),
        "list_tasks" => {
            let filter = TaskFilter {
                include_descendants: true,
                open_only: args.get("include_done").and_then(Value::as_bool) != Some(true),
                assignee_id: None,
                query: optional(args, "query").map(str::to_owned),
            };
            let scope = match optional(args, "workspace_id") {
                None | Some("") => Scope::All,
                Some("unfiled") => Scope::Unfiled,
                Some(id) => {
                    store.workspace(id).map_err(core)?;
                    Scope::Workspace(id)
                }
            };
            let tasks = store.tasks(scope, &filter).map_err(core)?;
            let rows: Vec<Value> = tasks.iter().take(LIST_LIMIT).map(task_row).collect();
            json!({"total": tasks.len(), "shown": rows.len(), "tasks": rows})
        }
        "get_task" => {
            let id = required(args, "id")?;
            let mut task = task_json(&store.task(id).map_err(core)?);
            task["comments"] = store
                .comments(id)
                .map_err(core)?
                .iter()
                .map(|c| json!({"author_id": c.author_id, "body": c.body, "pinned": c.pinned}))
                .collect();
            task["subtasks"] = store
                .subtasks(id)
                .map_err(core)?
                .iter()
                .map(task_row)
                .collect();
            task
        }
        "create_task" => {
            required(args, "title")?;
            let mut draft = TaskDraft::default();
            apply_fields(&mut draft, args)?;
            let workspace = optional(args, "workspace_id").filter(|id| !id.is_empty());
            task_json(&store.create_task(workspace, &draft).map_err(core)?)
        }
        "update_task" => {
            let id = required(args, "id")?;
            let mut draft = TaskDraft::from(&store.task(id).map_err(core)?);
            apply_fields(&mut draft, args)?;
            task_json(&store.update_task(id, &draft).map_err(core)?)
        }
        "move_task" => {
            let id = required(args, "id")?;
            let workspace = optional(args, "workspace_id").filter(|id| !id.is_empty());
            store.move_task(id, workspace).map_err(core)?;
            task_json(&store.task(id).map_err(core)?)
        }
        "delete_task" => {
            let id = required(args, "id")?;
            store.delete_task(id).map_err(core)?;
            json!({"deleted": id})
        }
        "add_comment" => {
            let comment = store
                .add_comment(
                    required(args, "task_id")?,
                    author_id,
                    required(args, "body")?,
                )
                .map_err(core)?;
            json!({"id": comment.id})
        }
        "create_workspace" => {
            let parent = optional(args, "parent_id").filter(|id| !id.is_empty());
            let w = store
                .create_workspace(parent, required(args, "name")?)
                .map_err(core)?;
            json!({"id": w.id, "name": w.name, "parent_id": w.parent_id})
        }
        "create_person" => {
            let p = store
                .create_person(required(args, "name")?, false)
                .map_err(core)?;
            json!({"id": p.id, "name": p.name})
        }
        other => return Err(format!("{other} というツールはありません")),
    };
    Ok(value.to_string())
}

/// 画面に出す、ツールの呼び出しの短い説明。
pub fn summary(name: &str, args: &Value) -> String {
    let arg = |key: &str| optional(args, key).unwrap_or_default().to_owned();
    match name {
        "list_workspaces" => "ワークスペース一覧を取得".into(),
        "list_people" => "担当者一覧を取得".into(),
        "list_tasks" => match optional(args, "query") {
            Some(q) if !q.is_empty() => format!("タスクを検索: {q}"),
            _ => "タスク一覧を取得".into(),
        },
        "get_task" => "タスクを取得".into(),
        "create_task" => format!("タスクを作成: {}", arg("title")),
        "update_task" => "タスクを更新".into(),
        "move_task" => "タスクを移動".into(),
        "delete_task" => "タスクを削除".into(),
        "add_comment" => "コメントを追加".into(),
        "create_workspace" => format!("ワークスペースを作成: {}", arg("name")),
        "create_person" => format!("担当者を追加: {}", arg("name")),
        GITHUB_FETCH => format!("GitHub から取得: {}", arg("url")),
        SLACK_FETCH => format!("Slack から取得: {}", arg("url")),
        RUN_SHELL => format!("コマンドを実行: {}", arg("command")),
        other => other.to_owned(),
    }
}

/// ユーザーのメッセージの最後に添える、今日の日付と今の画面。
/// `extra` は、今開いているワークスペースについての前提と指示。
pub fn turn_note(today: &str, place: &str, extra: &str) -> String {
    let mut note = format!("\n\n（アプリからの補足）\n今は {today} です。");
    for part in [place, extra] {
        if !part.trim().is_empty() {
            note.push('\n');
            note.push_str(part.trim());
        }
    }
    note
}

/// `run_shell` のコマンドが動くシェルの説明。
pub fn shell_name() -> &'static str {
    if cfg!(windows) {
        "Windows の cmd.exe（コマンドは `&` や `&&` でつなぐ。`;` ではつなげない。sh や PowerShell の書き方は使えない）"
    } else {
        "sh"
    }
}

/// AI への最初の指示。`context` は、ユーザーが設定に書いた、いつも伝える前提と指示。
/// 会話の途中で変わると、それまでの考えのブロックを送り返せなくなるので、
/// 日付や今の画面のように変わるものは入れない（[`turn_note`] でメッセージに添える）。
pub fn system_prompt(context: &str) -> String {
    let shell = shell_name();
    let context = if context.trim().is_empty() {
        String::new()
    } else {
        format!(
            "\n\nユーザーが設定に書いた前提と指示です。いつも従ってください。\n{}",
            context.trim()
        )
    };
    let prompt = format!(
        "あなたは、タスク管理アプリ malakt-mahoz の中で動くアシスタントです。\
         ツールを使って、ユーザーのタスク、ワークスペース、担当者を読み書きできます。\n\
         - 日付は YYYY-MM-DD、時刻つきなら YYYY-MM-DD HH:MM で渡します。時刻が分かる期限には時刻も入れてください。今の日時と、ユーザーが今開いている画面は、ユーザーのメッセージの最後にアプリが添えます。
\
         - 頼まれた操作は、確認を取らずにツールで実行してください。タスクの削除とシェルのコマンドは、アプリが実行の前にユーザーへ確認を出します。\n\
         - ID はツールで調べてから使ってください。ID を作らないでください。\n\
         - GitHub のリンクの中身は、必ず {GITHUB_FETCH} で取ってきてください（アプリが、ユーザーのログイン済みの gh で読みます）。{RUN_SHELL} で gh を呼ぶのは、{GITHUB_FETCH} で読めない種類のものだけにしてください。\n\
         - Slack のメッセージへのリンクの中身は、必ず {SLACK_FETCH} で取ってきてください（アプリが、ユーザーの Slack CLI で読みます）。\n\
         - {RUN_SHELL} のコマンドは {shell} で動きます。そのシェルの書き方で書いてください。\n\
         - タスクの追加、書き換え、整理のように元に戻せる操作は、「〜しましょうか」と聞き返さずに実行し、結果だけを伝えてください。聞き返すのは、頼まれたことが 2 通り以上に読めて、どちらかで結果が大きく変わるときだけです。\n\
         - ツールが失敗したら、やり方を変えてやり直してください。返事には最後の結果だけを書き、途中の失敗、その原因、ツールやシェルの事情は書かないでください。どうしてもできなかったときだけ、できなかったことと、ユーザーにしてほしいことを 1 文で書いてください。\n\
         - 返事は短く、ユーザーの言葉（ふつうは日本語）で書いてください。記法は使わず、ふつうの文章で書いてください。"
    );
    prompt + &context
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(store: &mut Store, name: &str, args: Value) -> Value {
        serde_json::from_str(&execute(store, None, name, &args).unwrap()).unwrap()
    }

    #[test]
    fn every_tool_has_a_closed_object_schema_and_a_unique_name() {
        let tools = definitions();
        let mut names: Vec<_> = tools.iter().map(|t| t.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), tools.len());
        for tool in &tools {
            assert_eq!(tool.schema["type"], "object", "{}", tool.name);
            assert_eq!(tool.schema["additionalProperties"], false, "{}", tool.name);
            for key in tool.schema["required"].as_array().unwrap() {
                assert!(
                    tool.schema["properties"]
                        .get(key.as_str().unwrap())
                        .is_some(),
                    "{} の required にない項目: {key}",
                    tool.name
                );
            }
            // 画面に出す説明がある
            assert_ne!(summary(tool.name, &json!({})), tool.name);
        }
    }

    #[test]
    fn only_irreversible_tools_ask_first() {
        let asking: Vec<_> = definitions()
            .iter()
            .map(|t| t.name)
            .filter(|n| needs_confirmation(n))
            .collect();
        assert_eq!(asking, ["delete_task", "run_shell"]);
    }

    #[test]
    fn tasks_are_created_read_changed_and_moved() {
        let mut store = Store::open_in_memory().unwrap();
        let ws = run(&mut store, "create_workspace", json!({"name": "家"}));
        let inner = run(
            &mut store,
            "create_workspace",
            json!({"name": "台所", "parent_id": ws["id"]}),
        );
        assert_eq!(inner["parent_id"], ws["id"]);
        let person = run(&mut store, "create_person", json!({"name": "奏"}));

        let task = run(
            &mut store,
            "create_task",
            json!({
                "title": "牛乳を買う", "workspace_id": inner["id"], "priority": "high",
                "due": "2026-12-01", "assignee_ids": [person["id"]], "categories": ["買い物"],
            }),
        );
        assert_eq!(task["status"], "needs_action");
        assert_eq!(task["priority"], "high");
        assert_eq!(task["due"], "2026-12-01");
        // 時刻つきの期限。ISO の書き方で渡されても読む
        let timed = run(
            &mut store,
            "update_task",
            json!({"id": task["id"], "due": "2026-12-01T18:30:00", "start": "2026/11/30 9時"}),
        );
        assert_eq!(timed["due"], "2026-12-01 18:30");
        assert_eq!(timed["start"], "2026-11-30 09:00");
        let back = run(
            &mut store,
            "update_task",
            json!({"id": task["id"], "due": "2026-12-01", "start": null}),
        );
        assert_eq!(back["due"], "2026-12-01");
        assert_eq!(task["workspace_id"], inner["id"]);
        let id = task["id"].as_str().unwrap().to_owned();

        // 渡した項目だけが変わる。null で期限がなくなる
        let changed = run(
            &mut store,
            "update_task",
            json!({"id": id, "status": "in_process", "due": null, "percent": 40}),
        );
        assert_eq!(changed["title"], "牛乳を買う");
        assert_eq!(changed["status"], "in_process");
        assert_eq!(changed["due"], Value::Null);
        assert_eq!(changed["percent"], 40);
        assert_eq!(changed["priority"], "high");
        assert_eq!(changed["assignee_ids"], json!([person["id"]]));

        run(
            &mut store,
            "add_comment",
            json!({"task_id": id, "body": "低脂肪で"}),
        );
        let sub = run(
            &mut store,
            "create_task",
            json!({"title": "店を決める", "parent_id": id}),
        );
        let read = run(&mut store, "get_task", json!({"id": id}));
        assert_eq!(read["comments"][0]["body"], "低脂肪で");
        assert_eq!(read["subtasks"][0]["id"], sub["id"]);

        // 親のワークスペースで探すと、子のワークスペースのタスクも出る
        let found = run(&mut store, "list_tasks", json!({"workspace_id": ws["id"]}));
        assert_eq!(found["total"], 1);
        assert!(found["tasks"][0].get("description").is_none());
        let unfiled = run(&mut store, "list_tasks", json!({"workspace_id": "unfiled"}));
        assert_eq!(unfiled["tasks"][0]["id"], sub["id"]);
        let by_query = run(&mut store, "list_tasks", json!({"query": "牛乳"}));
        assert_eq!(by_query["total"], 1);

        let moved = run(
            &mut store,
            "move_task",
            json!({"id": id, "workspace_id": null}),
        );
        assert_eq!(moved["workspace_id"], Value::Null);

        // 完了にすると、標準の一覧から消える
        run(
            &mut store,
            "update_task",
            json!({"id": id, "status": "completed"}),
        );
        assert_eq!(run(&mut store, "list_tasks", json!({}))["total"], 1);
        assert_eq!(
            run(&mut store, "list_tasks", json!({"include_done": true}))["total"],
            2
        );

        assert_eq!(
            run(&mut store, "list_workspaces", json!({}))
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(run(&mut store, "list_people", json!({}))[0]["name"], "奏");

        assert!(confirmation_text(&store, "delete_task", &json!({"id": id})).contains("牛乳を買う"));
        run(&mut store, "delete_task", json!({"id": id}));
        assert!(store.task(&id).is_err());
    }

    #[test]
    fn comments_are_written_as_the_given_author() {
        let mut store = Store::open_in_memory().unwrap();
        let me = store.create_person("自分", true).unwrap();
        let task = run(&mut store, "create_task", json!({"title": "a"}));
        let id = task["id"].as_str().unwrap();
        execute(
            &mut store,
            Some(&me.id),
            "add_comment",
            &json!({"task_id": id, "body": "b"}),
        )
        .unwrap();
        assert_eq!(store.comments(id).unwrap()[0].author_id, Some(me.id));
    }

    #[test]
    fn bad_calls_fail_with_a_reason_and_change_nothing() {
        let mut store = Store::open_in_memory().unwrap();
        let mut fail =
            |name: &str, args: Value| execute(&mut store, None, name, &args).unwrap_err();
        assert!(fail("create_task", json!({})).contains("title"));
        assert!(fail("create_task", json!({"title": "a", "status": "done"})).contains("status"));
        assert!(
            fail("create_task", json!({"title": "a", "priority": "urgent"})).contains("priority")
        );
        assert!(fail("create_task", json!({"title": "a", "percent": 101})).contains("percent"));
        assert!(fail("create_task", json!({"title": "a", "due": 5})).contains("due"));
        assert!(
            fail("create_task", json!({"title": "a", "categories": [1]})).contains("categories")
        );
        assert!(
            fail("create_task", json!({"title": "a", "due": "そのうち"}))
                .contains("正しくありません")
        );
        assert!(fail("update_task", json!({"id": "none"})).contains("見つかりません"));
        assert!(fail("list_tasks", json!({"workspace_id": "none"})).contains("見つかりません"));
        assert!(fail("delete_task", json!({"id": "none"})).contains("見つかりません"));
        assert!(fail("format_disk", json!({})).contains("format_disk"));
        assert!(store
            .tasks(Scope::All, &TaskFilter::default())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn confirmation_shows_the_command() {
        let store = Store::open_in_memory().unwrap();
        let text = confirmation_text(&store, RUN_SHELL, &json!({"command": "ls -la"}));
        assert!(text.ends_with("ls -la"));
        assert!(confirmation_text(&store, "delete_task", &json!({"id": "x"})).contains("x"));
    }

    #[test]
    fn the_system_prompt_stays_the_same_between_requests() {
        let prompt = system_prompt("");
        assert!(prompt.contains("github_fetch"));
        // シェルの種類と、聞き返さない・言い訳しない決まり
        assert!(prompt.contains(shell_name()));
        assert!(prompt.contains("聞き返さずに実行"));
        assert!(prompt.contains("途中の失敗"));
        assert!(!prompt.contains("前提と指示"));
        let prompt = system_prompt(" PR は必ずレビューする \n");
        assert!(prompt.ends_with("いつも従ってください。\nPR は必ずレビューする"));

        // 日付と今の画面は、メッセージに添える
        let note = turn_note("2026-11-24", "ユーザーは「家」を開いています。", "");
        assert!(note.contains("2026-11-24") && note.ends_with("「家」を開いています。"));
        assert!(turn_note("2026-11-24", "", "敬語で").ends_with("です。\n敬語で"));
    }
}
