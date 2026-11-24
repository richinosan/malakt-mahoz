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

/// ワークスペースのアイコンに付けられる色。
pub const WORKSPACE_COLORS: [&str; 8] = [
    "red", "orange", "yellow", "green", "teal", "blue", "purple", "pink",
];

/// タスクをまとめる単位。`parent_id` で入れ子にする。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: Id,
    pub parent_id: Option<Id>,
    pub name: String,
    /// 開催日など、このワークスペースの基準になる日。`YYYY-MM-DD`。
    /// タスク追加時の既定の期限を「この日の何日前」と決めるのに使う。
    #[serde(default)]
    pub date: Option<String>,
    /// 一覧で名前の前に出すアイコンの名前（`folder` など）。`None` なら既定のアイコン。
    #[serde(default)]
    pub icon: Option<String>,
    /// アイコンの色。[`WORKSPACE_COLORS`] のどれか。`None` なら文字と同じ色。
    #[serde(default)]
    pub color: Option<String>,
    pub position: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 既定の期限の決め方。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DueRule {
    /// 期限を入れない。
    #[default]
    None,
    /// ワークスペースの基準日から何日ずらすか（負は前）。基準日がなければ入れない。
    Relative { days: i32 },
    /// 決まった日。
    Fixed { date: String },
}

/// 基準日からずらせる日数の上限。
pub const MAX_DUE_OFFSET_DAYS: i32 = 3650;
/// 既定で一緒に表示するワークスペースの数の上限。
pub const MAX_DEFAULT_LINKS: usize = 20;

/// ワークスペースごとの「タスク追加時の既定値」。状態は常に未着手で、既定値にしない。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDefaults {
    pub workspace_id: Id,
    #[serde(default)]
    pub assignee_ids: Vec<Id>,
    #[serde(default)]
    pub due: DueRule,
    /// `HH:MM`。空なら日付だけ。
    #[serde(default)]
    pub due_time: String,
    /// `None` は指定なし。
    #[serde(default)]
    pub priority: Option<u8>,
    #[serde(default)]
    pub categories: Vec<String>,
    /// 説明の雛形。
    #[serde(default)]
    pub description: String,
    /// 追加したタスクを一緒に表示するワークスペース。
    #[serde(default)]
    pub link_workspace_ids: Vec<Id>,
}

impl TaskDefaults {
    /// 何かを入れる設定か（全部空なら、新しいタスクに何も入れない）。
    pub fn has_any(&self) -> bool {
        !self.assignee_ids.is_empty()
            || self.due != DueRule::None
            || self.priority.is_some()
            || !self.categories.is_empty()
            || !self.description.is_empty()
            || !self.link_workspace_ids.is_empty()
    }
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

/// 担当者の役（RACI）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// 実行責任者
    R,
    /// 説明責任者。1 つのタスクに 1 人まで。
    A,
    /// 相談先
    C,
    /// 報告先
    I,
}

impl Role {
    /// 並び順。複数の役を見せるときもこの順にする。
    pub const ALL: [Role; 4] = [Role::R, Role::A, Role::C, Role::I];

    pub fn code(self) -> &'static str {
        match self {
            Role::R => "R",
            Role::A => "A",
            Role::C => "C",
            Role::I => "I",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.code() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Role::R => "実行責任者",
            Role::A => "説明責任者",
            Role::C => "相談先",
            Role::I => "報告先",
        }
    }

    fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// 担当者 1 人が持つ役の集まり。担当者は必ず 1 つ以上の役を持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Roles(u8);

impl Default for Roles {
    /// 担当者を足したときの役は R。
    fn default() -> Self {
        Roles::R
    }
}

impl Roles {
    pub const R: Roles = Roles(1);
    pub const NONE: Roles = Roles(0);

    pub fn of(roles: &[Role]) -> Self {
        Roles(roles.iter().fold(0, |bits, role| bits | role.bit()))
    }

    pub fn contains(self, role: Role) -> bool {
        self.0 & role.bit() != 0
    }

    pub fn with(self, role: Role) -> Self {
        Roles(self.0 | role.bit())
    }

