//! AI がタスクを読み書きするためのツール。

use std::path::PathBuf;

use malakt_core::rules::{AiRule, Apply, RulePatch};
use malakt_core::view::Priority;
use malakt_core::{
    when, Comment, DueRule, Person, Role, Roles, Scope, Status, Store, Task, TaskDraft, TaskFilter,
    Workspace,
};
use serde_json::{json, Value};

use crate::shell::Shell;
use crate::wire::ToolDef;

/// GitHub のリンクの中身を取りに行くツール。実行は画面の側（シェルに出す）。
pub const GITHUB_FETCH: &str = "github_fetch";
/// Slack のリンクの中身を取りに行くツール。実行は画面の側（シェルに出す）。
pub const SLACK_FETCH: &str = "slack_fetch";
/// シェルでコマンドを実行するツール。実行は画面の側（毎回、確認を出す）。
pub const RUN_SHELL: &str = "run_shell";

const LIST_LIMIT: usize = 200;
/// `get_task` で返す変更履歴の数。
const CHANGES_LIMIT: usize = 30;
/// `get_workspace` で返すタスクとサブワークスペースの数。
const WORKSPACE_TASKS: usize = 150;
const WORKSPACE_SUBS: usize = 100;
/// `get_workspace` の結果の文字数の上限。超える分のタスクは載せず、件数だけを返す。
pub const WORKSPACE_CHARS: usize = 40_000;
/// `get_workspace` の一覧に載せる、タスクの説明の文字数。
const WORKSPACE_DESCRIPTION: usize = 200;

/// ワークスペースごとの、ターミナルを始めるフォルダーの保存先。
pub fn workspace_dir_key(workspace: &str) -> String {
    format!("term.dir.{workspace}")
}

/// 設定に書いたフォルダーやファイル。先頭の `~` はホームに読み替える。
pub fn expand_dir(text: &str) -> PathBuf {
    let text = text.trim();
    let rest = text
        .strip_prefix('~')
        .filter(|rest| rest.is_empty() || rest.starts_with(['/', '\\']));
    match (rest, dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest.trim_start_matches(['/', '\\'])),
        _ => PathBuf::from(text),
    }
}

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
        "urls": {"type": "array", "items": {"type": "string"},
                 "description": "タスクに付ける URL。渡すと URL がこの並びになる（足すだけなら add_urls）"},
        "add_urls": {"type": "array", "items": {"type": "string"},
                     "description": "今の URL の後ろに足す URL。同じものは足さない"},
        "remove_urls": {"type": "array", "items": {"type": "string"}, "description": "外す URL"},
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

/// AI のルールの項目。
fn rule_fields(extra: Value) -> Value {
    with(
        json!({
            "name": {"type": "string", "description": format!("名前（{} 文字まで）", malakt_core::rules::MAX_NAME_CHARS)},
            "body": {"type": "string", "description": format!("AI への指示（{} 文字まで）", malakt_core::rules::MAX_RULE_CHARS)},
            "enabled": {"type": "boolean", "description": "false で使わない"},
            "apply": {"type": "string", "enum": ["always", "chat", "auto"],
                      "description": "使う場面。いつも / ユーザーとのチャットだけ / 自動の処理（新しいタスクを整える、コメントから更新、GitHub の定期チェック）だけ"},
        }),
        extra,
    )
}

fn schedule_fields(extra: Value) -> Value {
    let max_days = malakt_core::schedule::MAX_DUE_DAYS;
    with(
        json!({
            "title": {"type": "string", "description": format!("作るタスクのタイトル（{} 文字まで）", malakt_core::schedule::MAX_TITLE_CHARS)},
            "schedule": {
                "type": "object",
                "description": "いつタスクを作るか",
                "properties": {
                    "unit": {"type": "string", "enum": ["day", "week", "month", "year"]},
                    "every": {"type": "integer", "minimum": 1, "maximum": malakt_core::repeat::MAX_EVERY, "description": "何日・何週間・何か月・何年ごとか（標準は 1）"},
                    "weekdays": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 6}, "description": "unit が week のときの曜日。0 = 月 … 6 = 日。省くと起点の曜日"},
                    "from": {"type": "string", "description": "数え始めの日 YYYY-MM-DD。毎月・毎年の日付になる。省くと今日（変えるときは今の起点）"},
                    "time": {"type": "string", "description": "タスクを作る時刻 HH:MM。空で 0:00"},
                },
                "required": ["unit"],
            },
            "ai_instruction": {"type": "string", "description": "作ったタスクで AI に頼むこと。空で頼まない"},
            "enabled": {"type": "boolean", "description": "false で止める"},
            // ここから下は、作るタスクの項目（create_task と同じもの）
            "description": {"type": ["string", "null"], "description": "作るタスクの説明。null でワークスペースの既定値"},
            "status": {"type": "string", "enum": ["needs_action", "in_process", "completed", "cancelled"],
                       "description": "作るタスクの状態。未着手 / 進行中 / 完了 / 中止"},
            "priority": {"type": ["string", "null"], "enum": ["none", "high", "mid", "low", null],
                         "description": "作るタスクの優先度。null でワークスペースの既定値"},
            "start_days": {"type": ["integer", "null"], "minimum": 0, "maximum": max_days, "description": "開始日を作った日の何日後にするか（0 で当日）。null で開始日なし"},
            "start_time": {"type": ["string", "null"], "description": "開始日の時刻 HH:MM。null か空で日付だけ"},
            "due_days": {"type": ["integer", "null"], "minimum": 0, "maximum": max_days, "description": "期限を作った日の何日後にするか（0 で当日）。null でワークスペースの既定値"},
            "due_time": {"type": ["string", "null"], "description": "期限の時刻 HH:MM。null か空で日付だけ"},
            "due_alerts": {
                "type": ["object", "null"],
                "description": "作るタスクの、期限をいつ知らせるか（set_due_alerts と同じ）。null で全体の標準",
                "properties": {
                    "timed_minutes": {"type": ["array", "null"], "items": {"type": "integer", "minimum": 0, "maximum": malakt_core::alert::MAX_MINUTES}, "description": "時刻つきの期限の何分前か。[] で知らせない、null で標準"},
                    "date_days": {"type": ["array", "null"], "items": {"type": "integer", "minimum": 0, "maximum": malakt_core::alert::MAX_DAYS}, "description": "日付だけの期限の何日前か。[] で知らせない、null で標準"},
                },
            },
            "percent": {"type": "integer", "minimum": 0, "maximum": 100, "description": "作るタスクの進捗"},
            "parent_id": {"type": ["string", "null"], "description": "親のタスクの ID（作るタスクをそのサブタスクにする）。null で親なし"},
            "assignee_ids": {"type": "array", "items": {"type": "string"}, "description": "作るタスクの担当者の ID。[] でワークスペースの既定値。足した人の役は R。役も決めるときは assignees を使う"},
            "assignees": {
                "type": "array",
                "description": "作るタスクの担当者と役（RACI）。渡すと担当者がこの並びになる（assignee_ids と一緒には渡さない）。\
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
            "categories": {"type": ["array", "null"], "items": {"type": "string"}, "description": "作るタスクのカテゴリ。null でワークスペースの既定値"},
            "urls": {"type": "array", "items": {"type": "string"},
                     "description": "作るタスクに付ける URL。渡すと URL がこの並びになる（足すだけなら add_urls）"},
            "add_urls": {"type": "array", "items": {"type": "string"}, "description": "今の URL の後ろに足す URL。同じものは足さない"},
            "remove_urls": {"type": "array", "items": {"type": "string"}, "description": "外す URL"},
            "linked_workspace_ids": {"type": ["array", "null"], "items": {"type": "string"},
                                     "description": "作るタスクを、入れるワークスペースのほかにも出すワークスペース（link_task と同じ）。null でワークスペースの既定値"},
            "subtasks": {"type": "array", "items": {"type": "string"}, "description": format!("作るタスクのサブタスクのタイトル（{} 件まで）。渡すとこの並びになる", malakt_core::schedule::MAX_SUBTASKS)},
        }),
        extra,
    )
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
            description: "タスク 1 件の内容と、コメント、サブタスク、添付、変更履歴を返す。",
            schema: object(json!({"id": {"type": "string"}}), &["id"]),
        },
        ToolDef {
            name: "get_workspace",
            description: "ワークスペース 1 つの中身を返す。上からの道筋、サブワークスペース（入れ子の全部）と\
                          それぞれのタスクの数、サブワークスペースも含めたタスクの一覧。\
                          多いときはタスクの一部だけを返す（tasks_total と tasks_shown を見る）。",
            schema: object(
                json!({
                    "id": {"type": "string"},
                    "include_done": {"type": "boolean", "description": "完了・中止のタスクも一覧に含める（標準は含めない）"},
                }),
                &["id"],
            ),
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
            name: "set_assignee_done",
            description: "担当者 1 人の「自分の分」の完了を付ける・外す。自分の分もほかの人の分も変えられる。\
                          全員の分がそろうとタスクが完了になる。完了したタスクで誰かの分を外すと、タスクは進行中に戻る。",
            schema: object(
                json!({
                    "task_id": {"type": "string"},
                    "person": {"type": "string", "description": "タスクの担当者の ID か名前（敬称は付けない）"},
                    "done": {"type": "boolean", "description": "true で完了にする、false で完了を外す"},
                }),
                &["task_id", "person", "done"],
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
                json!({
                    "task_id": {"type": "string"},
                    "body": {"type": "string"},
                    "reply_to": {"type": "string", "description": "返信するレビューのスレッドのルートのコメントの ID"},
                    "review": {"type": "boolean", "description": "レビューコメント（返信と解決ができるスレッド）にする"},
                }),
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
            name: "update_workspace",
            description: "ワークスペースの名前を変える、別のワークスペースの中へ移す、並び順を変える。parent_id が null なら、いちばん上に出す。index は、同じ親の中で何番目に置くか（0 が先頭。自分を除いて数え、多すぎると末尾）。渡した項目だけが変わる。",
            schema: object(
                json!({
                    "id": {"type": "string"},
                    "name": {"type": "string"},
                    "parent_id": {"type": ["string", "null"]},
                    "index": {"type": "integer", "minimum": 0},
                }),
                &["id"],
            ),
        },
        ToolDef {
            name: "delete_workspace",
            description: "ワークスペースを、サブワークスペースとタスクごと削除する。元に戻せないので、実行の前にユーザーに確認が出る。",
            schema: object(json!({"id": {"type": "string"}}), &["id"]),
        },
        ToolDef {
            name: "update_person",
            description: "担当者の名前とメモを変える。workspace_ids で使うワークスペースを決める（空の配列で、すべてのワークスペースで使う）。渡した項目だけが変わる。",
            schema: object(
                json!({
                    "id": {"type": "string"},
                    "name": {"type": "string"},
                    "note": {"type": "string"},
                    "workspace_ids": {"type": "array", "items": {"type": "string"}},
                }),
                &["id"],
            ),
        },
        ToolDef {
            name: "delete_person",
            description: "担当者を削除する（担当から外れるだけで、タスクは残る）。元に戻せないので、実行の前にユーザーに確認が出る。",
            schema: object(json!({"id": {"type": "string"}}), &["id"]),
        },
        ToolDef {
            name: "update_comment",
            description: "コメントを書き直す、ピン留めする・外す、レビューのスレッドを解決済みにする・戻す。渡した項目だけが変わる。",
            schema: object(
                json!({
                    "id": {"type": "string"},
                    "body": {"type": "string"},
                    "pinned": {"type": "boolean"},
                    "resolved": {"type": "boolean", "description": "レビューのスレッドのルートのコメントだけ"},
                }),
                &["id"],
            ),
        },
        ToolDef {
            name: "delete_comment",
            description: "コメントを削除する。元に戻せないので、実行の前にユーザーに確認が出る。",
            schema: object(json!({"id": {"type": "string"}}), &["id"]),
        },
        ToolDef {
            name: "link_task",
            description: "タスクを、入っているワークスペースのほかのワークスペースにも出す（同期する）。linked が false なら、そのワークスペースから外す。",
            schema: object(
                json!({
                    "id": {"type": "string"},
                    "workspace_id": {"type": "string"},
                    "linked": {"type": "boolean"},
                }),
                &["id", "workspace_id", "linked"],
            ),
        },
        ToolDef {
            name: "reorder_tasks",
            description: "タスクを手動の順（並び順「手動の順」）で、渡した ID の順に並べる。",
            schema: object(
                json!({"ids": {"type": "array", "items": {"type": "string"}}}),
                &["ids"],
            ),
        },
        ToolDef {
            name: "add_attachment",
            description: "ユーザーの PC のファイルをタスクに添付する。path はファイルの絶対パス（先頭の ~ はホーム）。1 ファイル 10 MiB まで。",
            schema: object(
                json!({"task_id": {"type": "string"}, "path": {"type": "string"}}),
                &["task_id", "path"],
            ),
        },
        ToolDef {
            name: "delete_attachment",
            description: "添付を削除する。元に戻せないので、実行の前にユーザーに確認が出る。",
            schema: object(json!({"id": {"type": "string"}}), &["id"]),
        },
        ToolDef {
            name: "issue_ticket",
            description: "タスクの完了チケット（ほかの人が malakt-mahoz redeem で完了を報告するための文字列）を発行して返す。person_id はだれの分として報告するか（省くと自分）。",
            schema: object(
                json!({"task_id": {"type": "string"}, "person_id": {"type": "string"}}),
                &["task_id"],
            ),
        },
        ToolDef {
            name: "revoke_tickets",
            description: "タスクの、まだ使われていない完了チケットをすべて取り消す。元に戻せないので、実行の前にユーザーに確認が出る。",
            schema: object(json!({"task_id": {"type": "string"}}), &["task_id"]),
        },
        ToolDef {
            name: "list_categories",
            description: "タスクに使われているカテゴリの一覧を返す。",
            schema: object(json!({}), &[]),
        },
        ToolDef {
            name: "list_notices",
            description: "通知（ベルの一覧）を新しい順に返す。",
            schema: object(
                json!({"limit": {"type": "integer", "minimum": 1, "maximum": 200}}),
                &[],
            ),
        },
        ToolDef {
            name: "mark_notices_read",
            description: "通知を既読にする。ids を省くと、すべて既読にする。",
            schema: object(
                json!({"ids": {"type": "array", "items": {"type": "string"}}}),
                &[],
            ),
        },
        ToolDef {
            name: "get_workspace_settings",
            description: "ワークスペースの基準日と、タスクを追加したときに入る値（既定値）を返す。",
            schema: object(json!({"id": {"type": "string"}}), &["id"]),
        },
        ToolDef {
            name: "update_workspace_settings",
            description: "ワークスペースの基準日と、タスクを追加したときに入る値を変える。渡した項目だけが変わる。",
            schema: object(
                json!({
                    "id": {"type": "string"},
                    "date": {"type": ["string", "null"], "description": "基準日（開催日など）YYYY-MM-DD。null でなくす"},
                    "assignee_ids": {"type": "array", "items": {"type": "string"}},
                    "due": {"type": "string", "enum": ["none", "relative", "fixed"],
                            "description": "期限の決め方。なし / 基準日からの日数 / 決まった日"},
                    "due_days": {"type": "integer", "description": "due が relative のとき、基準日からの日数（負は前）"},
                    "due_date": {"type": "string", "description": "due が fixed のときの日付 YYYY-MM-DD"},
                    "due_time": {"type": "string", "description": "期限の時刻 HH:MM。空で日付だけ"},
                    "priority": {"type": ["string", "null"], "enum": ["none", "high", "mid", "low", null],
                                 "description": "null で指定なし"},
                    "categories": {"type": "array", "items": {"type": "string"}},
                    "description": {"type": "string", "description": "説明の雛形"},
                    "link_workspace_ids": {"type": "array", "items": {"type": "string"},
                                           "description": "追加したタスクを一緒に表示するワークスペース"},
                    "terminal_dir": {"type": "string", "description": "このワークスペースのタスクでターミナルを開くフォルダー。空で消す"},
                }),
                &["id"],
            ),
        },
        ToolDef {
            name: "list_ai_rules",
            description: "AI のルール（ユーザーが決めた、AI がいつも従う指示）の一覧を返す。workspace_id を省くと、すべてのワークスペースで使うグローバルのルールと、ワークスペースごとのルールをすべて返す。ワークスペースのルールは、そのワークスペースとサブワークスペースで使う。",
            schema: object(json!({"workspace_id": {"type": "string"}}), &[]),
        },
        ToolDef {
            name: "create_ai_rule",
            description: "AI のルールを足す。workspace_id を省くとグローバル（すべてのワークスペース）、渡すとそのワークスペースとサブワークスペースで使う。",
            schema: object(rule_fields(json!({"workspace_id": {"type": "string"}})), &[]),
        },
        ToolDef {
            name: "update_ai_rule",
            description: "AI のルールを変える。渡した項目だけが変わる。",
            schema: object(rule_fields(json!({"id": {"type": "string"}})), &["id"]),
        },
        ToolDef {
            name: "delete_ai_rule",
            description: "AI のルールを削除する。使わないだけなら update_ai_rule で enabled を false にする。",
            schema: object(json!({"id": {"type": "string"}}), &["id"]),
        },
        ToolDef {
            name: "get_due_alerts",
            description: "期限をいつ知らせるか（OS の通知とリマインダー）を返す。task_id を渡すと、そのタスクで使うもの（own が null なら全体の標準のまま）。",
            schema: object(json!({"task_id": {"type": "string"}}), &[]),
        },
        ToolDef {
            name: "set_due_alerts",
            description: "期限をいつ知らせるかを変える。渡した項目だけが変わる。task_id を省くと全体の標準、渡すとそのタスクだけの上書き。timed_minutes は時刻つきの期限の何分前か（0 で期限ちょうど、例 [0, 5, 60, 1440]、[] で知らせない）。date_days は日付だけの期限の何日前か（0 で当日、1 で前日、[] で知らせない）。タスクで null を渡すと、その側は標準に合わせる。hour は日付だけの期限を知らせる時刻（0〜23 時、全体だけ）。",
            schema: object(
                json!({
                    "task_id": {"type": "string"},
                    "timed_minutes": {"type": ["array", "null"], "items": {"type": "integer", "minimum": 0, "maximum": malakt_core::alert::MAX_MINUTES}},
                    "date_days": {"type": ["array", "null"], "items": {"type": "integer", "minimum": 0, "maximum": malakt_core::alert::MAX_DAYS}},
                    "hour": {"type": "integer", "minimum": 0, "maximum": 23},
                }),
                &[],
            ),
        },
        ToolDef {
            name: "set_repeat",
            description: "タスクに繰り返しを付ける・変える・外す。繰り返しのあるタスクは、次の回の時刻になるとアプリがタスクを写して次の回を作り、繰り返しは新しい回へ移る（開始日と期限は回の日に合わせてずれる）。ai_instruction を付けると、回を作ったときにアプリの AI がその指示を実行して、結果をその回のタスクにコメントする（例: 週次の報告の下書き）。アプリが動いている間だけ動く。渡した項目だけが変わる。",
            schema: object(
                json!({
                    "task_id": {"type": "string"},
                    "repeat": {
                        "type": ["object", "null"],
                        "description": "null で繰り返しを外す",
                        "properties": {
                            "unit": {"type": "string", "enum": ["day", "week", "month", "year"]},
                            "every": {"type": "integer", "minimum": 1, "maximum": malakt_core::repeat::MAX_EVERY, "description": "何日・何週間・何か月・何年ごとか（標準は 1）"},
                            "weekdays": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 6}, "description": "unit が week のときの曜日。0 = 月 … 6 = 日。省くと起点の曜日"},
                            "from": {"type": "string", "description": "数え始めの日 YYYY-MM-DD。毎月・毎年の日付になる。省くとタスクの開始日か期限（なければ今日）"},
                            "time": {"type": "string", "description": "次の回を作る時刻 HH:MM。空で 0:00"},
                        },
                        "required": ["unit"],
                    },
                    "ai_instruction": {"type": "string", "description": "回を作ったときに AI に頼むこと。空で頼まない"},
                }),
                &["task_id"],
            ),
        },
        ToolDef {
            name: "list_schedules",
            description: "定時タスク（決まった時刻が来るたびに、雛形から新しいタスクを 1 件作る設定）の一覧を返す。workspace_id を省くと、ワークスペースの外に作るものと、ワークスペースごとのものをすべて返す。",
            schema: object(json!({"workspace_id": {"type": "string"}}), &[]),
        },
        ToolDef {
            name: "create_schedule",
            description: "定時タスクを足す。時刻が来るたびに、そのワークスペース（workspace_id を省くとワークスペースの外）に新しいタスクを 1 件作る。雛形にはタスクと同じ項目（説明・状態・優先度・担当と役・開始日・期限・通知・進捗・カテゴリ・URL・親タスク・ほかのワークスペース・サブタスク）を決められ、決めていない項目にはワークスペースの既定値が入る。開始日と期限は作った日の何日後か（start_days・due_days）で決める。添付は add_schedule_attachment。ai_instruction を付けると、作ったタスクでアプリの AI がその指示を実行して、結果をそのタスクにコメントする（例: 週次定例の報告の下書き）。前のタスクは書き換えない。1 つのタスクを毎回写すなら set_repeat。アプリが動いている間だけ動く。schedule を省くと今日の曜日の毎週 9:00。",
            schema: object(schedule_fields(json!({"workspace_id": {"type": "string"}})), &["title"]),
        },
        ToolDef {
            name: "update_schedule",
            description: "定時タスクを変える。渡した項目だけが変わる（作るタスクの項目も同じ）。schedule を変えたときと、enabled を true にしたときは、次の時刻を今から決め直す。",
            schema: object(schedule_fields(json!({"id": {"type": "string"}})), &["id"]),
        },
        ToolDef {
            name: "add_schedule_attachment",
            description: "ユーザーの PC のファイルを定時タスクの雛形に添付する。作るタスクには毎回この添付が付く。path はファイルの絶対パス（先頭の ~ はホーム）。1 ファイル 10 MiB まで。",
            schema: object(
                json!({"schedule_id": {"type": "string"}, "path": {"type": "string"}}),
                &["schedule_id", "path"],
            ),
        },
        ToolDef {
            name: "delete_schedule_attachment",
            description: "定時タスクの雛形から添付を外す。作ったタスクの添付は残る。",
            schema: object(json!({"id": {"type": "string"}}), &["id"]),
        },
        ToolDef {
            name: "delete_schedule",
            description: "定時タスクを削除する。作ったタスクは残る。止めるだけなら update_schedule で enabled を false にする。",
            schema: object(json!({"id": {"type": "string"}}), &["id"]),
        },
        ToolDef {
            name: "get_done_notices",
            description: "ほかの人が完了チケットで完了を報告したときに、通知を作るかの設定を返す。",
            schema: object(json!({}), &[]),
        },
        ToolDef {
            name: "set_done_notices",
            description: "ほかの人が完了を報告したときに、通知を作るかを変える。渡した項目だけが変わる。\
                          enabled は全体のオン・オフ。when は each（1 人が自分の分を完了にするたび）か finished（タスクが完了したときだけ）。\
                          roles は、完了にした人がこのどれかの役なら知らせる（R・A・C・I、1 つ以上）。\
                          muted_people は知らせない担当者（ID か名前）、muted_workspaces は知らせないワークスペースの ID（サブワークスペースも知らせない）。どちらも渡すとその並びになり、[] で全員・全部を知らせる。",
            schema: object(
                json!({
                    "enabled": {"type": "boolean"},
                    "when": {"type": "string", "enum": ["each", "finished"]},
                    "roles": {"type": "array", "items": {"type": "string", "enum": ["R", "A", "C", "I"]}},
                    "muted_people": {"type": "array", "items": {"type": "string"}},
                    "muted_workspaces": {"type": "array", "items": {"type": "string"}},
                }),
                &[],
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
            description: "ユーザーの PC でコマンドを 1 つ実行し、終了コードと出力を返す。動くシェル（PowerShell、bash、WSL など）は、ユーザーのメッセージの最後にアプリが書く。そのシェルの書き方で書く。実行の前にユーザーに確認が出る。",
            schema: object(json!({"command": {"type": "string"}}), &["command"]),
        },
    ]
}

