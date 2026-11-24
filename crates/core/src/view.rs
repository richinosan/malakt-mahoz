//! 一覧の絞り込み・並べ替えや、期限・優先度の見せ方。画面に依存しない。

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
}

impl Sort {
    pub const ALL: [Sort; 4] = [Sort::Due, Sort::Priority, Sort::Updated, Sort::Created];

    pub fn label(self) -> &'static str {
        match self {
            Sort::Due => "期限が近い順",
            Sort::Priority => "優先度が高い順",
            Sort::Updated => "更新が新しい順",
            Sort::Created => "作成が新しい順",
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
    pub priority: Option<Priority>,
    pub category: Option<String>,
    pub sort: Sort,
}

impl ListQuery {
    /// タブと並び順以外に、効いている絞り込みの数。
    pub fn active_count(&self) -> usize {
        usize::from(!self.q.trim().is_empty())
            + usize::from(self.status.is_some())
            + usize::from(self.assignee != AssigneeFilter::All)
            + usize::from(self.priority.is_some())
            + usize::from(self.category.is_some())
    }

    /// 絞り込みを外す。タブと並び順は残す。
    pub fn clear_filters(&mut self) {
        *self = ListQuery {
            tab: self.tab,
            sort: self.sort,
            ..Default::default()
        };
    }
}

/// （未完了の数, 完了の数）。
pub fn counts(tasks: &[Task]) -> (usize, usize) {
    let open = tasks.iter().filter(|t| t.status.is_open()).count();
    (open, tasks.len() - open)
}

/// 条件に合うタスクの添え字を、並び順どおりに返す。
pub fn apply(tasks: &[Task], query: &ListQuery, self_id: Option<&str>) -> Vec<usize> {
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
    hits.sort_by(|&a, &b| {
        let (a, b) = (&tasks[a], &tasks[b]);
        match query.sort {
            Sort::Due => due(a).cmp(&due(b)).then(prio(a).cmp(&prio(b))),
            Sort::Priority => prio(a).cmp(&prio(b)).then(due(a).cmp(&due(b))),
            Sort::Updated => b.updated_at.cmp(&a.updated_at),
            Sort::Created => b.created_at.cmp(&a.created_at),
        }
        .then_with(|| a.id.cmp(&b.id))
    });
    hits
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

pub fn due_state(due: &str, today: &str) -> DueState {
    if due < today {
        DueState::Overdue
    } else if due == today {
        DueState::Today
    } else if due <= add_days(today, DUE_SOON_DAYS).as_str() {
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

/// 期限の表示。今年なら「12/24」、ほかの年なら「2027/1/3」。
pub fn format_date(date: &str, today: &str) -> String {
    let part = |r: std::ops::Range<usize>| date.get(r).and_then(|v| v.parse::<u32>().ok());
    match (part(0..4), part(5..7), part(8..10)) {
        (Some(y), Some(m), Some(d)) if date.get(0..4) == today.get(0..4) => {
            let _ = y;
            format!("{m}/{d}")
        }
        (Some(y), Some(m), Some(d)) => format!("{y}/{m}/{d}"),
        _ => date.to_owned(),
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

/// 変更履歴の項目名。
pub fn field_label(field: &str) -> &str {
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
        "categories" => "カテゴリ",
        "workspace" => "ワークスペース",
        other => other,
    }
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
            categories: vec![],
            position: 0,
            created_at: 0,
            updated_at: 0,
            completed_at: None,
        }
    }

    fn ids(tasks: &[Task], q: &ListQuery, me: Option<&str>) -> Vec<String> {
        apply(tasks, q, me)
            .into_iter()
            .map(|i| tasks[i].id.clone())
            .collect()
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
        assert_eq!(Sort::ALL.map(Sort::label).len(), 4);
        assert_eq!(Sort::Due.label(), "期限が近い順");
    }

    #[test]
    fn due_states_and_dates() {
        let today = "2026-12-30";
        assert_eq!(due_state("2026-12-29", today), DueState::Overdue);
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
    fn field_labels() {
        assert_eq!(field_label("due"), "期限");
        assert_eq!(field_label("assignees"), "担当");
        assert_eq!(field_label("unknown"), "unknown");
    }
}
