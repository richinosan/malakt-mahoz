//! AI がタスクを読み書きするためのツール。

use malakt_core::view::Priority;
use malakt_core::{Person, Role, Roles, Scope, Status, Store, Task, TaskDraft, TaskFilter};
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
        "assignee_ids": {"type": "array", "items": {"type": "string"},
                         "description": "担当者の ID。足した人の役は R。役も決めるときは assignees を使う"},
        "assignees": {
            "type": "array",
            "description": "担当者と役（RACI）。渡すと担当者がこの並びになる（assignee_ids と一緒には渡さない）。\
                            person は担当者の ID か名前。登録されていない名前は、その場で担当者に登録する",
            "items": {
                "type": "object",
                "properties": {
                    "person": {"type": "string", "description": "担当者の ID か名前（敬称は付けない）"},
                    "roles": roles_schema("省くと、もう担当の人は今の役のまま、新しい人は R"),
                },
                "required": ["person"],
                "additionalProperties": false,
            },
        },
        "categories": {"type": "array", "items": {"type": "string"}},
    })
}

/// RACI の説明。ツールの説明とシステムの指示で同じ文を使う。
const RACI: &str = "R=実行責任者、A=説明責任者（1 つのタスクに 1 人まで）、C=相談先、I=報告先";

fn roles_schema(extra: &str) -> Value {
    json!({
        "type": "array",
        "items": {"type": "string", "enum": ["R", "A", "C", "I"]},
        "description": format!("役。{RACI}。{extra}"),
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
            description: "担当者の一覧を返す。workspace_ids が空の人はすべてのワークスペースで、\
                          そうでない人はそのワークスペース（とサブワークスペース）で選べる。",
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
            name: "set_assignee",
            description: SET_ASSIGNEE_TOOL,
            schema: object(
                json!({
                    "task_id": {"type": "string"},
                    "person": {"type": "string", "description": "担当者の ID か名前（敬称は付けない）"},
                    "roles": roles_schema("A を付けると、ほかの人の A は外れる。空にすると担当から外す"),
                }),
                &["task_id", "person", "roles"],
            ),
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
            description: COMMENT_TOOL,
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
            description: "担当者を登録する。workspace_id を渡すと、そのワークスペース（のいちばん上の親とサブワークスペース）で選べる担当者になり、省くとすべてのワークスペースで選べる担当者になる。",
            schema: object(
                json!({"name": {"type": "string"}, "workspace_id": {"type": "string"}}),
                &["name"],
            ),
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

const SET_ASSIGNEE_TOOL: &str = "タスクの担当者 1 人の役（RACI）を決める。担当でない人は担当に加える。\
person は担当者の ID か名前。登録されていない名前は、タスクのワークスペースの担当者としてその場で登録する。";

/// 担当者を ID か名前で探した結果。
enum Who {
    /// 登録されている担当者。`share` なら、このワークスペースでも選べるようにする。
    Known { id: String, share: bool },
    /// 登録されていない名前。
    New(String),
}

/// そのワークスペース（いちばん上の親）で選べる担当者か。どこにも入っていないタスクでは、誰でも選べる。
fn usable(person: &Person, root: Option<&str>) -> bool {
    person.workspace_ids.is_empty()
        || root.is_none_or(|r| person.workspace_ids.iter().any(|w| w == r))
}

/// 担当者を ID か名前で探す。同じ名前の人が何人もいて決められなければ、失敗にする。
fn find_person(people: &[Person], key: &str, root: Option<&str>) -> Result<Who, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("person に担当者の ID か名前が必要です".into());
    }
    if let Some(p) = people.iter().find(|p| p.id == key) {
        return Ok(Who::Known {
            id: p.id.clone(),
            share: !usable(p, root),
        });
    }
    let named: Vec<&Person> = people.iter().filter(|p| p.name.trim() == key).collect();
    let here: Vec<&Person> = named.iter().copied().filter(|p| usable(p, root)).collect();
    let pick = if here.is_empty() { &named } else { &here };
    match pick.as_slice() {
        [] => Ok(Who::New(key.to_owned())),
        [p] => Ok(Who::Known {
            id: p.id.clone(),
            share: !usable(p, root),
        }),
        many => Err(format!(
            "「{key}」という担当者が {} 人います。ID で指定してください: {}",
            many.len(),
            many.iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// 探した担当者を使える形にする。登録されていない名前はここで登録する。
fn settle_person(store: &mut Store, who: Who, workspace: Option<&str>) -> Result<String, String> {
    let core = |e: malakt_core::Error| e.to_string();
    match (who, workspace) {
        (Who::Known { id, share: true }, Some(w)) => {
            store.share_person(&id, w).map_err(core)?;
            Ok(id)
        }
        (Who::Known { id, .. }, _) => Ok(id),
        (Who::New(name), Some(w)) => Ok(store.create_local_person(&name, w).map_err(core)?.id),
        (Who::New(name), None) => Ok(store.create_person(&name, false).map_err(core)?.id),
    }
}

/// `["R", "C"]` の形を読む。
fn parse_roles(v: &Value) -> Result<Roles, String> {
    let bad = || format!("roles が正しくありません: {v}");
    let list = v.as_array().ok_or_else(bad)?;
    let mut roles = Roles::NONE;
    for code in list {
        let role = code
            .as_str()
            .and_then(|c| Role::parse(&c.trim().to_uppercase()))
            .ok_or_else(bad)?;
        roles = roles.with(role);
    }
    Ok(roles)
}

fn role_codes(roles: Roles) -> Vec<&'static str> {
    roles.iter().map(Role::code).collect()
}

/// 担当者と役の並び。役が `None` の人は、もう担当なら今の役のまま、新しい人は R。
type Plan = Vec<(Who, Option<Roles>)>;

/// `assignees` を読んで、確かめる。何も書き込まない。
fn plan_assignees(store: &Store, args: &Value, root: Option<&str>) -> Result<Option<Plan>, String> {
    let Some(v) = args.get("assignees") else {
        return Ok(None);
    };
    if args.get("assignee_ids").is_some() {
        return Err("assignee_ids と assignees は一緒に渡せません".into());
    }
    let bad = || format!("assignees が正しくありません: {v}");
    let people = store.people().map_err(|e| e.to_string())?;
    let mut plan: Plan = Vec::new();
    for entry in v.as_array().ok_or_else(bad)? {
        let key = entry
            .get("person")
            .and_then(Value::as_str)
            .ok_or_else(bad)?;
        let roles = match entry.get("roles") {
            None | Some(Value::Null) => None,
            Some(r) => {
                let roles = parse_roles(r)?;
                if roles.is_empty() {
                    return Err(format!("{key} の roles が空です。役は 1 つ以上必要です"));
                }
                Some(roles)
            }
        };
        let who = find_person(&people, key, root)?;
        let same = plan.iter().any(|(other, _)| match (other, &who) {
            (Who::Known { id: a, .. }, Who::Known { id: b, .. }) => a == b,
            (Who::New(a), Who::New(b)) => a == b,
            _ => false,
        });
        if same {
            return Err(format!("担当者 {key} が 2 回あります"));
        }
        plan.push((who, roles));
    }
    let accountable = plan
        .iter()
        .filter(|(_, r)| r.is_some_and(|r| r.contains(Role::A)))
        .count();
    if accountable > 1 {
        return Err("A（説明責任者）は 1 つのタスクに 1 人までです".into());
    }
    Ok(Some(plan))
}

/// 確かめた `assignees` を書き込む。担当者の並びを決めてから、役を付ける。
fn apply_assignees(store: &mut Store, task: Task, plan: Plan) -> Result<Task, String> {
    let core = |e: malakt_core::Error| e.to_string();
    let workspace = task.workspace_id.clone();
    let mut people = Vec::new();
    for (who, roles) in plan {
        people.push((settle_person(store, who, workspace.as_deref())?, roles));
    }
    let mut draft = TaskDraft::from(&task);
    draft.assignee_ids = people.iter().map(|(id, _)| id.clone()).collect();
    let mut task = store.update_task(&task.id, &draft).map_err(core)?;
    // A を付ける人を最後にする（ほかの人に付け直した役で A が外れないように）
    people.sort_by_key(|(_, r)| r.is_some_and(|r| r.contains(Role::A)));
    for (id, roles) in people {
        if let Some(roles) = roles.filter(|r| task.roles_of(&id) != Some(*r)) {
            task = store
                .set_assignee_roles(&task.id, &id, roles)
                .map_err(core)?;
        }
    }
    Ok(task)
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
        "assignees": task.assignee_ids.iter().map(|id| json!({
            "person_id": id,
            "roles": role_codes(task.roles_of(id).unwrap_or_default()),
        })).collect::<Vec<_>>(),
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
            .map(|p| json!({"id": p.id, "name": p.name, "is_self": p.is_self, "workspace_ids": p.workspace_ids}))
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
            let root = match workspace {
                Some(w) => Some(store.workspace_root(w).map_err(core)?),
                None => None,
            };
            let plan = plan_assignees(store, args, root.as_deref())?;
            let task = store.create_task(workspace, &draft).map_err(core)?;
            match plan {
                Some(plan) => task_json(&apply_assignees(store, task, plan)?),
                None => task_json(&task),
            }
        }
        "update_task" => {
            let id = required(args, "id")?;
            let old = store.task(id).map_err(core)?;
            let mut draft = TaskDraft::from(&old);
            apply_fields(&mut draft, args)?;
            let root = match &old.workspace_id {
                Some(w) => Some(store.workspace_root(w).map_err(core)?),
                None => None,
            };
            let plan = plan_assignees(store, args, root.as_deref())?;
            let task = store.update_task(id, &draft).map_err(core)?;
            match plan {
                Some(plan) => task_json(&apply_assignees(store, task, plan)?),
                None => task_json(&task),
            }
        }
        "set_assignee" => {
            let task = store.task(required(args, "task_id")?).map_err(core)?;
            let roles = parse_roles(args.get("roles").unwrap_or(&Value::Null))?;
            let root = match &task.workspace_id {
                Some(w) => Some(store.workspace_root(w).map_err(core)?),
                None => None,
            };
            let people = store.people().map_err(core)?;
            let who = find_person(&people, required(args, "person")?, root.as_deref())?;
            if roles.is_empty() {
                // 担当から外す。登録されていない人や担当でない人は、もう外れている
                if let Who::Known { id, .. } = who {
                    if task.roles_of(&id).is_some() {
                        let mut draft = TaskDraft::from(&task);
                        draft.assignee_ids.retain(|p| *p != id);
                        return Ok(task_json(&store.update_task(&task.id, &draft).map_err(core)?).to_string());
                    }
                }
                task_json(&task)
            } else {
                let id = settle_person(store, who, task.workspace_id.as_deref())?;
                let mut task = task;
                if task.roles_of(&id).is_none() {
                    let mut draft = TaskDraft::from(&task);
                    draft.assignee_ids.push(id.clone());
                    task = store.update_task(&task.id, &draft).map_err(core)?;
                }
                task_json(&store.set_assignee_roles(&task.id, &id, roles).map_err(core)?)
            }
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
            let name = required(args, "name")?;
            let p = match optional(args, "workspace_id").filter(|id| !id.is_empty()) {
                Some(w) => store.create_local_person(name, w),
                None => store.create_person(name, false),
            }
            .map_err(core)?;
            json!({"id": p.id, "name": p.name, "workspace_ids": p.workspace_ids})
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
        "set_assignee" => match args.get("roles").and_then(Value::as_array) {
            Some(r) if r.is_empty() => format!("担当から外す: {}", arg("person")),
            Some(r) => format!(
                "担当者の役を設定: {}（{}）",
                arg("person"),
                r.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("・")
            ),
            None => format!("担当者の役を設定: {}", arg("person")),
        },
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
/// コメントに書いてよいこと。アプリの中の AI にも、MCP でつなぐ外のエージェントにも同じ文を渡す。
pub const COMMENT_RULE: &str = "コメントは、あとでタスクを読む人に要る事実だけを書いてください（決まったこと、分かったこと、変わった状態、次にすること）。「確認しました」「レビューしました」「対応しました」のように、作業をしたことだけを伝える文は書かないでください。「まだ〜していません」「〜は未確認です」のように、していないことも書かないでください。書く事実がなければ、コメントを書かないでください。";

const COMMENT_TOOL: &str = "タスクにコメントを書く。書くのは、あとでタスクを読む人に要る事実だけ（決まったこと、分かったこと、変わった状態、次にすること）。「確認しました」「レビューしました」のように作業をしたことだけを伝える文と、「まだ〜していません」のようにしていないことは書かない。書く事実がなければ呼ばない。";

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
         - 担当者は ID か名前で指定できます。登録されていない人の名前が出たら、聞き返さずにその名前で指定してください（アプリがその場で担当者に登録します）。担当者の役（RACI: {RACI}）は set_assignee か、create_task / update_task の assignees で付けてください。\n\
         - GitHub のリンクの中身は、必ず {GITHUB_FETCH} で取ってきてください（アプリが、ユーザーのログイン済みの gh で読みます）。{RUN_SHELL} で gh を呼ぶのは、{GITHUB_FETCH} で読めない種類のものだけにしてください。\n\
         - Slack のメッセージへのリンクの中身は、必ず {SLACK_FETCH} で取ってきてください（アプリが、ユーザーの Slack CLI で読みます）。\n\
         - {RUN_SHELL} のコマンドは {shell} で動きます。そのシェルの書き方で書いてください。\n\
         - タスクの追加、書き換え、整理のように元に戻せる操作は、「〜しましょうか」と聞き返さずに実行し、結果だけを伝えてください。聞き返すのは、頼まれたことが 2 通り以上に読めて、どちらかで結果が大きく変わるときだけです。\n\
         - ツールが失敗したら、やり方を変えてやり直してください。返事には最後の結果だけを書き、途中の失敗、その原因、ツールやシェルの事情は書かないでください。どうしてもできなかったときだけ、できなかったことと、ユーザーにしてほしいことを 1 文で書いてください。\n\
         - {COMMENT_RULE}ユーザーが文面を決めてコメントを頼んだときは、その文面のまま書いてください。\n\
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
    fn comments_without_facts_are_forbidden_everywhere_the_tool_is_offered() {
        let tool = definitions()
            .into_iter()
            .find(|t| t.name == "add_comment")
            .unwrap();
        for text in [tool.description, &system_prompt(""), COMMENT_RULE] {
            assert!(text.contains("事実だけ"), "{text}");
            assert!(text.contains("していないこと"), "{text}");
            assert!(text.contains("書く事実がなければ"), "{text}");
        }
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
    fn roles_are_set_by_name_and_new_names_are_registered_in_the_workspace() {
        let mut store = Store::open_in_memory().unwrap();
        let me = store.create_person("自分", true).unwrap();
        let ws = run(&mut store, "create_workspace", json!({"name": "イベント"}));
        let sub = run(
            &mut store,
            "create_workspace",
            json!({"name": "告知", "parent_id": ws["id"]}),
        );
        let task = run(
            &mut store,
            "create_task",
            json!({
                "title": "チラシを作る", "workspace_id": sub["id"],
                "assignees": [{"person": me.id, "roles": ["R", "A"]}, {"person": "佐藤"}],
            }),
        );
        let people = run(&mut store, "list_people", json!({}));
        let sato = people
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "佐藤")
            .unwrap()
            .clone();
        // 登録されていない名前は、いちばん上の親のワークスペースの担当者になる
        assert_eq!(sato["workspace_ids"], json!([ws["id"]]));
        // 返す内容に、担当者ごとの役が入る
        let mine = task["assignees"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["person_id"] == me.id.as_str())
            .unwrap();
        assert_eq!(mine["roles"], json!(["R", "A"]));
        let id = task["id"].as_str().unwrap().to_owned();
        let roles = |store: &Store, person: &str| {
            role_codes(
                store
                    .task(&id)
                    .unwrap()
                    .roles_of(person)
                    .unwrap_or(Roles::NONE),
            )
        };
        assert_eq!(roles(&store, &me.id), ["R", "A"]);
        assert_eq!(roles(&store, sato["id"].as_str().unwrap()), ["R"]);

        // C（相談先）を名前で付ける。登録されていない人は、その場で登録して担当に加える
        let t = run(
            &mut store,
            "set_assignee",
            json!({"task_id": id, "person": "鈴木", "roles": ["C"]}),
        );
        assert_eq!(t["assignee_ids"].as_array().unwrap().len(), 3);
        let suzuki = store
            .people()
            .unwrap()
            .into_iter()
            .find(|p| p.name == "鈴木")
            .unwrap();
        assert_eq!(suzuki.workspace_ids, [ws["id"].as_str().unwrap()]);
        assert_eq!(roles(&store, &suzuki.id), ["C"]);
        // 同じ名前は、登録し直さずに同じ人を使う
        run(
            &mut store,
            "set_assignee",
            json!({"task_id": id, "person": " 鈴木 ", "roles": ["C", "I"]}),
        );
        assert_eq!(store.people().unwrap().len(), 3);
        assert_eq!(roles(&store, &suzuki.id), ["C", "I"]);

        // A を付けると、ほかの人の A は外れる
        run(
            &mut store,
            "set_assignee",
            json!({"task_id": id, "person": "佐藤", "roles": ["a"]}),
        );
        assert_eq!(roles(&store, sato["id"].as_str().unwrap()), ["A"]);
        assert_eq!(roles(&store, &me.id), ["R"]);

        // update_task の assignees は並びを置き換え、役を省いた人は今の役のまま
        let t = run(
            &mut store,
            "update_task",
            json!({"id": id, "assignees": [{"person": "佐藤"}, {"person": me.id, "roles": ["A"]}]}),
        );
        assert_eq!(t["assignee_ids"].as_array().unwrap().len(), 2);
        assert_eq!(roles(&store, &me.id), ["A"]);
        assert_eq!(roles(&store, sato["id"].as_str().unwrap()), ["R"]);

        // 役を空にすると担当から外す
        run(
            &mut store,
            "set_assignee",
            json!({"task_id": id, "person": "佐藤", "roles": []}),
        );
        assert_eq!(
            store.task(&id).unwrap().assignee_ids,
            std::slice::from_ref(&me.id)
        );

        // 別のワークスペースのローカル担当者は、名前で選ぶとこちらでも使えるようにする
        let other = run(&mut store, "create_workspace", json!({"name": "家"}));
        let home = run(
            &mut store,
            "create_task",
            json!({"title": "買い物", "workspace_id": other["id"]}),
        );
        run(
            &mut store,
            "set_assignee",
            json!({"task_id": home["id"], "person": "鈴木", "roles": ["I"]}),
        );
        let suzuki = store
            .people()
            .unwrap()
            .into_iter()
            .find(|p| p.id == suzuki.id)
            .unwrap();
        assert_eq!(suzuki.workspace_ids.len(), 2);
        assert_eq!(store.people().unwrap().len(), 3);

        // どこにも入っていないタスクでは、すべてのワークスペースの担当者として登録する
        let loose = run(&mut store, "create_task", json!({"title": "メモ"}));
        run(
            &mut store,
            "set_assignee",
            json!({"task_id": loose["id"], "person": "高橋", "roles": ["R"]}),
        );
        let takahashi = store
            .people()
            .unwrap()
            .into_iter()
            .find(|p| p.name == "高橋")
            .unwrap();
        assert!(takahashi.workspace_ids.is_empty());

        // create_person に workspace_id を渡すと、そのワークスペースの担当者になる
        let local = run(
            &mut store,
            "create_person",
            json!({"name": "伊藤", "workspace_id": sub["id"]}),
        );
        assert_eq!(local["workspace_ids"], json!([ws["id"]]));
    }

    #[test]
    fn bad_assignees_fail_before_anything_is_written() {
        let mut store = Store::open_in_memory().unwrap();
        let task = run(&mut store, "create_task", json!({"title": "a"}));
        let id = task["id"].as_str().unwrap();
        let mut fail =
            |name: &str, args: Value| execute(&mut store, None, name, &args).unwrap_err();
        assert!(fail(
            "update_task",
            json!({"id": id, "title": "b", "assignees": [{"person": "x", "roles": ["A"]}, {"person": "y", "roles": ["A"]}]})
        )
        .contains("1 人まで"));
        assert!(fail(
            "update_task",
            json!({"id": id, "assignees": [{"person": "x"}, {"person": "x"}]})
        )
        .contains("2 回"));
        assert!(fail(
            "update_task",
            json!({"id": id, "assignees": [{"person": "x", "roles": []}]})
        )
        .contains("1 つ以上"));
        assert!(fail(
            "update_task",
            json!({"id": id, "assignees": [{"person": "x", "roles": ["Z"]}]})
        )
        .contains("roles"));
        assert!(fail(
            "update_task",
            json!({"id": id, "assignee_ids": [], "assignees": []})
        )
        .contains("一緒に"));
        assert!(fail(
            "set_assignee",
            json!({"task_id": id, "person": " ", "roles": ["R"]})
        )
        .contains("person"));
        assert!(fail(
            "set_assignee",
            json!({"task_id": "none", "person": "x", "roles": ["R"]})
        )
        .contains("見つかりません"));
        assert!(store.people().unwrap().is_empty());
        assert_eq!(store.task(id).unwrap().title, "a");
    }

    #[test]
    fn two_people_with_the_same_name_need_an_id() {
        let mut store = Store::open_in_memory().unwrap();
        store.create_person("佐藤", false).unwrap();
        store.create_person("佐藤", false).unwrap();
        let task = run(&mut store, "create_task", json!({"title": "a"}));
        let error = execute(
            &mut store,
            None,
            "set_assignee",
            &json!({"task_id": task["id"], "person": "佐藤", "roles": ["C"]}),
        )
        .unwrap_err();
        assert!(error.contains("2 人"), "{error}");
        assert!(store
            .task(task["id"].as_str().unwrap())
            .unwrap()
            .assignee_ids
            .is_empty());
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
