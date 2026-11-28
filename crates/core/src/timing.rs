//! 所要時間（開始の基準から完了の基準までの経過）と、その統計。
//!
//! どこからどこまでを数えるかは仕事によって違う（作った時から数えたい、進行中にした時から数えたい、など）ので、
//! 基準（[`Basis`]）を選べる。基準は全体の標準・ワークスペース・タスクの順に上書きする（[`resolve`]）。
//!
//! 所要時間は時計の経過で、夜や休みの日も入る。ポモドーロで記録する「作業時間」（手を動かした時間の合計）とは別のもの。

use std::collections::HashMap;

use crate::model::{Id, Status, Task};

/// どこから数えるか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StartBasis {
    /// タスクを作った時
    Created,
    /// 担当が付いた時
    Assigned,
    /// 進行中にした時
    #[default]
    InProcess,
    /// 開始日（時刻がなければその日の 0:00）
    StartDate,
}

impl StartBasis {
    pub const ALL: [StartBasis; 4] = [
        StartBasis::Created,
        StartBasis::Assigned,
        StartBasis::InProcess,
        StartBasis::StartDate,
    ];

    pub fn code(self) -> &'static str {
        match self {
            StartBasis::Created => "created",
            StartBasis::Assigned => "assigned",
            StartBasis::InProcess => "in_process",
            StartBasis::StartDate => "start_date",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.code() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            StartBasis::Created => "作った時",
            StartBasis::Assigned => "担当が付いた時",
            StartBasis::InProcess => "進行中にした時",
            StartBasis::StartDate => "開始日",
        }
    }
}

/// どこまで数えるか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EndBasis {
    /// タスクを完了にした時
    #[default]
    Completed,
    /// 自分の分を完了にした時（自分が担当でなければ、タスクを完了にした時）
    SelfDone,
}

impl EndBasis {
    pub const ALL: [EndBasis; 2] = [EndBasis::Completed, EndBasis::SelfDone];

    pub fn code(self) -> &'static str {
        match self {
            EndBasis::Completed => "completed",
            EndBasis::SelfDone => "self_done",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.code() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            EndBasis::Completed => "完了にした時",
            EndBasis::SelfDone => "自分の分を完了にした時",
        }
    }
}

/// 所要時間の基準。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Basis {
    pub start: StartBasis,
    pub end: EndBasis,
}

impl Basis {
    pub const fn new(start: StartBasis, end: EndBasis) -> Self {
        Basis { start, end }
    }

    /// 保存する形（`in_process>completed`）。
    pub fn code(&self) -> String {
        format!("{}>{}", self.start.code(), self.end.code())
    }

    /// 保存した形を読む。読めなければ `None`。
    pub fn parse(s: &str) -> Option<Self> {
        let (start, end) = s.trim().split_once('>')?;
        Some(Basis {
            start: StartBasis::parse(start.trim())?,
            end: EndBasis::parse(end.trim())?,
        })
    }

    /// 「進行中にした時 → 完了にした時」。
    pub fn label(&self) -> String {
        format!("{} → {}", self.start.label(), self.end.label())
    }
}

impl serde::Serialize for Basis {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.code())
    }
}

impl<'de> serde::Deserialize<'de> for Basis {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Basis::parse(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("所要時間の基準が読めません: {text}")))
    }
}

/// 全体の標準を保存する設定の名前。
pub const BASIS_SETTING: &str = "timing.basis";

/// タスク・ワークスペース・全体の順に、最初に決まっている基準。
pub fn resolve(task: Option<Basis>, workspace: Option<Basis>, standard: Basis) -> Basis {
    task.or(workspace).unwrap_or(standard)
}

/// タスクに使う基準。タスク → 入っているワークスペース（決まっていなければ親へたどる）→ 全体の標準 の順。
pub fn basis_of(task: &Task, workspaces: &[crate::model::Workspace], standard: Basis) -> Basis {
    let workspace = task
        .workspace_id
        .as_deref()
        .and_then(|id| workspace_basis(workspaces, id));
    resolve(task.time_basis, workspace, standard)
}

