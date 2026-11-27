//! 一覧の絞り込み・並べ替えや、期限・優先度の見せ方。画面に依存しない。

use std::collections::{HashMap, HashSet};

use crate::model::*;

/// 一覧のタブ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    /// 未完了（未着手・進行中）
    #[default]
    Open,
    /// 完了（完了・中止）
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    #[default]
    Due,
    Priority,
    Updated,
    Created,
    Title,
    Status,
    /// 担当者の名前の順。未割り当ては最後。
    Assignee,
    /// ドラッグで決めた順。
    Manual,
}

impl Sort {
    pub const ALL: [Sort; 8] = [
        Sort::Due,
        Sort::Priority,
        Sort::Updated,
        Sort::Created,
        Sort::Title,
        Sort::Status,
        Sort::Assignee,
        Sort::Manual,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Sort::Due => "期限が近い順",
            Sort::Priority => "優先度が高い順",
            Sort::Updated => "更新が新しい順",
            Sort::Created => "作成が新しい順",
            Sort::Title => "タイトル順",
            Sort::Status => "状態順",
            Sort::Assignee => "担当順",
            Sort::Manual => "手動の順",
        }
    }
}

/// 優先度の区分。値は 0〜9 で持ち、画面では 4 つに丸める。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Priority {
    None,
    High,
    Mid,
    Low,
}

impl Priority {
    pub const ALL: [Priority; 4] = [Priority::None, Priority::High, Priority::Mid, Priority::Low];

    /// 1〜4 は高、5 は中、6〜9 は低、それ以外はなし。
    pub fn of(value: u8) -> Self {
        match value {
            1..=4 => Priority::High,
            5 => Priority::Mid,
            6..=9 => Priority::Low,
            _ => Priority::None,
        }
    }

    /// 保存するときの代表値。
    pub fn value(self) -> u8 {
        match self {
            Priority::None => 0,
            Priority::High => 1,
            Priority::Mid => 5,
            Priority::Low => 9,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Priority::None => "なし",
            Priority::High => "高",
            Priority::Mid => "中",
            Priority::Low => "低",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AssigneeFilter {
    #[default]
    All,
    /// 自分
    Me,
    /// 未割り当て
    Unassigned,
    Person(Id),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ListQuery {
    pub tab: Tab,
    pub q: String,
    pub status: Option<Status>,
    pub assignee: AssigneeFilter,
    /// 自分の役。自分がその役を持つタスクだけにする。
    pub my_role: Option<Role>,
    pub priority: Option<Priority>,
    pub category: Option<String>,
    pub sort: Sort,
    /// 並び順を逆にする。
    pub descending: bool,
}

impl ListQuery {
    /// タブと並び順以外に、効いている絞り込みの数。
    pub fn active_count(&self) -> usize {
        usize::from(!self.q.trim().is_empty())
            + usize::from(self.status.is_some())
            + usize::from(self.assignee != AssigneeFilter::All)
            + usize::from(self.my_role.is_some())
            + usize::from(self.priority.is_some())
            + usize::from(self.category.is_some())
    }

    /// 絞り込みを外す。タブと並び順は残す。
    pub fn clear_filters(&mut self) {
        *self = ListQuery {
            tab: self.tab,
            sort: self.sort,
            descending: self.descending,
            ..Default::default()
        };
    }
}

/// （未完了の数, 完了の数）。
pub fn counts(tasks: &[Task]) -> (usize, usize) {
    let open = tasks.iter().filter(|t| t.status.is_open()).count();
    (open, tasks.len() - open)
}

/// 条件に合うタスクの添え字を、並び順どおりに返す。担当順では担当者の id で比べる。
pub fn apply(tasks: &[Task], query: &ListQuery, self_id: Option<&str>) -> Vec<usize> {
    apply_named(tasks, query, self_id, &|id| id.to_owned())
}

/// [`apply`] と同じ。担当順では `name` で引いた担当者の名前で比べる。
pub fn apply_named(
    tasks: &[Task],
    query: &ListQuery,
    self_id: Option<&str>,
    name: &dyn Fn(&str) -> String,
) -> Vec<usize> {
    let q = crate::text::normalize_line(&query.q).to_lowercase();
    let mut hits: Vec<usize> = tasks
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            let tab_ok = match query.status {
                // 状態を選んだら、タブより状態を優先する
                Some(s) => t.status == s,
                None => t.status.is_open() == (query.tab == Tab::Open),
            };
            tab_ok
                && match &query.assignee {
                    AssigneeFilter::All => true,
                    AssigneeFilter::Me => {
                        self_id.is_some_and(|me| t.assignee_ids.iter().any(|a| a == me))
                    }
                    AssigneeFilter::Unassigned => t.assignee_ids.is_empty(),
                    AssigneeFilter::Person(id) => t.assignee_ids.contains(id),
                }
                && query.my_role.is_none_or(|role| {
                    self_id
                        .and_then(|me| t.roles_of(me))
                        .is_some_and(|roles| roles.contains(role))
                })
                && query.priority.is_none_or(|p| Priority::of(t.priority) == p)
                && query
                    .category
                    .as_ref()
                    .is_none_or(|c| t.categories.contains(c))
                && (q.is_empty()
                    || t.title.to_lowercase().contains(&q)
                    || t.description.to_lowercase().contains(&q))
        })
        .map(|(i, _)| i)
        .collect();
    // 優先度なし（0）と期限なしは最後に回す
    let prio = |t: &Task| if t.priority == 0 { u8::MAX } else { t.priority };
    let due = |t: &Task| (t.due.is_none(), t.due.clone());
    // 担当順の比べる名前。担当の並びどおりに、小文字にして持つ
    let people: HashMap<usize, Vec<String>> = if query.sort == Sort::Assignee {
        hits.iter()
            .map(|&i| {
                let names = tasks[i]
                    .assignee_ids
                    .iter()
                    .map(|id| name(id).to_lowercase())
                    .collect();
                (i, names)
            })
            .collect()
    } else {
        HashMap::new()
    };
    let person = |i: usize| {
        let names = &people[&i];
        (names.is_empty(), names)
    };
    hits.sort_by(|&ia, &ib| {
        let (a, b) = (&tasks[ia], &tasks[ib]);
        let order = match query.sort {
            Sort::Due => due(a).cmp(&due(b)).then(prio(a).cmp(&prio(b))),
            Sort::Priority => prio(a).cmp(&prio(b)).then(due(a).cmp(&due(b))),
            Sort::Updated => b.updated_at.cmp(&a.updated_at),
            Sort::Created => b.created_at.cmp(&a.created_at),
            Sort::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
            Sort::Status => (a.status as u8).cmp(&(b.status as u8)),
            Sort::Assignee => person(ia).cmp(&person(ib)).then(due(a).cmp(&due(b))),
            Sort::Manual => a.position.cmp(&b.position),
        };
        if query.descending {
            order.reverse()
        } else {
            order
        }
        .then_with(|| a.id.cmp(&b.id))
    });
    hits
}

/// 一覧の 1 行。タスクか、サブワークスペースの見出し。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListRow {
    /// `index` は [`Outline::visible`] の中の位置。
    Task { index: usize, depth: usize },
    /// `workspace` はワークスペースの並びの中の位置。`count` は、その下（さらに下のサブワークスペースを含む）で
    /// 条件に合うタスクの数。
    Folder {
        workspace: usize,
        depth: usize,
        count: usize,
        open: bool,
    },
}

/// ワークスペースの一覧を、サブワークスペースを入れ子にして並べたもの。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outline {
    /// 画面に出ているタスクの添え字。上から順。
    pub visible: Vec<usize>,
    /// `visible` と同じ並びで、そのタスクを出しているワークスペース（ワークスペースの並びの中の位置）。
    pub homes: Vec<Option<usize>>,
    pub rows: Vec<ListRow>,
}

