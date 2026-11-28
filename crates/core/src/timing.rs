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
    /// タスクに付けた GitHub の PR を作った時
    PrOpened,
}

impl StartBasis {
    pub const ALL: [StartBasis; 5] = [
        StartBasis::Created,
        StartBasis::Assigned,
        StartBasis::InProcess,
        StartBasis::StartDate,
        StartBasis::PrOpened,
    ];

    pub fn code(self) -> &'static str {
        match self {
            StartBasis::Created => "created",
            StartBasis::Assigned => "assigned",
            StartBasis::InProcess => "in_process",
            StartBasis::StartDate => "start_date",
            StartBasis::PrOpened => "pr_opened",
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
            StartBasis::PrOpened => "PR を作った時",
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
    /// タスクに付けた GitHub の PR に、作った人以外が初めてレビューした時
    PrReviewed,
    /// タスクに付けた GitHub の PR をマージした時
    PrMerged,
}

impl EndBasis {
    pub const ALL: [EndBasis; 4] = [
        EndBasis::Completed,
        EndBasis::SelfDone,
        EndBasis::PrReviewed,
        EndBasis::PrMerged,
    ];

    pub fn code(self) -> &'static str {
        match self {
            EndBasis::Completed => "completed",
            EndBasis::SelfDone => "self_done",
            EndBasis::PrReviewed => "pr_reviewed",
            EndBasis::PrMerged => "pr_merged",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.code() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            EndBasis::Completed => "完了にした時",
            EndBasis::SelfDone => "自分の分を完了にした時",
            EndBasis::PrReviewed => "PR の 1 回目のレビュー",
            EndBasis::PrMerged => "PR をマージした時",
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

    /// GitHub の PR の時刻を使うか。使うなら、タスクに付けた PR の時刻を読みに行く。
    pub fn uses_pr(&self) -> bool {
        self.start == StartBasis::PrOpened
            || matches!(self.end, EndBasis::PrReviewed | EndBasis::PrMerged)
    }
}

/// タスクに付けた GitHub の PR の時刻（GitHub から読んだもの。UNIX 時刻、ミリ秒）。
///
/// 所要時間の基準に PR の時刻を選んだタスクだけ、アプリが `gh` で読んで残す。
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct PrTimes {
    /// 読んだ PR（`https://github.com/owner/repo/pull/12`）
    pub url: String,
    /// PR を作った時
    pub opened: i64,
    /// 作った人以外が初めてレビューした時
    #[serde(default)]
    pub reviewed: Option<i64>,
    /// マージした時
    #[serde(default)]
    pub merged: Option<i64>,
    /// GitHub から読んだ時
    pub checked: i64,
}

impl PrTimes {
    /// まだ変わりうるか（マージしていなければ、レビューやマージがこれから付く）。
    pub fn settled(&self) -> bool {
        self.merged.is_some()
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

/// PR の時刻を読み直す間隔（ミリ秒）。マージするまでは、この間隔で読み直す。
pub const PR_REFRESH_MS: i64 = 60 * 60 * 1000;
/// 完了にしたあとも PR の時刻を読み直す長さ（ミリ秒）。タスクを先に完了にして、あとでマージすることがある。
pub const PR_AFTER_DONE_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// 1 回に読む PR の数の上限。残りは次の回に読む。
pub const PR_MAX_READS: usize = 30;

/// PR の時刻についてすること。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrJob {
    /// GitHub から読む
    Read { id: Id, url: String },
    /// PR のリンクを外したので、残した時刻を消す
    Clear { id: Id },
}

/// タスクに付けた GitHub の PR のリンク（`https://github.com/owner/repo/pull/12` の形）。
/// URL 欄 → タイトル → 説明の順に探し、最初に見つかったもの。
pub fn pr_url(task: &Task) -> Option<String> {
    task.urls
        .iter()
        .map(String::as_str)
        .chain([task.title.as_str(), task.description.as_str()])
        .find_map(find_pr)
}

fn find_pr(text: &str) -> Option<String> {
    const HOST: &str = "https://github.com/";
    let mut rest = text;
    while let Some(at) = rest.find(HOST) {
        let candidate = &rest[at..];
        // リンクは ASCII だけ。日本語の文に続けて書かれていても、そこで切る
        let end = candidate
            .find(|c: char| !c.is_ascii_graphic() || "<>\"'()[]{}`".contains(c))
            .unwrap_or(candidate.len());
        let path = candidate[HOST.len()..end]
            .split(['?', '#'])
            .next()
            .unwrap_or("");
        let parts: Vec<&str> = path.split('/').collect();
        if let [owner, repo, "pull", number, ..] = parts.as_slice() {
            let name = |s: &str| {
                !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
            };
            if name(owner) && name(repo) && number.parse::<u64>().is_ok() {
                return Some(format!("{HOST}{owner}/{repo}/pull/{number}"));
            }
        }
        rest = &candidate[end.max(HOST.len())..];
    }
    None
}

/// PR の時刻を読む・消すタスク。所要時間の基準に PR の時刻を使うタスクだけを見る。
///
/// 読むのは、まだ読んでいない・リンクが変わった・`force` で頼まれたものと、マージしていない PR を
/// [`PR_REFRESH_MS`] より前に読んだもの（完了にしたタスクは、完了から [`PR_AFTER_DONE_MS`] の間だけ）。
/// 頼まれたもの → まだ読んでいないもの → 古く読んだものの順に、[`PR_MAX_READS`] 件まで。
pub fn pr_jobs(
    tasks: &[Task],
    workspaces: &[crate::model::Workspace],
    standard: Basis,
    now: i64,
    force: Option<&str>,
) -> Vec<PrJob> {
    let mut clears = Vec::new();
    // (順番, 読んだ時刻, id, URL)
    let mut reads: Vec<(u8, i64, Id, String)> = Vec::new();
    for task in tasks {
        if task.status == Status::Cancelled || !basis_of(task, workspaces, standard).uses_pr() {
            continue;
        }
        let forced = force == Some(task.id.as_str());
        let Some(url) = pr_url(task) else {
            if task.pr.is_some() {
                clears.push(PrJob::Clear {
                    id: task.id.clone(),
                });
            }
            continue;
        };
        let rank = match &task.pr {
            _ if forced => 0,
            Some(pr) if pr.url == url => {
                let watching = task.status.is_open()
                    || task
                        .completed_at
                        .is_some_and(|done| pr.checked < done + PR_AFTER_DONE_MS);
                if pr.settled() || !watching || now - pr.checked < PR_REFRESH_MS {
                    continue;
                }
                2
            }
            _ => 1,
        };
        let checked = task.pr.as_ref().map_or(i64::MIN, |pr| pr.checked);
        reads.push((rank, checked, task.id.clone(), url));
    }
    reads.sort_by_key(|r| (r.0, r.1));
    clears
        .into_iter()
        .chain(
            reads
                .into_iter()
                .take(PR_MAX_READS)
                .map(|(_, _, id, url)| PrJob::Read { id, url }),
        )
        .collect()
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
    /// 終わりに実際に使った基準。PR の時刻がないまま完了にしたタスクは、完了にした時まで数える
    pub to: EndBasis,
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

/// 残した PR の時刻。今付いている PR のものだけ（リンクを付け替えたら、読み直すまで使わない）。
pub fn pr_of(task: &Task) -> Option<&PrTimes> {
    let pr = task.pr.as_ref()?;
    (pr_url(task).as_deref() == Some(pr.url.as_str())).then_some(pr)
}

/// その基準の始まりの時刻。なければ `None`。
fn start_of(task: &Task, basis: StartBasis) -> Option<i64> {
    match basis {
        StartBasis::Created => Some(task.created_at),
        StartBasis::Assigned => task.assigned_at,
        StartBasis::InProcess => task.started_at,
        StartBasis::StartDate => task.start.as_deref().and_then(start_date_ms),
        StartBasis::PrOpened => pr_of(task).map(|pr| pr.opened),
    }
}

/// その基準の終わりの時刻と、実際に使った基準。まだなら `None`。
///
/// PR の基準で、PR の時刻がまだない（PR がない、まだレビューやマージがない）まま完了にしたタスクは、
/// 完了にした時を使う。
fn end_of(task: &Task, basis: EndBasis, self_id: Option<&str>) -> Option<(i64, EndBasis)> {
    let pr = match basis {
        EndBasis::PrReviewed => pr_of(task).and_then(|pr| pr.reviewed),
        EndBasis::PrMerged => pr_of(task).and_then(|pr| pr.merged),
        EndBasis::Completed | EndBasis::SelfDone => {
            return own_end_of(task, basis, self_id).map(|at| (at, basis))
        }
    };
    match pr {
        Some(at) => Some((at, basis)),
        None => own_end_of(task, EndBasis::Completed, self_id).map(|at| (at, EndBasis::Completed)),
    }
}

/// タスクの中で決まる終わりの時刻（完了にした時・自分の分を完了にした時）。
fn own_end_of(task: &Task, basis: EndBasis, self_id: Option<&str>) -> Option<i64> {
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
        EndBasis::PrReviewed | EndBasis::PrMerged => None,
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
        Some((end, to)) => {
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
                to,
            })
        }
        None => {
            let start = chosen?;
            (task.status.is_open() && start <= now).then_some(Span {
                start,
                end: now,
                running: true,
                from: basis.start,
                to: basis.end,
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

/// グラフの 1 本。終わったタスクの数を、予想工数に収まったもの・超えたもの・予想工数のないものに分ける。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bar {
    pub label: String,
    pub within: usize,
    pub over: usize,
    pub unestimated: usize,
}

impl Bar {
    pub fn total(&self) -> usize {
        self.within + self.over + self.unestimated
    }
}

/// 統計のグラフ。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Charts {
    /// 完了数の推移（古い順）。期間が 7 日・30 日なら日ごと、90 日なら週ごと、1 年なら月ごと、
    /// すべてなら月ごと（2 年を超えたら年ごと）
    pub trend: Vec<Bar>,
    /// 所要時間の分布（短い順）
    pub spread: Vec<Bar>,
    /// 所要時間が予想工数の何 % だったかの分布（予想工数のあるものだけ。小さい順）
    pub ratio: Vec<Bar>,
}

/// 所要時間の分布の区切り（この長さ未満）と名前。最後は上限なし。
const SPREAD: [(i64, &str); 7] = [
    (HOUR, "〜1時間"),
    (4 * HOUR, "〜4時間"),
    (DAY, "〜1日"),
    (3 * DAY, "〜3日"),
    (7 * DAY, "〜1週間"),
    (30 * DAY, "〜1か月"),
    (i64::MAX, "1か月〜"),
];

/// 予想工数との比の区切り（この % 以下）と名前。100 % までが収まったもの。
const RATIO: [(i64, &str); 7] = [
    (50, "〜50%"),
    (80, "〜80%"),
    (100, "〜100%"),
    (125, "〜125%"),
    (150, "〜150%"),
    (200, "〜200%"),
    (i64::MAX, "200%〜"),
];

const HOUR: i64 = 60 * 60 * 1000;
const DAY: i64 = 24 * HOUR;

/// 推移の区切り方。
enum Step {
    Day(usize),
    Week(usize),
    Month(usize),
    Year(usize),
}

fn local_date(ms: i64) -> Option<chrono::NaiveDate> {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.date_naive())
}

fn months_between(a: chrono::NaiveDate, b: chrono::NaiveDate) -> i64 {
    use chrono::Datelike;
    (b.year() as i64 - a.year() as i64) * 12 + b.month() as i64 - a.month() as i64
}

/// グラフを出す。数えるタスクは [`stats`] と同じ（`period` の期間に終わったもの）。
pub fn charts(items: &[Item<'_>], self_id: Option<&str>, now: i64, period: Period) -> Charts {
    use chrono::Datelike;
    let since = period.since(now);
    let mut done: Vec<(&Task, Span)> = Vec::new();
    for item in items {
        if item.task.status == Status::Cancelled {
            continue;
        }
        if let Some(s) = span(item.task, item.basis, self_id, now).filter(|s| !s.running) {
            if since.is_none_or(|since| s.end >= since) {
                done.push((item.task, s));
            }
        }
    }
    let Some(today) = local_date(now) else {
        return Charts::default();
    };
    // 予想工数に収まったか（予想工数がなければ None）
    let fits = |task: &Task, s: &Span| task.estimate.map(|e| s.ms() <= e.total_ms());
    let add = |bar: &mut Bar, fit: Option<bool>| match fit {
        Some(true) => bar.within += 1,
        Some(false) => bar.over += 1,
        None => bar.unestimated += 1,
    };

    let first = done
        .iter()
        .filter_map(|(_, s)| local_date(s.end))
        .min()
        .unwrap_or(today);
    let step = match period {
        Period::Week => Step::Day(7),
        Period::Month => Step::Day(30),
        Period::Quarter => Step::Week(13),
        Period::Year => Step::Month(12),
        Period::All => {
            let months = months_between(first, today) + 1;
            if months > 24 {
                Step::Year((today.year() - first.year()) as usize + 1)
            } else {
                Step::Month(months.max(1) as usize)
            }
        }
    };
    let (count, label): (usize, Box<dyn Fn(usize) -> String>) = match step {
        Step::Day(n) => (
            n,
            Box::new(move |i| {
                let d = today - chrono::Days::new((n - 1 - i) as u64);
                format!("{}/{}", d.month(), d.day())
            }),
        ),
        Step::Week(n) => (
            n,
            Box::new(move |i| {
                let d = today - chrono::Days::new(((n - 1 - i) * 7 + 6) as u64);
                format!("{}/{}〜", d.month(), d.day())
            }),
        ),
        Step::Month(n) => (
            n,
            Box::new(move |i| {
                let d = today
                    .with_day(1)
                    .and_then(|d| d.checked_sub_months(chrono::Months::new((n - 1 - i) as u32)))
                    .unwrap_or(today);
                if d.month() == 1 || i == 0 {
                    format!("{}/{}", d.year(), d.month())
                } else {
                    format!("{}月", d.month())
                }
            }),
        ),
        Step::Year(n) => (
            n,
            Box::new(move |i| format!("{}", today.year() - (n - 1 - i) as i32)),
        ),
    };
    let mut trend: Vec<Bar> = (0..count)
        .map(|i| Bar {
            label: label(i),
            ..Bar::default()
        })
        .collect();
    for (task, s) in &done {
        let Some(date) = local_date(s.end) else {
            continue;
        };
        // 今から数えて何個前の区切りか
        let back = match step {
            Step::Day(_) => (today - date).num_days(),
            Step::Week(_) => (today - date).num_days() / 7,
            Step::Month(_) => months_between(date, today),
            Step::Year(_) => (today.year() - date.year()) as i64,
        };
        if back < 0 || back as usize >= count {
            continue;
        }
        add(&mut trend[count - 1 - back as usize], fits(task, s));
    }

    let mut spread: Vec<Bar> = SPREAD
        .iter()
        .map(|(_, l)| Bar {
            label: (*l).into(),
            ..Bar::default()
        })
        .collect();
    let mut ratio: Vec<Bar> = RATIO
        .iter()
        .map(|(_, l)| Bar {
            label: (*l).into(),
            ..Bar::default()
        })
        .collect();
    for (task, s) in &done {
        let ix = SPREAD
            .iter()
            .position(|(max, _)| s.ms() < *max)
            .unwrap_or(0);
        add(&mut spread[ix], fits(task, s));
        if let Some(est) = task.estimate.filter(|e| e.total_ms() > 0) {
            let percent = (s.ms() as i128 * 100 / est.total_ms() as i128) as i64;
            let ix = RATIO
                .iter()
                .position(|(max, _)| percent <= *max)
                .unwrap_or(0);
            add(&mut ratio[ix], Some(percent <= 100));
        }
    }
    Charts {
        trend,
        spread,
        ratio,
    }
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
            pr: None,
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
    fn prのリンクを探す() {
        let mut t = task(Status::InProcess);
        t.description = "レビュー待ち（https://github.com/o/r.js/pull/12/files?x=1）".into();
        assert_eq!(
            pr_url(&t).as_deref(),
            Some("https://github.com/o/r.js/pull/12")
        );
        // URL 欄が先。issue は PR ではない
        t.urls = vec![
            "https://github.com/o/r/issues/3".into(),
            "https://github.com/a/b/pull/7".into(),
        ];
        assert_eq!(pr_url(&t).as_deref(), Some("https://github.com/a/b/pull/7"));
        t.urls.clear();
        t.description = "https://github.com/o/r/pull/x".into();
        assert_eq!(pr_url(&t), None);
    }

    #[test]
    fn prの時刻を読むタスクを選ぶ() {
        let pr_basis = Some(Basis::new(StartBasis::PrOpened, EndBasis::PrMerged));
        let url = "https://github.com/o/r/pull/1";
        let with = |id: &str, basis: Option<Basis>, pr: Option<PrTimes>| {
            let mut t = task(Status::InProcess);
            t.id = id.into();
            t.time_basis = basis;
            t.urls = vec![url.into()];
            t.pr = pr;
            t
        };
        let times = |checked: i64, merged: Option<i64>| PrTimes {
            url: url.into(),
            opened: 0,
            reviewed: None,
            merged,
            checked,
        };
        let now = 10 * PR_REFRESH_MS;
        let mut gone = with("gone", pr_basis, Some(times(now, None)));
        gone.urls.clear();
        let tasks = vec![
            // 基準が PR でなければ見ない
            with("plain", None, None),
            with("fresh", pr_basis, Some(times(now - 1, None))),
            with("stale", pr_basis, Some(times(now - PR_REFRESH_MS, None))),
            with("merged", pr_basis, Some(times(0, Some(1)))),
            with("new", pr_basis, None),
            gone,
        ];
        let read = |id: &str| PrJob::Read {
            id: id.into(),
            url: url.into(),
        };
        assert_eq!(
            pr_jobs(&tasks, &[], Basis::default(), now, None),
            vec![
                PrJob::Clear { id: "gone".into() },
                read("new"),
                read("stale")
            ]
        );
        // 頼まれたものは、読んだばかりでも、マージしていても読む
        assert_eq!(
            pr_jobs(&tasks, &[], Basis::default(), now, Some("merged"))[1],
            read("merged")
        );
        // 全体の標準が PR なら、タスクに基準がなくても読む
        let all = pr_jobs(
            &tasks,
            &[],
            Basis::new(StartBasis::PrOpened, EndBasis::PrMerged),
            now,
            None,
        );
        assert!(all.contains(&read("plain")));
        // 完了にしてから長くたったものは読み直さない
        let mut done = with("done", pr_basis, Some(times(now - PR_REFRESH_MS, None)));
        done.status = Status::Completed;
        done.completed_at = Some(now - PR_REFRESH_MS - PR_AFTER_DONE_MS - 1);
        assert!(pr_jobs(&[done.clone()], &[], Basis::default(), now, None).is_empty());
        done.completed_at = Some(now - PR_REFRESH_MS * 2);
        assert_eq!(
            pr_jobs(&[done], &[], Basis::default(), now, None),
            vec![read("done")]
        );
    }

    #[test]
    fn prの時刻から数える() {
        let mut t = task(Status::InProcess);
        t.created_at = 0;
        t.started_at = Some(H);
        let b = Basis::new(StartBasis::PrOpened, EndBasis::PrMerged);
        assert!(b.uses_pr());
        assert!(!Basis::default().uses_pr());
        // PR をまだ読んでいなければ、終わっていないタスクは数えない
        assert_eq!(span(&t, b, None, 10 * H), None);
        t.urls = vec!["https://github.com/o/r/pull/1".into()];
        t.pr = Some(PrTimes {
            url: "https://github.com/o/r/pull/1".into(),
            opened: 2 * H,
            reviewed: None,
            merged: None,
            checked: 3 * H,
        });
        let s = span(&t, b, None, 10 * H).unwrap();
        assert_eq!(
            (s.start, s.running, s.from, s.to),
            (2 * H, true, StartBasis::PrOpened, EndBasis::PrMerged)
        );
        // レビューとマージが付くと、そこまで（タスクが終わっていなくても終わり）
        t.pr.as_mut().unwrap().reviewed = Some(4 * H);
        t.pr.as_mut().unwrap().merged = Some(6 * H);
        let s = span(&t, b, None, 10 * H).unwrap();
        assert_eq!((s.ms(), s.running), (4 * H, false));
        let r = Basis::new(StartBasis::PrOpened, EndBasis::PrReviewed);
        assert_eq!(span(&t, r, None, 10 * H).unwrap().ms(), 2 * H);
        // リンクを別の PR に付け替えたら、読み直すまで前の PR の時刻は使わない
        t.urls = vec!["https://github.com/o/r/pull/2".into()];
        assert_eq!(span(&t, b, None, 10 * H), None);
        t.urls = vec!["https://github.com/o/r/pull/1".into()];
        // PR がないまま完了にしたタスクは、進行中にした時 → 完了にした時
        t.pr = None;
        t.status = Status::Completed;
        t.completed_at = Some(5 * H);
        let s = span(&t, b, None, 10 * H).unwrap();
        assert_eq!(
            (s.ms(), s.from, s.to),
            (4 * H, StartBasis::InProcess, EndBasis::Completed)
        );
        // 保存する形
        assert_eq!(b.code(), "pr_opened>pr_merged");
        assert_eq!(
            Basis::parse("created>pr_reviewed"),
            Some(Basis::new(StartBasis::Created, EndBasis::PrReviewed))
        );
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
    fn グラフ() {
        const D: i64 = 24 * H;
        // 今日の正午を今にして、日付の変わり目にかからないようにする
        let now = {
            use chrono::TimeZone;
            let noon = chrono::Local::now()
                .date_naive()
                .and_hms_opt(12, 0, 0)
                .unwrap();
            chrono::Local
                .from_local_datetime(&noon)
                .earliest()
                .unwrap()
                .timestamp_millis()
        };
        // 今日 2 件（30 分で予想 1 時間、5 時間で予想 2 時間）、2 日前に 1 件（2 日、予想なし）、40 日前に 1 件
        let mut a = done(Some(now - H), now - H / 2);
        a.estimate = Some(Estimate::new(0, 0, 1, 0));
        let mut b = done(Some(now - 5 * H), now);
        b.estimate = Some(Estimate::new(0, 0, 2, 0));
        let c = done(Some(now - 4 * D), now - 2 * D);
        let old = done(Some(now - 41 * D), now - 40 * D);
        let tasks = [a, b, c, old];
        let items: Vec<Item> = tasks
            .iter()
            .map(|task| Item {
                task,
                basis: Basis::default(),
            })
            .collect();
        let g = charts(&items, None, now, Period::Week);
        assert_eq!(g.trend.len(), 7);
        let last = g.trend.last().unwrap();
        assert_eq!((last.within, last.over, last.unestimated), (1, 1, 0));
        assert_eq!(g.trend[4].unestimated, 1);
        assert_eq!(g.trend.iter().map(Bar::total).sum::<usize>(), 3);
        // 分布: 30 分 → 〜1時間、5 時間 → 〜1日、2 日 → 〜3日
        let spread: Vec<usize> = g.spread.iter().map(Bar::total).collect();
        assert_eq!(spread, [1, 0, 1, 1, 0, 0, 0]);
        // 予想工数との比: 50 % → 〜50%、250 % → 200%〜
        let ratio: Vec<(usize, usize)> = g.ratio.iter().map(|b| (b.within, b.over)).collect();
        assert_eq!(
            ratio,
            [(1, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 1)]
        );
        // 90 日は週ごと、1 年は月ごと。すべては一番古いものからの月ごと
        let q = charts(&items, None, now, Period::Quarter);
        assert_eq!(q.trend.len(), 13);
        assert_eq!(q.trend.iter().map(Bar::total).sum::<usize>(), 4);
        assert_eq!(charts(&items, None, now, Period::Year).trend.len(), 12);
        let all = charts(&items, None, now, Period::All);
        assert!((2..=3).contains(&all.trend.len()));
        assert_eq!(all.trend.iter().map(Bar::total).sum::<usize>(), 4);
        assert!(charts(&[], None, now, Period::All).trend.len() == 1);
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
