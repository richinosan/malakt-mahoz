//! タスクの変更履歴を、画面に出す形にする。
//!
//! 変更履歴の行は項目ごとに 1 行ある。同じ時刻に同じ人が変えた行を 1 回の変更（[`Entry`]）にまとめ、
//! 項目ごとの差分（[`FieldChange`]）と、コメント欄に挟む出来事の文（[`Event`]）を作る。

use crate::actor::Actor;
use crate::diff::{self, Hunk, Part, PartKind};
use crate::model::{Change, Roles, Status, CREATED_FIELD};
use crate::view::Priority;

/// 名前を引くための、画面側の知識。
pub trait Names {
    /// 担当者の名前。いなくなった人は、そうと分かる文言。
    fn person(&self, id: &str) -> String;
    /// ワークスペースの名前。
    fn workspace(&self, id: &str) -> String;
    /// タスクの題。消したタスクは、そうと分かる文言。
    fn task(&self, id: &str) -> String;
    /// 自分の担当者の ID。
    fn self_id(&self) -> Option<&str>;
}

/// 1 回の変更。同じ時刻に同じ人が変えた項目のまとまり。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// 画面の中で、この変更を指す名前（最初の行の ID）。
    pub id: String,
    pub at: i64,
    pub actor: Actor,
    /// タスクを作ったときの記録か。項目は作ったときの値。
    pub created: bool,
    pub changes: Vec<Change>,
}

/// 作成の記録の行に入っている、作ったときの値。空からの変更の形にする。
pub fn created_values(c: &Change) -> Vec<Change> {
    let Ok(serde_json::Value::Object(values)) = serde_json::from_str(&c.new_value) else {
        return Vec::new();
    };
    values
        .into_iter()
        .filter_map(|(field, value)| {
            Some(Change {
                id: format!("{}#{field}", c.id),
                task_id: c.task_id.clone(),
                old_value: String::new(),
                new_value: value.as_str()?.to_owned(),
                field,
                created_at: c.created_at,
                actor: c.actor.clone(),
            })
        })
        .collect()
}

/// 古い順の変更履歴を、1 回の変更ごとにまとめる。古い順のまま返す。
pub fn entries(changes: &[Change]) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for c in changes {
        let created = c.field == CREATED_FIELD;
        match out.last_mut() {
            Some(last)
                if !created
                    && !last.created
                    && last.at == c.created_at
                    && last.actor == c.actor =>
            {
                last.changes.push(c.clone());
            }
            _ => out.push(Entry {
                id: c.id.clone(),
                at: c.created_at,
                actor: c.actor.clone(),
                created,
                changes: if created {
                    created_values(c)
                } else {
                    vec![c.clone()]
                },
            }),
        }
    }
    out
}

/// いちばん古い記録が作成でなければ、それより前の変更は分からない。
pub fn misses_older(entries: &[Entry]) -> bool {
    entries.first().is_some_and(|e| !e.created)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Added,
    Removed,
    Changed,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Added => "追加",
            Kind::Removed => "削除",
            Kind::Changed => "変更",
        }
    }
}

/// 1 つの項目の差分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldChange {
    pub field: String,
    pub label: &'static str,
    pub kind: Kind,
    /// 変更前と変更後の値。ないときは空。
    pub before: String,
    pub after: String,
    /// 変わったところの強調。`kind` が変更のときだけ入る。
    pub before_parts: Vec<Part>,
    pub after_parts: Vec<Part>,
    /// 長い文（説明）の、行ごとの差分。入っていれば `before_parts` などの代わりに使う。
    pub hunks: Vec<Hunk>,
}

/// 項目の名前。
pub fn field_label(field: &str) -> &'static str {
    match field {
        "title" => "タイトル",
        "description" => "説明",
        "status" => "状態",
        "priority" => "優先度",
        "start" => "開始日",
        "due" => "期限",
        "percent" => "進捗",
        "parent" => "親タスク",
        "assignees" => "担当",
        "assignee_roles" => "担当者の役",
        "assignee_done" => "担当者ごとの完了",
        "categories" => "カテゴリ",
        "urls" => "URL",
        "workspace" => "ワークスペース",
        "link" => "表示するワークスペース",
        crate::repeat::FIELD => "繰り返し",
        crate::repeat::AI_FIELD => "繰り返しの AI への指示",
        _ => "その他",
    }
}