    pub fn without(self, role: Role) -> Self {
        Roles(self.0 & !role.bit())
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn iter(self) -> impl Iterator<Item = Role> {
        Role::ALL.into_iter().filter(move |r| self.contains(*r))
    }

    /// `R,A` の形を読む。読めない・空のときは R。
    pub fn parse(s: &str) -> Self {
        let roles = Roles(
            s.split(',')
                .filter_map(|code| Role::parse(code.trim()))
                .fold(0, |bits, role| bits | role.bit()),
        );
        if roles.is_empty() {
            Roles::R
        } else {
            roles
        }
    }

    /// 「実行責任者・説明責任者」の形。
    pub fn labels(self) -> String {
        self.iter().map(Role::label).collect::<Vec<_>>().join("・")
    }
}

impl std::fmt::Display for Roles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, role) in self.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            f.write_str(role.code())?;
        }
        Ok(())
    }
}

impl Serialize for Roles {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Roles {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(Roles::parse(&String::deserialize(d)?))
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
    /// 担当者ごとの役。`assignee_ids` と同じ並び。
    #[serde(default)]
    pub assignee_roles: Vec<Roles>,
    /// 担当者ごとの「自分の分」を完了にした時刻。`assignee_ids` と同じ並び。
    #[serde(default)]
    pub assignee_done: Vec<Option<i64>>,
    #[serde(default)]
    pub categories: Vec<String>,
    /// 入っているワークスペースのほかに、このタスクを表示するワークスペース。足した順。
    #[serde(default)]
    pub linked_workspace_ids: Vec<Id>,
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
    /// このコメントと一緒に変えたタスクの状態。完了なら「完了報告」。
    #[serde(default)]
    pub status: Option<Status>,
    /// レビューコメント（スレッドのルート）か。
    #[serde(default)]
    pub review: bool,
    /// 説明の文章のどこに付けたレビューコメントか。
    #[serde(default)]
    pub anchor: Option<Anchor>,
    /// 返信なら、スレッドのルートのコメント。
    #[serde(default)]
    pub reply_to: Option<Id>,
    /// スレッドを解決済みにした時刻。ルートだけが持つ。
    #[serde(default)]
    pub resolved_at: Option<i64>,
    /// 本文で `@名前` と書かれた人。
    #[serde(default)]
    pub mentions: Vec<Id>,
}

impl Comment {
    /// 返信を付けられ、解決済みにできるコメントか。
    pub fn is_thread_root(&self) -> bool {
        self.review && self.reply_to.is_none()
    }

    pub fn is_edited(&self) -> bool {
        self.updated_at != self.created_at
    }

    pub fn is_completion_report(&self) -> bool {
        self.status == Some(Status::Completed)
    }
}

/// レビューコメントを付けた場所。選んだ文字列と、その前後の文脈。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Anchor {
    pub exact: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub suffix: String,
}

/// これから書くコメント。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewComment {
    pub body: String,
    /// 一緒にタスクの状態を変える。
    pub status: Option<Status>,
    /// レビューコメントにする。
    pub review: bool,
    /// 説明の文章の選択範囲に付ける（レビューコメントになる）。
    pub anchor: Option<Anchor>,
    /// このスレッドへの返信にする。
    pub reply_to: Option<Id>,
}

/// タスクに添付したファイル。中身は別に持ち、ここは名前や大きさだけ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: Id,
    pub task_id: Id,
    pub author_id: Option<Id>,
    /// ファイル名（フォルダーは含まない）。
    pub name: String,
    /// 元の大きさ（バイト）。
    pub size: u64,
    /// 中身の SHA-256（16 進）。同じ中身のファイルは 1 つだけ保存する。
    pub hash: String,
    pub created_at: i64,
}

impl Attachment {
    /// 画面にそのまま出せる画像か。拡張子で決める。
    pub fn is_image(&self) -> bool {
        let lower = self.name.to_lowercase();
        ["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg"]
            .iter()
            .any(|ext| lower.ends_with(&format!(".{ext}")))
    }
}

/// 添付できるファイルの大きさ（10 MiB）。
pub const MAX_ATTACHMENT_BYTES: u64 = 10 * 1024 * 1024;
/// 1 つのタスクに添付できる数。
pub const MAX_ATTACHMENTS_PER_TASK: usize = 500;

/// 1 つのタスクにピン留めできるコメントの数。
pub const MAX_PINNED_COMMENTS: usize = 10;
/// 1 つのコメントでメンションできる人数。
pub const MAX_MENTIONS: usize = 50;

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