/// ワークスペースに決めた基準（決まっていなければ親へたどる）。どこにもなければ `None`。
pub fn workspace_basis(workspaces: &[crate::model::Workspace], id: &str) -> Option<Basis> {
    let mut at = Some(id);
    for _ in 0..=workspaces.len() {
        let w = workspaces.iter().find(|w| Some(w.id.as_str()) == at)?;
        if w.time_basis.is_some() {
            return w.time_basis;
        }
        at = w.parent_id.as_deref();
    }
    None
}

/// 数えた区間。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// 始まり（UNIX 時刻、ミリ秒）
    pub start: i64,
    /// 終わり。数えている途中なら今の時刻
    pub end: i64,
    /// まだ終わっていない（今も数えている）
    pub running: bool,
    /// 始まりに実際に使った基準。選んだ基準の時刻がないときは、ほかの時刻から数える
    pub from: StartBasis,
}

impl Span {
    pub fn ms(&self) -> i64 {
        self.end - self.start
    }
}

/// 開始日（`YYYY-MM-DD` か `YYYY-MM-DD HH:MM`、端末の時刻帯）の時刻。
fn start_date_ms(when: &str) -> Option<i64> {
    use chrono::TimeZone;
    let date = chrono::NaiveDate::parse_from_str(crate::date_of(when), "%Y-%m-%d").ok()?;
    let (h, m) = crate::time_of(when)
        .and_then(crate::when::parse_time)
        .unwrap_or((0, 0));
    let at = date.and_hms_opt(h, m, 0)?;
    chrono::Local
        .from_local_datetime(&at)
        .earliest()
        .map(|t| t.timestamp_millis())
}

/// その基準の始まりの時刻。なければ `None`。
fn start_of(task: &Task, basis: StartBasis) -> Option<i64> {
    match basis {
        StartBasis::Created => Some(task.created_at),
        StartBasis::Assigned => task.assigned_at,
        StartBasis::InProcess => task.started_at,
        StartBasis::StartDate => task.start.as_deref().and_then(start_date_ms),
    }
}

/// その基準の終わりの時刻。まだなら `None`。
fn end_of(task: &Task, basis: EndBasis, self_id: Option<&str>) -> Option<i64> {
    let completed = (task.status == Status::Completed)
        .then_some(task.completed_at)
        .flatten();
    match basis {
        EndBasis::Completed => completed,
        EndBasis::SelfDone => match self_id.filter(|me| task.roles_of(me).is_some()) {
            Some(me) => {
                let i = task.assignee_ids.iter().position(|id| id == me)?;
                task.assignee_done.get(i).copied().flatten().or(completed)
            }
            None => completed,
        },
    }
}

/// 所要時間の区間。
///
/// 終わったタスクで、選んだ始まりの時刻がないとき（進行中にしないで完了にした、など）は、
/// 進行中にした時 → 担当が付いた時 → 作った時 の順に、あるものから数える。
/// 終わっていないタスクは、選んだ始まりの時刻があるときだけ、今までの経過を返す。
/// 中止したタスク、始まりが終わりより後のものは `None`。
pub fn span(task: &Task, basis: Basis, self_id: Option<&str>, now: i64) -> Option<Span> {
    if task.status == Status::Cancelled {
        return None;
    }
    let chosen = start_of(task, basis.start);
    match end_of(task, basis.end, self_id) {
        Some(end) => {
            let (from, start) = match chosen {
                Some(at) => (basis.start, at),
                None => [
                    StartBasis::InProcess,
                    StartBasis::Assigned,
                    StartBasis::Created,
                ]
                .into_iter()
                .find_map(|b| start_of(task, b).map(|at| (b, at)))?,
            };
            (start <= end).then_some(Span {
                start,
                end,
                running: false,
                from,
            })
        }
        None => {
            let start = chosen?;
            (task.status.is_open() && start <= now).then_some(Span {
                start,
                end: now,
                running: true,
                from: basis.start,
            })
        }
    }
}