/// 項目の並び順（タイトルが先、説明はあと）。
fn field_order(field: &str) -> usize {
    const ORDER: [&str; 17] = [
        "title",
        "urls",
        "status",
        "assignee_done",
        "assignees",
        "assignee_roles",
        "due",
        "start",
        "priority",
        "categories",
        "parent",
        "percent",
        "workspace",
        "link",
        crate::repeat::FIELD,
        crate::repeat::AI_FIELD,
        "description",
    ];
    ORDER
        .iter()
        .position(|f| *f == field)
        .unwrap_or(ORDER.len())
}

fn list(value: &str) -> Vec<&str> {
    value.split(',').filter(|v| !v.is_empty()).collect()
}

/// URL の並び。URL には「,」が入るので、改行で区切ってある。
fn url_list(value: &str) -> Vec<&str> {
    value.split('\n').filter(|v| !v.is_empty()).collect()
}

/// 期限・開始の値（`YYYY-MM-DD` と、時刻つきなら ` HH:MM`）を、年つきの日付にする。
pub fn date_text(value: &str) -> String {
    value.replace('-', "/")
}

/// 期限・開始を、出来事の文に入れる形にする。その年のものは年を省く。
fn short_date(value: &str, at: i64) -> String {
    let year = crate::view::format_datetime(at)
        .get(..4)
        .unwrap_or_default()
        .to_owned();
    let (date, time) = value.split_once(' ').unwrap_or((value, ""));
    let mut parts = date.split('-');
    let (y, m, d) = (parts.next(), parts.next(), parts.next());
    let date = match (y, m, d) {
        (Some(y), Some(m), Some(d)) => {
            let (m, d) = (m.trim_start_matches('0'), d.trim_start_matches('0'));
            if y == year {
                format!("{m}/{d}")
            } else {
                format!("{y}/{m}/{d}")
            }
        }
        _ => date.to_owned(),
    };
    if time.is_empty() {
        date
    } else {
        format!("{date} {time}")
    }
}

/// 1 つの値を、項目に合わせて表示の文字にする。値がなければ空。
fn show(field: &str, value: &str, names: &dyn Names) -> String {
    if value.is_empty() {
        return String::new();
    }
    match field {
        "status" => Status::parse(value).map_or_else(|| value.to_owned(), |s| s.label().into()),
        "priority" => match Priority::of(value.parse().unwrap_or(0)) {
            Priority::None => String::new(),
            p => p.label().into(),
        },
        "percent" => match value.parse::<u8>().unwrap_or(0) {
            0 => String::new(),
            n => format!("{n}%"),
        },
        "start" | "due" => date_text(value),
        "assignees" => list(value)
            .into_iter()
            .map(|id| names.person(id))
            .collect::<Vec<_>>()
            .join("、"),
        "categories" => list(value).join("、"),
        "urls" => url_list(value).join("\n"),
        "workspace" | "link" => names.workspace(value),
        "parent" => names.task(value),
        _ => value.to_owned(),
    }
}

/// 並びの値で、相手側にないものだけを強調する。
fn list_parts(items: &[String], other: &[String], highlight: PartKind, sep: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            parts.push(Part::new(PartKind::Equal, sep));
        }
        let kind = if other.contains(item) {
            PartKind::Equal
        } else {
            highlight
        };
        parts.push(Part::new(kind, item.clone()));
    }
    parts
}

