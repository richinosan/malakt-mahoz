use serde::{Deserialize, Serialize};

/// 小文字の ULID。
pub type Id = String;

/// 新しい ID。同じミリ秒の中で続けて作っても、作った順に大きくなる。
/// 変更履歴などを「時刻、ID」の順に並べたとき、作った順が保たれる。
pub fn new_id() -> Id {
    use std::sync::Mutex;
    static GENERATOR: Mutex<ulid::Generator> = Mutex::new(ulid::Generator::new());
    let mut generator = GENERATOR.lock().unwrap_or_else(|e| e.into_inner());
    generator
        .generate()
        // 同じミリ秒で乱数部分があふれたとき（まず起きない）は、順序を保てない普通の ID にする
        .unwrap_or_else(|_| ulid::Ulid::new())
        .to_string()
        .to_lowercase()
}

/// UNIX 時刻（ミリ秒）。
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// タスクをまとめる単位。`parent_id` で入れ子にする。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: Id,
    pub parent_id: Option<Id>,
    pub name: String,
    pub position: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 担当者。自分も、アカウントを持たない人も同じ扱い。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub id: Id,
    pub name: String,
    pub is_self: bool,
    pub note: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// iCalendar の VTODO の STATUS と同じ 4 つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    NeedsAction,
    InProcess,
    Completed,
    Cancelled,
}

impl Status {
    pub const ALL: [Status; 4] = [
        Status::NeedsAction,
        Status::InProcess,
        Status::Completed,
        Status::Cancelled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Status::NeedsAction => "needs_action",
            Status::InProcess => "in_process",
            Status::Completed => "completed",
            Status::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::NeedsAction => "未着手",
            Status::InProcess => "進行中",
            Status::Completed => "完了",
            Status::Cancelled => "中止",
        }
    }

    pub fn is_open(self) -> bool {
        matches!(self, Status::NeedsAction | Status::InProcess)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: Id,
    /// 入っているワークスペース。`None` はどこにも入れていないタスク。
    pub workspace_id: Option<Id>,
    pub title: String,
    pub description: String,
    pub status: Status,
    /// 0 = なし、1 = 最高 … 9 = 最低（VTODO の PRIORITY と同じ）。
    pub priority: u8,
    /// 開始日。`YYYY-MM-DD`。
    #[serde(default)]
    pub start: Option<String>,
    /// 期限。`YYYY-MM-DD`。
    pub due: Option<String>,
    /// 進捗（0〜100）。完了にすると 100 になる。
    #[serde(default)]
    pub percent: u8,
    /// 親タスク。サブタスクなら入る。
    #[serde(default)]
    pub parent_id: Option<Id>,
    pub assignee_ids: Vec<Id>,
    #[serde(default)]
    pub categories: Vec<String>,
    pub position: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: Id,
    pub task_id: Id,
    pub author_id: Option<Id>,
    pub body: String,
    pub pinned: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// タスクの変更履歴 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub id: Id,
    pub task_id: Id,
    pub field: String,
    pub old_value: String,
    pub new_value: String,
    pub created_at: i64,
}

/// タスクの新規作成・更新で渡す内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDraft {
    pub title: String,
    pub description: String,
    pub status: Status,
    pub priority: u8,
    pub start: Option<String>,
    pub due: Option<String>,
    pub percent: u8,
    pub parent_id: Option<Id>,
    pub assignee_ids: Vec<Id>,
    pub categories: Vec<String>,
}

impl Default for TaskDraft {
    fn default() -> Self {
        Self {
            title: String::new(),
            description: String::new(),
            status: Status::NeedsAction,
            priority: 0,
            start: None,
            due: None,
            percent: 0,
            parent_id: None,
            assignee_ids: Vec::new(),
            categories: Vec::new(),
        }
    }
}

impl From<&Task> for TaskDraft {
    fn from(t: &Task) -> Self {
        Self {
            title: t.title.clone(),
            description: t.description.clone(),
            status: t.status,
            priority: t.priority,
            start: t.start.clone(),
            due: t.due.clone(),
            percent: t.percent,
            parent_id: t.parent_id.clone(),
            assignee_ids: t.assignee_ids.clone(),
            categories: t.categories.clone(),
        }
    }
}

/// どこのタスクを見るか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    /// すべて
    All,
    /// どのワークスペースにも入れていないもの
    Unfiled,
    Workspace(&'a str),
}

impl<'a> From<&'a str> for Scope<'a> {
    fn from(id: &'a str) -> Self {
        Scope::Workspace(id)
    }
}

impl<'a> From<&'a String> for Scope<'a> {
    fn from(id: &'a String) -> Self {
        Scope::Workspace(id)
    }
}

/// 一覧の絞り込み。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskFilter {
    /// 子孫のワークスペースのタスクも含める。
    pub include_descendants: bool,
    /// 完了・中止を除く。
    pub open_only: bool,
    pub assignee_id: Option<Id>,
    /// タイトルと説明の部分一致。
    pub query: Option<String>,
}

/// `YYYY-MM-DD` として正しい日付か。
pub fn is_valid_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let num = |r: std::ops::Range<usize>| s.get(r).and_then(|v| v.parse::<u32>().ok());
    let (Some(y), Some(m), Some(d)) = (num(0..4), num(5..7), num(8..10)) else {
        return false;
    };
    if !b
        .iter()
        .enumerate()
        .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        return false;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let max = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=max).contains(&d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_lowercase_and_unique() {
        let a = new_id();
        let b = new_id();
        assert_eq!(a.len(), 26);
        assert_eq!(a, a.to_lowercase());
        assert_ne!(a, b);
    }

    #[test]
    fn ids_grow_in_creation_order() {
        // 時計が粗い OS では、同じミリ秒の中でいくつも作られる
        let ids: Vec<Id> = (0..2000).map(|_| new_id()).collect();
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn status_round_trips() {
        for s in Status::ALL {
            assert_eq!(Status::parse(s.as_str()), Some(s));
        }
        assert_eq!(Status::parse("done"), None);
        assert!(Status::NeedsAction.is_open());
        assert!(Status::InProcess.is_open());
        assert!(!Status::Completed.is_open());
        assert!(!Status::Cancelled.is_open());
    }

    #[test]
    fn dates_are_validated() {
        assert!(is_valid_date("2026-11-24"));
        assert!(is_valid_date("2024-02-29"));
        assert!(is_valid_date("2000-02-29"));
        assert!(!is_valid_date("2026-02-29"));
        assert!(!is_valid_date("1900-02-29"));
        assert!(!is_valid_date("2026-13-01"));
        assert!(!is_valid_date("2026-00-10"));
        assert!(!is_valid_date("2026-04-31"));
        assert!(!is_valid_date("2026-04-00"));
        assert!(!is_valid_date("2026/04/01"));
        assert!(!is_valid_date("2026-4-1"));
        assert!(!is_valid_date("+026-04-01"));
        assert!(!is_valid_date("2026-04-01 "));
    }
}