/// 実行の前にユーザーの確認が要るツールか。
pub fn needs_confirmation(name: &str) -> bool {
    matches!(
        name,
        "delete_task"
            | "delete_workspace"
            | "delete_person"
            | "delete_comment"
            | "delete_attachment"
            | "revoke_tickets"
            | RUN_SHELL
    )
}

/// データベースを書き換えるツール。1 回の呼び出しを、元に戻す 1 つの段にする。
pub const WRITES: &[&str] = &[
    "create_task",
    "update_task",
    "set_assignee",
    "set_assignee_done",
    "move_task",
    "delete_task",
    "add_comment",
    "create_workspace",
    "create_person",
    "update_workspace",
    "delete_workspace",
    "update_person",
    "delete_person",
    "update_comment",
    "delete_comment",
    "link_task",
    "reorder_tasks",
    "add_attachment",
    "delete_attachment",
    "issue_ticket",
    "revoke_tickets",
    "mark_notices_read",
    "update_workspace_settings",
    "set_done_notices",
    "create_ai_rule",
    "update_ai_rule",
    "delete_ai_rule",
    "set_repeat",
    "create_schedule",
    "update_schedule",
    "add_schedule_attachment",
    "delete_schedule_attachment",
    "delete_schedule",
];

/// 確認に出す文。`shell` は、[`RUN_SHELL`] のコマンドが動くシェル。
pub fn confirmation_text(store: &Store, name: &str, args: &Value, shell: &Shell) -> String {
    match name {
        "delete_task" => {
            let id = args.get("id").and_then(Value::as_str).unwrap_or_default();
            match store.task(id) {
                Ok(task) => format!("タスク「{}」を削除します。元に戻せません。", task.title),
                Err(_) => format!("タスク（ID: {id}）を削除します。元に戻せません。"),
            }
        }
        "delete_workspace" => {
            let id = args.get("id").and_then(Value::as_str).unwrap_or_default();
            match store.workspace(id) {
                Ok(w) => format!(
                    "ワークスペース「{}」を、サブワークスペースとタスクごと削除します。元に戻せません。",
                    w.name
                ),
                Err(_) => format!("ワークスペース（ID: {id}）を削除します。元に戻せません。"),
            }
        }
        "delete_person" => {
            let id = args.get("id").and_then(Value::as_str).unwrap_or_default();
            match person_by_id(store, id) {
                Ok(p) => format!("担当者「{}」を削除します。元に戻せません。", p.name),
                Err(_) => format!("担当者（ID: {id}）を削除します。元に戻せません。"),
            }
        }
        "delete_comment" => {
            let id = args.get("id").and_then(Value::as_str).unwrap_or_default();
            match store.comment(id) {
                Ok(c) => format!(
                    "コメント「{}」を削除します。元に戻せません。",
                    short(&c.body, 40)
                ),
                Err(_) => format!("コメント（ID: {id}）を削除します。元に戻せません。"),
            }
        }
        "delete_attachment" => {
            let id = args.get("id").and_then(Value::as_str).unwrap_or_default();
            match store.attachment(id) {
                Ok(a) => format!("添付「{}」を削除します。元に戻せません。", a.name),
                Err(_) => format!("添付（ID: {id}）を削除します。元に戻せません。"),
            }
        }
        "revoke_tickets" => {
            let id = args
                .get("task_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match store.task(id) {
                Ok(t) => format!(
                    "タスク「{}」の、使われていない完了チケットをすべて取り消します。元に戻せません。",
                    t.title
                ),
                Err(_) => format!("タスク（ID: {id}）の完了チケットを取り消します。元に戻せません。"),
            }
        }
        RUN_SHELL => {
            let command = args
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default();
            format!("次のコマンドを {} で実行します。\n{command}", shell.label())
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

fn parse_priority(v: &str) -> Result<u8, String> {
    Ok(match v {
        "none" => Priority::None,
        "high" => Priority::High,
        "mid" => Priority::Mid,
        "low" => Priority::Low,
        other => return Err(format!("priority が正しくありません: {other}")),
    }
    .value())
}

/// 文字列の配列。
fn strings(v: &Value, key: &str) -> Result<Vec<String>, String> {
    v.as_array()
        .and_then(|a| a.iter().map(|s| s.as_str().map(str::to_owned)).collect())
        .ok_or_else(|| format!("{key} が正しくありません: {v}"))
}

/// 先頭の `max` 文字。長ければ「…」を付ける。
fn short(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or_default();
    let cut: String = line.chars().take(max).collect();
    if cut.chars().count() < text.chars().count() {
        format!("{cut}…")
    } else {
        cut
    }
}

fn person_by_id(store: &Store, id: &str) -> Result<Person, String> {
    store
        .people()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("担当者が見つかりません: {id}"))
}

fn person_json(p: &Person) -> Value {
    json!({"id": p.id, "name": p.name, "note": p.note, "is_self": p.is_self, "workspace_ids": p.workspace_ids})
}

fn workspace_json(w: &Workspace) -> Value {
    json!({"id": w.id, "name": w.name, "parent_id": w.parent_id, "icon": w.icon, "color": w.color, "date": w.date})
}

fn rule_json(r: &AiRule) -> Value {
    json!({
        "id": r.id,
        "workspace_id": r.workspace_id,
        "name": r.name,
        "body": r.body,
        "enabled": r.enabled,
        "apply": r.apply.as_str(),
    })
}

/// ルールの書き換え。渡された項目だけを持つ。
fn rule_patch(args: &Value) -> Result<RulePatch, String> {
    let apply = match optional(args, "apply") {
        None => None,
        Some(text) => {
            Some(Apply::parse(text).ok_or_else(|| format!("apply が正しくありません: {text}"))?)
        }
    };
    let enabled = match args.get("enabled") {
        None => None,
        Some(Value::Bool(on)) => Some(*on),
        Some(other) => return Err(format!("enabled が正しくありません: {other}")),
    };
    Ok(RulePatch {
        name: optional(args, "name").map(str::to_owned),
        body: optional(args, "body").map(str::to_owned),
        enabled,
        apply,
    })
}

fn comment_json(c: &Comment) -> Value {
    json!({
        "id": c.id,
        "author_id": c.author_id,
        "body": c.body,
        "pinned": c.pinned,
        "review": c.review,
        "reply_to": c.reply_to,
        "resolved": c.resolved_at.is_some(),
        "status": c.status.map(Status::as_str),
        "at": malakt_core::view::format_datetime(c.created_at),
    })
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
        draft.priority = parse_priority(v)?;
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
            *slot = strings(v, key)?;
        }
    }
    edit_urls(&mut draft.urls, args)
}

/// `urls`（置き換え）・`add_urls`・`remove_urls` を URL の並びに当てる。
fn edit_urls(urls: &mut Vec<String>, args: &Value) -> Result<(), String> {
    if let Some(v) = args.get("urls") {
        *urls = strings(v, "urls")?;
    }
    // URL は書いた形（https:// を省いたものなど）でも、保存した形と同じものとして扱う
    let same = |a: &str, b: &str| {
        let clean = |u: &str| malakt_core::weblink::clean(u).unwrap_or_else(|| u.trim().to_owned());
        clean(a) == clean(b)
    };
    if let Some(v) = args.get("add_urls") {
        for url in strings(v, "add_urls")? {
            if !urls.iter().any(|u| same(u, &url)) {
                urls.push(url);
            }
        }
    }
    if let Some(v) = args.get("remove_urls") {
        let remove = strings(v, "remove_urls")?;
        let missing: Vec<&String> = remove
            .iter()
            .filter(|r| !urls.iter().any(|u| same(u, r)))
            .collect();
        if !missing.is_empty() {
            let names: Vec<String> = missing.iter().map(|m| format!("「{m}」")).collect();
            return Err(format!("タスクに付いていない URL です: {}", names.join("")));
        }
        urls.retain(|u| !remove.iter().any(|r| same(u, r)));
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
        "linked_workspace_ids": task.linked_workspace_ids,
        "parent_id": task.parent_id,
        "assignee_ids": task.assignee_ids,
        "assignees": task.assignee_ids.iter().map(|id| json!({
            "person_id": id,
            "roles": role_codes(task.roles_of(id).unwrap_or_default()),
            "done": task.is_done_by(id),
        })).collect::<Vec<_>>(),
        "categories": task.categories,
        "urls": task.urls,
    })
}