/// 1 行の変更履歴の差分。見た目が変わらないものは `None`。
pub fn field_change(c: &Change, names: &dyn Names) -> Option<FieldChange> {
    let field = c.field.as_str();
    let label = field_label(field);
    let (before, after) = match field {
        // 「人=R,A」の形
        "assignee_roles" => {
            let side = |value: &str| match value.split_once('=') {
                Some((person, roles)) => format!(
                    "{}（{}）",
                    names.person(person),
                    Roles::parse(roles).labels()
                ),
                None => value.to_owned(),
            };
            (side(&c.old_value), side(&c.new_value))
        }
        // 完了にした人の ID が新しい側、戻した人の ID が古い側に入る
        "assignee_done" => {
            let person = if c.old_value.is_empty() {
                &c.new_value
            } else {
                &c.old_value
            };
            let name = names.person(person);
            let state = |done: bool| format!("{name}：{}", if done { "完了" } else { "未完了" });
            (
                state(!c.old_value.is_empty()),
                state(!c.new_value.is_empty()),
            )
        }
        _ => (
            show(field, &c.old_value, names),
            show(field, &c.new_value, names),
        ),
    };
    if before == after {
        return None;
    }
    let kind = if before.is_empty() {
        Kind::Added
    } else if after.is_empty() {
        Kind::Removed
    } else {
        Kind::Changed
    };
    let mut change = FieldChange {
        field: c.field.clone(),
        label,
        kind,
        before,
        after,
        before_parts: Vec::new(),
        after_parts: Vec::new(),
        hunks: Vec::new(),
    };
    if kind != Kind::Changed {
        return Some(change);
    }
    match field {
        "description" => {
            change.hunks = diff::hunks(&change.before, &change.after);
        }
        "assignees" | "categories" | "urls" => {
            let items = |value: &str| -> Vec<String> {
                let values = if field == "urls" {
                    url_list(value)
                } else {
                    list(value)
                };
                values
                    .into_iter()
                    .map(|v| {
                        if field == "assignees" {
                            names.person(v)
                        } else {
                            v.to_owned()
                        }
                    })
                    .collect()
            };
            let (old, new) = (items(&c.old_value), items(&c.new_value));
            let sep = if field == "urls" { "\n" } else { "、" };
            change.before_parts = list_parts(&old, &new, PartKind::Delete, sep);
            change.after_parts = list_parts(&new, &old, PartKind::Insert, sep);
        }
        _ => {
            let (before, after) = diff::inline(&change.before, &change.after);
            change.before_parts = before;
            change.after_parts = after;
        }
    }
    Some(change)
}

/// 1 回の変更の、項目ごとの差分。作ったときの記録は、値が入っている項目を「追加」として返す。
pub fn field_changes(entry: &Entry, names: &dyn Names) -> Vec<FieldChange> {
    let mut changes: Vec<FieldChange> = entry
        .changes
        .iter()
        .filter_map(|c| field_change(c, names))
        .collect();
    changes.sort_by_key(|c| field_order(&c.field));
    changes
}

/// コメント欄に挟む、変更の出来事 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// 画面の中で、この出来事を指す名前。
    pub key: String,
    pub at: i64,
    pub actor: Actor,
    pub icon: &'static str,
    /// 主語のあとに続く文（「タイトルを「旧」から「新」に変更しました」）。
    pub text: String,
    /// 説明の変更。差分は変更履歴で見る。
    pub opens_history: bool,
}

fn quote(text: &str) -> String {
    format!("「{text}」")
}

fn person_ref(name: &str) -> String {
    format!("{name} さん")
}

/// 状態を変えた出来事の動詞。
pub fn status_verb(before: Option<Status>, after: Status) -> &'static str {
    match (before, after) {
        (_, Status::Completed) => "完了にしました",
        (_, Status::Cancelled) => "中止にしました",
        (Some(Status::InProcess), Status::NeedsAction) => "未着手に戻しました",
        (_, Status::NeedsAction) => "再開しました",
        (_, Status::InProcess) => "進行中にしました",
    }
}

/// 状態を変えたコメント（時刻と、変えた状態）。同じ出来事をコメントの側でも出すので、変更の側では出さない。
pub type StatusComment = (i64, Status);

/// コメントと同時の状態の変更とみなす時間の幅（ミリ秒）。状態を変えてから、コメントを書く。
const WITH_COMMENT_MS: i64 = 2_000;