impl Outline {
    /// 入れ子にしない一覧。
    pub fn flat(hits: Vec<usize>) -> Self {
        Outline {
            rows: (0..hits.len())
                .map(|index| ListRow::Task { index, depth: 0 })
                .collect(),
            homes: vec![None; hits.len()],
            visible: hits,
        }
    }

    /// `visible` のその位置のタスクが、何行目にあるか。
    pub fn row_of(&self, index: usize) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| matches!(row, ListRow::Task { index: i, .. } if *i == index))
    }
}

/// サブワークスペースを見出しの行として先に、`root` のタスクをその後ろに並べる。開いているサブワークスペース
/// （`open`）は、見出しの下にさらに下のサブワークスペースと中のタスクを出す。
///
/// `hits` は [`apply`] の結果。タスクは、出ているワークスペースのうち上から順にたどって最初の 1 か所にだけ出す。
/// `hide_empty` なら、条件に合うタスクがないサブワークスペースは出さない（絞り込み中）。
pub fn outline(
    tasks: &[Task],
    hits: &[usize],
    workspaces: &[Workspace],
    root: &str,
    open: &HashSet<Id>,
    hide_empty: bool,
) -> Outline {
    let Some(root) = workspaces.iter().position(|w| w.id == root) else {
        return Outline::flat(hits.to_vec());
    };
    let mut children: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, w) in workspaces.iter().enumerate() {
        if let Some(parent) = w.parent_id.as_deref() {
            children.entry(parent).or_default().push(i);
        }
    }
    for list in children.values_mut() {
        list.sort_by_key(|&i| (workspaces[i].position, workspaces[i].id.as_str()));
    }
    let kids = |i: usize| -> &[usize] {
        children
            .get(workspaces[i].id.as_str())
            .map_or(&[], Vec::as_slice)
    };
    // root から上から順にたどった並び（ワークスペース, 深さ, 親の位置）。深い入れ子でもスタックを
    // 使い切らないよう、再帰しない
    let mut order: Vec<(usize, usize, usize)> = Vec::new();
    let mut rank: HashMap<&str, usize> = HashMap::new();
    let mut stack = vec![(root, 0, 0)];
    while let Some((i, depth, up)) = stack.pop() {
        // 親をたどって自分に戻るデータでも、止まるようにする
        if rank.contains_key(workspaces[i].id.as_str()) {
            continue;
        }
        let at = order.len();
        rank.insert(workspaces[i].id.as_str(), at);
        order.push((i, depth, up));
        stack.extend(kids(i).iter().rev().map(|&k| (k, depth + 1, at)));
    }
    // ワークスペースごとのタスク。並びは `hits` のまま
    let mut own: Vec<Vec<usize>> = vec![Vec::new(); order.len()];
    for &hit in hits {
        let at = tasks[hit]
            .places()
            .filter_map(|id| rank.get(id.as_str()).copied())
            .min()
            .unwrap_or(0);
        own[at].push(hit);
    }
    // 下のサブワークスペースを含めた数。`order` では子が親より後ろにあるので、後ろから足し上げる
    let mut total: Vec<usize> = own.iter().map(Vec::len).collect();
    let mut below: Vec<Vec<usize>> = vec![Vec::new(); order.len()];
    for at in (1..order.len()).rev() {
        let up = order[at].2;
        total[up] += total[at];
        below[up].push(at);
    }
    enum Step {
        Folder(usize),
        Tasks(usize),
    }
    let mut out = Outline::default();
    // `below` は後ろから足したので、そのまま積むと上から順に取り出せる
    let mut steps = vec![Step::Tasks(0)];
    steps.extend(below[0].iter().map(|&at| Step::Folder(at)));
    while let Some(step) = steps.pop() {
        match step {
            Step::Folder(at) => {
                let (workspace, depth, _) = order[at];
                let count = total[at];
                if hide_empty && count == 0 {
                    continue;
                }
                let is_open = open.contains(&workspaces[workspace].id);
                out.rows.push(ListRow::Folder {
                    workspace,
                    depth: depth - 1,
                    count,
                    open: is_open,
                });
                if is_open {
                    steps.push(Step::Tasks(at));
                    steps.extend(below[at].iter().map(|&at| Step::Folder(at)));
                }
            }
            Step::Tasks(at) => {
                let (workspace, depth, _) = order[at];
                for &hit in &own[at] {
                    out.rows.push(ListRow::Task {
                        index: out.visible.len(),
                        depth,
                    });
                    out.visible.push(hit);
                    out.homes.push(Some(workspace));
                }
            }
        }
    }
    out
}