/// 文字数で切る。切ったときは末尾に … を付ける。
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut out: String = text.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// ワークスペース 1 つの中身（`get_workspace`）。AI チャットに添えたワークスペースにも使う。
/// タスクは `WORKSPACE_TASKS` 件と `WORKSPACE_CHARS` 文字までにして、残りは件数だけを返す。
pub fn workspace_context(store: &Store, id: &str, include_done: bool) -> Result<Value, String> {
    let core = |e: malakt_core::Error| e.to_string();
    let path = store.workspace_path(id).map_err(core)?;
    let all = store.workspaces().map_err(core)?;
    // サブワークスペースは、左のメニューと同じ並び（親のすぐ後ろに子）
    let mut children: std::collections::HashMap<&str, Vec<&Workspace>> = Default::default();
    for w in &all {
        if let Some(parent) = w.parent_id.as_deref() {
            children.entry(parent).or_default().push(w);
        }
    }
    for list in children.values_mut() {
        list.sort_by_key(|w| (w.position, w.id.clone()));
    }
    let mut subs: Vec<&Workspace> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = [id].into();
    let mut stack: Vec<&Workspace> = children
        .get(id)
        .into_iter()
        .flatten()
        .rev()
        .copied()
        .collect();
    while let Some(w) = stack.pop() {
        // 壊れたデータで輪になっていても止まる
        if !seen.insert(&w.id) {
            continue;
        }
        subs.push(w);
        stack.extend(children.get(w.id.as_str()).into_iter().flatten().rev());
    }
    let tasks = store
        .tasks(
            Scope::Workspace(id),
            &TaskFilter {
                include_descendants: true,
                open_only: false,
                assignee_id: None,
                query: None,
            },
        )
        .map_err(core)?;
    let shows = |task: &Task, workspace: &str| {
        task.workspace_id.as_deref() == Some(workspace)
            || task.linked_workspace_ids.iter().any(|w| w == workspace)
    };
    let counts = |workspace: &str| {
        let here = tasks.iter().filter(|t| shows(t, workspace));
        let open = here.clone().filter(|t| t.status.is_open()).count();
        (open, here.count() - open)
    };
    let sub_rows: Vec<Value> = subs
        .iter()
        .take(WORKSPACE_SUBS)
        .map(|w| {
            let (open, done) = counts(&w.id);
            json!({
                "id": w.id,
                "name": w.name,
                "parent_id": w.parent_id,
                "open_tasks": open,
                "done_tasks": done,
            })
        })
        .collect();
    let picked: Vec<&Task> = tasks
        .iter()
        .filter(|t| include_done || t.status.is_open())
        .collect();
    let open = tasks.iter().filter(|t| t.status.is_open()).count();
    let me = path.last().map(workspace_json).unwrap_or(Value::Null);
    let mut out = json!({
        "workspace": me,
        "path": path.iter().map(|w| w.name.as_str()).collect::<Vec<_>>().join(" / "),
        "sub_workspaces_total": subs.len(),
        "sub_workspaces": sub_rows,
        "open_tasks": open,
        "done_tasks": tasks.len() - open,
        "tasks_total": picked.len(),
        "tasks_shown": 0,
        "tasks": [],
    });
    // 文字数に収まるだけのタスクを載せる
    let mut budget = WORKSPACE_CHARS.saturating_sub(out.to_string().chars().count() + 200);
    let mut rows = Vec::new();
    for task in picked.iter().take(WORKSPACE_TASKS) {
        let mut row = task_row(task);
        if !task.description.is_empty() {
            row["description"] = json!(clip(&task.description, WORKSPACE_DESCRIPTION));
        }
        let size = row.to_string().chars().count() + 1;
        if size > budget {
            break;
        }
        budget -= size;
        rows.push(row);
    }
    out["tasks_shown"] = json!(rows.len());
    if rows.len() < picked.len() {
        out["note"] = json!(
            "タスクが多いので、一部だけを載せています。残りは list_tasks に workspace_id と query を渡して読めます"
        );
    }
    out["tasks"] = Value::Array(rows);
    Ok(out)
}

/// 一覧では、説明を省いて短くする。
/// ほかの人の完了を知らせるかの設定。担当者とワークスペースは名前も添える。
fn done_notices_json(store: &Store) -> Result<Value, String> {
    let core = |e: malakt_core::Error| e.to_string();
    let notices = store.done_notices().map_err(core)?;
    let people = store.people().map_err(core)?;
    let workspaces = store.workspaces().map_err(core)?;
    let mut out = notices.to_json();
    out["muted_people"] = notices
        .muted_people
        .iter()
        .filter_map(|id| people.iter().find(|p| &p.id == id))
        .map(|p| json!({"id": p.id, "name": p.name}))
        .collect();
    out["muted_workspaces"] = notices
        .muted_workspaces
        .iter()
        .filter_map(|id| workspaces.iter().find(|w| &w.id == id))
        .map(|w| json!({"id": w.id, "name": w.name}))
        .collect();
    Ok(out)
}

/// 期限をいつ知らせるか。`task_id` があれば、そのタスクの上書きも。
fn due_alerts_json(store: &Store, task_id: Option<&str>) -> Result<Value, String> {
    use malakt_core::alert;
    let core = |e: malakt_core::Error| e.to_string();
    let standard = store.due_alert_standard().map_err(core)?;
    let text = |timed: &[u32], date: &[u32]| {
        json!({
            "timed": alert::summary(timed, alert::timed_label),
            "date": alert::summary(date, alert::date_label),
        })
    };
    let mut out = json!({
        "standard": {
            "timed_minutes": standard.timed,
            "date_days": standard.date,
            "hour": standard.hour,
            "text": text(&standard.timed, &standard.date),
        },
    });
    if let Some(id) = task_id {
        store.task(id).map_err(core)?;
        let own = store.task_alerts(id).map_err(core)?;
        let timed = own.timed.clone().unwrap_or_else(|| standard.timed.clone());
        let date = own.date.clone().unwrap_or_else(|| standard.date.clone());
        out["task_id"] = json!(id);
        out["own"] = if own.is_standard() {
            Value::Null
        } else {
            json!({"timed_minutes": own.timed, "date_days": own.date})
        };
        out["used"] = json!({
            "timed_minutes": timed,
            "date_days": date,
            "text": text(&timed, &date),
        });
    }
    Ok(out)
}

/// タスクの繰り返し。なければ null。
/// 繰り返しの決まり（`set_repeat` の repeat、定時タスクの schedule）を読む。`from` を省いたら `default_from`。
fn parse_rule(
    v: &Value,
    key: &str,
    default_from: &str,
) -> Result<malakt_core::repeat::Rule, String> {
    use malakt_core::repeat::{Rule, Unit};
    let unit = v
        .get("unit")
        .and_then(Value::as_str)
        .and_then(Unit::parse)
        .ok_or_else(|| format!("{key}.unit は day・week・month・year のどれかです"))?;
    let every = match v.get("every") {
        None | Some(Value::Null) => 1,
        Some(n) => n
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| format!("{key}.every は 1 以上の整数です: {n}"))?,
    };
    let weekdays = match v.get("weekdays") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(days)) => days
            .iter()
            .map(|d| {
                d.as_u64()
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or_else(|| format!("{key}.weekdays は 0〜6 です: {d}"))
            })
            .collect::<Result<_, String>>()?,
        Some(d) => return Err(format!("{key}.weekdays は整数の並びです: {d}")),
    };
    let from = match v.get("from").and_then(Value::as_str) {
        Some(from) if !from.trim().is_empty() => from.trim().to_owned(),
        _ => default_from.to_owned(),
    };
    let time = v
        .get("time")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    Rule {
        unit,
        every,
        weekdays,
        from,
        time,
    }
    .clean()
    .map_err(|e| e.to_string())
}

fn rule_value(rule: &malakt_core::repeat::Rule) -> Value {
    json!({
        "unit": rule.unit.as_str(),
        "every": rule.every,
        "weekdays": rule.weekdays,
        "from": rule.from,
        "time": rule.time,
        "text": rule.label(),
    })
}

fn schedule_json(store: &Store, s: &malakt_core::schedule::Schedule) -> Result<Value, String> {
    let t = &s.task;
    let files = store
        .schedule_files(&s.id)
        .map_err(|e| e.to_string())?
        .iter()
        .map(|f| json!({"id": f.id, "name": f.name, "size": f.size}))
        .collect::<Vec<_>>();
    Ok(json!({
        "id": s.id,
        "workspace_id": s.workspace_id,
        "title": s.title,
        "schedule": rule_value(&s.rule),
        "ai_instruction": s.ai,
        "enabled": s.enabled,
        "next_at": s.next_at,
        "next_text": malakt_core::repeat::next_label(&s.next_at),
        "description": t.description,
        "status": t.status.as_str(),
        "priority": t.priority.map(priority_name),
        "start_days": t.start_days,
        "start_time": t.start_time,
        "due_days": s.due_days,
        "due_time": s.due_time,
        "due_alerts": if t.alerts_timed.is_none() && t.alerts_date.is_none() {
            Value::Null
        } else {
            json!({"timed_minutes": t.alerts_timed, "date_days": t.alerts_date})
        },
        "percent": t.percent,
        "parent_id": t.parent_id,
        "assignee_ids": s.assignee_ids,
        "assignees": s.assignee_ids.iter().map(|id| json!({
            "person_id": id,
            "roles": role_codes(s.roles_of(id).unwrap_or_default()),
        })).collect::<Vec<_>>(),
        "categories": t.categories,
        "urls": t.urls,
        "linked_workspace_ids": t.linked_workspace_ids,
        "subtasks": t.subtasks,
        "attachments": files,
    }))
}

/// null なら `Some(None)`、文字列なら `Some(Some(…))`、なければ `None`。
fn nullable_string(args: &Value, key: &str) -> Result<Option<Option<String>>, String> {
    match args.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(t)) => Ok(Some(Some(t.clone()))),
        Some(other) => Err(format!("{key} は文字列か null です: {other}")),
    }
}

/// null なら `Some(None)`、整数なら `Some(Some(…))`、なければ `None`。
fn nullable_days(args: &Value, key: &str) -> Result<Option<Option<i64>>, String> {
    match args.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(n) => {
            Ok(Some(Some(n.as_i64().ok_or_else(|| {
                format!("{key} は 0 以上の整数か null です: {n}")
            })?)))
        }
    }
}

/// 時刻（HH:MM）。null か空で「なし」。
fn nullable_time(args: &Value, key: &str) -> Result<Option<Option<String>>, String> {
    Ok(nullable_string(args, key)?
        .map(|t| t.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())))
}

/// 雛形の、作るタスクの項目のうち、渡されたものだけを `t` に重ねる。何か渡されていれば true。
fn apply_template(t: &mut malakt_core::schedule::Template, args: &Value) -> Result<bool, String> {
    const KEYS: &[&str] = &[
        "description",
        "status",
        "priority",
        "start_days",
        "start_time",
        "due_alerts",
        "percent",
        "parent_id",
        "categories",
        "urls",
        "add_urls",
        "remove_urls",
        "linked_workspace_ids",
        "subtasks",
    ];
    if !KEYS.iter().any(|k| args.get(*k).is_some()) {
        return Ok(false);
    }
    if let Some(v) = nullable_string(args, "description")? {
        t.description = v;
    }
    if let Some(v) = optional(args, "status") {
        t.status = Status::parse(v).ok_or_else(|| format!("status が正しくありません: {v}"))?;
    }
    if let Some(v) = nullable_string(args, "priority")? {
        t.priority = v.as_deref().map(parse_priority).transpose()?;
    }
    if let Some(v) = nullable_days(args, "start_days")? {
        t.start_days = v;
    }
    if let Some(v) = nullable_time(args, "start_time")? {
        t.start_time = v;
    }
    if let Some(v) = args.get("percent") {
        let n = v
            .as_u64()
            .filter(|n| *n <= 100)
            .ok_or_else(|| format!("percent が正しくありません: {v}"))?;
        t.percent = n as u8;
    }
    if let Some(v) = nullable_string(args, "parent_id")? {
        t.parent_id = v.filter(|p| !p.is_empty());
    }
    match args.get("categories") {
        None => {}
        Some(Value::Null) => t.categories = None,
        Some(v) => t.categories = Some(strings(v, "categories")?),
    }
    match args.get("linked_workspace_ids") {
        None => {}
        Some(Value::Null) => t.linked_workspace_ids = None,
        Some(v) => t.linked_workspace_ids = Some(strings(v, "linked_workspace_ids")?),
    }
    if let Some(v) = args.get("subtasks") {
        t.subtasks = strings(v, "subtasks")?;
    }
    edit_urls(&mut t.urls, args)?;
    match args.get("due_alerts") {
        None => {}
        Some(Value::Null) => {
            t.alerts_timed = None;
            t.alerts_date = None;
        }
        Some(Value::Object(alerts)) => {
            let list = |key: &str| -> Result<Option<Option<Vec<u32>>>, String> {
                match alerts.get(key) {
                    None => Ok(None),
                    Some(Value::Null) => Ok(Some(None)),
                    Some(Value::Array(items)) => items
                        .iter()
                        .map(|v| {
                            v.as_u64()
                                .and_then(|n| u32::try_from(n).ok())
                                .ok_or_else(|| {
                                    format!("due_alerts.{key} は 0 以上の整数の並びです: {v}")
                                })
                        })
                        .collect::<Result<Vec<u32>, String>>()
                        .map(|v| Some(Some(v))),
                    Some(v) => Err(format!("due_alerts.{key} は整数の並びです: {v}")),
                }
            };
            if let Some(v) = list("timed_minutes")? {
                t.alerts_timed = v;
            }
            if let Some(v) = list("date_days")? {
                t.alerts_date = v;
            }
        }
        Some(other) => return Err(format!("due_alerts が正しくありません: {other}")),
    }
    Ok(true)
}

/// 定時タスクの書き換え。渡された項目だけを持つ。`from` は決まりの起点を省いたときの日。
/// `old` は今の定時タスク（足すときは `None`）、`root` は入れるワークスペースのいちばん上。
/// 担当者の登録はしない（名前だけの人は `Plan` に残す）。
fn schedule_patch(
    store: &Store,
    args: &Value,
    from: &str,
    old: Option<&malakt_core::schedule::Schedule>,
    root: Option<&str>,
) -> Result<(malakt_core::schedule::SchedulePatch, Option<Plan>), String> {
    let rule = match args.get("schedule") {
        None | Some(Value::Null) => None,
        Some(v) => Some(parse_rule(v, "schedule", from)?),
    };
    let enabled = match args.get("enabled") {
        None => None,
        Some(Value::Bool(on)) => Some(*on),
        Some(other) => return Err(format!("enabled が正しくありません: {other}")),
    };
    let assignee_ids = match args.get("assignee_ids") {
        None => None,
        Some(v) => Some(strings(v, "assignee_ids")?),
    };
    let mut task = old.map(|s| s.task.clone()).unwrap_or_default();
    let touched = apply_template(&mut task, args)?;
    let plan = plan_assignees(store, args, root)?;
    Ok((
        malakt_core::schedule::SchedulePatch {
            title: optional(args, "title").map(str::to_owned),
            rule,
            ai: optional(args, "ai_instruction").map(str::to_owned),
            assignee_ids,
            due_days: nullable_days(args, "due_days")?,
            due_time: nullable_time(args, "due_time")?,
            enabled,
            task: touched.then_some(task),
        },
        plan,
    ))
}

/// `assignees` で決めた担当者を登録し、担当と役を `patch` に入れる。
fn settle_schedule_assignees(
    store: &mut Store,
    patch: &mut malakt_core::schedule::SchedulePatch,
    plan: Plan,
    old: Option<&malakt_core::schedule::Schedule>,
    workspace: Option<&str>,
) -> Result<(), String> {
    let mut ids = Vec::new();
    let mut roles = Vec::new();
    for (who, wanted) in plan {
        let id = settle_person(store, who, workspace)?;
        let current = old.and_then(|s| s.roles_of(&id));
        roles.push(wanted.or(current).unwrap_or_default());
        ids.push(id);
    }
    let mut task = patch
        .task
        .take()
        .or_else(|| old.map(|s| s.task.clone()))
        .unwrap_or_default();
    task.roles = roles;
    patch.assignee_ids = Some(ids);
    patch.task = Some(task);
    Ok(())
}