/// 1 回の変更から、出来事を作る。作ったときの記録は「作成しました」だけにする。
pub fn events(entry: &Entry, names: &dyn Names, status_comments: &[StatusComment]) -> Vec<Event> {
    let mut out = Vec::new();
    let mut add = |field: &str, icon: &'static str, text: String, opens_history: bool| {
        out.push(Event {
            key: format!("{}#{field}", entry.id),
            at: entry.at,
            actor: entry.actor.clone(),
            icon,
            text,
            opens_history,
        });
    };
    if entry.created {
        add("created", "plus", "このタスクを作成しました".into(), false);
        return out;
    }
    let mut changes: Vec<&Change> = entry.changes.iter().collect();
    changes.sort_by_key(|c| field_order(&c.field));
    let status_change = changes.iter().find(|c| c.field == "status");
    let involves_completed = status_change.is_some_and(|c| {
        c.old_value == Status::Completed.as_str() || c.new_value == Status::Completed.as_str()
    });
    let me = entry.actor.person(names.self_id());
    for c in changes {
        let (old, new) = (c.old_value.as_str(), c.new_value.as_str());
        match c.field.as_str() {
            "title" => {
                let text = if old.is_empty() {
                    format!("タイトルを{}に設定しました", quote(new))
                } else if new.is_empty() {
                    "タイトルを空にしました".into()
                } else {
                    format!("タイトルを{}から{}に変更しました", quote(old), quote(new))
                };
                add("title", "pen", text, false);
            }
            "status" => {
                let Some(after) = Status::parse(new) else {
                    continue;
                };
                let with_comment = status_comments.iter().any(|(at, status)| {
                    *status == after && (0..=WITH_COMMENT_MS).contains(&(at - entry.at))
                });
                if !with_comment {
                    let icon = match after {
                        Status::Completed => "circle-check",
                        Status::Cancelled => "circle-x",
                        Status::NeedsAction => "refresh-cw",
                        Status::InProcess => "circle-dot",
                    };
                    add(
                        "status",
                        icon,
                        status_verb(Status::parse(old), after).into(),
                        false,
                    );
                }
            }
            "assignee_done" => {
                let (person, done) = if old.is_empty() {
                    (new, true)
                } else {
                    (old, false)
                };
                let whose = if me == Some(person) {
                    "自分".to_owned()
                } else {
                    person_ref(&names.person(person))
                };
                if done {
                    add(
                        "assignee_done",
                        "circle-check",
                        format!("{whose}の分を完了にしました"),
                        false,
                    );
                } else {
                    add(
                        "assignee_done",
                        "refresh-cw",
                        format!("{whose}の分を未完了に戻しました"),
                        false,
                    );
                }
            }
            "assignees" => {
                let (before, after) = (list(old), list(new));
                let names_of = |ids: Vec<&&str>| {
                    ids.into_iter()
                        .map(|id| person_ref(&names.person(id)))
                        .collect::<Vec<_>>()
                        .join("、")
                };
                let added: Vec<&&str> = after.iter().filter(|id| !before.contains(id)).collect();
                let removed: Vec<&&str> = before.iter().filter(|id| !after.contains(id)).collect();
                if !added.is_empty() {
                    let text = format!("{}を担当に追加しました", names_of(added));
                    add("assignees-added", "user", text, false);
                }
                if !removed.is_empty() {
                    let text = format!("{}を担当から外しました", names_of(removed));
                    add("assignees-removed", "user", text, false);
                }
            }
            "assignee_roles" => {
                let (Some((person, before)), Some((_, after))) =
                    (old.split_once('='), new.split_once('='))
                else {
                    continue;
                };
                add(
                    "assignee_roles",
                    "users",
                    format!(
                        "{}の役を{}から{}に変更しました",
                        person_ref(&names.person(person)),
                        quote(&Roles::parse(before).labels()),
                        quote(&Roles::parse(after).labels())
                    ),
                    false,
                );
            }
            field @ ("due" | "start") => {
                let label = field_label(field);
                let text = if new.is_empty() {
                    format!("{label}を外しました")
                } else {
                    let verb = if old.is_empty() { "設定" } else { "変更" };
                    format!("{label}を {} に{verb}しました", short_date(new, entry.at))
                };
                add(field, "calendar-days", text, false);
            }
            "priority" => {
                let label = |v: &str| match Priority::of(v.parse().unwrap_or(0)) {
                    Priority::None => None,
                    p => Some(p.label()),
                };
                let text = match (label(old), label(new)) {
                    (_, None) => "優先度を外しました".to_owned(),
                    (None, Some(p)) => format!("優先度を{}に設定しました", quote(p)),
                    (Some(_), Some(p)) => format!("優先度を{}に変更しました", quote(p)),
                };
                add("priority", "flag", text, false);
            }
            "categories" => {
                let (before, after) = (list(old), list(new));
                let quoted = |items: Vec<&&str>| {
                    items
                        .into_iter()
                        .map(|c| quote(c))
                        .collect::<Vec<_>>()
                        .join("")
                };
                let added: Vec<&&str> = after.iter().filter(|c| !before.contains(c)).collect();
                let removed: Vec<&&str> = before.iter().filter(|c| !after.contains(c)).collect();
                if !added.is_empty() {
                    let text = format!("カテゴリ{}を追加しました", quoted(added));
                    add("categories-added", "tag", text, false);
                }
                if !removed.is_empty() {
                    let text = format!("カテゴリ{}を外しました", quoted(removed));
                    add("categories-removed", "tag", text, false);
                }
            }
            "urls" => {
                let (before, after) = (url_list(old), url_list(new));
                let quoted = |items: Vec<&&str>| {
                    items
                        .into_iter()
                        .map(|u| quote(u))
                        .collect::<Vec<_>>()
                        .join("")
                };
                let added: Vec<&&str> = after.iter().filter(|u| !before.contains(u)).collect();
                let removed: Vec<&&str> = before.iter().filter(|u| !after.contains(u)).collect();
                if !added.is_empty() {
                    let text = format!("URL {}を追加しました", quoted(added));
                    add("urls-added", "link", text, false);
                }
                if !removed.is_empty() {
                    let text = format!("URL {}を外しました", quoted(removed));
                    add("urls-removed", "link", text, false);
                }
            }
            "parent" => {
                let text = if new.is_empty() {
                    "親タスクを外しました".to_owned()
                } else {
                    let verb = if old.is_empty() { "設定" } else { "変更" };
                    format!("親タスクを{}に{verb}しました", quote(&names.task(new)))
                };
                add("parent", "corner-down-right", text, false);
            }
            // 完了にする・完了から戻すときの進捗は、状態の変更に伴うものなので出さない
            "percent" if !involves_completed => {
                let n = new.parse::<u8>().unwrap_or(0);
                add(
                    "percent",
                    "sliders-horizontal",
                    format!("進捗を {n}% に変更しました"),
                    false,
                );
            }
            "workspace" => {
                let text = if new.is_empty() {
                    "どのワークスペースにも入れない状態にしました".to_owned()
                } else {
                    format!("{}に移しました", quote(&names.workspace(new)))
                };
                add("workspace", "folder", text, false);
            }
            "link" => {
                let text = if old.is_empty() {
                    format!("{}にも表示しました", quote(&names.workspace(new)))
                } else {
                    format!("{}での表示をやめました", quote(&names.workspace(old)))
                };
                add("link", "layers", text, false);
            }
            "description" => {
                let text = if old.trim().is_empty() {
                    "説明を追加しました"
                } else if new.trim().is_empty() {
                    "説明を削除しました"
                } else {
                    "説明を編集しました"
                };
                add("description", "pen", text.into(), true);
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;

    impl Names for Fake {
        fn person(&self, id: &str) -> String {
            match id {
                "me" => "かなで".into(),
                "p1" => "佐藤".into(),
                "p2" => "鈴木".into(),
                _ => "（いなくなった担当者）".into(),
            }
        }
        fn workspace(&self, id: &str) -> String {
            format!("WS-{id}")
        }
        fn task(&self, id: &str) -> String {
            format!("T-{id}")
        }
        fn self_id(&self) -> Option<&str> {
            Some("me")
        }
    }

    fn change(id: &str, at: i64, actor: Actor, field: &str, old: &str, new: &str) -> Change {
        Change {
            id: id.into(),
            task_id: "t".into(),
            field: field.into(),
            old_value: old.into(),
            new_value: new.into(),
            created_at: at,
            actor,
        }
    }

    fn texts(entry: &Entry, comments: &[StatusComment]) -> Vec<String> {
        events(entry, &Fake, comments)
            .into_iter()
            .map(|e| e.text)
            .collect()
    }

    #[test]
    fn groups_by_time_and_actor() {
        let ai = Actor::ai("Claude", None);
        let all = [
            change("1", 10, Actor::Me, CREATED_FIELD, "", r#"{"title":"下見"}"#),
            change("2", 10, Actor::Me, "title", "下見", "会場の"),
            change("3", 20, Actor::Me, "title", "会場の", "会場の下見"),
            change("4", 20, Actor::Me, "due", "", "2026-11-30"),
            change("5", 20, ai.clone(), "priority", "0", "1"),
            change("6", 30, Actor::Me, "status", "needs_action", "completed"),
        ];
        let entries = entries(&all);
        assert_eq!(entries.len(), 5, "作ったのと同じ時刻の変更も、別の 1 回");
        assert!(entries[0].created);
        assert_eq!(entries[0].changes.len(), 1, "作ったときの値");
        assert_eq!(entries[0].changes[0].new_value, "下見");
        assert_eq!(entries[2].changes.len(), 2);
        assert_eq!(entries[3].actor, ai);
        assert!(!misses_older(&entries));
        assert!(misses_older(&entries[1..]));
        assert!(!misses_older(&[]));
    }

    #[test]
    fn a_new_creation_starts_its_own_entry() {
        let all = [
            change("1", 10, Actor::Me, "title", "a", "b"),
            change("2", 10, Actor::Me, CREATED_FIELD, "", ""),
        ];
        assert_eq!(entries(&all).len(), 2);
    }

    #[test]
    fn field_changes_have_kinds_and_highlights() {
        let entry = &entries(&[
            change("1", 20, Actor::Me, "title", "会場の下見", "会場の予約"),
            change("2", 20, Actor::Me, "due", "", "2026-11-30 18:00"),
            change("3", 20, Actor::Me, "priority", "5", "0"),
            change("4", 20, Actor::Me, "assignees", "p1", "p1,p2"),
            change(
                "5",
                20,
                Actor::Me,
                "description",
                "1\n2\n3",
                "1\n2を直す\n3",
            ),
            change("6", 20, Actor::Me, "percent", "0", "0"),
        ])[0];
        let changes = field_changes(entry, &Fake);
        let get = |field: &str| changes.iter().find(|c| c.field == field).unwrap();
        assert_eq!(changes.len(), 5, "見た目が変わらない進捗は出さない");
        assert_eq!(changes[0].field, "title", "タイトルが先");
        assert_eq!(changes.last().unwrap().field, "description", "説明はあと");
        let title = get("title");
        assert_eq!(title.kind, Kind::Changed);
        assert!(title
            .before_parts
            .contains(&Part::new(PartKind::Delete, "下見")));
        let due = get("due");
        assert_eq!(
            (due.kind, due.after.as_str()),
            (Kind::Added, "2026/11/30 18:00")
        );
        let prio = get("priority");
        assert_eq!((prio.kind, prio.before.as_str()), (Kind::Removed, "中"));
        let people = get("assignees");
        assert_eq!(people.after, "佐藤、鈴木");
        assert!(people
            .after_parts
            .contains(&Part::new(PartKind::Insert, "鈴木")));
        assert!(!people
            .after_parts
            .contains(&Part::new(PartKind::Insert, "佐藤")));
        assert_eq!(get("description").hunks.len(), 1);
    }

    #[test]
    fn roles_and_per_person_completion_read_as_changes() {
        let entry = &entries(&[
            change("1", 20, Actor::Me, "assignee_roles", "p1=R", "p1=R,A"),
            change("2", 20, Actor::Me, "assignee_done", "", "p2"),
        ])[0];
        let changes = field_changes(entry, &Fake);
        assert_eq!(changes[0].before, "鈴木：未完了");
        assert_eq!(changes[0].after, "鈴木：完了");
        assert_eq!(changes[1].after, "佐藤（実行責任者・説明責任者）");
    }

    #[test]
    fn urls_read_as_added_and_removed() {
        let (a, b, c) = (
            "https://a.jp/?x=1,2",
            "https://b.jp/",
            "https://github.com/o/r/pull/1",
        );
        let entry = &entries(&[change(
            "1",
            20,
            Actor::Me,
            "urls",
            &format!("{a}\n{b}"),
            &format!("{a}\n{c}"),
        )])[0];
        assert_eq!(
            texts(entry, &[]),
            [
                format!("URL 「{c}」を追加しました"),
                format!("URL 「{b}」を外しました"),
            ]
        );
        let changes = field_changes(entry, &Fake);
        assert_eq!(changes[0].label, "URL");
        assert_eq!(changes[0].before, format!("{a}\n{b}"));
        // 「,」の入った URL も 1 つのまま
        assert_eq!(changes[0].after_parts[0], Part::new(PartKind::Equal, a));
        assert_eq!(changes[0].after_parts[2], Part::new(PartKind::Insert, c));
    }

    #[test]
    fn creation_is_one_event() {
        let entry = &entries(&[change(
            "1",
            10,
            Actor::Me,
            CREATED_FIELD,
            "",
            r#"{"title":"下見","status":"needs_action","assignees":"p1,p2"}"#,
        )])[0];
        assert_eq!(texts(entry, &[]), ["このタスクを作成しました"]);
        // 履歴には、作ったときの値が追加として出る
        let changes = field_changes(entry, &Fake);
        assert_eq!(changes.len(), 3);
        assert!(changes.iter().all(|c| c.kind == Kind::Added));
        assert_eq!(changes[0].after, "下見");
        assert_eq!(changes[1].after, "未着手");
        // 壊れた JSON でも止まらない
        let broken = change("1", 10, Actor::Me, CREATED_FIELD, "", "{");
        assert!(created_values(&broken).is_empty());
    }

    #[test]
    fn events_read_like_sentences() {
        let entry = &entries(&[
            change("1", 20, Actor::Me, "title", "下見", "会場の下見"),
            change("2", 20, Actor::Me, "status", "in_process", "needs_action"),
            change("3", 20, Actor::Me, "assignees", "p1", "p2"),
            change("4", 20, Actor::Me, "categories", "a", "b"),
            change("5", 20, Actor::Me, "parent", "", "x"),
            change("6", 20, Actor::Me, "priority", "0", "1"),
            change("7", 20, Actor::Me, "workspace", "a", "b"),
            change("8", 20, Actor::Me, "link", "", "c"),
            change("9", 20, Actor::Me, "description", "", "本文"),
            change("10", 20, Actor::Me, "percent", "0", "40"),
            change("11", 20, Actor::Me, "assignee_roles", "p1=R", "p1=A"),
            change("12", 20, Actor::Me, "due", "2026-11-30", ""),
        ])[0];
        assert_eq!(
            texts(entry, &[]),
            [
                "タイトルを「下見」から「会場の下見」に変更しました",
                "未着手に戻しました",
                "鈴木 さんを担当に追加しました",
                "佐藤 さんを担当から外しました",
                "佐藤 さんの役を「実行責任者」から「説明責任者」に変更しました",
                "期限を外しました",
                "優先度を「高」に設定しました",
                "カテゴリ「b」を追加しました",
                "カテゴリ「a」を外しました",
                "親タスクを「T-x」に設定しました",
                "進捗を 40% に変更しました",
                "「WS-b」に移しました",
                "「WS-c」にも表示しました",
                "説明を追加しました",
            ]
        );
        assert!(events(entry, &Fake, &[]).last().unwrap().opens_history);
    }

    #[test]
    fn completion_hides_its_percent_and_a_status_comment_hides_the_status() {
        let entry = &entries(&[
            change("1", 1000, Actor::Me, "status", "needs_action", "completed"),
            change("2", 1000, Actor::Me, "percent", "0", "100"),
        ])[0];
        assert_eq!(texts(entry, &[]), ["完了にしました"]);
        // 完了報告のコメントがあれば、コメントの側で出す
        assert!(texts(entry, &[(1005, Status::Completed)]).is_empty());
        // 別の状態のコメントや、離れた時刻のコメントは関係ない
        assert_eq!(texts(entry, &[(1005, Status::Cancelled)]).len(), 1);
        assert_eq!(texts(entry, &[(9000, Status::Completed)]).len(), 1);
    }

    #[test]
    fn own_share_is_mine() {
        let entry = &entries(&[change("1", 20, Actor::Me, "assignee_done", "", "me")])[0];
        assert_eq!(texts(entry, &[]), ["自分の分を完了にしました"]);
        let entry = &entries(&[change(
            "1",
            20,
            Actor::Ticket(Some("p1".into())),
            "assignee_done",
            "p1",
            "",
        )])[0];
        assert_eq!(texts(entry, &[]), ["自分の分を未完了に戻しました"]);
        let entry = &entries(&[change("1", 20, Actor::Me, "assignee_done", "", "p1")])[0];
        assert_eq!(texts(entry, &[]), ["佐藤 さんの分を完了にしました"]);
    }

    #[test]
    fn dates_drop_the_year_of_the_event() {
        let at = chrono_ms("2026-11-20T12:00:00");
        assert_eq!(short_date("2026-11-30", at), "11/30");
        assert_eq!(short_date("2027-01-05 09:30", at), "2027/1/5 09:30");
        assert_eq!(date_text("2026-11-30 18:00"), "2026/11/30 18:00");
    }

    fn chrono_ms(local: &str) -> i64 {
        use chrono::TimeZone;
        let naive = chrono::NaiveDateTime::parse_from_str(local, "%Y-%m-%dT%H:%M:%S").unwrap();
        chrono::Local
            .from_local_datetime(&naive)
            .single()
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn 項目の名前() {
        for (field, label) in [
            ("title", "タイトル"),
            ("description", "説明"),
            ("status", "状態"),
            ("priority", "優先度"),
            ("start", "開始日"),
            ("due", "期限"),
            ("percent", "進捗"),
            ("parent", "親タスク"),
            ("assignees", "担当"),
            ("assignee_roles", "担当者の役"),
            ("assignee_done", "担当者ごとの完了"),
            ("categories", "カテゴリ"),
            ("urls", "URL"),
            ("workspace", "ワークスペース"),
            ("link", "表示するワークスペース"),
            (crate::repeat::FIELD, "繰り返し"),
            (crate::repeat::AI_FIELD, "繰り返しの AI への指示"),
            ("ない項目", "その他"),
        ] {
            assert_eq!(field_label(field), label, "{field}");
        }
    }

    #[test]
    fn 変更の種類の名前() {
        assert_eq!(Kind::Added.label(), "追加");
        assert_eq!(Kind::Removed.label(), "削除");
        assert_eq!(Kind::Changed.label(), "変更");
    }

    #[test]
    fn 値は項目に合わせて表示の文字になる() {
        let n = &Fake;
        assert_eq!(show("percent", "50", n), "50%");
        assert_eq!(show("percent", "0", n), "");
        assert_eq!(show("categories", "a,b", n), "a、b");
        assert_eq!(
            show("urls", "https://a/x,y\nhttps://b", n),
            "https://a/x,y\nhttps://b"
        );
        assert_eq!(
            show("urls", "https://a\n\nhttps://b\n", n),
            "https://a\nhttps://b"
        );
        assert_eq!(show("workspace", "w1", n), "WS-w1");
        assert_eq!(show("link", "w1", n), "WS-w1");
        assert_eq!(show("parent", "t1", n), "T-t1");
        assert_eq!(show("assignees", "p1,p2", n), "佐藤、鈴木");
        assert_eq!(show("title", "そのまま", n), "そのまま");
        assert_eq!(show("percent", "", n), "");
    }

    #[test]
    fn 一覧の項目は相手側にないものだけを強調する() {
        let change = |field: &str, old: &str, new: &str| {
            field_change(&change_of(field, old, new), &Fake).unwrap()
        };
        let c = change("categories", "a,b", "b,c");
        assert_eq!(
            c.before_parts,
            [
                Part::new(PartKind::Delete, "a"),
                Part::new(PartKind::Equal, "、"),
                Part::new(PartKind::Equal, "b"),
            ]
        );
        assert_eq!(
            c.after_parts,
            [
                Part::new(PartKind::Equal, "b"),
                Part::new(PartKind::Equal, "、"),
                Part::new(PartKind::Insert, "c"),
            ]
        );
        let c = change("urls", "https://a/x,y\nhttps://b", "https://b\nhttps://c");
        assert_eq!(
            c.after_parts,
            [
                Part::new(PartKind::Equal, "https://b"),
                Part::new(PartKind::Equal, "\n"),
                Part::new(PartKind::Insert, "https://c"),
            ]
        );
        assert_eq!(
            c.before_parts[0],
            Part::new(PartKind::Delete, "https://a/x,y")
        );
        let c = change("assignees", "p1", "p1,p2");
        assert_eq!(c.after_parts[0], Part::new(PartKind::Equal, "佐藤"));
        assert_eq!(c.after_parts[2], Part::new(PartKind::Insert, "鈴木"));
    }

    fn change_of(field: &str, old: &str, new: &str) -> Change {
        change("c", 0, Actor::Me, field, old, new)
    }
}