/// 期限の状態。期限切れ・今日・近い（3 日以内）を目立たせるのに使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DueState {
    Overdue,
    Today,
    Soon,
    Later,
}

/// 期限が「近い」とする日数。
pub const DUE_SOON_DAYS: i64 = 3;
/// ホームの「期限が近いタスク」に出す日数。
pub const HOME_DUE_DAYS: i64 = 30;

/// 期限の近さ。`now` は今日の日付か、時刻つきのいま（`YYYY-MM-DD HH:MM`）。
/// 時刻つきの期限は、いまの時刻も分かるときだけ、今日のうちに期限切れになる。
pub fn due_state(due: &str, now: &str) -> DueState {
    let (day, today) = (crate::date_of(due), crate::date_of(now));
    if day < today {
        DueState::Overdue
    } else if day == today {
        match (crate::time_of(due), crate::time_of(now)) {
            (Some(at), Some(clock)) if at < clock => DueState::Overdue,
            _ => DueState::Today,
        }
    } else if day <= add_days(today, DUE_SOON_DAYS).as_str() {
        DueState::Soon
    } else {
        DueState::Later
    }
}

/// 今日の日付（端末の時刻帯）。`YYYY-MM-DD`。
pub fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// `YYYY-MM-DD` に日数を足す。暦の上の計算なので時差の影響を受けない。
/// 日付として読めなければ、そのまま返す。
pub fn add_days(date: &str, days: i64) -> String {
    match chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") {
        Ok(d) => (d + chrono::Duration::days(days))
            .format("%Y-%m-%d")
            .to_string(),
        Err(_) => date.to_owned(),
    }
}

/// `date` に、`from` の時刻があればそれを付ける。日付だけを変えるときに時刻を残すのに使う。
pub fn with_time_of(date: &str, from: Option<&str>) -> String {
    match from.and_then(crate::time_of) {
        Some(time) => format!("{date} {time}"),
        None => date.to_owned(),
    }
}

/// いまの日時（端末の時刻帯）。`YYYY-MM-DD HH:MM`。
pub fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()
}

/// 期限の表示。今年なら「12/24」、ほかの年なら「2027/1/3」。時刻があれば「12/24 18:00」。
pub fn format_date(when: &str, today: &str) -> String {
    let date = crate::date_of(when);
    let part = |r: std::ops::Range<usize>| date.get(r).and_then(|v| v.parse::<u32>().ok());
    let day = match (part(0..4), part(5..7), part(8..10)) {
        (Some(_), Some(m), Some(d)) if date.get(0..4) == today.get(0..4) => format!("{m}/{d}"),
        (Some(y), Some(m), Some(d)) => format!("{y}/{m}/{d}"),
        _ => return when.to_owned(),
    };
    match crate::time_of(when) {
        Some(time) => format!("{day} {time}"),
        None => day,
    }
}

/// 一覧での期限の見せ方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DueFormat {
    /// 「11/24～11/27」のように日付で出す。
    #[default]
    Date,
    /// 「明日」「あと 3 日」「2 日前」のように今日から数えて出す。
    Relative,
}

impl DueFormat {
    pub const ALL: [DueFormat; 2] = [DueFormat::Date, DueFormat::Relative];

    pub fn label(self) -> &'static str {
        match self {
            DueFormat::Date => "日付",
            DueFormat::Relative => "相対",
        }
    }

    /// 保存するときの名前。
    pub fn key(self) -> &'static str {
        match self {
            DueFormat::Date => "date",
            DueFormat::Relative => "relative",
        }
    }

    /// 保存した名前から読む。読めなければ日付。
    pub fn from_key(key: Option<&str>) -> DueFormat {
        match key {
            Some("relative") => DueFormat::Relative,
            _ => DueFormat::Date,
        }
    }
}