fn repeat_json(store: &Store, task_id: &str) -> Result<Value, String> {
    use malakt_core::repeat;
    let Some(r) = store.repeat(task_id).map_err(|e| e.to_string())? else {
        return Ok(Value::Null);
    };
    Ok(json!({
        "unit": r.rule.unit.as_str(),
        "every": r.rule.every,
        "weekdays": r.rule.weekdays,
        "from": r.rule.from,
        "time": r.rule.time,
        "text": r.rule.label(),
        "next_at": r.next_at,
        "next_text": repeat::next_label(&r.next_at),
        "ai_instruction": r.ai,
    }))
}

fn task_row(task: &Task) -> Value {
    let mut row = task_json(task);
    if let Some(map) = row.as_object_mut() {
        map.remove("description");
        map.remove("start");
        map.remove("percent");
    }
    row
}

/// 添付するファイルを読む。名前と中身。
fn read_upload(path: &str) -> Result<(String, Vec<u8>), String> {
    let path = expand_dir(path);
    let meta = std::fs::metadata(&path)
        .map_err(|e| format!("ファイルを読めません（{}）: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("ファイルではありません: {}", path.display()));
    }
    if meta.len() > malakt_core::MAX_ATTACHMENT_BYTES {
        return Err(format!(
            "添付できるのは 1 ファイル {} MiB までです（{} は {} MiB）",
            malakt_core::MAX_ATTACHMENT_BYTES / 1024 / 1024,
            path.display(),
            meta.len() / 1024 / 1024
        ));
    }
    let bytes = std::fs::read(&path)
        .map_err(|e| format!("ファイルを読めません（{}）: {e}", path.display()))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok((name, bytes))
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
            .map(workspace_json)
            .collect(),
        "list_people" => store
            .people()
            .map_err(core)?
            .iter()
            .map(person_json)
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
        "get_workspace" => workspace_context(
            store,
            required(args, "id")?,
            args.get("include_done").and_then(Value::as_bool) == Some(true),
        )?,
        "get_task" => {
            let id = required(args, "id")?;
            let mut task = task_json(&store.task(id).map_err(core)?);
            task["comments"] = store
                .comments(id)
                .map_err(core)?
                .iter()
                .map(comment_json)
                .collect();
            task["attachments"] = store
                .attachments(id)
                .map_err(core)?
                .iter()
                .map(|a| json!({"id": a.id, "name": a.name, "size": a.size}))
                .collect();
            // 変更履歴は新しいものから
            let changes = store.changes(id).map_err(core)?;
            task["changes"] = changes
                .iter()
                .rev()
                .take(CHANGES_LIMIT)
                .map(|c| {
                    json!({
                        "field": c.field,
                        "from": c.old_value,
                        "to": c.new_value,
                        "at": malakt_core::view::format_datetime(c.created_at),
                    })
                })
                .collect();
            task["active_tickets"] = json!(store.active_ticket_count(id).map_err(core)?);
            task["due_alerts"] = due_alerts_json(store, Some(id))?;
            task["repeat"] = repeat_json(store, id)?;
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
                        return Ok(
                            task_json(&store.update_task(&task.id, &draft).map_err(core)?)
                                .to_string(),
                        );
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
                task_json(
                    &store
                        .set_assignee_roles(&task.id, &id, roles)
                        .map_err(core)?,
                )
            }
        }
        "set_assignee_done" => {
            let task = store.task(required(args, "task_id")?).map_err(core)?;
            let done = match args.get("done") {
                Some(Value::Bool(b)) => *b,
                other => {
                    return Err(format!(
                        "done が正しくありません: {}",
                        other.unwrap_or(&Value::Null)
                    ))
                }
            };
            let key = required(args, "person")?;
            // 探すのはこのタスクの担当者だけ（担当でない人は、完了にできない）
            let assignees: Vec<Person> = store
                .people()
                .map_err(core)?
                .into_iter()
                .filter(|p| task.roles_of(&p.id).is_some())
                .collect();
            let id = match find_person(&assignees, key, None)? {
                Who::Known { id, .. } => id,
                Who::New(name) => {
                    return Err(format!("「{name}」はこのタスクの担当者ではありません"))
                }
            };
            task_json(&store.set_assignee_done(&task.id, &id, done).map_err(core)?)
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
            let new = malakt_core::NewComment {
                body: required(args, "body")?.to_owned(),
                review: args.get("review").and_then(Value::as_bool) == Some(true),
                reply_to: optional(args, "reply_to")
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned),
                ..Default::default()
            };
            let comment = store
                .add_comment_with(required(args, "task_id")?, author_id, &new)
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
                Some(workspace) => store.create_local_person(name, workspace),
                None => store.create_person(name, false),
            }
            .map_err(core)?;
            person_json(&p)
        }
        "update_workspace" => {
            let id = required(args, "id")?;
            let before = store.workspace(id).map_err(core)?;
            let name = optional(args, "name").map(str::trim);
            if name == Some("") {
                return Err("name が空です".into());
            }
            // 移す先を先に確かめて、名前だけ変わることがないようにする
            let parent = match args.get("parent_id") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(Value::String(s)) if s.is_empty() => Some(None),
                Some(Value::String(s)) => {
                    let inside = store.workspace_subtree_ids(id).map_err(core)?;
                    if inside.iter().any(|w| w == s) {
                        return Err("ワークスペースを、自分かその中へは移せません".into());
                    }
                    store.workspace(s).map_err(core)?;
                    Some(Some(s.as_str()))
                }
                Some(other) => return Err(format!("parent_id が正しくありません: {other}")),
            };
            let index = match args.get("index") {
                None => None,
                Some(v) => Some(
                    v.as_u64()
                        .ok_or_else(|| format!("index が正しくありません: {v}"))?
                        as usize,
                ),
            };
            if let Some(name) = name.filter(|n| *n != before.name) {
                store.rename_workspace(id, name).map_err(core)?;
            }
            match index {
                Some(index) => {
                    let parent = parent.unwrap_or(before.parent_id.as_deref());
                    store.place_workspace(id, parent, index).map_err(core)?;
                }
                None => {
                    if let Some(parent) = parent.filter(|p| *p != before.parent_id.as_deref()) {
                        store.move_workspace(id, parent).map_err(core)?;
                    }
                }
            }
            workspace_json(&store.workspace(id).map_err(core)?)
        }
        "delete_workspace" => {
            let id = required(args, "id")?;
            store.delete_workspace(id).map_err(core)?;
            json!({"deleted": id})
        }
        "update_person" => {
            let id = required(args, "id")?;
            let person = person_by_id(store, id)?;
            let name = optional(args, "name").unwrap_or(&person.name);
            let note = optional(args, "note").unwrap_or(&person.note);
            let workspaces = match args.get("workspace_ids") {
                None => None,
                Some(v) => Some(strings(v, "workspace_ids")?),
            };
            // 先に確かめて、名前だけ変わることがないようにする
            if let Some(ids) = &workspaces {
                if person.is_self && !ids.is_empty() {
                    return Err("「自分」は、すべてのワークスペースで使います".into());
                }
                for w in ids {
                    store.workspace(w).map_err(core)?;
                }
            }
            if name.trim().is_empty() {
                return Err("name が空です".into());
            }
            store.update_person(id, name, note).map_err(core)?;
            if let Some(ids) = workspaces {
                store.set_person_workspaces(id, &ids).map_err(core)?;
            }
            person_json(&person_by_id(store, id)?)
        }
        "delete_person" => {
            let id = required(args, "id")?;
            if person_by_id(store, id)?.is_self {
                return Err("「自分」は削除できません".into());
            }
            store.delete_person(id).map_err(core)?;
            json!({"deleted": id})
        }
        "update_comment" => {
            let id = required(args, "id")?;
            let comment = store.comment(id).map_err(core)?;
            let body = optional(args, "body");
            let flag = |key: &str| -> Result<Option<bool>, String> {
                match args.get(key) {
                    None => Ok(None),
                    Some(Value::Bool(b)) => Ok(Some(*b)),
                    Some(other) => Err(format!("{key} が正しくありません: {other}")),
                }
            };
            let (pinned, resolved) = (flag("pinned")?, flag("resolved")?);
            // 先に確かめて、一部だけ変わることがないようにする
            if body.is_some_and(|b| b.trim().is_empty()) {
                return Err("body が空です".into());
            }
            if resolved.is_some() && !comment.is_thread_root() {
                return Err("解決済みにできるのはレビューコメントだけです".into());
            }
            if let Some(body) = body.filter(|b| *b != comment.body) {
                store.update_comment(id, body).map_err(core)?;
            }
            if let Some(pinned) = pinned {
                store.set_comment_pinned(id, pinned).map_err(core)?;
            }
            if let Some(resolved) = resolved {
                store.set_comment_resolved(id, resolved).map_err(core)?;
            }
            comment_json(&store.comment(id).map_err(core)?)
        }
        "delete_comment" => {
            let id = required(args, "id")?;
            store.delete_comment(id).map_err(core)?;
            json!({"deleted": id})
        }
        "link_task" => {
            let id = required(args, "id")?;
            let workspace = required(args, "workspace_id")?;
            let linked = args
                .get("linked")
                .and_then(Value::as_bool)
                .ok_or("linked が必要です")?;
            let task = if linked {
                store.link_task(id, workspace)
            } else {
                store.unlink_task(id, workspace)
            }
            .map_err(core)?;
            task_json(&task)
        }
        "reorder_tasks" => {
            let ids = strings(args.get("ids").unwrap_or(&Value::Null), "ids")?;
            for id in &ids {
                store.task(id).map_err(core)?;
            }
            store.set_task_order(&ids).map_err(core)?;
            json!({"ordered": ids.len()})
        }
        "add_attachment" => {
            let task_id = required(args, "task_id")?;
            let (name, bytes) = read_upload(required(args, "path")?)?;
            let a = store
                .add_attachment(task_id, author_id, &name, &bytes)
                .map_err(core)?;
            json!({"id": a.id, "name": a.name, "size": a.size})
        }
        "delete_attachment" => {
            let id = required(args, "id")?;
            store.delete_attachment(id).map_err(core)?;
            json!({"deleted": id})
        }
        "issue_ticket" => {
            let task_id = required(args, "task_id")?;
            let person = optional(args, "person_id").filter(|id| !id.is_empty());
            let ticket = store.issue_ticket(task_id, person).map_err(core)?;
            json!({
                "ticket": ticket.secret,
                "person_id": ticket.person_id,
                "whole_task": ticket.whole_task,
                "usage": "malakt-mahoz redeem <ticket> --message \"完了報告\"",
            })
        }
        "revoke_tickets" => {
            let task_id = required(args, "task_id")?;
            json!({"revoked": store.revoke_tickets(task_id).map_err(core)?})
        }
        "list_categories" => json!(store.categories().map_err(core)?),
        "list_notices" => {
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .map_or(50, |n| n.clamp(1, 200) as usize);
            store
                .notices(limit)
                .map_err(core)?
                .iter()
                .map(|n| {
                    json!({
                        "id": n.id,
                        "text": n.headline(),
                        "task_id": n.task_id,
                        "excerpt": n.excerpt,
                        "unread": n.is_unread(),
                        "at": malakt_core::view::format_datetime(n.created_at),
                    })
                })
                .collect()
        }
        "mark_notices_read" => match args.get("ids") {
            None => json!({"read": store.mark_all_notices_read().map_err(core)?}),
            Some(v) => {
                let ids = strings(v, "ids")?;
                for id in &ids {
                    store.mark_notice_read(id).map_err(core)?;
                }
                json!({"read": ids.len()})
            }
        },
        "get_done_notices" => done_notices_json(store)?,
        "set_done_notices" => {
            let mut notices = store.done_notices().map_err(core)?;
            if let Some(v) = args.get("enabled") {
                notices.enabled = v
                    .as_bool()
                    .ok_or_else(|| format!("enabled が正しくありません: {v}"))?;
            }
            if let Some(v) = args.get("when") {
                notices.when = v
                    .as_str()
                    .and_then(malakt_core::done_notice::When::parse)
                    .ok_or_else(|| format!("when が正しくありません: {v}"))?;
            }
            if let Some(v) = args.get("roles") {
                notices.roles = parse_roles(v)?;
            }
            if let Some(v) = args.get("muted_people") {
                let people = store.people().map_err(core)?;
                let mut ids = Vec::new();
                for key in strings(v, "muted_people")? {
                    match find_person(&people, &key, None)? {
                        Who::Known { id, .. } => ids.push(id),
                        Who::New(name) => return Err(format!("「{name}」という担当者はいません")),
                    }
                }
                notices.muted_people = ids;
            }
            if let Some(v) = args.get("muted_workspaces") {
                notices.muted_workspaces = strings(v, "muted_workspaces")?;
            }
            store.set_done_notices(&notices).map_err(core)?;
            done_notices_json(store)?
        }
        "get_due_alerts" => due_alerts_json(store, optional(args, "task_id"))?,
        "set_repeat" => {
            use malakt_core::repeat;
            let id = required(args, "task_id")?;
            let task = store.task(id).map_err(core)?;
            let old = store.repeat(id).map_err(core)?;
            let now = malakt_core::view::now();
            // 先に全部の値を確かめ、1 つでも正しくなければ何も変えない
            let rule = match args.get("repeat") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(v) => {
                    // 変えるときは、今の起点のまま
                    let from = old
                        .as_ref()
                        .map_or_else(|| repeat::anchor(&task, &now), |r| r.rule.from.clone());
                    Some(Some(parse_rule(v, "repeat", &from)?))
                }
            };
            let ai = optional(args, "ai_instruction");
            if rule.is_none() && ai.is_none() {
                return Err("repeat か ai_instruction のどちらかを渡してください".into());
            }
            if let Some(text) = ai {
                if text.trim().chars().count() > repeat::MAX_AI_CHARS {
                    return Err(format!(
                        "AI への指示は {} 文字までです",
                        repeat::MAX_AI_CHARS
                    ));
                }
                let will_repeat = match &rule {
                    Some(rule) => rule.is_some(),
                    None => old.is_some(),
                };
                if !will_repeat && !text.trim().is_empty() {
                    return Err("AI への指示は、繰り返しのあるタスクにだけ付けられます".into());
                }
            }
            if let Some(rule) = &rule {
                store.set_repeat(id, rule.as_ref(), &now).map_err(core)?;
            }
            if let Some(text) = ai {
                if store.repeat(id).map_err(core)?.is_some() {
                    store.set_repeat_ai(id, text).map_err(core)?;
                }
            }
            json!({"task_id": id, "repeat": repeat_json(store, id)?})
        }
        "list_schedules" => {
            let json_of = |list: &[malakt_core::schedule::Schedule]| {
                list.iter()
                    .map(|s| schedule_json(store, s))
                    .collect::<Result<Vec<Value>, String>>()
            };
            match optional(args, "workspace_id").filter(|w| !w.is_empty()) {
                Some(id) => {
                    store.workspace(id).map_err(core)?;
                    Value::Array(json_of(&store.schedules(Some(id)).map_err(core)?)?)
                }
                None => {
                    let mut workspaces = Vec::new();
                    for w in store.workspaces().map_err(core)? {
                        let list = store.schedules(Some(&w.id)).map_err(core)?;
                        if !list.is_empty() {
                            workspaces.push(json!({
                                "id": w.id,
                                "name": w.name,
                                "schedules": json_of(&list)?,
                            }));
                        }
                    }
                    let outside = json_of(&store.schedules(None).map_err(core)?)?;
                    json!({"outside": outside, "workspaces": workspaces})
                }
            }
        }
        "create_schedule" => {
            let workspace = optional(args, "workspace_id").filter(|w| !w.is_empty());
            let now = malakt_core::view::now();
            let root = match workspace {
                Some(w) => Some(store.workspace_root(w).map_err(core)?),
                None => None,
            };
            // 先に全部の値を確かめ、1 つでも正しくなければ何も作らない
            let (mut patch, plan) = schedule_patch(
                store,
                args,
                malakt_core::date_of(&now),
                None,
                root.as_deref(),
            )?;
            let title = required(args, "title")?;
            if let Some(ids) = &patch.assignee_ids {
                let people = store.people().map_err(core)?;
                if let Some(id) = ids.iter().find(|id| !people.iter().any(|p| &p.id == *id)) {
                    return Err(format!("担当者 {id} が見つかりません"));
                }
            }
            if let Some(text) = &patch.ai {
                if text.trim().chars().count() > malakt_core::repeat::MAX_AI_CHARS {
                    return Err(format!(
                        "AI への指示は {} 文字までです",
                        malakt_core::repeat::MAX_AI_CHARS
                    ));
                }
            }
            for key in ["due_days", "start_days"] {
                let days = if key == "due_days" {
                    patch.due_days
                } else {
                    patch.task.as_ref().map(|t| t.start_days)
                };
                if let Some(Some(days)) = days {
                    if !(0..=malakt_core::schedule::MAX_DUE_DAYS).contains(&days) {
                        return Err(format!(
                            "{key} は 0 から {} です",
                            malakt_core::schedule::MAX_DUE_DAYS
                        ));
                    }
                }
            }
            if let Some(Some(time)) = &patch.due_time {
                if !malakt_core::is_valid_time(time) {
                    return Err(format!("due_time が正しくありません: {time}"));
                }
            }
            if let Some(task) = &patch.task {
                let count = patch.assignee_ids.as_ref().map_or(0, Vec::len);
                malakt_core::schedule::clean_template(task, count).map_err(core)?;
                if let Some(parent) = &task.parent_id {
                    store.task(parent).map_err(core)?;
                }
                for id in task.linked_workspace_ids.iter().flatten() {
                    store.workspace(id).map_err(core)?;
                }
            }
            let made = store
                .create_schedule(workspace, title, patch.rule.as_ref(), &now)
                .map_err(core)?;
            let result = (|| {
                if let Some(plan) = plan {
                    settle_schedule_assignees(store, &mut patch, plan, None, workspace)?;
                }
                let rest = malakt_core::schedule::SchedulePatch {
                    title: None,
                    rule: None,
                    ..patch
                };
                store.update_schedule(&made.id, &rest, &now).map_err(core)
            })();
            match result {
                Ok(s) => schedule_json(store, &s)?,
                Err(e) => {
                    // 半分だけの定時タスクを残さない
                    store.delete_schedule(&made.id).map_err(core)?;
                    return Err(e);
                }
            }
        }
        "update_schedule" => {
            let id = required(args, "id")?;
            let old = store.schedule(id).map_err(core)?;
            let now = malakt_core::view::now();
            let root = match &old.workspace_id {
                Some(w) => Some(store.workspace_root(w).map_err(core)?),
                None => None,
            };
            let (mut patch, plan) =
                schedule_patch(store, args, &old.rule.from, Some(&old), root.as_deref())?;
            if let Some(task) = &patch.task {
                let count = patch
                    .assignee_ids
                    .as_ref()
                    .map_or(old.assignee_ids.len(), Vec::len);
                // 担当者を登録する前に、ほかの項目を確かめる
                malakt_core::schedule::clean_template(task, count).map_err(core)?;
                if let Some(parent) = &task.parent_id {
                    store.task(parent).map_err(core)?;
                }
                for id in task.linked_workspace_ids.iter().flatten() {
                    store.workspace(id).map_err(core)?;
                }
            }
            if let Some(plan) = plan {
                settle_schedule_assignees(
                    store,
                    &mut patch,
                    plan,
                    Some(&old),
                    old.workspace_id.as_deref(),
                )?;
            }
            let s = store.update_schedule(id, &patch, &now).map_err(core)?;
            schedule_json(store, &s)?
        }
        "add_schedule_attachment" => {
            let schedule_id = required(args, "schedule_id")?;
            let (name, bytes) = read_upload(required(args, "path")?)?;
            let f = store
                .add_schedule_file(schedule_id, &name, &bytes)
                .map_err(core)?;
            json!({"id": f.id, "name": f.name, "size": f.size})
        }
        "delete_schedule_attachment" => {
            let id = required(args, "id")?;
            store.delete_schedule_file(id).map_err(core)?;
            json!({"deleted": id})
        }
        "delete_schedule" => {
            store.delete_schedule(required(args, "id")?).map_err(core)?;
            json!({"deleted": true})
        }
        "set_due_alerts" => {
            let task_id = optional(args, "task_id");
            let list = |key: &str| -> Result<Option<Option<Vec<u32>>>, String> {
                match args.get(key) {
                    None => Ok(None),
                    Some(Value::Null) => Ok(Some(None)),
                    Some(Value::Array(items)) => items
                        .iter()
                        .map(|v| {
                            v.as_u64()
                                .and_then(|n| u32::try_from(n).ok())
                                .ok_or_else(|| format!("{key} は 0 以上の整数の並びです: {v}"))
                        })
                        .collect::<Result<Vec<u32>, String>>()
                        .map(|v| Some(Some(v))),
                    Some(v) => Err(format!("{key} は整数の並びです: {v}")),
                }
            };
            let timed = list("timed_minutes")?;
            let date = list("date_days")?;
            let hour = match args.get("hour") {
                None | Some(Value::Null) => None,
                Some(v) => Some(
                    v.as_u64()
                        .and_then(|n| u32::try_from(n).ok())
                        .ok_or_else(|| format!("hour は 0〜23 です: {v}"))?,
                ),
            };
            if timed.is_none() && date.is_none() && hour.is_none() {
                return Err("timed_minutes・date_days・hour のどれかを渡してください".into());
            }
            // 先に全部の値を確かめ、1 つでも正しくなければ何も変えない
            for values in [&timed, &date].into_iter().flatten().flatten() {
                if values.len() > malakt_core::alert::MAX_ALERTS {
                    return Err(format!(
                        "通知は 1 つの期限に {} 個までです",
                        malakt_core::alert::MAX_ALERTS
                    ));
                }
            }
            if let Some(v) = timed.as_ref().and_then(Option::as_deref) {
                malakt_core::alert::clean_timed(v).map_err(core)?;
            }
            if let Some(v) = date.as_ref().and_then(Option::as_deref) {
                malakt_core::alert::clean_date(v).map_err(core)?;
            }
            match task_id {
                Some(id) => {
                    if hour.is_some() {
                        return Err(
                            "hour は全体の標準だけで変えられます（task_id を省いてください）"
                                .into(),
                        );
                    }
                    let mut own = store.task_alerts(id).map_err(core)?;
                    if let Some(v) = timed {
                        own.timed = v;
                    }
                    if let Some(v) = date {
                        own.date = v;
                    }
                    store.set_task_alerts(&own).map_err(core)?;
                }
                None => {
                    if matches!(timed, Some(None)) || matches!(date, Some(None)) {
                        return Err(
                            "全体の標準には null を渡せません。[] で知らせないにできます".into(),
                        );
                    }
                    if let Some(hour) = hour {
                        store.set_due_alert_hour(hour).map_err(core)?;
                    }
                    if let Some(Some(v)) = &timed {
                        store.set_due_alert_timed(v).map_err(core)?;
                    }
                    if let Some(Some(v)) = &date {
                        store.set_due_alert_date(v).map_err(core)?;
                    }
                }
            }
            due_alerts_json(store, task_id)?
        }
        "get_workspace_settings" => {
            let id = required(args, "id")?;
            let workspace = store.workspace(id).map_err(core)?;
            let defaults = store.task_defaults(id).map_err(core)?;
            let (due, due_days, due_date) = match &defaults.due {
                DueRule::None => ("none", None, None),
                DueRule::Relative { days } => ("relative", Some(*days), None),
                DueRule::Fixed { date } => ("fixed", None, Some(date.clone())),
            };
            json!({
                "id": id,
                "date": workspace.date,
                "assignee_ids": defaults.assignee_ids,
                "due": due,
                "due_days": due_days,
                "due_date": due_date,
                "due_time": defaults.due_time,
                "priority": defaults.priority.map(priority_name),
                "categories": defaults.categories,
                "description": defaults.description,
                "link_workspace_ids": defaults.link_workspace_ids,
                "terminal_dir": store.setting(&workspace_dir_key(id)).map_err(core)?.unwrap_or_default(),
            })
        }
        "list_ai_rules" => match optional(args, "workspace_id").filter(|w| !w.is_empty()) {
            Some(id) => {
                store.workspace(id).map_err(core)?;
                store
                    .ai_rules(Some(id))
                    .map_err(core)?
                    .iter()
                    .map(rule_json)
                    .collect()
            }
            None => {
                let mut workspaces = Vec::new();
                for w in store.workspaces().map_err(core)? {
                    let rules = store.ai_rules(Some(&w.id)).map_err(core)?;
                    if !rules.is_empty() {
                        workspaces.push(json!({
                            "id": w.id,
                            "name": w.name,
                            "rules": rules.iter().map(rule_json).collect::<Vec<_>>(),
                        }));
                    }
                }
                let global: Vec<Value> = store
                    .ai_rules(None)
                    .map_err(core)?
                    .iter()
                    .map(rule_json)
                    .collect();
                json!({"global": global, "workspaces": workspaces})
            }
        },
        "create_ai_rule" => {
            let workspace = optional(args, "workspace_id").filter(|w| !w.is_empty());
            let patch = rule_patch(args)?;
            let rule = store
                .create_ai_rule(
                    workspace,
                    patch.name.as_deref().unwrap_or_default(),
                    patch.body.as_deref().unwrap_or_default(),
                )
                .map_err(core)?;
            // 有効・無効と使う場面は、足したあとに変える
            let rule = store
                .update_ai_rule(
                    &rule.id,
                    &RulePatch {
                        name: None,
                        body: None,
                        ..patch
                    },
                )
                .map_err(core)?;
            rule_json(&rule)
        }
        "update_ai_rule" => {
            let id = required(args, "id")?;
            rule_json(&store.update_ai_rule(id, &rule_patch(args)?).map_err(core)?)
        }
        "delete_ai_rule" => {
            store.delete_ai_rule(required(args, "id")?).map_err(core)?;
            json!({"deleted": true})
        }
        "update_workspace_settings" => {
            let id = required(args, "id")?;
            store.workspace(id).map_err(core)?;
            let mut defaults = store.task_defaults(id).map_err(core)?;
            if let Some(v) = args.get("assignee_ids") {
                defaults.assignee_ids = strings(v, "assignee_ids")?;
            }
            if let Some(v) = args.get("categories") {
                defaults.categories = strings(v, "categories")?;
            }
            if let Some(v) = args.get("link_workspace_ids") {
                defaults.link_workspace_ids = strings(v, "link_workspace_ids")?;
            }
            if let Some(v) = optional(args, "description") {
                defaults.description = v.to_owned();
            }
            if let Some(v) = optional(args, "due_time") {
                defaults.due_time = v.to_owned();
            }
            match args.get("priority") {
                None => {}
                Some(Value::Null) => defaults.priority = None,
                Some(Value::String(s)) => defaults.priority = Some(parse_priority(s)?),
                Some(other) => return Err(format!("priority が正しくありません: {other}")),
            }
            let now = malakt_core::view::now();
            let days = args.get("due_days").map(|v| {
                v.as_i64()
                    .and_then(|n| i32::try_from(n).ok())
                    .ok_or_else(|| format!("due_days が正しくありません: {v}"))
            });
            let date_of = |key: &str| optional(args, key).map(|d| when::resolve(d, &now, None));
            match optional(args, "due") {
                None => {
                    // 決め方はそのままで、日数か日付だけを変える
                    match &mut defaults.due {
                        DueRule::Relative { days: d } => {
                            if let Some(days) = days {
                                *d = days?;
                            }
                        }
                        DueRule::Fixed { date } => {
                            if let Some(d) = date_of("due_date") {
                                *date = d;
                            }
                        }
                        DueRule::None => {}
                    }
                }
                Some("none") => defaults.due = DueRule::None,
                Some("relative") => {
                    defaults.due = DueRule::Relative {
                        days: days.transpose()?.unwrap_or(0),
                    }
                }
                Some("fixed") => {
                    defaults.due = DueRule::Fixed {
                        date: date_of("due_date")
                            .ok_or("due が fixed のときは due_date が必要です")?,
                    }
                }
                Some(other) => return Err(format!("due が正しくありません: {other}")),
            }
            let date = match args.get("date") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(Value::String(s)) if s.trim().is_empty() => Some(None),
                Some(Value::String(s)) => Some(Some(when::resolve(s, &now, None))),
                Some(other) => return Err(format!("date が正しくありません: {other}")),
            };
            let dir = optional(args, "terminal_dir").map(str::trim);
            if let Some(dir) = dir.filter(|d| !d.is_empty()) {
                if !expand_dir(dir).is_dir() {
                    return Err(format!("フォルダーが見つかりません: {dir}"));
                }
            }
            // 先に全部を確かめて、一部だけ保存されることがないようにする
            malakt_core::defaults::clean(&defaults).map_err(core)?;
            if let Some(Some(d)) = &date {
                if !malakt_core::is_valid_date(d) {
                    return Err(format!("date が正しくありません: {d}"));
                }
            }
            if let Some(date) = date {
                store
                    .set_workspace_date(id, date.as_deref())
                    .map_err(core)?;
            }
            store.set_task_defaults(&defaults).map_err(core)?;
            if let Some(dir) = dir {
                store
                    .set_setting(&workspace_dir_key(id), (!dir.is_empty()).then_some(dir))
                    .map_err(core)?;
            }
            return execute(
                store,
                author_id,
                "get_workspace_settings",
                &json!({"id": id}),
            );
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
        "get_workspace" => "ワークスペースを取得".into(),
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
        "set_assignee_done" => {
            if args.get("done").and_then(Value::as_bool) == Some(false) {
                format!("担当者の完了を外す: {}", arg("person"))
            } else {
                format!("担当者の分を完了にする: {}", arg("person"))
            }
        }
        "move_task" => "タスクを移動".into(),
        "delete_task" => "タスクを削除".into(),
        "add_comment" => "コメントを追加".into(),
        "create_workspace" => format!("ワークスペースを作成: {}", arg("name")),
        "create_person" => format!("担当者を追加: {}", arg("name")),
        "update_workspace" => "ワークスペースを更新".into(),
        "delete_workspace" => "ワークスペースを削除".into(),
        "update_person" => "担当者を更新".into(),
        "delete_person" => "担当者を削除".into(),
        "update_comment" => "コメントを更新".into(),
        "delete_comment" => "コメントを削除".into(),
        "link_task" => "タスクの同期を変更".into(),
        "reorder_tasks" => "タスクを並べ替え".into(),
        "add_attachment" => format!("ファイルを添付: {}", arg("path")),
        "delete_attachment" => "添付を削除".into(),
        "issue_ticket" => "完了チケットを発行".into(),
        "revoke_tickets" => "完了チケットを取り消し".into(),
        "list_categories" => "カテゴリ一覧を取得".into(),
        "list_notices" => "通知を取得".into(),
        "mark_notices_read" => "通知を既読にする".into(),
        "get_workspace_settings" => "ワークスペースの設定を取得".into(),
        "get_done_notices" => "完了の通知の設定を取得".into(),
        "set_done_notices" => "完了の通知の設定を変更".into(),
        "get_due_alerts" => "期限の通知のタイミングを取得".into(),
        "set_due_alerts" => "期限の通知のタイミングを変更".into(),
        "update_workspace_settings" => "ワークスペースの設定を変更".into(),
        "list_ai_rules" => "AI のルールを取得".into(),
        "create_ai_rule" => format!("AI のルールを追加: {}", arg("name")),
        "update_ai_rule" => "AI のルールを変更".into(),
        "delete_ai_rule" => "AI のルールを削除".into(),
        "list_schedules" => "定時タスクを取得".into(),
        "create_schedule" => format!("定時タスクを追加: {}", arg("title")),
        "update_schedule" => "定時タスクを変更".into(),
        "add_schedule_attachment" => format!("定時タスクにファイルを添付: {}", arg("path")),
        "delete_schedule_attachment" => "定時タスクの添付を外す".into(),
        "delete_schedule" => "定時タスクを削除".into(),
        "set_repeat" => match args.get("repeat") {
            Some(Value::Null) => "繰り返しを外す".into(),
            _ => "繰り返しを設定".into(),
        },
        GITHUB_FETCH => format!("GitHub から取得: {}", arg("url")),
        SLACK_FETCH => format!("Slack から取得: {}", arg("url")),
        RUN_SHELL => format!("コマンドを実行: {}", arg("command")),
        other => other.to_owned(),
    }
}