impl Task {
    /// このタスクが出ているワークスペースすべて。入っている場所が先頭。
    pub fn places(&self) -> impl Iterator<Item = &Id> {
        self.workspace_id.iter().chain(&self.linked_workspace_ids)
    }

    /// そのワークスペースに出ているか。
    pub fn is_in(&self, workspace_id: &str) -> bool {
        self.places().any(|id| id == workspace_id)
    }

    fn assignee_index(&self, person_id: &str) -> Option<usize> {
        self.assignee_ids.iter().position(|id| id == person_id)
    }

    /// その担当者の役。担当者でなければ `None`。
    pub fn roles_of(&self, person_id: &str) -> Option<Roles> {
        let i = self.assignee_index(person_id)?;
        Some(self.assignee_roles.get(i).copied().unwrap_or_default())
    }

    /// その担当者が自分の分を完了にしているか。
    pub fn is_done_by(&self, person_id: &str) -> bool {
        self.assignee_index(person_id)
            .and_then(|i| self.assignee_done.get(i).copied().flatten())
            .is_some()
    }

    /// 自分の分を完了にした人数と、担当者の人数。
    pub fn done_count(&self) -> (usize, usize) {
        let done = (0..self.assignee_ids.len())
            .filter(|&i| self.assignee_done.get(i).copied().flatten().is_some())
            .count();
        (done, self.assignee_ids.len())
    }
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
/// `HH:MM`（24 時間）として正しいか。
pub fn is_valid_time(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 5
        && b[2] == b':'
        && [0, 1, 3, 4].iter().all(|&i| b[i].is_ascii_digit())
        && &s[0..2] <= "23"
        && &s[3..5] <= "59"
}

/// 期限や開始として正しいか。`YYYY-MM-DD` か、時刻つきの `YYYY-MM-DD HH:MM`。
pub fn is_valid_when(s: &str) -> bool {
    match s.split_once(' ') {
        None => is_valid_date(s),
        Some((date, time)) => is_valid_date(date) && is_valid_time(time),
    }
}

/// 期限や開始の、日付の部分。
pub fn date_of(when: &str) -> &str {
    when.split_once(' ').map_or(when, |(date, _)| date)
}

/// 期限や開始の、時刻の部分。なければ `None`。
pub fn time_of(when: &str) -> Option<&str> {
    when.split_once(' ').map(|(_, time)| time)
}

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
    fn roles_parse_and_print_in_fixed_order() {
        let roles = Roles::parse("A, R");
        assert_eq!(roles.to_string(), "R,A");
        assert_eq!(roles.labels(), "実行責任者・説明責任者");
        assert!(roles.contains(Role::A) && !roles.contains(Role::C));
        assert_eq!(roles.without(Role::R).to_string(), "A");
        assert_eq!(Roles::NONE.with(Role::I), Roles::of(&[Role::I]));
        // 読めない・空のときは R
        assert_eq!(Roles::parse(""), Roles::R);
        assert_eq!(Roles::parse("X"), Roles::default());
        // JSON では文字列
        assert_eq!(serde_json::to_string(&roles).unwrap(), "\"R,A\"");
        assert_eq!(
            serde_json::from_str::<Roles>("\"C,I\"")
                .unwrap()
                .to_string(),
            "C,I"
        );
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
        assert!(is_valid_when("2026-11-24"));
        assert!(is_valid_when("2026-11-24 09:30"));
        assert!(is_valid_when("2026-11-24 23:59"));
        assert!(!is_valid_when("2026-11-24 24:00"));
        assert!(!is_valid_when("2026-11-24 9:30"));
        assert!(!is_valid_when("2026-11-24 09:60"));
        assert!(!is_valid_when("2026-11-24T09:30"));
        assert!(!is_valid_when("2026-11-24 "));
        assert!(!is_valid_when("2026-02-30 09:30"));
        assert!(!is_valid_time("１２:００"));
        assert_eq!(date_of("2026-11-24 09:30"), "2026-11-24");
        assert_eq!(date_of("2026-11-24"), "2026-11-24");
        assert_eq!(time_of("2026-11-24 09:30"), Some("09:30"));
        assert_eq!(time_of("2026-11-24"), None);
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