/// 所要時間が予想工数を超えたか。どちらかがなければ `false`。
pub fn over_estimate(task: &Task, span: Option<Span>) -> bool {
    match (task.estimate, span) {
        (Some(est), Some(span)) => span.ms() > est.total_ms(),
        _ => false,
    }
}

/// 統計の数字。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Stats {
    /// 終わったタスクの数（期間の中で終わったもの）
    pub done: usize,
    /// 所要時間の合計・平均・中央値・最長（ミリ秒）。終わったタスクがなければ平均以下は `None`
    pub total_ms: i64,
    pub mean_ms: Option<i64>,
    pub median_ms: Option<i64>,
    pub longest_ms: Option<i64>,
    /// 終わったタスクのうち、予想工数があるものの数
    pub estimated: usize,
    /// そのうち、所要時間が予想工数に収まったもの
    pub within: usize,
    /// 予想工数があるもので、予想工数の合計と所要時間の合計
    pub estimated_ms: i64,
    pub estimated_actual_ms: i64,
    /// 終わっていないタスクの数と、そのうち予想工数があるものの数・予想工数の合計（残り）
    pub open: usize,
    pub open_estimated: usize,
    pub open_estimate_ms: i64,
    /// 今も数えているタスクのうち、予想工数を超えているもの
    pub open_over: usize,
}

impl Stats {
    /// 予想工数に収まった割合（0〜100）。予想工数のあるタスクがなければ `None`。
    pub fn within_percent(&self) -> Option<u32> {
        (self.estimated > 0).then(|| (self.within * 100 / self.estimated) as u32)
    }

    /// 所要時間の合計が予想工数の合計の何 % か。予想工数のあるタスクがなければ `None`。
    pub fn actual_percent(&self) -> Option<u32> {
        (self.estimated_ms > 0)
            .then(|| (self.estimated_actual_ms as i128 * 100 / self.estimated_ms as i128) as u32)
    }
}

/// 統計に入れるタスク 1 つ。
#[derive(Debug, Clone, Copy)]
pub struct Item<'a> {
    pub task: &'a Task,
    pub basis: Basis,
}

/// 統計を出す。`since` があれば、その時刻より後に終わったタスクだけを「終わった」に数える。
/// 中止したタスクは数えない。
pub fn stats(items: &[Item<'_>], self_id: Option<&str>, now: i64, since: Option<i64>) -> Stats {
    let mut out = Stats::default();
    let mut spans: Vec<i64> = Vec::new();
    for item in items {
        let task = item.task;
        if task.status == Status::Cancelled {
            continue;
        }
        let span = span(task, item.basis, self_id, now);
        let ended = span.filter(|s| !s.running);
        match ended {
            Some(s) => {
                if since.is_some_and(|since| s.end < since) {
                    continue;
                }
                out.done += 1;
                spans.push(s.ms());
                if let Some(est) = task.estimate {
                    out.estimated += 1;
                    out.estimated_ms += est.total_ms();
                    out.estimated_actual_ms += s.ms();
                    if s.ms() <= est.total_ms() {
                        out.within += 1;
                    }
                }
            }
            None if task.status.is_open() => {
                out.open += 1;
                if let Some(est) = task.estimate {
                    out.open_estimated += 1;
                    out.open_estimate_ms += est.total_ms();
                }
                if over_estimate(task, span) {
                    out.open_over += 1;
                }
            }
            // 完了にしたが時刻が分からない（読み込んだ古いデータなど）
            None => {}
        }
    }
    out.total_ms = spans.iter().sum();
    if !spans.is_empty() {
        spans.sort_unstable();
        out.mean_ms = Some(out.total_ms / spans.len() as i64);
        let mid = spans.len() / 2;
        out.median_ms = Some(if spans.len().is_multiple_of(2) {
            (spans[mid - 1] + spans[mid]) / 2
        } else {
            spans[mid]
        });
        out.longest_ms = spans.last().copied();
    }
    out
}

/// 担当者ごとの統計。担当者のいないタスクは `None` の行に入る。並びは担当者の id 順、`None` は最後。
pub fn stats_by_person(
    items: &[Item<'_>],
    self_id: Option<&str>,
    now: i64,
    since: Option<i64>,
) -> Vec<(Option<Id>, Stats)> {
    let mut groups: HashMap<Option<&str>, Vec<Item<'_>>> = HashMap::new();
    for item in items {
        if item.task.assignee_ids.is_empty() {
            groups.entry(None).or_default().push(*item);
        }
        for id in &item.task.assignee_ids {
            groups.entry(Some(id.as_str())).or_default().push(*item);
        }
    }
    let mut out: Vec<(Option<Id>, Stats)> = groups
        .into_iter()
        .map(|(id, items)| (id.map(str::to_owned), stats(&items, self_id, now, since)))
        .filter(|(_, s)| s.done > 0 || s.open > 0)
        .collect();
    out.sort_by(|a, b| match (&a.0, &b.0) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(x), Some(y)) => x.cmp(y),
    });
    out
}