/// 一覧での期限の表示。開始が期限より前の日なら「11/24～11/27」のように範囲で出す。
/// 開始と期限が同じ日で、どちらにも時刻があれば「12/2 17:00–18:00」のように時間帯で出す。
/// それ以外は期限だけ。
pub fn format_span(start: Option<&str>, due: &str, today: &str) -> String {
    let text = format_date(due, today);
    match span_of(start, due) {
        Span::Range(start) => format!("{}～{text}", format_date(start, today)),
        Span::Hours(from) => {
            let (day, to) = text.split_once(' ').unwrap_or((&text, ""));
            format!("{day} {from}–{to}")
        }
        Span::Due => text,
    }
}

/// 一覧での期限の表示を、見せ方に合わせて作る。
pub fn format_due(start: Option<&str>, due: &str, today: &str, format: DueFormat) -> String {
    match format {
        DueFormat::Date => format_span(start, due, today),
        DueFormat::Relative => format_span_relative(start, due, today),
    }
}

/// 期限のない、開始だけのタスクの表示。「11/24～」「11/24 10:00～」、相対なら「2 日前～」「明日 10:00～」。
pub fn format_start_only(start: &str, today: &str, format: DueFormat) -> String {
    let text = match format {
        DueFormat::Date => format_date(start, today),
        DueFormat::Relative => relative_day(start, today, false),
    };
    format!("{text}～")
}

/// `format_span` の相対の版。「明日」「あと 3 日」「2 日前～あと 3 日」「今日 17:00–18:00」。
pub fn format_span_relative(start: Option<&str>, due: &str, today: &str) -> String {
    match span_of(start, due) {
        Span::Range(start) => format!(
            "{}～{}",
            relative_day(start, today, false),
            relative_day(due, today, true)
        ),
        Span::Hours(from) => {
            let day = relative_day(crate::date_of(due), today, true);
            let to = crate::time_of(due).unwrap_or_default();
            format!("{day} {from}–{to}")
        }
        Span::Due => relative_day(due, today, true),
    }
}

/// 開始と期限の組み合わせ方。
enum Span<'a> {
    /// 開始の日（時刻なし）から期限まで。
    Range(&'a str),
    /// 同じ日の、開始の時刻から期限の時刻まで。
    Hours(&'a str),
    /// 期限だけ。
    Due,
}

fn span_of<'a>(start: Option<&'a str>, due: &str) -> Span<'a> {
    let Some(start) = start else {
        return Span::Due;
    };
    let (day, due_day) = (crate::date_of(start), crate::date_of(due));
    if day < due_day && crate::is_valid_date(day) {
        return Span::Range(day);
    }
    match crate::time_of(start) {
        Some(from) if day == due_day && crate::time_of(due).is_some_and(|to| from < to) => {
            Span::Hours(from)
        }
        _ => Span::Due,
    }
}

/// 今日から数えた日の呼び方。今日・明日・昨日には時刻を添える。
/// 期限（`due`）の先の日は「あと 3 日」、開始の先の日は「3 日後」、過ぎた日は「2 日前」。
fn relative_day(when: &str, today: &str, due: bool) -> String {
    let parse = |d: &str| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok();
    let (Some(day), Some(now)) = (parse(crate::date_of(when)), parse(crate::date_of(today))) else {
        return format_date(when, today);
    };
    let days = (day - now).num_days();
    let name = match days {
        0 => "今日".to_owned(),
        1 => "明日".to_owned(),
        -1 => "昨日".to_owned(),
        n if n < 0 => return format!("{} 日前", -n),
        n if due => return format!("あと {n} 日"),
        n => return format!("{n} 日後"),
    };
    match crate::time_of(when) {
        Some(time) => format!("{name} {time}"),
        None => name,
    }
}

/// 「3 分前」のような相対表示。7 日以上前は日付にする。
pub fn format_relative(at_ms: i64, now_ms: i64) -> String {
    let secs = (now_ms - at_ms).max(0) / 1000;
    match secs {
        0..=59 => "たった今".to_owned(),
        60..=3599 => format!("{} 分前", secs / 60),
        3600..=86_399 => format!("{} 時間前", secs / 3600),
        86_400..=604_799 => format!("{} 日前", secs / 86_400),
        _ => format_datetime(at_ms).chars().take(10).collect(),
    }
}

/// 端末の時刻帯での「2026/11/24 13:05」。
pub fn format_datetime(at_ms: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_millis_opt(at_ms).single() {
        Some(t) => t.format("%Y/%m/%d %H:%M").to_string(),
        None => String::new(),
    }
}