/// ユーザーのメッセージの最後に添える、今日の日付と今の画面。
/// `extra` は、今のワークスペースのルールなど、この実行についての補足。
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

/// [`turn_note`] に添える、[`RUN_SHELL`] のコマンドが動くシェルの説明。
/// `in_tab` は、ユーザーが今開いているターミナルのタブのシェルか。
pub fn shell_note(shell: &Shell, in_tab: bool) -> String {
    let place = if in_tab {
        "（ユーザーが今開いているターミナルのタブと同じシェル）"
    } else {
        " "
    };
    format!(
        "{RUN_SHELL} のコマンドは {}{place}で動きます。{}",
        shell.label(),
        shell.guide()
    )
}

/// ルールを AI に渡す文にする。1 つずつ名前を見出しにする。
fn rules_text<'a>(rules: impl IntoIterator<Item = &'a AiRule>) -> String {
    rules
        .into_iter()
        .map(|r| format!("## {}\n{}", r.name, r.body))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// グローバルのルールのうち、今の実行で使うもの（[`system_prompt`] に渡す）。
/// `auto` は、アプリが始めた実行（新しいタスクを整える、コメントから更新、定期チェック）。
pub fn global_rules(store: &Store, auto: bool) -> malakt_core::Result<String> {
    let rules = store.ai_rules(None)?;
    Ok(rules_text(rules.iter().filter(|r| {
        r.enabled && !r.body.is_empty() && r.apply.applies(auto)
    })))
}

/// ワークスペースのルールのうち、今の実行で使うもの（メッセージに添える）。
/// 親のワークスペースのルールも使う。同じルールは 1 回だけ入れる。
pub fn workspace_rules(
    store: &Store,
    workspaces: &[&str],
    auto: bool,
) -> malakt_core::Result<String> {
    let mut seen = std::collections::HashSet::new();
    let mut groups: Vec<(String, Vec<AiRule>)> = Vec::new();
    for id in workspaces {
        for (name, rule) in store.ai_rules_in(id)? {
            if !rule.apply.applies(auto) || !seen.insert(rule.id.clone()) {
                continue;
            }
            match groups.iter_mut().find(|(n, _)| *n == name) {
                Some((_, rules)) => rules.push(rule),
                None => groups.push((name, vec![rule])),
            }
        }
    }
    Ok(groups
        .iter()
        .map(|(name, rules)| {
            format!(
                "ワークスペース「{name}」のルールです。いつも従ってください。\n{}",
                rules_text(rules)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n"))
}

/// AI への最初の指示。`rules` は、ユーザーが決めたグローバルのルール（[`global_rules`]）。
/// 会話の途中で変わると、それまでの考えのブロックを送り返せなくなるので、
/// 日付や今の画面のように変わるものは入れない（[`turn_note`] でメッセージに添える）。
/// コメントに書いてよいこと。アプリの中の AI にも、MCP でつなぐ外のエージェントにも同じ文を渡す。
pub const COMMENT_RULE: &str = "コメントは、あとでタスクを読む人に要る事実だけを書いてください（決まったこと、分かったこと、変わった状態、次にすること）。「確認しました」「レビューしました」「対応しました」のように、作業をしたことだけを伝える文は書かないでください。「まだ〜していません」「〜は未確認です」のように、していないことも書かないでください。書く事実がなければ、コメントを書かないでください。";

const COMMENT_TOOL: &str = "タスクにコメントを書く。書くのは、あとでタスクを読む人に要る事実だけ（決まったこと、分かったこと、変わった状態、次にすること）。「確認しました」「レビューしました」のように作業をしたことだけを伝える文と、「まだ〜していません」のようにしていないことは書かない。書く事実がなければ呼ばない。";

pub fn system_prompt(rules: &str) -> String {
    let rules = if rules.trim().is_empty() {
        String::new()
    } else {
        format!(
            "\n\nユーザーが決めたルールです。いつも従ってください。\n{}",
            rules.trim()
        )
    };
    let prompt = format!(
        "あなたは、タスク管理アプリ malakt-mahoz の中で動くアシスタントです。\
         ツールを使って、ユーザーのタスク、ワークスペース、担当者、コメントを読み書きでき、\
         画面（開く場所、一覧の絞り込みと並び順、表の列、パネル、ペイン）、カスタマイズ（テーマ、レイアウト、背景、色、書体、密度）、\
         AI の設定、ロック、エクスポートとインポートも操作できます。\n\
         - アプリでできる操作は、ユーザーに頼まれたら、どれもツールで実行してください。「できません」「ご自身で操作してください」と答える前に、使えるツールを確かめてください。今の値が要るときは get_screen、get_customization、get_ai_settings で読んでください。\n\
         - 日付は YYYY-MM-DD、時刻つきなら YYYY-MM-DD HH:MM で渡します。時刻が分かる期限には時刻も入れてください。今の日時と、ユーザーが今開いている画面は、ユーザーのメッセージの最後にアプリが添えます。
\
         - 頼まれた操作は、確認を取らずにツールで実行してください。削除のように元に戻せない操作とシェルのコマンドは、アプリが実行の前にユーザーへ確認を出します。\n\
         - ID はツールで調べてから使ってください。ID を作らないでください。\n\
         - 担当者は ID か名前で指定できます。登録されていない人の名前が出たら、聞き返さずにその名前で指定してください（アプリがその場で担当者に登録します）。担当者の役（RACI: {RACI}）は set_assignee か、create_task / update_task の assignees で付けてください。担当者ごとの「自分の分」の完了は、自分の分もほかの人の分も set_assignee_done で付け外しできます。\n\
         - GitHub のリンクの中身は、必ず {GITHUB_FETCH} で取ってきてください（アプリが、ユーザーのログイン済みの gh で読みます）。{RUN_SHELL} で gh を呼ぶのは、{GITHUB_FETCH} で読めない種類のものだけにしてください。\n\
         - Slack のメッセージへのリンクの中身は、必ず {SLACK_FETCH} で取ってきてください（アプリが、ユーザーの Slack CLI で読みます）。\n\
         - {RUN_SHELL} のコマンドが動くシェルは、ユーザーのメッセージの最後にアプリが書きます。そのシェルの書き方で書いてください。\n\
         - タスクの追加、書き換え、整理のように元に戻せる操作は、「〜しましょうか」と聞き返さずに実行し、結果だけを伝えてください。聞き返すのは、頼まれたことが 2 通り以上に読めて、どちらかで結果が大きく変わるときだけです。\n\
         - ツールが失敗したら、やり方を変えてやり直してください。返事には最後の結果だけを書き、途中の失敗、その原因、ツールやシェルの事情は書かないでください。どうしてもできなかったときだけ、できなかったことと、ユーザーにしてほしいことを 1 文で書いてください。\n\
         - {COMMENT_RULE}ユーザーが文面を決めてコメントを頼んだときは、その文面のまま書いてください。\n\
         - 「毎週」「毎朝」のように、決まった時刻ごとに新しいタスクを作ってほしいと頼まれたら（週次定例の報告など）、create_schedule で定時タスクにしてください。時刻が来るたびに新しいタスクを 1 件作ります。作ったタスクで AI にしてほしい作業（報告の下書きなど）は ai_instruction に書いてください。今あるタスクに繰り返しを付けるよう頼まれたときは、set_repeat を使ってください。定時タスクの雛形には、タスクと同じ項目（URL・説明・状態・優先度・担当と役・開始日・期限・通知・進捗・カテゴリ・親タスク・ほかのワークスペース・サブタスク・添付）を入れられます。次から作るタスクに入れるよう頼まれたものは、update_schedule（添付は add_schedule_attachment）で雛形に入れてください。\n\
         - ユーザーに「これからはこうして」「いつも〜して」のように、これから先も続けてほしいことを頼まれたら、list_ai_rules で今のルールを読み、create_ai_rule か update_ai_rule でルールにしてください。あるワークスペースだけのことなら、そのワークスペースのルールにしてください。\n\
         - 返事は短く、ユーザーの言葉（ふつうは日本語）で書いてください。記法は使わず、ふつうの文章で書いてください。"
    );
    prompt + &rules
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(store: &mut Store, name: &str, args: Value) -> Value {
        serde_json::from_str(&execute(store, None, name, &args).unwrap()).unwrap()
    }

    #[test]
    fn repeats_are_set_changed_and_removed() {
        let mut store = Store::open_in_memory().unwrap();
        let task = run(
            &mut store,
            "create_task",
            // 2099-11-30 は月曜
            json!({"title": "週次定例の報告", "due": "2099-11-30 10:00"}),
        );
        let id = task["id"].as_str().unwrap();
        // 繰り返しのないタスクには、AI への指示だけを付けられない
        let err = execute(
            &mut store,
            None,
            "set_repeat",
            &json!({"task_id": id, "ai_instruction": "下書きを書く"}),
        )
        .unwrap_err();
        assert!(err.contains("繰り返しのある"), "{err}");
        let set = run(
            &mut store,
            "set_repeat",
            json!({
                "task_id": id,
                "repeat": {"unit": "week", "time": "09:00"},
                "ai_instruction": "先週のコメントから報告の下書きを書く",
            }),
        );
        let repeat = &set["repeat"];
        assert_eq!(repeat["text"], "毎週 月 9:00");
        assert_eq!(repeat["from"], "2099-11-30");
        assert_eq!(repeat["next_at"], "2099-12-07 09:00");
        assert_eq!(
            repeat["ai_instruction"],
            "先週のコメントから報告の下書きを書く"
        );
        // 渡した項目だけが変わる
        let changed = run(
            &mut store,
            "set_repeat",
            json!({"task_id": id, "repeat": {"unit": "week", "every": 2, "weekdays": [0, 3]}}),
        );
        assert_eq!(changed["repeat"]["text"], "2 週間ごと 月・木");
        assert_eq!(
            changed["repeat"]["ai_instruction"],
            "先週のコメントから報告の下書きを書く"
        );
        assert_eq!(
            run(&mut store, "get_task", json!({"id": id}))["repeat"]["text"],
            "2 週間ごと 月・木"
        );
        // 正しくない値は、何も変えない
        for bad in [
            json!({"task_id": id, "repeat": {"unit": "hour"}}),
            json!({"task_id": id, "repeat": {"unit": "day", "every": 0}}),
            json!({"task_id": id, "repeat": {"unit": "week", "weekdays": [7]}}),
            json!({"task_id": id, "repeat": null, "ai_instruction": "x"}),
            json!({"task_id": id}),
        ] {
            assert!(
                execute(&mut store, None, "set_repeat", &bad).is_err(),
                "{bad}"
            );
        }
        assert_eq!(
            run(&mut store, "get_task", json!({"id": id}))["repeat"]["text"],
            "2 週間ごと 月・木"
        );
        let removed = run(
            &mut store,
            "set_repeat",
            json!({"task_id": id, "repeat": null}),
        );
        assert!(removed["repeat"].is_null());
        assert_eq!(
            summary("set_repeat", &json!({"repeat": null})),
            "繰り返しを外す"
        );
    }

    #[test]
    fn schedules_are_created_changed_listed_and_deleted() {
        let mut store = Store::open_in_memory().unwrap();
        let ws = run(&mut store, "create_workspace", json!({"name": "定例"}));
        let ws_id = ws["id"].as_str().unwrap();
        let person = run(&mut store, "create_person", json!({"name": "佐藤"}));
        let person_id = person["id"].as_str().unwrap();
        let made = run(
            &mut store,
            "create_schedule",
            json!({
                "workspace_id": ws_id,
                "title": "週次定例の報告",
                // 2099-11-30 は月曜
                "schedule": {"unit": "week", "from": "2099-11-30", "time": "09:00"},
                "ai_instruction": "先週の進み具合をまとめる",
                "assignee_ids": [person_id],
                "due_days": 1,
                "due_time": "17:00",
            }),
        );
        assert_eq!(made["schedule"]["text"], "毎週 月 9:00");
        assert_eq!(made["next_at"], "2099-11-30 09:00");
        assert_eq!(made["ai_instruction"], "先週の進み具合をまとめる");
        assert_eq!(made["assignee_ids"], json!([person_id]));
        assert_eq!(made["due_days"], 1);
        let id = made["id"].as_str().unwrap();
        // 渡した項目だけが変わる
        let changed = run(
            &mut store,
            "update_schedule",
            json!({"id": id, "schedule": {"unit": "week", "weekdays": [4], "time": "18:00"}, "due_days": null}),
        );
        assert_eq!(changed["schedule"]["text"], "毎週 金 18:00");
        assert_eq!(changed["schedule"]["from"], "2099-11-30");
        assert!(changed["due_days"].is_null());
        assert_eq!(changed["title"], "週次定例の報告");
        let list = run(&mut store, "list_schedules", json!({}));
        assert_eq!(list["outside"], json!([]));
        assert_eq!(list["workspaces"][0]["schedules"][0]["id"], id);
        // 正しくない値は、何も作らない
        for bad in [
            json!({"title": "x", "schedule": {"unit": "hour"}}),
            json!({"title": "x", "due_days": -1}),
            json!({"title": "x", "due_days": 0, "due_time": "25:00"}),
            json!({"title": "x", "assignee_ids": ["nobody"]}),
            json!({}),
        ] {
            assert!(
                execute(&mut store, None, "create_schedule", &bad).is_err(),
                "{bad}"
            );
        }
        assert_eq!(store.all_schedules().unwrap().len(), 1);
        assert!(WRITES.contains(&"create_schedule"));
        assert_eq!(
            summary("create_schedule", &json!({"title": "日報"})),
            "定時タスクを追加: 日報"
        );
        run(&mut store, "delete_schedule", json!({"id": id}));
        assert!(store.all_schedules().unwrap().is_empty());
    }

    #[test]
    fn schedule_templates_take_every_task_field() {
        let mut store = Store::open_in_memory().unwrap();
        let ws = run(&mut store, "create_workspace", json!({"name": "経理"}));
        let ws_id = ws["id"].as_str().unwrap();
        let other = run(&mut store, "create_workspace", json!({"name": "共有"}));
        let other_id = other["id"].as_str().unwrap();
        let parent = run(
            &mut store,
            "create_task",
            json!({"title": "経理", "workspace_id": ws_id}),
        );
        let made = run(
            &mut store,
            "create_schedule",
            json!({
                "workspace_id": ws_id,
                "title": "ktc 請求書の作成",
                // 2099-11-28 は土曜
                "schedule": {"unit": "month", "from": "2099-11-28", "time": "09:00"},
                "urls": ["https://epilysi.stargate65535.workers.dev/"],
                "description": "先月分",
                "status": "in_process",
                "priority": "high",
                "start_days": 0,
                "start_time": "10:00",
                "due_days": 3,
                "due_alerts": {"date_days": [1]},
                "percent": 10,
                "parent_id": parent["id"],
                "assignees": [{"person": "経理担当", "roles": ["R", "A"]}],
                "categories": ["請求"],
                "linked_workspace_ids": [other_id],
                "subtasks": ["金額を確かめる"],
            }),
        );
        assert_eq!(
            made["urls"],
            json!(["https://epilysi.stargate65535.workers.dev/"])
        );
        assert_eq!(made["description"], "先月分");
        assert_eq!(made["status"], "in_process");
        assert_eq!(made["priority"], "high");
        assert_eq!(made["start_days"], 0);
        assert_eq!(made["start_time"], "10:00");
        assert_eq!(
            made["due_alerts"],
            json!({"timed_minutes": null, "date_days": [1]})
        );
        assert_eq!(made["percent"], 10);
        assert_eq!(made["parent_id"], parent["id"]);
        assert_eq!(made["assignees"][0]["roles"], json!(["R", "A"]));
        assert_eq!(made["categories"], json!(["請求"]));
        assert_eq!(made["linked_workspace_ids"], json!([other_id]));
        assert_eq!(made["subtasks"], json!(["金額を確かめる"]));
        // 名前だけの担当者は、そのワークスペースの担当者として登録する
        let people = run(&mut store, "list_people", json!({}));
        assert!(people
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "経理担当"));
        let id = made["id"].as_str().unwrap();

        // 渡した項目だけが変わる。URL は足すことも外すこともできる
        let changed = run(
            &mut store,
            "update_schedule",
            json!({"id": id, "add_urls": ["b.example/x"], "remove_urls": ["epilysi.stargate65535.workers.dev/"], "priority": null}),
        );
        assert_eq!(changed["urls"], json!(["https://b.example/x"]));
        assert!(changed["priority"].is_null());
        assert_eq!(changed["description"], "先月分");
        assert_eq!(changed["assignees"][0]["roles"], json!(["R", "A"]));

        // 添付も付けられ、作るタスクに入る
        let dir = std::env::temp_dir().join(format!("malakt-schedule-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("見積.txt");
        std::fs::write(&path, b"abc").unwrap();
        let file = run(
            &mut store,
            "add_schedule_attachment",
            json!({"schedule_id": id, "path": path.to_string_lossy()}),
        );
        assert_eq!(file["name"], "見積.txt");
        let list = run(&mut store, "list_schedules", json!({"workspace_id": ws_id}));
        assert_eq!(list[0]["attachments"][0]["name"], "見積.txt");
        let tasks = store.sweep_schedules("2099-11-28 09:00").unwrap();
        let task = run(&mut store, "get_task", json!({"id": tasks[0]}));
        assert_eq!(task["urls"], json!(["https://b.example/x"]));
        assert_eq!(task["status"], "in_process");
        assert_eq!(task["start"], "2099-11-28 10:00");
        assert_eq!(task["due"], "2099-12-01");
        assert_eq!(task["attachments"][0]["name"], "見積.txt");
        assert_eq!(task["subtasks"][0]["title"], "金額を確かめる");
        assert_eq!(task["due_alerts"]["own"]["date_days"], json!([1]));
        run(
            &mut store,
            "delete_schedule_attachment",
            json!({"id": file["id"]}),
        );
        assert!(store.schedule_files(id).unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(WRITES.contains(&"add_schedule_attachment"));
        assert!(WRITES.contains(&"delete_schedule_attachment"));

        // 正しくない値は何も変えない
        for bad in [
            json!({"id": id, "status": "done"}),
            json!({"id": id, "priority": "urgent"}),
            json!({"id": id, "percent": 101}),
            json!({"id": id, "start_days": -1}),
            json!({"id": id, "urls": ["リンク"]}),
            json!({"id": id, "remove_urls": ["https://none.example/"]}),
            json!({"id": id, "parent_id": "ない"}),
            json!({"id": id, "linked_workspace_ids": ["ない"]}),
            json!({"id": id, "due_alerts": {"date_days": [99]}}),
            json!({"id": id, "assignees": [{"person": "x", "roles": []}]}),
        ] {
            let before = store.schedule(id).unwrap();
            assert!(
                execute(&mut store, None, "update_schedule", &bad).is_err(),
                "{bad}"
            );
            assert_eq!(store.schedule(id).unwrap(), before, "{bad}");
        }
        // 足すときも、正しくない値なら何も作らない
        let count = store.all_schedules().unwrap().len();
        assert!(execute(
            &mut store,
            None,
            "create_schedule",
            &json!({"title": "x", "urls": ["リンク"]})
        )
        .is_err());
        assert!(execute(
            &mut store,
            None,
            "create_schedule",
            &json!({"title": "x", "parent_id": "ない"})
        )
        .is_err());
        assert_eq!(store.all_schedules().unwrap().len(), count);
    }

    #[test]
    fn a_workspace_comes_with_its_sub_workspaces_and_tasks() {
        let mut store = Store::open_in_memory().unwrap();
        let event = run(&mut store, "create_workspace", json!({"name": "イベント"}));
        let ad = run(
            &mut store,
            "create_workspace",
            json!({"name": "告知", "parent_id": event["id"]}),
        );
        let poster = run(
            &mut store,
            "create_workspace",
            json!({"name": "ポスター", "parent_id": ad["id"]}),
        );
        let venue = run(
            &mut store,
            "create_workspace",
            json!({"name": "会場", "parent_id": event["id"]}),
        );
        let other = run(&mut store, "create_workspace", json!({"name": "家"}));
        run(
            &mut store,
            "create_task",
            json!({"title": "会場を予約", "workspace_id": venue["id"], "description": "駅から近いところ"}),
        );
        run(
            &mut store,
            "create_task",
            json!({"title": "ポスターを刷る", "workspace_id": poster["id"]}),
        );
        run(
            &mut store,
            "create_task",
            json!({"title": "日程を決めた", "workspace_id": event["id"], "status": "completed"}),
        );
        run(
            &mut store,
            "create_task",
            json!({"title": "掃除", "workspace_id": other["id"]}),
        );

        let got = run(&mut store, "get_workspace", json!({"id": event["id"]}));
        assert_eq!(got["workspace"]["name"], "イベント");
        assert_eq!(got["path"], "イベント");
        // 入れ子の全部を、親のすぐ後ろに子の並びで
        let subs: Vec<&str> = got["sub_workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["name"].as_str().unwrap())
            .collect();
        assert_eq!(subs, ["告知", "ポスター", "会場"]);
        assert_eq!(got["sub_workspaces"][1]["open_tasks"], 1);
        assert_eq!(got["open_tasks"], 2);
        assert_eq!(got["done_tasks"], 1);
        // 標準では、完了したタスクとほかのワークスペースのタスクは一覧にない
        let titles: Vec<&str> = got["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["title"].as_str().unwrap())
            .collect();
        assert_eq!(titles.len(), 2);
        assert!(titles.contains(&"会場を予約") && titles.contains(&"ポスターを刷る"));
        assert_eq!(got["tasks_total"], 2);
        assert_eq!(got["tasks_shown"], 2);
        assert!(got.get("note").is_none());
        let venue_task = got["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["title"] == "会場を予約")
            .unwrap();
        assert_eq!(venue_task["description"], "駅から近いところ");

        let all = run(
            &mut store,
            "get_workspace",
            json!({"id": event["id"], "include_done": true}),
        );
        assert_eq!(all["tasks_total"], 3);
        let sub = run(&mut store, "get_workspace", json!({"id": poster["id"]}));
        assert_eq!(sub["path"], "イベント / 告知 / ポスター");
        assert!(execute(&mut store, None, "get_workspace", &json!({"id": "nope"})).is_err());
    }

    #[test]
    fn a_large_workspace_is_cut_to_fit() {
        let mut store = Store::open_in_memory().unwrap();
        let ws = run(&mut store, "create_workspace", json!({"name": "大きい"}));
        let long = "説明".repeat(300);
        for i in 0..(WORKSPACE_TASKS + 20) {
            run(
                &mut store,
                "create_task",
                json!({"title": format!("タスク {i}"), "workspace_id": ws["id"], "description": long}),
            );
        }
        let text = execute(&mut store, None, "get_workspace", &json!({"id": ws["id"]})).unwrap();
        assert!(text.chars().count() <= WORKSPACE_CHARS, "{}", text.len());
        let got: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(got["tasks_total"], WORKSPACE_TASKS + 20);
        let shown = got["tasks_shown"].as_u64().unwrap() as usize;
        assert!(shown > 0 && shown <= WORKSPACE_TASKS, "{shown}");
        assert_eq!(got["tasks"].as_array().unwrap().len(), shown);
        assert!(got["note"].is_string());
        // 説明は短くしてある
        let description = got["tasks"][0]["description"].as_str().unwrap();
        assert_eq!(description.chars().count(), WORKSPACE_DESCRIPTION + 1);
    }

    #[test]
    fn urls_are_set_added_and_removed() {
        let mut store = Store::open_in_memory().unwrap();
        let task = run(
            &mut store,
            "create_task",
            json!({"title": "会場", "urls": ["https://a.jp/", "github.com/o/r/issues/2"]}),
        );
        assert_eq!(
            task["urls"],
            json!(["https://a.jp/", "https://github.com/o/r/issues/2"])
        );
        // 足すときは後ろに。https:// を省いて書いても同じものは足さない
        let added = run(
            &mut store,
            "update_task",
            json!({"id": task["id"], "add_urls": ["a.jp/", "https://b.jp/x"]}),
        );
        assert_eq!(
            added["urls"],
            json!([
                "https://a.jp/",
                "https://github.com/o/r/issues/2",
                "https://b.jp/x"
            ])
        );
        let removed = run(
            &mut store,
            "update_task",
            json!({"id": task["id"], "remove_urls": ["https://github.com/o/r/issues/2"]}),
        );
        assert_eq!(removed["urls"], json!(["https://a.jp/", "https://b.jp/x"]));
        let got = run(&mut store, "get_task", json!({"id": task["id"]}));
        assert_eq!(got["urls"], removed["urls"]);
        // 付いていない URL を外す・URL でないものを足すと失敗し、何も変わらない
        for (args, text) in [
            (
                json!({"remove_urls": ["https://c.jp/"]}),
                "「https://c.jp/」",
            ),
            (json!({"add_urls": ["メモ"]}), "「メモ」"),
            (json!({"urls": [1]}), "urls"),
        ] {
            let mut args = args;
            args["id"] = task["id"].clone();
            let err = execute(&mut store, None, "update_task", &args).unwrap_err();
            assert!(err.contains(text), "{err}");
        }
        let id = task["id"].as_str().unwrap();
        assert_eq!(
            store.task(id).unwrap().urls,
            ["https://a.jp/", "https://b.jp/x"]
        );
        // 全部を置き換える
        let set = run(&mut store, "update_task", json!({"id": id, "urls": []}));
        assert_eq!(set["urls"], json!([]));
    }

    #[test]
    fn due_alerts_can_be_read_and_changed() {
        let mut store = Store::open_in_memory().unwrap();
        let task = run(
            &mut store,
            "create_task",
            json!({"title": "会議", "due": "2026-12-24 18:00"}),
        );
        let got = run(&mut store, "get_due_alerts", json!({}));
        assert_eq!(got["standard"]["timed_minutes"], json!([0]));
        assert_eq!(got["standard"]["text"]["date"], "当日");
        let set = run(
            &mut store,
            "set_due_alerts",
            json!({"timed_minutes": [5, 0], "date_days": [1], "hour": 8}),
        );
        assert_eq!(set["standard"]["timed_minutes"], json!([0, 5]));
        assert_eq!(set["standard"]["hour"], 8);
        assert_eq!(set["standard"]["text"]["timed"], "期限ちょうど、5 分前");
        // タスクだけの上書きと、標準に戻す
        let own = run(
            &mut store,
            "set_due_alerts",
            json!({"task_id": task["id"], "timed_minutes": [15]}),
        );
        assert_eq!(own["used"]["timed_minutes"], json!([15]));
        assert_eq!(own["used"]["date_days"], json!([1]));
        let task_json = run(&mut store, "get_task", json!({"id": task["id"]}));
        assert_eq!(task_json["due_alerts"]["own"]["timed_minutes"], json!([15]));
        let back = run(
            &mut store,
            "set_due_alerts",
            json!({"task_id": task["id"], "timed_minutes": null}),
        );
        assert_eq!(back["own"], Value::Null);
        // 正しくない値では何も変えない
        for bad in [
            json!({}),
            json!({"timed_minutes": [0], "date_days": [99]}),
            json!({"hour": 24}),
            json!({"timed_minutes": null}),
            json!({"task_id": task["id"], "hour": 7}),
            json!({"task_id": "nope", "timed_minutes": [0]}),
        ] {
            assert!(
                execute(&mut store, None, "set_due_alerts", &bad).is_err(),
                "{bad}"
            );
        }
        let got = run(&mut store, "get_due_alerts", json!({}));
        assert_eq!(got["standard"]["timed_minutes"], json!([0, 5]));
        assert_eq!(got["standard"]["date_days"], json!([1]));
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
        assert_eq!(
            asking,
            [
                "delete_task",
                "delete_workspace",
                "delete_person",
                "delete_comment",
                "delete_attachment",
                "revoke_tickets",
                "run_shell"
            ]
        );
        // 書き込みのツールは、どれも一覧にある
        let names: Vec<_> = definitions().iter().map(|t| t.name).collect();
        for name in WRITES {
            assert!(names.contains(name), "{name}");
        }
    }

    #[test]
    fn workspaces_people_and_comments_are_changed_and_removed() {
        let mut store = Store::open_in_memory().unwrap();
        let home = run(&mut store, "create_workspace", json!({"name": "家"}));
        let work = run(&mut store, "create_workspace", json!({"name": "仕事"}));
        // 名前を変えて、別のワークスペースの中へ移す
        let moved = run(
            &mut store,
            "update_workspace",
            json!({"id": work["id"], "name": "会社", "parent_id": home["id"]}),
        );
        assert_eq!(moved["name"], "会社");
        assert_eq!(moved["parent_id"], home["id"]);
        // 自分の中へは移せず、名前も変わらない
        let err = execute(
            &mut store,
            None,
            "update_workspace",
            &json!({"id": home["id"], "name": "x", "parent_id": work["id"]}),
        )
        .unwrap_err();
        assert!(err.contains("移せません"), "{err}");
        assert_eq!(
            store.workspace(home["id"].as_str().unwrap()).unwrap().name,
            "家"
        );
        let top = run(
            &mut store,
            "update_workspace",
            json!({"id": work["id"], "parent_id": null}),
        );
        assert_eq!(top["parent_id"], Value::Null);

        // 担当者: そのワークスペースだけの人を作り、名前・メモ・使う場所を変える
        let local = run(
            &mut store,
            "create_person",
            json!({"name": "鈴木", "workspace_id": home["id"]}),
        );
        assert_eq!(local["workspace_ids"], json!([home["id"]]));
        let changed = run(
            &mut store,
            "update_person",
            json!({"id": local["id"], "name": "鈴木さん", "note": "経理", "workspace_ids": []}),
        );
        assert_eq!(changed["name"], "鈴木さん");
        assert_eq!(changed["note"], "経理");
        assert_eq!(changed["workspace_ids"], json!([]));
        store.ensure_self().unwrap();
        let me = store
            .people()
            .unwrap()
            .into_iter()
            .find(|p| p.is_self)
            .unwrap();
        assert!(
            execute(&mut store, None, "delete_person", &json!({"id": me.id}))
                .unwrap_err()
                .contains("自分")
        );
        assert!(confirmation_text(
            &store,
            "delete_person",
            &json!({"id": local["id"]}),
            &Shell::Cmd
        )
        .contains("鈴木さん"));
        run(&mut store, "delete_person", json!({"id": local["id"]}));
        assert_eq!(store.people().unwrap().len(), 1);

        // コメント: 返信、書き直し、ピン留め、解決、削除
        let task = run(
            &mut store,
            "create_task",
            json!({"title": "見積もり", "workspace_id": home["id"]}),
        );
        let review = run(
            &mut store,
            "add_comment",
            json!({"task_id": task["id"], "body": "金額を確認", "review": true}),
        );
        run(
            &mut store,
            "add_comment",
            json!({"task_id": task["id"], "body": "OK です", "reply_to": review["id"]}),
        );
        let updated = run(
            &mut store,
            "update_comment",
            json!({"id": review["id"], "body": "金額を再確認", "pinned": true, "resolved": true}),
        );
        assert_eq!(updated["body"], "金額を再確認");
        assert_eq!(updated["pinned"], true);
        assert_eq!(updated["resolved"], true);
        let read = run(&mut store, "get_task", json!({"id": task["id"]}));
        assert_eq!(read["comments"][1]["reply_to"], review["id"]);
        run(
            &mut store,
            "delete_comment",
            json!({"id": read["comments"][1]["id"]}),
        );
        assert_eq!(
            store.comments(task["id"].as_str().unwrap()).unwrap().len(),
            1
        );

        // 同期: ほかのワークスペースにも出し、外す
        let linked = run(
            &mut store,
            "link_task",
            json!({"id": task["id"], "workspace_id": work["id"], "linked": true}),
        );
        assert_eq!(linked["linked_workspace_ids"], json!([work["id"]]));
        let unlinked = run(
            &mut store,
            "link_task",
            json!({"id": task["id"], "workspace_id": work["id"], "linked": false}),
        );
        assert_eq!(unlinked["linked_workspace_ids"], json!([]));

        // ワークスペースを消すと、中のタスクも消える
        assert!(confirmation_text(
            &store,
            "delete_workspace",
            &json!({"id": home["id"]}),
            &Shell::Cmd
        )
        .contains("家"));
        run(&mut store, "delete_workspace", json!({"id": home["id"]}));
        assert!(store.task(task["id"].as_str().unwrap()).is_err());
        assert_eq!(
            run(&mut store, "list_workspaces", json!({}))
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn workspace_settings_are_read_and_written_together() {
        let mut store = Store::open_in_memory().unwrap();
        let ws = run(&mut store, "create_workspace", json!({"name": "文化祭"}));
        let other = run(&mut store, "create_workspace", json!({"name": "広報"}));
        let p = run(&mut store, "create_person", json!({"name": "田中"}));
        let dir = tempfile::tempdir().unwrap();
        let saved = run(
            &mut store,
            "update_workspace_settings",
            json!({
                "id": ws["id"], "date": "2026-12-20", "due": "relative", "due_days": -7,
                "due_time": "18:00", "priority": "high", "assignee_ids": [p["id"]],
                "categories": ["準備"], "description": "やること:", "link_workspace_ids": [other["id"]],
                "terminal_dir": dir.path().display().to_string(),
            }),
        );
        assert_eq!(saved["date"], "2026-12-20");
        assert_eq!(saved["due"], "relative");
        assert_eq!(saved["due_days"], -7);
        assert_eq!(saved["priority"], "high");
        // 決め方を変えずに日数だけ変える
        let days = run(
            &mut store,
            "update_workspace_settings",
            json!({"id": ws["id"], "due_days": -3}),
        );
        assert_eq!(days["due_days"], -3);
        assert_eq!(days["due_time"], "18:00");
        // 既定値が新しいタスクに入る
        let draft = store.draft_for(ws["id"].as_str(), "受付").unwrap();
        assert_eq!(draft.due.as_deref(), Some("2026-12-17 18:00"));
        assert_eq!(
            draft.assignee_ids,
            vec![p["id"].as_str().unwrap().to_owned()]
        );

        // 1 つでも正しくなければ、何も変えない
        for bad in [
            json!({"id": ws["id"], "date": "あさって以降", "categories": ["x"]}),
            json!({"id": ws["id"], "categories": ["x"], "terminal_dir": "/no/such/folder/here"}),
            json!({"id": ws["id"], "categories": ["x"], "due": "fixed"}),
        ] {
            assert!(
                execute(&mut store, None, "update_workspace_settings", &bad).is_err(),
                "{bad}"
            );
        }
        let after = run(
            &mut store,
            "get_workspace_settings",
            json!({"id": ws["id"]}),
        );
        assert_eq!(after["categories"], json!(["準備"]));
        assert_eq!(after["date"], "2026-12-20");
        let cleared = run(
            &mut store,
            "update_workspace_settings",
            json!({"id": ws["id"], "date": null, "due": "none", "terminal_dir": ""}),
        );
        assert_eq!(cleared["date"], Value::Null);
        assert_eq!(cleared["due"], "none");
        assert_eq!(cleared["terminal_dir"], "");
    }

    #[test]
    fn ai_rules_are_listed_added_changed_and_removed() {
        let mut store = Store::open_in_memory().unwrap();
        let work = run(&mut store, "create_workspace", json!({"name": "仕事"}));
        let team = run(
            &mut store,
            "create_workspace",
            json!({"name": "チーム", "parent_id": work["id"]}),
        );
        let global = run(
            &mut store,
            "create_ai_rule",
            json!({"name": "言葉づかい", "body": "敬語で答える"}),
        );
        assert_eq!(global["workspace_id"], Value::Null);
        assert_eq!(global["enabled"], true);
        assert_eq!(global["apply"], "always");
        let local = run(
            &mut store,
            "create_ai_rule",
            json!({"workspace_id": work["id"], "body": "PR は優先度を高", "apply": "auto"}),
        );
        assert_eq!(local["name"], malakt_core::rules::NEW_RULE_NAME);
        assert_eq!(local["apply"], "auto");
        let all = run(&mut store, "list_ai_rules", json!({}));
        assert_eq!(all["global"][0]["body"], "敬語で答える");
        assert_eq!(all["workspaces"][0]["name"], "仕事");
        assert_eq!(all["workspaces"][0]["rules"][0]["id"], local["id"]);
        let own = run(
            &mut store,
            "list_ai_rules",
            json!({"workspace_id": team["id"]}),
        );
        assert_eq!(own, json!([]));

        // チャットではグローバルのルールだけ、自動の処理では親のワークスペースのルールも使う
        let team_id = team["id"].as_str().unwrap();
        assert_eq!(
            global_rules(&store, false).unwrap(),
            "## 言葉づかい\n敬語で答える"
        );
        assert_eq!(workspace_rules(&store, &[team_id], false).unwrap(), "");
        assert_eq!(
            workspace_rules(&store, &[team_id, team_id], true).unwrap(),
            "ワークスペース「仕事」のルールです。いつも従ってください。\n## 新規ルール\nPR は優先度を高"
        );
        let off = run(
            &mut store,
            "update_ai_rule",
            json!({"id": global["id"], "enabled": false, "name": "敬語"}),
        );
        assert_eq!(off["name"], "敬語");
        assert_eq!(global_rules(&store, false).unwrap(), "");
        for bad in [
            json!({"id": global["id"], "apply": "sometimes"}),
            json!({"id": global["id"], "name": " "}),
            json!({"id": "nope"}),
        ] {
            assert!(
                execute(&mut store, None, "update_ai_rule", &bad).is_err(),
                "{bad}"
            );
        }
        // 消したルールは元に戻せるので、確認は出さない
        assert!(!needs_confirmation("delete_ai_rule"));
        run(&mut store, "delete_ai_rule", json!({"id": global["id"]}));
        assert_eq!(
            run(&mut store, "list_ai_rules", json!({}))["global"],
            json!([])
        );
    }

    #[test]
    fn attachments_tickets_order_and_notices() {
        let mut store = Store::open_in_memory().unwrap();
        let a = run(&mut store, "create_task", json!({"title": "a"}));
        let b = run(&mut store, "create_task", json!({"title": "b"}));
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("見積.txt");
        std::fs::write(&file, "100 円").unwrap();
        let added = run(
            &mut store,
            "add_attachment",
            json!({"task_id": a["id"], "path": file.display().to_string()}),
        );
        assert_eq!(added["name"], "見積.txt");
        let read = run(&mut store, "get_task", json!({"id": a["id"]}));
        assert_eq!(read["attachments"][0]["id"], added["id"]);
        assert!(confirmation_text(
            &store,
            "delete_attachment",
            &json!({"id": added["id"]}),
            &Shell::Cmd
        )
        .contains("見積.txt"));
        run(&mut store, "delete_attachment", json!({"id": added["id"]}));
        assert!(store
            .attachments(a["id"].as_str().unwrap())
            .unwrap()
            .is_empty());
        assert!(execute(
            &mut store,
            None,
            "add_attachment",
            &json!({"task_id": a["id"], "path": dir.path().display().to_string()}),
        )
        .unwrap_err()
        .contains("ファイルではありません"));

        let ticket = run(&mut store, "issue_ticket", json!({"task_id": a["id"]}));
        assert!(!ticket["ticket"].as_str().unwrap().is_empty());
        assert_eq!(
            run(&mut store, "get_task", json!({"id": a["id"]}))["active_tickets"],
            1
        );
        assert_eq!(
            run(&mut store, "revoke_tickets", json!({"task_id": a["id"]}))["revoked"],
            1
        );

        // 手動の順に並べる
        run(
            &mut store,
            "reorder_tasks",
            json!({"ids": [b["id"], a["id"]]}),
        );
        let (pos_a, pos_b) = (
            store.task(a["id"].as_str().unwrap()).unwrap().position,
            store.task(b["id"].as_str().unwrap()).unwrap().position,
        );
        assert!(pos_b < pos_a, "{pos_a} {pos_b}");

        run(
            &mut store,
            "update_task",
            json!({"id": a["id"], "categories": ["買い物", "家"]}),
        );
        assert_eq!(
            run(&mut store, "list_categories", json!({})),
            json!(["家", "買い物"])
        );
        assert_eq!(run(&mut store, "list_notices", json!({})), json!([]));
        assert_eq!(run(&mut store, "mark_notices_read", json!({}))["read"], 0);
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

        assert!(
            confirmation_text(&store, "delete_task", &json!({"id": id}), &Shell::Cmd)
                .contains("牛乳を買う")
        );
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
    fn anyones_part_can_be_done_and_undone() {
        let mut store = Store::open_in_memory().unwrap();
        let me = store.create_person("自分", true).unwrap();
        let sato = store.create_person("佐藤", false).unwrap();
        let stranger = store.create_person("田中", false).unwrap();
        let task = run(
            &mut store,
            "create_task",
            json!({"title": "原稿", "assignees": [{"person": me.id}, {"person": "佐藤"}]}),
        );
        let id = task["id"].as_str().unwrap().to_owned();
        assert_eq!(task["assignees"][1]["done"], json!(false));
        // ほかの人の分を名前で完了にする
        let done = run(
            &mut store,
            "set_assignee_done",
            json!({"task_id": id, "person": "佐藤", "done": true}),
        );
        assert_eq!(done["assignees"][1]["done"], json!(true));
        assert_eq!(done["status"], json!("needs_action"));
        // 最後の 1 人でタスクが完了になる
        let all = run(
            &mut store,
            "set_assignee_done",
            json!({"task_id": id, "person": me.id, "done": true}),
        );
        assert_eq!(all["status"], json!("completed"));
        // 再レビューで指摘したら、相手の完了を外す。タスクは進行中に戻り、自分の分は残る
        let back = run(
            &mut store,
            "set_assignee_done",
            json!({"task_id": id, "person": sato.id, "done": false}),
        );
        assert_eq!(back["status"], json!("in_process"));
        assert_eq!(back["assignees"][0]["done"], json!(true));
        assert_eq!(back["assignees"][1]["done"], json!(false));
        assert!(store.task(&id).unwrap().is_done_by(&me.id));
        // 担当でない人・知らない人・正しくない値は、何も変えずに失敗する
        for bad in [
            json!({"task_id": id, "person": stranger.id, "done": true}),
            json!({"task_id": id, "person": "田中", "done": true}),
            json!({"task_id": id, "person": "いない人", "done": true}),
            json!({"task_id": id, "person": "佐藤", "done": "yes"}),
            json!({"task_id": id, "person": "佐藤"}),
            json!({"task_id": "nope", "person": "佐藤", "done": true}),
        ] {
            let error = execute(&mut store, None, "set_assignee_done", &bad).unwrap_err();
            assert!(!error.is_empty(), "{bad}");
        }
        assert!(!store.task(&id).unwrap().is_done_by(&sato.id));
        assert_eq!(store.people().unwrap().len(), 3);
        assert_eq!(
            summary(
                "set_assignee_done",
                &json!({"person": "佐藤", "done": false})
            ),
            "担当者の完了を外す: 佐藤"
        );
    }

    #[test]
    fn done_notices_are_read_and_changed() {
        let mut store = Store::open_in_memory().unwrap();
        store.create_person("自分", true).unwrap();
        let sato = store.create_person("佐藤", false).unwrap();
        let event = store.create_workspace(None, "イベント").unwrap();
        let got = run(&mut store, "get_done_notices", json!({}));
        assert_eq!(got["enabled"], json!(true));
        assert_eq!(got["when"], json!("each"));
        assert_eq!(got["roles"], json!(["R", "A", "C", "I"]));
        let set = run(
            &mut store,
            "set_done_notices",
            json!({"when": "finished", "roles": ["a", "R"], "muted_people": ["佐藤"], "muted_workspaces": [event.id]}),
        );
        assert_eq!(set["when"], json!("finished"));
        assert_eq!(set["roles"], json!(["R", "A"]));
        assert_eq!(
            set["muted_people"],
            json!([{"id": sato.id, "name": "佐藤"}])
        );
        assert_eq!(
            set["muted_workspaces"],
            json!([{"id": event.id, "name": "イベント"}])
        );
        // 渡さない項目は変わらない
        let off = run(&mut store, "set_done_notices", json!({"enabled": false}));
        assert_eq!(off["enabled"], json!(false));
        assert_eq!(off["when"], json!("finished"));
        // 正しくない値では何も変えない
        for bad in [
            json!({"roles": []}),
            json!({"roles": ["X"]}),
            json!({"when": "sometimes"}),
            json!({"muted_people": ["いない人"]}),
            json!({"muted_workspaces": ["nope"]}),
            json!({"enabled": "no", "when": "each"}),
        ] {
            assert!(
                execute(&mut store, None, "set_done_notices", &bad).is_err(),
                "{bad}"
            );
        }
        let after = store.done_notices().unwrap();
        assert!(!after.enabled && after.muted_people == [sato.id.clone()]);
        // [] で全員・全部に戻す
        let all = run(
            &mut store,
            "set_done_notices",
            json!({"enabled": true, "when": "each", "roles": ["R", "A", "C", "I"], "muted_people": [], "muted_workspaces": []}),
        );
        assert_eq!(all["muted_people"], json!([]));
        assert!(store.done_notices().unwrap().is_standard());
    }

    #[test]
    fn workspaces_are_reordered_by_index() {
        let mut store = Store::open_in_memory().unwrap();
        let a = store.create_workspace(None, "A").unwrap();
        let b = store.create_workspace(None, "B").unwrap();
        let c = store.create_workspace(None, "C").unwrap();
        let names = |store: &Store, parent: Option<&str>| -> Vec<String> {
            let mut list: Vec<Workspace> = store
                .workspaces()
                .unwrap()
                .into_iter()
                .filter(|w| w.parent_id.as_deref() == parent)
                .collect();
            list.sort_by_key(|w| w.position);
            list.into_iter().map(|w| w.name).collect()
        };
        run(
            &mut store,
            "update_workspace",
            json!({"id": c.id, "index": 0}),
        );
        assert_eq!(names(&store, None), ["C", "A", "B"]);
        // 親と並び順を一緒に変える
        run(
            &mut store,
            "update_workspace",
            json!({"id": b.id, "parent_id": a.id, "index": 0}),
        );
        assert_eq!(names(&store, None), ["C", "A"]);
        assert_eq!(names(&store, Some(&a.id)), ["B"]);
        run(
            &mut store,
            "update_workspace",
            json!({"id": c.id, "parent_id": a.id, "index": 0}),
        );
        assert_eq!(names(&store, Some(&a.id)), ["C", "B"]);
        assert!(execute(
            &mut store,
            None,
            "update_workspace",
            &json!({"id": a.id, "index": -1})
        )
        .is_err());
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
        let wsl = Shell::Wsl {
            distro: "Ubuntu".into(),
        };
        let text = confirmation_text(&store, RUN_SHELL, &json!({"command": "ls -la"}), &wsl);
        assert_eq!(text, "次のコマンドを WSL の Ubuntu で実行します。\nls -la");
        assert!(confirmation_text(&store, "delete_task", &json!({"id": "x"}), &wsl).contains("x"));
    }

    #[test]
    fn the_shell_is_named_with_each_message() {
        let pwsh = Shell::PowerShell {
            program: "pwsh.exe".into(),
            legacy: false,
        };
        let note = shell_note(&pwsh, false);
        assert!(note.starts_with("run_shell のコマンドは PowerShell 7 で動きます。"));
        assert!(note.contains("bash や cmd の書き方は使えません"));
        let note = shell_note(
            &Shell::Wsl {
                distro: "Ubuntu".into(),
            },
            true,
        );
        assert!(note.contains(
            "WSL の Ubuntu（ユーザーが今開いているターミナルのタブと同じシェル）で動きます"
        ));
    }

    #[test]
    fn the_system_prompt_stays_the_same_between_requests() {
        let prompt = system_prompt("");
        assert!(prompt.contains("github_fetch"));
        // シェルはメッセージごとに書くので、最初の指示には入れない。聞き返さない・言い訳しない決まり
        assert!(prompt.contains("メッセージの最後にアプリが書きます"));
        assert!(!prompt.contains("PowerShell"));
        assert!(prompt.contains("聞き返さずに実行"));
        // アプリの操作は、言い訳せずにツールで行う
        assert!(prompt.contains("どれもツールで実行"));
        assert!(prompt.contains("get_customization"));
        assert!(prompt.contains("途中の失敗"));
        assert!(!prompt.contains("前提と指示"));
        let prompt = system_prompt(" PR は必ずレビューする \n");
        assert!(prompt.ends_with("いつも従ってください。\nPR は必ずレビューする"));
        assert!(prompt.contains("create_ai_rule"));

        // 日付と今の画面は、メッセージに添える
        let note = turn_note("2026-11-24", "ユーザーは「家」を開いています。", "");
        assert!(note.contains("2026-11-24") && note.ends_with("「家」を開いています。"));
        assert!(turn_note("2026-11-24", "", "敬語で").ends_with("です。\n敬語で"));
    }
}