/// 統計の期間。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Period {
    Week,
    #[default]
    Month,
    Quarter,
    Year,
    All,
}

impl Period {
    pub const ALL: [Period; 5] = [
        Period::Week,
        Period::Month,
        Period::Quarter,
        Period::Year,
        Period::All,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Period::Week => "7 日",
            Period::Month => "30 日",
            Period::Quarter => "90 日",
            Period::Year => "1 年",
            Period::All => "すべて",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Period::Week => "week",
            Period::Month => "month",
            Period::Quarter => "quarter",
            Period::Year => "year",
            Period::All => "all",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.key() == s)
    }

    /// この期間の始まり（`now` から数える）。すべてなら `None`。
    pub fn since(self, now: i64) -> Option<i64> {
        const DAY: i64 = 24 * 60 * 60 * 1000;
        let days = match self {
            Period::Week => 7,
            Period::Month => 30,
            Period::Quarter => 90,
            Period::Year => 365,
            Period::All => return None,
        };
        Some(now - days * DAY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::estimate::Estimate;
    use crate::model::Roles;

    const H: i64 = 60 * 60 * 1000;

    fn task(status: Status) -> Task {
        Task {
            id: "t".into(),
            workspace_id: None,
            title: "t".into(),
            description: String::new(),
            status,
            priority: 0,
            start: None,
            due: None,
            percent: 0,
            parent_id: None,
            assignee_ids: Vec::new(),
            assignee_roles: Vec::new(),
            assignee_done: Vec::new(),
            categories: Vec::new(),
            urls: Vec::new(),
            linked_workspace_ids: Vec::new(),
            position: 0,
            created_at: 0,
            updated_at: 0,
            completed_at: None,
            created_by: crate::Actor::Me,
            updated_by: crate::Actor::Me,
            estimate: None,
            time_basis: None,
            started_at: None,
            assigned_at: None,
        }
    }

    fn done(start: Option<i64>, end: i64) -> Task {
        let mut t = task(Status::Completed);
        t.started_at = start;
        t.completed_at = Some(end);
        t
    }

    #[test]
    fn 基準は保存した形と往復する() {
        for start in StartBasis::ALL {
            for end in EndBasis::ALL {
                let b = Basis::new(start, end);
                assert_eq!(Basis::parse(&b.code()), Some(b));
            }
        }
        assert_eq!(Basis::default().code(), "in_process>completed");
        assert_eq!(Basis::parse("x>completed"), None);
        assert_eq!(Basis::parse("created"), None);
        assert_eq!(Basis::default().label(), "進行中にした時 → 完了にした時");
        let json =
            serde_json::to_string(&Basis::new(StartBasis::Created, EndBasis::SelfDone)).unwrap();
        assert_eq!(json, "\"created>self_done\"");
        assert!(serde_json::from_str::<Basis>("\"a>b\"").is_err());
    }

    #[test]
    fn タスク_ワークスペース_全体の順に決まる() {
        let a = Basis::new(StartBasis::Created, EndBasis::Completed);
        let b = Basis::new(StartBasis::Assigned, EndBasis::Completed);
        let c = Basis::default();
        assert_eq!(resolve(Some(a), Some(b), c), a);
        assert_eq!(resolve(None, Some(b), c), b);
        assert_eq!(resolve(None, None, c), c);
    }

    #[test]
    fn 進行中から完了までを数える() {
        let t = done(Some(2 * H), 5 * H);
        let s = span(&t, Basis::default(), None, 100 * H).unwrap();
        assert_eq!(
            (s.ms(), s.running, s.from),
            (3 * H, false, StartBasis::InProcess)
        );
    }

    #[test]
    fn 進行中にしないで完了したら_担当が付いた時か作った時から() {
        let mut t = done(None, 5 * H);
        t.created_at = H;
        assert_eq!(
            span(&t, Basis::default(), None, 0).map(|s| (s.ms(), s.from)),
            Some((4 * H, StartBasis::Created))
        );
        t.assigned_at = Some(3 * H);
        assert_eq!(
            span(&t, Basis::default(), None, 0).map(|s| (s.ms(), s.from)),
            Some((2 * H, StartBasis::Assigned))
        );
    }

    #[test]
    fn 作った時や担当が付いた時を選べる() {
        let mut t = done(Some(4 * H), 6 * H);
        t.created_at = H;
        t.assigned_at = Some(2 * H);
        let of = |start| {
            span(&t, Basis::new(start, EndBasis::Completed), None, 0)
                .unwrap()
                .ms()
        };
        assert_eq!(of(StartBasis::Created), 5 * H);
        assert_eq!(of(StartBasis::Assigned), 4 * H);
        assert_eq!(of(StartBasis::InProcess), 2 * H);
    }

    #[test]
    fn 開始日から数える() {
        let mut t = task(Status::Completed);
        t.start = Some("2026-11-01 09:00".into());
        let start = start_date_ms("2026-11-01 09:00").unwrap();
        t.completed_at = Some(start + 3 * H);
        let b = Basis::new(StartBasis::StartDate, EndBasis::Completed);
        assert_eq!(span(&t, b, None, 0).unwrap().ms(), 3 * H);
        // 時刻がなければその日の 0:00
        assert_eq!(
            start_date_ms("2026-11-01").unwrap() + 9 * H,
            start_date_ms("2026-11-01 09:00").unwrap()
        );
        // 開始日が完了より後なら出さない
        t.start = Some("2026-12-01".into());
        assert_eq!(span(&t, b, None, 0), None);
    }

    #[test]
    fn 終わっていなければ今までの経過で_始まっていなければ出さない() {
        let mut t = task(Status::InProcess);
        t.started_at = Some(H);
        let s = span(&t, Basis::default(), None, 4 * H).unwrap();
        assert_eq!((s.ms(), s.running), (3 * H, true));
        let mut t = task(Status::NeedsAction);
        t.created_at = H;
        assert_eq!(span(&t, Basis::default(), None, 4 * H), None);
        let created = Basis::new(StartBasis::Created, EndBasis::Completed);
        assert_eq!(span(&t, created, None, 4 * H).unwrap().ms(), 3 * H);
        // 中止は数えない
        let mut t = done(Some(H), 2 * H);
        t.status = Status::Cancelled;
        assert_eq!(span(&t, Basis::default(), None, 4 * H), None);
    }

    #[test]
    fn 自分の分を完了にした時まで() {
        let mut t = task(Status::InProcess);
        t.started_at = Some(H);
        t.assignee_ids = vec!["me".into(), "you".into()];
        t.assignee_roles = vec![Roles::R; 2];
        t.assignee_done = vec![Some(3 * H), None];
        let b = Basis::new(StartBasis::InProcess, EndBasis::SelfDone);
        let s = span(&t, b, Some("me"), 10 * H).unwrap();
        assert_eq!((s.ms(), s.running), (2 * H, false));
        // 自分が担当でなければ、タスクを完了にした時
        let s = span(&t, b, Some("other"), 10 * H).unwrap();
        assert!(s.running);
        t.status = Status::Completed;
        t.completed_at = Some(5 * H);
        assert_eq!(span(&t, b, Some("other"), 10 * H).unwrap().ms(), 4 * H);
    }

    #[test]
    fn 予想工数を超えたか() {
        let mut t = done(Some(0), 3 * H);
        let s = span(&t, Basis::default(), None, 0);
        assert!(!over_estimate(&t, s));
        t.estimate = Some(Estimate::new(0, 0, 2, 0));
        assert!(over_estimate(&t, s));
        t.estimate = Some(Estimate::new(0, 0, 3, 0));
        assert!(!over_estimate(&t, s));
    }

    #[test]
    fn 統計を出す() {
        let mut a = done(Some(0), 2 * H);
        a.estimate = Some(Estimate::new(0, 0, 3, 0));
        let mut b = done(Some(0), 4 * H);
        b.estimate = Some(Estimate::new(0, 0, 2, 0));
        let c = done(Some(0), 6 * H);
        let mut open = task(Status::InProcess);
        open.started_at = Some(0);
        open.estimate = Some(Estimate::new(0, 0, 1, 0));
        let mut waiting = task(Status::NeedsAction);
        waiting.estimate = Some(Estimate::new(0, 1, 0, 0));
        let mut cancelled = done(Some(0), H);
        cancelled.status = Status::Cancelled;
        let tasks = [a, b, c, open, waiting, cancelled];
        let items: Vec<Item> = tasks
            .iter()
            .map(|task| Item {
                task,
                basis: Basis::default(),
            })
            .collect();
        let s = stats(&items, None, 10 * H, None);
        assert_eq!(s.done, 3);
        assert_eq!(s.total_ms, 12 * H);
        assert_eq!(s.mean_ms, Some(4 * H));
        assert_eq!(s.median_ms, Some(4 * H));
        assert_eq!(s.longest_ms, Some(6 * H));
        assert_eq!((s.estimated, s.within), (2, 1));
        assert_eq!(s.within_percent(), Some(50));
        assert_eq!((s.estimated_ms, s.estimated_actual_ms), (5 * H, 6 * H));
        assert_eq!(s.actual_percent(), Some(120));
        assert_eq!((s.open, s.open_estimated), (2, 2));
        assert_eq!(s.open_estimate_ms, 25 * H);
        assert_eq!(s.open_over, 1);
        // 期間の外で終わったものは数えない
        let s = stats(&items, None, 10 * H, Some(3 * H));
        assert_eq!(s.done, 2);
        assert_eq!(s.median_ms, Some(5 * H));
        let empty = stats(&[], None, 0, None);
        assert_eq!(
            (
                empty.mean_ms,
                empty.within_percent(),
                empty.actual_percent()
            ),
            (None, None, None)
        );
    }

    #[test]
    fn 担当者ごとの統計() {
        let mut a = done(Some(0), 2 * H);
        a.assignee_ids = vec!["p1".into(), "p2".into()];
        let mut b = done(Some(0), 4 * H);
        b.assignee_ids = vec!["p2".into()];
        let c = done(Some(0), H);
        let tasks = [a, b, c];
        let items: Vec<Item> = tasks
            .iter()
            .map(|task| Item {
                task,
                basis: Basis::default(),
            })
            .collect();
        let rows = stats_by_person(&items, None, 0, None);
        let summary: Vec<(Option<&str>, usize, i64)> = rows
            .iter()
            .map(|(id, s)| (id.as_deref(), s.done, s.total_ms))
            .collect();
        assert_eq!(
            summary,
            vec![(Some("p1"), 1, 2 * H), (Some("p2"), 2, 6 * H), (None, 1, H)]
        );
    }

    #[test]
    fn 期間() {
        assert_eq!(Period::All.since(100), None);
        assert_eq!(Period::Week.since(8 * 86_400_000), Some(86_400_000));
        for p in Period::ALL {
            assert_eq!(Period::parse(p.key()), Some(p));
        }
    }
}