/// バイト数を「1.2 MB」の形にする。
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, status: Status) -> Task {
        Task {
            id: id.into(),
            workspace_id: Some("w".into()),
            title: format!("Task {id}"),
            description: String::new(),
            status,
            priority: 0,
            start: None,
            due: None,
            percent: 0,
            parent_id: None,
            assignee_ids: vec![],
            assignee_roles: vec![],
            assignee_done: vec![],
            categories: vec![],
            urls: vec![],
            position: 0,
            created_at: 0,
            updated_at: 0,
            linked_workspace_ids: Vec::new(),
            completed_at: None,
            created_by: crate::Actor::Unknown,
            updated_by: crate::Actor::Unknown,
        }
    }

    fn ids(tasks: &[Task], q: &ListQuery, me: Option<&str>) -> Vec<String> {
        apply(tasks, q, me)
            .into_iter()
            .map(|i| tasks[i].id.clone())
            .collect()
    }

    fn space(id: &str, parent: Option<&str>, position: i64) -> Workspace {
        Workspace {
            id: id.into(),
            parent_id: parent.map(Into::into),
            name: id.to_uppercase(),
            date: None,
            icon: None,
            color: None,
            position,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn in_space(id: &str, workspace: &str) -> Task {
        Task {
            workspace_id: Some(workspace.into()),
            ..task(id, Status::NeedsAction)
        }
    }

    /// 行を「深さ:名前」で書き出す。見出しは `[名前 数]`、開いていれば `v`、閉じていれば `>` を付ける。
    fn drawn(tasks: &[Task], spaces: &[Workspace], open: &[&str], hide_empty: bool) -> Vec<String> {
        let hits = apply(tasks, &ListQuery::default(), None);
        let open = open.iter().map(|id| id.to_string()).collect();
        let outline = outline(tasks, &hits, spaces, "w", &open, hide_empty);
        assert_eq!(outline.visible.len(), outline.homes.len());
        outline
            .rows
            .iter()
            .map(|row| match *row {
                ListRow::Task { index, depth } => {
                    let task = &tasks[outline.visible[index]];
                    let home = &spaces[outline.homes[index].unwrap()].id;
                    format!("{depth}:{}@{home}", task.id)
                }
                ListRow::Folder {
                    workspace,
                    depth,
                    count,
                    open,
                } => format!(
                    "{depth}:{}[{} {count}]",
                    if open { 'v' } else { '>' },
                    spaces[workspace].id
                ),
            })
            .collect()
    }

    fn nested() -> (Vec<Task>, Vec<Workspace>) {
        let spaces = vec![
            space("other", None, 0),
            space("w", None, 1),
            space("b", Some("w"), 1),
            space("a", Some("w"), 0),
            space("a1", Some("a"), 0),
            space("a1x", Some("a1"), 0),
        ];
        let tasks = vec![
            in_space("t1", "w"),
            in_space("t2", "a"),
            in_space("t3", "a1"),
            in_space("t4", "a1x"),
            in_space("t5", "b"),
        ];
        (tasks, spaces)
    }

    #[test]
    fn outline_folds_sub_workspaces() {
        let (tasks, spaces) = nested();
        // 閉じていれば見出しだけ。数は、さらに下のサブワークスペースの分も含む
        assert_eq!(
            drawn(&tasks, &spaces, &[], false),
            ["0:>[a 3]", "0:>[b 1]", "0:t1@w"]
        );
        // 開くと、下のサブワークスペース、中のタスクの順に出る
        assert_eq!(
            drawn(&tasks, &spaces, &["a"], false),
            ["0:v[a 3]", "1:>[a1 2]", "1:t2@a", "0:>[b 1]", "0:t1@w"]
        );
        // サブのサブのサブまで開ける
        assert_eq!(
            drawn(&tasks, &spaces, &["a", "a1", "a1x", "b"], false),
            [
                "0:v[a 3]",
                "1:v[a1 2]",
                "2:v[a1x 1]",
                "3:t4@a1x",
                "2:t3@a1",
                "1:t2@a",
                "0:v[b 1]",
                "1:t5@b",
                "0:t1@w"
            ]
        );
        // 上が閉じていれば、下が開いていても出ない
        assert_eq!(
            drawn(&tasks, &spaces, &["a1", "a1x"], false),
            ["0:>[a 3]", "0:>[b 1]", "0:t1@w"]
        );
    }

    #[test]
    fn outline_hides_empty_folders_only_when_asked() {
        let (mut tasks, spaces) = nested();
        tasks.retain(|t| t.id != "t5" && t.id != "t2" && t.id != "t3");
        assert_eq!(
            drawn(&tasks, &spaces, &["a"], false),
            ["0:v[a 1]", "1:>[a1 1]", "0:>[b 0]", "0:t1@w"]
        );
        assert_eq!(
            drawn(&tasks, &spaces, &["a", "b"], true),
            ["0:v[a 1]", "1:>[a1 1]", "0:t1@w"]
        );
    }

    #[test]
    fn outline_shows_a_task_once() {
        let (mut tasks, spaces) = nested();
        // a1 に入っていて w にも出しているタスクは、w の行として 1 度だけ出る
        tasks[2].linked_workspace_ids = vec!["w".into()];
        // よそに入っていて b に出しているタスクは、b の下に出る
        tasks.push(Task {
            linked_workspace_ids: vec!["b".into()],
            ..in_space("t6", "other")
        });
        assert_eq!(
            drawn(&tasks, &spaces, &["a", "a1", "b"], false),
            [
                "0:v[a 2]",
                "1:v[a1 1]",
                "2:>[a1x 1]",
                "1:t2@a",
                "0:v[b 2]",
                "1:t5@b",
                "1:t6@b",
                "0:t1@w",
                "0:t3@w"
            ]
        );
    }

    #[test]
    fn outline_row_of_and_flat() {
        let (tasks, spaces) = nested();
        let hits = apply(&tasks, &ListQuery::default(), None);
        let open = ["a".to_string()].into_iter().collect();
        let nested = outline(&tasks, &hits, &spaces, "w", &open, false);
        // t2 は 3 行目、t1 は最後の行
        assert_eq!(nested.row_of(0), Some(2));
        assert_eq!(nested.row_of(1), Some(4));
        assert_eq!(nested.row_of(2), None);
        // ワークスペースが見つからなければ、入れ子にしない
        let flat = outline(&tasks, &hits, &spaces, "gone", &open, false);
        assert_eq!(flat, Outline::flat(hits.clone()));
        assert_eq!(flat.visible, hits);
        assert_eq!(flat.row_of(4), Some(4));
    }

    #[test]
    fn outline_stops_on_a_parent_loop() {
        let mut spaces = vec![space("w", Some("a"), 0), space("a", Some("w"), 0)];
        spaces[0].parent_id = Some("a".into());
        let tasks = vec![in_space("t1", "w"), in_space("t2", "a")];
        assert_eq!(
            drawn(&tasks, &spaces, &["a", "w"], false),
            ["0:v[a 1]", "1:t2@a", "0:t1@w"]
        );
    }

    #[test]
    fn priority_buckets() {
        let expect = [
            (0, Priority::None),
            (1, Priority::High),
            (4, Priority::High),
            (5, Priority::Mid),
            (6, Priority::Low),
            (9, Priority::Low),
            (10, Priority::None),
        ];
        for (v, p) in expect {
            assert_eq!(Priority::of(v), p, "{v}");
        }
        for p in Priority::ALL {
            assert_eq!(Priority::of(p.value()), p);
        }
        assert_eq!(
            Priority::ALL.map(Priority::label),
            ["なし", "高", "中", "低"]
        );
    }

    #[test]
    fn tabs_and_counts() {
        let tasks = [
            task("a", Status::NeedsAction),
            task("b", Status::InProcess),
            task("c", Status::Completed),
            task("d", Status::Cancelled),
        ];
        assert_eq!(counts(&tasks), (2, 2));
        assert_eq!(counts(&[]), (0, 0));
        let mut q = ListQuery::default();
        assert_eq!(ids(&tasks, &q, None), ["a", "b"]);
        q.tab = Tab::Closed;
        assert_eq!(ids(&tasks, &q, None), ["c", "d"]);
        // 状態を選ぶとタブより優先
        q.status = Some(Status::InProcess);
        assert_eq!(ids(&tasks, &q, None), ["b"]);
    }

    #[test]
    fn filters() {
        let mut a = task("a", Status::NeedsAction);
        a.assignee_ids = vec!["me".into()];
        a.priority = 2;
        a.categories = vec!["設営".into()];
        a.description = "Bring CABLES".into();
        let mut b = task("b", Status::NeedsAction);
        b.assignee_ids = vec!["other".into()];
        b.priority = 5;
        let c = task("c", Status::NeedsAction);
        let tasks = [a, b, c];

        let with = |f: &dyn Fn(&mut ListQuery)| {
            let mut q = ListQuery::default();
            f(&mut q);
            q
        };
        assert_eq!(
            ids(
                &tasks,
                &with(&|q| q.assignee = AssigneeFilter::Me),
                Some("me")
            ),
            ["a"]
        );
        // 自分が分からなければ、自分の担当は 0 件
        assert!(ids(&tasks, &with(&|q| q.assignee = AssigneeFilter::Me), None).is_empty());
        assert_eq!(
            ids(
                &tasks,
                &with(&|q| q.assignee = AssigneeFilter::Unassigned),
                None
            ),
            ["c"]
        );
        assert_eq!(
            ids(
                &tasks,
                &with(&|q| q.assignee = AssigneeFilter::Person("other".into())),
                None
            ),
            ["b"]
        );
        assert_eq!(
            ids(&tasks, &with(&|q| q.priority = Some(Priority::High)), None),
            ["a"]
        );
        assert_eq!(
            ids(&tasks, &with(&|q| q.priority = Some(Priority::None)), None),
            ["c"]
        );
        assert_eq!(
            ids(&tasks, &with(&|q| q.category = Some("設営".into())), None),
            ["a"]
        );
        assert!(ids(&tasks, &with(&|q| q.category = Some("広報".into())), None).is_empty());
        assert_eq!(
            ids(&tasks, &with(&|q| q.q = " cables ".into()), None),
            ["a"]
        );
        assert_eq!(ids(&tasks, &with(&|q| q.q = "task B".into()), None), ["b"]);
        assert_eq!(ids(&tasks, &with(&|q| q.q = "  ".into()), None).len(), 3);

        let mut q = with(&|q| {
            q.q = "x".into();
            q.status = Some(Status::Completed);
            q.assignee = AssigneeFilter::Me;
            q.priority = Some(Priority::Low);
            q.category = Some("c".into());
            q.tab = Tab::Closed;
            q.sort = Sort::Created;
        });
        assert_eq!(q.active_count(), 5);
        q.clear_filters();
        assert_eq!(q.active_count(), 0);
        assert_eq!((q.tab, q.sort), (Tab::Closed, Sort::Created));
        assert_eq!(with(&|q| q.q = " ".into()).active_count(), 0);
        assert_eq!(
            with(&|q| q.status = Some(Status::Completed)).active_count(),
            1
        );
    }

    #[test]
    fn sorting() {
        let mk = |id: &str, due: Option<&str>, prio: u8, updated: i64, created: i64| {
            let mut t = task(id, Status::NeedsAction);
            t.due = due.map(str::to_owned);
            t.priority = prio;
            t.updated_at = updated;
            t.created_at = created;
            t
        };
        let tasks = [
            mk("a", None, 1, 10, 4),
            mk("b", Some("2026-12-02"), 0, 40, 3),
            mk("c", Some("2026-12-01"), 9, 30, 2),
            mk("d", Some("2026-12-01"), 5, 20, 1),
            mk("e", None, 0, 20, 1),
        ];
        let sorted = |sort| {
            ids(
                &tasks,
                &ListQuery {
                    sort,
                    ..Default::default()
                },
                None,
            )
        };
        assert_eq!(sorted(Sort::Due), ["d", "c", "b", "a", "e"]);
        assert_eq!(sorted(Sort::Priority), ["a", "d", "c", "b", "e"]);
        assert_eq!(sorted(Sort::Updated), ["b", "c", "d", "e", "a"]);
        assert_eq!(sorted(Sort::Created), ["a", "b", "c", "d", "e"]);
        assert_eq!(sorted(Sort::Title), ["a", "b", "c", "d", "e"]);
        let reversed = ListQuery {
            descending: true,
            ..Default::default()
        };
        assert_eq!(ids(&tasks, &reversed, None), ["e", "a", "b", "c", "d"]);
        assert_eq!(Sort::ALL.map(Sort::label).len(), 8);
        assert_eq!(Sort::Due.label(), "期限が近い順");
    }

    #[test]
    fn sorting_by_assignee_uses_names_and_puts_unassigned_last() {
        let mk = |id: &str, people: &[&str], due: Option<&str>| {
            let mut t = task(id, Status::NeedsAction);
            t.assignee_ids = people.iter().map(|p| (*p).to_owned()).collect();
            t.due = due.map(str::to_owned);
            t
        };
        let tasks = [
            mk("a", &[], None),
            mk("b", &["p2"], None),
            mk("c", &["p1", "p2"], None),
            mk("d", &["p1"], Some("2026-12-02")),
            mk("e", &["p1"], Some("2026-12-01")),
        ];
        // p1 は「佐藤」、p2 は「Aoki」。id ではなく名前で比べる
        let name = |id: &str| match id {
            "p1" => "佐藤".to_owned(),
            _ => "Aoki".to_owned(),
        };
        let sorted = |descending| {
            let query = ListQuery {
                sort: Sort::Assignee,
                descending,
                ..Default::default()
            };
            apply_named(&tasks, &query, None, &name)
                .into_iter()
                .map(|i| tasks[i].id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(sorted(false), ["b", "e", "d", "c", "a"]);
        assert_eq!(sorted(true), ["a", "c", "d", "e", "b"]);
    }

    #[test]
    fn same_day_start_and_due_show_as_a_span() {
        let today = "2026-11-25";
        let span = |start: Option<&str>, due: &str| format_span(start, due, today);
        assert_eq!(
            span(Some("2026-12-02 17:00"), "2026-12-02 18:00"),
            "12/2 17:00–18:00"
        );
        assert_eq!(
            span(Some("2027-01-05 09:00"), "2027-01-05 10:30"),
            "2027/1/5 09:00–10:30"
        );
        // 開始が前の日なら、開始の日から期限までの範囲
        assert_eq!(
            span(Some("2026-12-01 17:00"), "2026-12-02 18:00"),
            "12/1～12/2 18:00"
        );
        assert_eq!(span(Some("2026-11-24"), "2026-11-27"), "11/24～11/27");
        assert_eq!(span(Some("2026-12-28"), "2027-01-03"), "12/28～2027/1/3");
        // 時刻がない、開始が期限より後のときは期限だけ
        assert_eq!(span(Some("2026-12-05"), "2026-12-02"), "12/2");
        assert_eq!(span(Some("2026-12-02"), "2026-12-02 18:00"), "12/2 18:00");
        assert_eq!(span(Some("2026-12-02 17:00"), "2026-12-02"), "12/2");
        assert_eq!(
            span(Some("2026-12-02 19:00"), "2026-12-02 18:00"),
            "12/2 18:00"
        );
        assert_eq!(span(None, "2026-12-02 18:00"), "12/2 18:00");
    }

    #[test]
    fn due_shown_relative_to_today() {
        let today = "2026-11-25";
        let rel =
            |start: Option<&str>, due: &str| format_due(start, due, today, DueFormat::Relative);
        assert_eq!(rel(None, "2026-11-25"), "今日");
        assert_eq!(rel(None, "2026-11-26"), "明日");
        assert_eq!(rel(None, "2026-11-24"), "昨日");
        assert_eq!(rel(None, "2026-11-28"), "あと 3 日");
        assert_eq!(rel(None, "2026-11-23"), "2 日前");
        assert_eq!(rel(None, "2027-01-04"), "あと 40 日");
        assert_eq!(rel(None, "2026-11-26 18:00"), "明日 18:00");
        assert_eq!(rel(None, "2026-11-30 18:00"), "あと 5 日");
        // 開始があれば範囲。開始の先の日は「〜日後」
        assert_eq!(rel(Some("2026-11-23"), "2026-11-28"), "2 日前～あと 3 日");
        assert_eq!(rel(Some("2026-11-26"), "2026-11-30"), "明日～あと 5 日");
        assert_eq!(rel(Some("2026-11-27"), "2026-11-30"), "2 日後～あと 5 日");
        assert_eq!(
            rel(Some("2026-11-25 17:00"), "2026-11-25 18:00"),
            "今日 17:00–18:00"
        );
        assert_eq!(rel(Some("2026-11-29"), "2026-11-28"), "あと 3 日");
        // 期限がなく開始だけなら「〜から」の形
        let start_only = |start: &str, format| format_start_only(start, today, format);
        assert_eq!(start_only("2026-11-24", DueFormat::Date), "11/24～");
        assert_eq!(
            start_only("2026-11-24 10:00", DueFormat::Date),
            "11/24 10:00～"
        );
        assert_eq!(start_only("2027-01-04", DueFormat::Date), "2027/1/4～");
        assert_eq!(start_only("2026-11-23", DueFormat::Relative), "2 日前～");
        assert_eq!(start_only("2026-11-25", DueFormat::Relative), "今日～");
        assert_eq!(
            start_only("2026-11-26 10:00", DueFormat::Relative),
            "明日 10:00～"
        );
        assert_eq!(start_only("2026-11-28", DueFormat::Relative), "3 日後～");
        // 日付として読めないものは、そのまま
        assert_eq!(rel(None, "bad"), "bad");
        // 日付の見せ方は、これまでどおり
        assert_eq!(
            format_due(Some("2026-11-24"), "2026-11-27", today, DueFormat::Date),
            "11/24～11/27"
        );
        for format in DueFormat::ALL {
            assert_eq!(DueFormat::from_key(Some(format.key())), format);
        }
        assert_eq!(DueFormat::from_key(None), DueFormat::Date);
        assert_eq!(DueFormat::from_key(Some("x")), DueFormat::Date);
    }

    #[test]
    fn due_states_and_dates() {
        let today = "2026-12-30";
        assert_eq!(due_state("2026-12-29", today), DueState::Overdue);
        assert_eq!(due_state("2026-12-29 23:00", today), DueState::Overdue);
        assert_eq!(due_state("2026-12-30 09:00", today), DueState::Today);
        assert_eq!(
            due_state("2026-12-30 09:00", "2026-12-30 08:59"),
            DueState::Today
        );
        assert_eq!(
            due_state("2026-12-30 09:00", "2026-12-30 09:00"),
            DueState::Today
        );
        assert_eq!(
            due_state("2026-12-30 09:00", "2026-12-30 09:01"),
            DueState::Overdue
        );
        assert_eq!(due_state("2026-12-30", "2026-12-30 23:59"), DueState::Today);
        assert_eq!(
            due_state("2026-12-31 00:00", "2026-12-30 23:59"),
            DueState::Soon
        );
        assert_eq!(super::format_date("2026-12-30 09:00", today), "12/30 09:00");
        assert_eq!(
            super::format_date("2027-01-03 18:30", today),
            "2027/1/3 18:30"
        );
        assert_eq!(due_state("2026-12-30", today), DueState::Today);
        assert_eq!(due_state("2026-12-31", today), DueState::Soon);
        assert_eq!(due_state("2027-01-02", today), DueState::Soon);
        assert_eq!(due_state("2027-01-03", today), DueState::Later);
        assert_eq!(add_days("2026-12-30", 3), "2027-01-02");
        assert_eq!(add_days("2024-03-01", -1), "2024-02-29");
        assert_eq!(add_days("bad", 3), "bad");
        assert_eq!(format_date("2026-12-04", today), "12/4");
        assert_eq!(format_date("2027-01-03", today), "2027/1/3");
        assert_eq!(format_date("bad", today), "bad");
        assert!(crate::is_valid_date(&super::today()));
    }

    #[test]
    fn relative_times() {
        let now = 1_800_000_000_000;
        let ago = |secs: i64| format_relative(now - secs * 1000, now);
        assert_eq!(ago(0), "たった今");
        assert_eq!(ago(59), "たった今");
        assert_eq!(ago(60), "1 分前");
        assert_eq!(ago(3599), "59 分前");
        assert_eq!(ago(3600), "1 時間前");
        assert_eq!(ago(86_399), "23 時間前");
        assert_eq!(ago(86_400), "1 日前");
        assert_eq!(ago(604_799), "6 日前");
        assert_eq!(ago(604_800).len(), 10);
        // 時計がずれて未来になっていても崩れない
        assert_eq!(ago(-100), "たった今");
        assert_eq!(format_datetime(now).len(), 16);
    }

    #[test]
    fn filters_by_my_role() {
        let mut a = task("a", Status::NeedsAction);
        a.assignee_ids = vec!["me".into(), "you".into()];
        a.assignee_roles = vec![Roles::of(&[Role::R, Role::A]), Roles::of(&[Role::C])];
        // 役の記録がない担当者は R として読む
        let mut b = task("b", Status::NeedsAction);
        b.assignee_ids = vec!["me".into()];
        let mut c = task("c", Status::NeedsAction);
        c.assignee_ids = vec!["you".into()];
        c.assignee_roles = vec![Roles::of(&[Role::A])];
        let tasks = [a, b, c];
        let pick = |role, me| {
            let query = ListQuery {
                my_role: Some(role),
                ..Default::default()
            };
            assert_eq!(query.active_count(), 1);
            apply(&tasks, &query, me)
                .into_iter()
                .map(|i| tasks[i].id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(pick(Role::R, Some("me")), ["a", "b"]);
        assert_eq!(pick(Role::A, Some("me")), ["a"]);
        assert!(pick(Role::C, Some("me")).is_empty());
        assert_eq!(pick(Role::C, Some("you")), ["a"]);
        // 自分が分からなければ 0 件
        assert!(pick(Role::R, None).is_empty());
    }

    #[test]
    fn bytes_are_readable() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1_572_864), "1.5 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
        assert_eq!(format_bytes(u64::MAX), "16777216.0 TB");
    }
}
