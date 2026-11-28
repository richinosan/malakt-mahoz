//! 定時タスク。決まった時刻が来るたびに、雛形から新しいタスクを 1 件作る。
//!
//! 繰り返し（`repeat`）が 1 つのタスクを写して次の回を作るのに対して、定時タスクはワークスペース
//! （またはどのワークスペースにも入れない場所）の設定で、雛形を持つ。雛形にはタスクで決められる
//! 項目がすべてある（タイトル・URL・説明・状態・優先度・担当と役・開始日・期限・通知・進捗・
//! カテゴリ・親タスク・ほかのワークスペース・サブタスク・添付）。開始日と期限は、回の日から何日後か
//! で持つ。決めていない項目には、ワークスペースの既定値が入る。AI への指示があれば、作ったタスクを
//! AI の待ち行列（繰り返しと同じ `repeat_ai`）に入れる。
//!
//! アプリが動いていなかった間の回は、いちばん新しい 1 回だけを作る（溜めない）。
//! 設定（`schedules`）は元に戻せる。最後に作った回（`schedule_last`）は元に戻す相手にしない。
//! 元に戻しても、同じ回をもう一度作らない。

use chrono::Duration;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::actor::Actor;
use crate::alert;
use crate::model::*;
use crate::repeat::{clean_ai, date, ymd, Rule};
use crate::store::{clean_draft, insert_task, record_created};
use crate::text::{normalize, normalize_line};
use crate::{Error, Result, Store};

/// 1 つの場所（ワークスペース 1 つ、またはワークスペースの外）に置ける定時タスクの数。
pub const MAX_SCHEDULES: usize = 50;
/// タイトルの長さの上限。
pub const MAX_TITLE_CHARS: usize = 200;
/// 担当の数の上限。
pub const MAX_ASSIGNEES: usize = 20;
/// 期限を回の日から何日後にできるか。
pub const MAX_DUE_DAYS: i64 = 365;
/// サブタスクの数の上限。
pub const MAX_SUBTASKS: usize = 100;
/// 添付の数の上限。
pub const MAX_FILES: usize = 50;
/// 名前を付けずに足した定時タスクのタイトル。
pub const NEW_TITLE: &str = "新規定時タスク";
/// 新しく足したときの時刻。
pub const NEW_TIME: &str = "09:00";
/// `schedule_last` に残す行の数。消した定時タスクの分は、これを超えたら古いものから消す。
const MAX_LAST_ROWS: i64 = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: Id,
    /// タスクを作るワークスペース。なければワークスペースの外（未所属）に作る。
    pub workspace_id: Option<Id>,
    /// 作るタスクのタイトル。
    pub title: String,
    pub rule: Rule,
    /// 作ったタスクで AI に頼むこと。空なら頼まない。
    #[serde(default)]
    pub ai: String,
    /// 作るタスクの担当。空ならワークスペースの既定値。
    #[serde(default)]
    pub assignee_ids: Vec<Id>,
    /// 期限を回の日の何日後にするか。`None` ならワークスペースの既定値。
    #[serde(default)]
    pub due_days: Option<i64>,
    /// 期限の時刻（`HH:MM`）。`None` なら日付だけ。
    #[serde(default)]
    pub due_time: Option<String>,
    pub enabled: bool,
    /// 次にタスクを作る時刻（`YYYY-MM-DD HH:MM`）。
    pub next_at: String,
    pub position: i64,
    pub created_at: i64,
    pub updated_at: i64,
    /// タイトル・担当・期限のほかの、作るタスクの項目。
    #[serde(default)]
    pub task: Template,
}

/// 雛形の、タイトル・担当・期限のほかの項目。タスクで決められるものはすべてここにある。
/// `None` の項目は、ワークスペースの既定値を使う。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Template {
    /// 説明。`None` ならワークスペースの既定値。
    pub description: Option<String>,
    /// 作ったときの状態。
    pub status: Status,
    /// 優先度（0 = なし、1 = 最高 … 9 = 最低）。`None` ならワークスペースの既定値。
    pub priority: Option<u8>,
    /// 開始日を回の日の何日後にするか。`None` なら開始日なし。
    pub start_days: Option<i64>,
    /// 開始日の時刻（`HH:MM`）。`None` なら日付だけ。
    pub start_time: Option<String>,
    /// 進捗（0〜100）。
    pub percent: u8,
    /// 親タスク。作ったタスクをそのサブタスクにする。親がもうなければ、親なしで作る。
    pub parent_id: Option<Id>,
    /// 担当者ごとの役。`Schedule::assignee_ids` と同じ並び。足りない分は R。
    pub roles: Vec<Roles>,
    /// カテゴリ。`None` ならワークスペースの既定値。
    pub categories: Option<Vec<String>>,
    /// URL。足した順。
    pub urls: Vec<String>,
    /// ほかに表示するワークスペース。`None` ならワークスペースの既定値。
    pub linked_workspace_ids: Option<Vec<Id>>,
    /// 時刻つきの期限の通知（何分前）。`None` なら全体の標準。
    pub alerts_timed: Option<Vec<u32>>,
    /// 日付だけの期限の通知（何日前）。`None` なら全体の標準。
    pub alerts_date: Option<Vec<u32>>,
    /// サブタスクのタイトル。並びのとおりに作る。
    pub subtasks: Vec<String>,
}

impl Default for Template {
    fn default() -> Self {
        Template {
            description: None,
            status: Status::NeedsAction,
            priority: None,
            start_days: None,
            start_time: None,
            percent: 0,
            parent_id: None,
            roles: Vec::new(),
            categories: None,
            urls: Vec::new(),
            linked_workspace_ids: None,
            alerts_timed: None,
            alerts_date: None,
            subtasks: Vec::new(),
        }
    }
}

/// 雛形に付けた添付。中身は添付と同じ `blobs` に、ハッシュごとに 1 つだけ置く。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleFile {
    pub id: Id,
    pub schedule_id: Id,
    pub name: String,
    pub size: u64,
    pub hash: String,
    pub created_at: i64,
}

/// 回の日 `day` の `days` 日後（`time` があれば時刻つき）。
fn day_after(day: &str, days: Option<i64>, time: Option<&str>) -> Option<String> {
    let d = date(day)? + Duration::days(days?);
    Some(match time {
        Some(time) => format!("{} {time}", ymd(d)),
        None => ymd(d),
    })
}

/// 「当日」「翌日 17:00」「3 日後」。`days` が `None` なら `None`。
fn offset_label(days: Option<i64>, time: Option<&str>) -> Option<String> {
    let mut text = due_days_label(days?);
    if let Some(time) = time {
        text.push(' ');
        text.push_str(time.strip_prefix('0').unwrap_or(time));
    }
    Some(text)
}

impl Schedule {
    /// 回の日に作るタスクの期限。期限を決めていなければ `None`。
    pub fn due_on(&self, day: &str) -> Option<String> {
        day_after(day, self.due_days, self.due_time.as_deref())
    }

    /// 期限の短い表示（「当日」「翌日 17:00」「3 日後」）。決めていなければ `None`。
    pub fn due_label(&self) -> Option<String> {
        offset_label(self.due_days, self.due_time.as_deref())
    }

    /// 回の日に作るタスクの開始日。決めていなければ `None`。
    pub fn start_on(&self, day: &str) -> Option<String> {
        day_after(day, self.task.start_days, self.task.start_time.as_deref())
    }

    /// 開始日の短い表示。決めていなければ `None`。
    pub fn start_label(&self) -> Option<String> {
        offset_label(self.task.start_days, self.task.start_time.as_deref())
    }

    /// 担当者の役。担当でなければ `None`。
    pub fn roles_of(&self, person_id: &str) -> Option<Roles> {
        let ix = self.assignee_ids.iter().position(|p| p == person_id)?;
        Some(self.task.roles.get(ix).copied().unwrap_or_default())
    }
}

/// 「当日」「翌日」「3 日後」「1 週間後」。
pub fn due_days_label(days: i64) -> String {
    match days {
        0 => "当日".into(),
        1 => "翌日".into(),
        d if d > 0 && d % 7 == 0 => format!("{} 週間後", d / 7),
        d => format!("{d} 日後"),
    }
}

/// 書き換え。`None` の項目は変えない。
#[derive(Debug, Clone, Default)]
pub struct SchedulePatch {
    pub title: Option<String>,
    pub rule: Option<Rule>,
    pub ai: Option<String>,
    pub assignee_ids: Option<Vec<Id>>,
    /// `Some(None)` で期限を決めない。
    pub due_days: Option<Option<i64>>,
    /// `Some(None)` で時刻を決めない。
    pub due_time: Option<Option<String>>,
    pub enabled: Option<bool>,
    /// タイトル・担当・期限のほかの項目。渡すと全部をこれにする。
    /// 渡さずに担当を変えたときは、残った人の役はそのまま、足した人は R。
    pub task: Option<Template>,
}

const COLUMNS: &str = "id, workspace_id, title, rule, ai, assignees, due_days, due_time, \
                       enabled, next_at, position, created_at, updated_at, task";

fn json_error(ix: usize, e: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(ix, rusqlite::types::Type::Text, Box::new(e))
}

fn from_row(r: &Row) -> rusqlite::Result<Schedule> {
    let rule: String = r.get(3)?;
    let assignees: String = r.get(5)?;
    let task: String = r.get(13)?;
    Ok(Schedule {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        title: r.get(2)?,
        rule: serde_json::from_str(&rule).map_err(|e| json_error(3, e))?,
        ai: r.get(4)?,
        assignee_ids: serde_json::from_str(&assignees).map_err(|e| json_error(5, e))?,
        due_days: r.get(6)?,
        due_time: r.get(7)?,
        enabled: r.get(8)?,
        next_at: r.get(9)?,
        position: r.get(10)?,
        created_at: r.get(11)?,
        updated_at: r.get(12)?,
        task: serde_json::from_str(&task).map_err(|e| json_error(13, e))?,
    })
}

/// ID が同じものは上書きし、なければ足す。上書きは UPDATE にして、元に戻すで前の中身に戻るようにする。
pub(crate) fn save(conn: &Connection, s: &Schedule) -> Result<()> {
    conn.prepare_cached(&format!(
        "INSERT INTO schedules({COLUMNS}) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
         ON CONFLICT(id) DO UPDATE SET workspace_id = excluded.workspace_id, \
         title = excluded.title, rule = excluded.rule, ai = excluded.ai, \
         assignees = excluded.assignees, due_days = excluded.due_days, \
         due_time = excluded.due_time, enabled = excluded.enabled, next_at = excluded.next_at, \
         position = excluded.position, created_at = excluded.created_at, \
         updated_at = excluded.updated_at, task = excluded.task"
    ))?
    .execute(params![
        s.id,
        s.workspace_id,
        s.title,
        serde_json::to_string(&s.rule)?,
        s.ai,
        serde_json::to_string(&s.assignee_ids)?,
        s.due_days,
        s.due_time,
        s.enabled,
        s.next_at,
        s.position,
        s.created_at,
        s.updated_at,
        serde_json::to_string(&s.task)?
    ])?;
    Ok(())
}

fn clean_title(title: &str) -> Result<String> {
    let title = normalize_line(title);
    if title.is_empty() {
        return Err(Error::Invalid("定時タスクのタイトルが空です".into()));
    }
    let count = title.chars().count();
    if count > MAX_TITLE_CHARS {
        return Err(Error::Invalid(format!(
            "定時タスクのタイトルは {MAX_TITLE_CHARS} 文字までです（今は {count} 文字）"
        )));
    }
    Ok(title)
}

fn clean_assignees(ids: &[Id]) -> Result<Vec<Id>> {
    let mut out: Vec<Id> = Vec::new();
    for id in ids {
        if !id.is_empty() && !out.contains(id) {
            out.push(id.clone());
        }
    }
    if out.len() > MAX_ASSIGNEES {
        return Err(Error::Invalid(format!(
            "定時タスクの担当は {MAX_ASSIGNEES} 人までです"
        )));
    }
    Ok(out)
}

fn clean_due(days: Option<i64>, time: Option<&str>) -> Result<(Option<i64>, Option<String>)> {
    clean_offset(days, time, "期限")
}

/// 回の日から何日後か（期限・開始日）と、その時刻を確かめる。
fn clean_offset(
    days: Option<i64>,
    time: Option<&str>,
    what: &str,
) -> Result<(Option<i64>, Option<String>)> {
    let Some(days) = days else {
        return Ok((None, None));
    };
    if !(0..=MAX_DUE_DAYS).contains(&days) {
        return Err(Error::Invalid(format!(
            "定時タスクの{what}は 0 から {MAX_DUE_DAYS} 日後です"
        )));
    }
    let time = match time.map(str::trim) {
        None | Some("") => None,
        Some(t) if is_valid_time(t) => Some(t.to_owned()),
        Some(t) => {
            return Err(Error::Invalid(format!(
                "定時タスクの{what}の時刻が正しくありません: {t}"
            )))
        }
    };
    Ok((Some(days), time))
}

/// 役を担当の数にそろえる。足りない分と空の役は R。A（説明責任者）は最初の 1 人だけ。
fn clean_roles(roles: &[Roles], count: usize) -> Vec<Roles> {
    let mut out: Vec<Roles> = (0..count)
        .map(|i| {
            roles
                .get(i)
                .copied()
                .filter(|r| !r.is_empty())
                .unwrap_or_default()
        })
        .collect();
    let mut seen_a = false;
    for r in &mut out {
        if r.contains(Role::A) {
            if seen_a {
                let rest = r.without(Role::A);
                *r = if rest.is_empty() { Roles::R } else { rest };
            }
            seen_a = true;
        }
    }
    out
}

/// 雛形の、タイトル・担当・期限のほかの項目を整えて確かめる。`assignees` は担当の数。
/// ほかのデータ（親タスクやワークスペース）があるかどうかは見ない。
pub fn clean_template(t: &Template, assignees: usize) -> Result<Template> {
    if t.priority.is_some_and(|p| p > 9) {
        return Err(Error::Invalid("優先度は 0 から 9 です".into()));
    }
    if t.percent > 100 {
        return Err(Error::Invalid("進捗は 0 から 100 です".into()));
    }
    let (start_days, start_time) = clean_offset(t.start_days, t.start_time.as_deref(), "開始日")?;
    let categories = t.categories.as_ref().map(|list| {
        let mut list: Vec<String> = list
            .iter()
            .map(|c| normalize_line(c))
            .filter(|c| !c.is_empty())
            .collect();
        list.sort_unstable();
        list.dedup();
        list
    });
    let urls = crate::weblink::clean_all(&t.urls).map_err(Error::Invalid)?;
    let linked_workspace_ids = t.linked_workspace_ids.as_ref().map(|ids| {
        let mut out: Vec<Id> = Vec::new();
        for id in ids {
            if !id.is_empty() && !out.contains(id) {
                out.push(id.clone());
            }
        }
        out
    });
    let mut subtasks = Vec::new();
    for title in &t.subtasks {
        let title = normalize_line(title);
        if title.is_empty() {
            continue;
        }
        let count = title.chars().count();
        if count > MAX_TITLE_CHARS {
            return Err(Error::Invalid(format!(
                "サブタスクのタイトルは {MAX_TITLE_CHARS} 文字までです（今は {count} 文字）"
            )));
        }
        subtasks.push(title);
    }
    if subtasks.len() > MAX_SUBTASKS {
        return Err(Error::Invalid(format!(
            "定時タスクのサブタスクは {MAX_SUBTASKS} 件までです"
        )));
    }
    Ok(Template {
        description: t.description.as_deref().map(normalize),
        status: t.status,
        priority: t.priority,
        start_days,
        start_time,
        // 完了にすると進捗は 100% になる
        percent: if t.status == Status::Completed {
            100
        } else {
            t.percent
        },
        parent_id: t.parent_id.clone().filter(|p| !p.is_empty()),
        roles: clean_roles(&t.roles, assignees),
        categories,
        urls,
        linked_workspace_ids,
        alerts_timed: t
            .alerts_timed
            .as_deref()
            .map(alert::clean_timed)
            .transpose()?,
        alerts_date: t
            .alerts_date
            .as_deref()
            .map(alert::clean_date)
            .transpose()?,
        subtasks,
    })
}

/// 取り込む定時タスクを整えて確かめる。
pub fn clean(s: &Schedule) -> Result<Schedule> {
    if s.id.is_empty() || !is_valid_when(&s.next_at) {
        return Err(Error::Invalid(format!(
            "定時タスク「{}」が正しくありません",
            s.title
        )));
    }
    let (due_days, due_time) = clean_due(s.due_days, s.due_time.as_deref())?;
    let assignee_ids = clean_assignees(&s.assignee_ids)?;
    // 重なりを外す前の並びで役を引き、外したあとの並びにそろえる
    let roles: Vec<Roles> = assignee_ids
        .iter()
        .map(|id| s.roles_of(id).unwrap_or_default())
        .collect();
    let task = clean_template(
        &Template {
            roles,
            ..s.task.clone()
        },
        assignee_ids.len(),
    )?;
    Ok(Schedule {
        title: clean_title(&s.title)?,
        rule: s.rule.clean()?,
        ai: clean_ai(&s.ai)?,
        assignee_ids,
        due_days,
        due_time,
        task,
        ..s.clone()
    })
}

/// 次にタスクを作る時刻。作る時刻が `now` より後の、最初の回。
fn next_at(rule: &Rule, now: &str) -> Result<String> {
    let today = date(date_of(now))
        .ok_or_else(|| Error::Invalid(format!("日時が正しくありません: {now}")))?;
    let before = today - Duration::days(1);
    rule.upcoming(before, now)
        .map(|d| rule.when(d))
        .ok_or_else(|| Error::Invalid("次の回が見つかりません".into()))
}

impl Store {
    /// その場所の定時タスク。`None` ならワークスペースの外。並びは足した順。
    pub fn schedules(&self, workspace: Option<&str>) -> Result<Vec<Schedule>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM schedules WHERE workspace_id IS ?1 ORDER BY position, id"
        ))?;
        let rows = stmt.query_map(params![workspace], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// すべての定時タスク。
    pub fn all_schedules(&self) -> Result<Vec<Schedule>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM schedules ORDER BY position, id"
        ))?;
        let rows = stmt.query_map([], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn schedule(&self, id: &str) -> Result<Schedule> {
        self.conn
            .prepare_cached(&format!("SELECT {COLUMNS} FROM schedules WHERE id = ?1"))?
            .query_row(params![id], from_row)
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("定時タスク {id}")))
    }

    /// 定時タスクを足す。タイトルが空なら「新規定時タスク」、決まりがなければ今日の曜日の毎週 9:00。
    /// `now` は今の日時（`YYYY-MM-DD HH:MM`）。足したとたんにタスクは作らない。
    pub fn create_schedule(
        &mut self,
        workspace: Option<&str>,
        title: &str,
        rule: Option<&Rule>,
        now: &str,
    ) -> Result<Schedule> {
        if let Some(id) = workspace {
            self.workspace(id)?;
        }
        let title = if title.trim().is_empty() {
            NEW_TITLE.to_owned()
        } else {
            clean_title(title)?
        };
        let rule = match rule {
            Some(rule) => rule.clean()?,
            None => Rule {
                time: Some(NEW_TIME.into()),
                ..Rule::weekly(date_of(now))
            }
            .clean()?,
        };
        let next_at = next_at(&rule, now)?;
        let (count, last): (usize, Option<i64>) = self.conn.query_row(
            "SELECT COUNT(*), MAX(position) FROM schedules WHERE workspace_id IS ?1",
            params![workspace],
            |r| Ok((r.get::<_, i64>(0)? as usize, r.get(1)?)),
        )?;
        if count >= MAX_SCHEDULES {
            return Err(Error::Invalid(format!(
                "定時タスクは 1 つの場所に {MAX_SCHEDULES} 個までです"
            )));
        }
        let now_ms = now_ms();
        let schedule = Schedule {
            id: new_id(),
            workspace_id: workspace.map(str::to_owned),
            title,
            rule,
            ai: String::new(),
            assignee_ids: Vec::new(),
            due_days: None,
            due_time: None,
            enabled: true,
            next_at,
            position: last.map_or(0, |p| p + 1),
            created_at: now_ms,
            updated_at: now_ms,
            task: Template::default(),
        };
        save(&self.conn, &schedule)?;
        Ok(schedule)
    }

    /// 書き換える。決まりを変えたときと、有効にしたときは、次の時刻を `now` から決め直す。
    pub fn update_schedule(
        &mut self,
        id: &str,
        patch: &SchedulePatch,
        now: &str,
    ) -> Result<Schedule> {
        let mut s = self.schedule(id)?;
        let before = s.clone();
        if let Some(title) = &patch.title {
            s.title = clean_title(title)?;
        }
        if let Some(rule) = &patch.rule {
            s.rule = rule.clean()?;
        }
        if let Some(ai) = &patch.ai {
            s.ai = clean_ai(ai)?;
        }
        if let Some(task) = &patch.task {
            s.task = task.clone();
        }
        if let Some(ids) = &patch.assignee_ids {
            let ids = clean_assignees(ids)?;
            let people = self.people()?;
            if let Some(id) = ids.iter().find(|id| !people.iter().any(|p| p.id == **id)) {
                return Err(Error::NotFound(format!("担当者 {id}")));
            }
            // 役を渡していなければ、残った人の役はそのまま、足した人は R
            if patch.task.is_none() {
                s.task.roles = ids
                    .iter()
                    .map(|id| before.roles_of(id).unwrap_or_default())
                    .collect();
            }
            s.assignee_ids = ids;
        }
        s.task = clean_template(&s.task, s.assignee_ids.len())?;
        if let (Some(ids), Some(own)) = (&mut s.task.linked_workspace_ids, &s.workspace_id) {
            ids.retain(|id| id != own);
        }
        if patch.task.is_some() {
            self.check_template(&s)?;
        }
        let days = patch.due_days.unwrap_or(s.due_days);
        let time = match &patch.due_time {
            Some(time) => time.clone(),
            None => s.due_time.clone(),
        };
        (s.due_days, s.due_time) = clean_due(days, time.as_deref())?;
        if let Some(enabled) = patch.enabled {
            s.enabled = enabled;
        }
        if s.rule != before.rule || (s.enabled && !before.enabled) {
            s.next_at = next_at(&s.rule, now)?;
        }
        if s == before {
            return Ok(s);
        }
        s.updated_at = now_ms();
        save(&self.conn, &s)?;
        Ok(s)
    }

    /// 雛形が指す親タスクとワークスペースがあるかを確かめる。
    fn check_template(&self, s: &Schedule) -> Result<()> {
        if let Some(parent) = &s.task.parent_id {
            self.task(parent)?;
        }
        if let Some(ids) = &s.task.linked_workspace_ids {
            if s.workspace_id.is_none() && !ids.is_empty() {
                return Err(Error::Invalid(
                    "未所属の定時タスクは、ほかのワークスペースに表示できません".into(),
                ));
            }
            for id in ids {
                self.workspace(id)?;
            }
        }
        Ok(())
    }

    pub fn delete_schedule(&mut self, id: &str) -> Result<()> {
        let n = self
            .conn
            .execute("DELETE FROM schedules WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(Error::NotFound(format!("定時タスク {id}")));
        }
        Ok(())
    }

    /// 時刻が来た定時タスクの、タスクを作る。作ったタスクの ID を返す。
    /// 1 つの定時タスクにつき、作るのは 1 回分だけ（時刻が過ぎた回がいくつもあれば、いちばん新しいもの）。
    /// 作ったタスクは元に戻す相手にしない。
    pub fn sweep_schedules(&mut self, now: &str) -> Result<Vec<Id>> {
        let due: Vec<Schedule> = self
            .conn
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM schedules WHERE enabled = 1 AND next_at <= ?1 \
                 ORDER BY next_at, id"
            ))?
            .query_map(params![now], from_row)?
            .collect::<rusqlite::Result<_>>()?;
        if due.is_empty() {
            return Ok(Vec::new());
        }
        // 前の変更は、その前の段にする
        self.undo_mark()?;
        let mut made = Vec::new();
        let mut errors = Vec::new();
        for s in due {
            let title = s.title.clone();
            match self.run_schedule(s, now) {
                Ok(Some(id)) => made.push(id),
                Ok(None) => {}
                Err(e) => errors.push(format!("「{title}」: {e}")),
            }
        }
        self.undo_skip()?;
        if !errors.is_empty() {
            return Err(Error::Invalid(errors.join("、")));
        }
        Ok(made)
    }

    /// 1 つの定時タスクの、タスクを作る。同じ回をもう作っていれば、次の時刻だけを進める。
    fn run_schedule(&mut self, mut s: Schedule, now: &str) -> Result<Option<Id>> {
        let rule = &s.rule;
        let Some(mut day) = date(date_of(&s.next_at)) else {
            return Err(Error::Invalid(format!(
                "次の時刻が正しくありません: {}",
                s.next_at
            )));
        };
        // 時刻が過ぎた回のうち、いちばん新しいもの
        while let Some(next) = rule
            .next_after(day)
            .filter(|d| rule.when(*d).as_str() <= now)
        {
            day = next;
        }
        let next = rule.next_after(day);
        let at = ymd(day);
        let last: Option<String> = self
            .conn
            .prepare_cached("SELECT at FROM schedule_last WHERE schedule_id = ?1")?
            .query_row(params![s.id], |r| r.get(0))
            .optional()?;
        let made_already = last.as_deref().is_some_and(|last| last >= at.as_str());
        let made = if made_already {
            None
        } else {
            Some(self.scheduled_task(&s, &at)?)
        };
        let now_ms = now_ms();
        let tx = self.conn.transaction()?;
        if let Some(made) = &made {
            let task = &made.task;
            insert_task(&tx, task)?;
            record_created(&tx, task, &Actor::Schedule)?;
            for sub in &made.subtasks {
                insert_task(&tx, sub)?;
                record_created(&tx, sub, &Actor::Schedule)?;
            }
            if !made.alerts.is_standard() {
                alert::save(&tx, &made.alerts)?;
            }
            for a in &made.files {
                crate::attachment::insert(&tx, a)?;
            }
            tx.execute(
                "INSERT INTO schedule_last(schedule_id, at) VALUES (?1, ?2) \
                 ON CONFLICT(schedule_id) DO UPDATE SET at = excluded.at",
                params![s.id, at],
            )?;
            tx.execute(
                "DELETE FROM schedule_last WHERE schedule_id NOT IN \
                 (SELECT id FROM schedules) AND rowid NOT IN \
                 (SELECT rowid FROM schedule_last ORDER BY at DESC LIMIT ?1)",
                params![MAX_LAST_ROWS],
            )?;
            if !s.ai.is_empty() {
                tx.execute(
                    "INSERT OR REPLACE INTO repeat_ai(task_id, prompt, failed, created_at) \
                     VALUES (?1, ?2, 0, ?3)",
                    params![task.id, s.ai, now_ms],
                )?;
                crate::repeat::trim_ai_queue(&tx)?;
            }
        }
        match next {
            Some(next) => {
                s.next_at = s.rule.when(next);
                s.updated_at = now_ms;
                save(&tx, &s)?;
            }
            // 次の回がない（ずっと先）なら、止める
            None => {
                tx.execute(
                    "UPDATE schedules SET enabled = 0, updated_at = ?2 WHERE id = ?1",
                    params![s.id, now_ms],
                )?;
            }
        }
        tx.commit()?;
        Ok(made.map(|m| m.task.id))
    }

    /// 回の日 `at` に作るタスク。ワークスペースの既定値に、雛形の項目を重ねる。
    fn scheduled_task(&self, s: &Schedule, at: &str) -> Result<Made> {
        let workspace = s.workspace_id.as_deref();
        let t = &s.task;
        let mut draft = self.draft_for(workspace, &s.title)?;
        let people = self.people()?;
        // 担当は、登録している人だけ
        let assignees: Vec<Id> = s
            .assignee_ids
            .iter()
            .filter(|id| people.iter().any(|p| &p.id == *id))
            .cloned()
            .collect();
        if !assignees.is_empty() {
            draft.assignee_ids = assignees;
        }
        if let Some(description) = &t.description {
            draft.description = description.clone();
        }
        if let Some(priority) = t.priority {
            draft.priority = priority;
        }
        if let Some(categories) = &t.categories {
            draft.categories = categories.clone();
        }
        if let Some(due) = s.due_on(at) {
            draft.due = Some(due);
        }
        draft.status = t.status;
        draft.percent = t.percent;
        draft.start = s.start_on(at);
        draft.urls = t.urls.clone();
        // 親タスクを消したあとなら、親なしで作る
        draft.parent_id = match &t.parent_id {
            Some(parent) if self.task(parent).is_ok() => Some(parent.clone()),
            _ => None,
        };
        let draft = clean_draft(&draft)?;
        // 雛形の担当なら雛形の役、ワークスペースの既定値の担当なら R
        let roles: Vec<Roles> = draft
            .assignee_ids
            .iter()
            .map(|id| s.roles_of(id).unwrap_or_default())
            .collect();
        // ほかに表示するワークスペース。消したものと、入れる先そのものは外す
        let linked: Vec<Id> = match (workspace, &t.linked_workspace_ids) {
            (None, _) => Vec::new(),
            (Some(_), Some(ids)) => ids.clone(),
            (Some(w), None) => self.task_defaults(w)?.link_workspace_ids,
        };
        let workspaces = self.workspaces()?;
        let linked: Vec<Id> = linked
            .into_iter()
            .filter(|id| Some(id.as_str()) != workspace && workspaces.iter().any(|w| &w.id == id))
            .collect();
        let position: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM tasks WHERE workspace_id IS ?1",
            params![workspace],
            |r| r.get(0),
        )?;
        let now = now_ms();
        // タスクの項目を足したら、ここで止まる。雛形にも同じ項目を足すこと
        let TaskDraft {
            title,
            description,
            status,
            priority,
            start,
            due,
            percent,
            parent_id,
            assignee_ids,
            categories,
            urls,
        } = draft;
        let done = status == Status::Completed;
        let task = Task {
            id: new_id(),
            workspace_id: s.workspace_id.clone(),
            title,
            description,
            status,
            priority,
            start,
            due,
            percent,
            parent_id,
            assignee_roles: roles,
            assignee_done: vec![done.then_some(now); assignee_ids.len()],
            assignee_ids,
            categories,
            urls,
            linked_workspace_ids: linked,
            position,
            created_at: now,
            updated_at: now,
            completed_at: done.then_some(now),
            created_by: Actor::Schedule,
            updated_by: Actor::Schedule,
        };
        // サブタスクは、画面で足したときと同じく、タイトルだけで作る
        let subtasks: Vec<Task> = t
            .subtasks
            .iter()
            .enumerate()
            .map(|(i, title)| Task {
                id: new_id(),
                workspace_id: s.workspace_id.clone(),
                title: title.clone(),
                description: String::new(),
                status: Status::NeedsAction,
                priority: 0,
                start: None,
                due: None,
                percent: 0,
                parent_id: Some(task.id.clone()),
                assignee_ids: Vec::new(),
                assignee_roles: Vec::new(),
                assignee_done: Vec::new(),
                categories: Vec::new(),
                urls: Vec::new(),
                linked_workspace_ids: Vec::new(),
                position: position + 1 + i as i64,
                created_at: now,
                updated_at: now,
                completed_at: None,
                created_by: Actor::Schedule,
                updated_by: Actor::Schedule,
            })
            .collect();
        let alerts = alert::TaskAlerts {
            task_id: task.id.clone(),
            timed: t.alerts_timed.clone(),
            date: t.alerts_date.clone(),
        };
        // 添付は中身を写さず、同じ中身を指す
        let files = self
            .schedule_files(&s.id)?
            .into_iter()
            .map(|f| Attachment {
                id: new_id(),
                task_id: task.id.clone(),
                author_id: None,
                name: f.name,
                size: f.size,
                hash: f.hash,
                created_at: now,
            })
            .collect();
        Ok(Made {
            task,
            subtasks,
            alerts,
            files,
        })
    }

    /// 雛形の添付。足した順。
    pub fn schedule_files(&self, schedule_id: &str) -> Result<Vec<ScheduleFile>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {FILE_COLUMNS} FROM schedule_files WHERE schedule_id = ?1 \
             ORDER BY created_at, id"
        ))?;
        let rows = stmt.query_map(params![schedule_id], file_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// すべての雛形の添付（書き出し用）。
    pub(crate) fn all_schedule_files(&self) -> Result<Vec<ScheduleFile>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {FILE_COLUMNS} FROM schedule_files ORDER BY id"
        ))?;
        let rows = stmt.query_map([], file_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 雛形にファイルを添付する。大きさの上限は、タスクの添付と同じ。
    pub fn add_schedule_file(
        &mut self,
        schedule_id: &str,
        name: &str,
        bytes: &[u8],
    ) -> Result<ScheduleFile> {
        if bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
            return Err(Error::Invalid(format!(
                "添付できるのは 1 ファイル {} MiB までです",
                MAX_ATTACHMENT_BYTES / 1024 / 1024
            )));
        }
        self.schedule(schedule_id)?;
        let count: usize = self.conn.query_row(
            "SELECT COUNT(*) FROM schedule_files WHERE schedule_id = ?1",
            params![schedule_id],
            |r| r.get(0),
        )?;
        if count >= MAX_FILES {
            return Err(Error::Invalid(format!(
                "定時タスクに添付できるのは {MAX_FILES} 件までです"
            )));
        }
        let file = ScheduleFile {
            id: new_id(),
            schedule_id: schedule_id.to_owned(),
            name: crate::attachment::clean_name(name),
            size: bytes.len() as u64,
            hash: crate::attachment::hash_of(bytes),
            created_at: now_ms(),
        };
        let tx = self.conn.transaction()?;
        crate::attachment::put_blob(&tx, &file.hash, bytes)?;
        insert_file(&tx, &file)?;
        tx.commit()?;
        Ok(file)
    }

    /// 雛形の添付を外す。ほかに同じ中身を指すものがなければ、中身も消える。
    pub fn delete_schedule_file(&mut self, id: &str) -> Result<()> {
        let n = self
            .conn
            .execute("DELETE FROM schedule_files WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(Error::NotFound(format!("定時タスクの添付 {id}")));
        }
        Ok(())
    }
}

/// 1 回分で作るもの。
struct Made {
    task: Task,
    subtasks: Vec<Task>,
    alerts: alert::TaskAlerts,
    files: Vec<Attachment>,
}

const FILE_COLUMNS: &str = "id, schedule_id, name, size, hash, created_at";

fn file_from_row(r: &Row) -> rusqlite::Result<ScheduleFile> {
    Ok(ScheduleFile {
        id: r.get(0)?,
        schedule_id: r.get(1)?,
        name: r.get(2)?,
        size: r.get::<_, i64>(3)? as u64,
        hash: r.get(4)?,
        created_at: r.get(5)?,
    })
}

pub(crate) fn insert_file(conn: &Connection, f: &ScheduleFile) -> rusqlite::Result<usize> {
    conn.prepare_cached(
        "INSERT INTO schedule_files(id, schedule_id, name, size, hash, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(id) DO UPDATE SET name = excluded.name",
    )?
    .execute(params![
        f.id,
        f.schedule_id,
        f.name,
        f.size as i64,
        f.hash,
        f.created_at
    ])
}

#[cfg(test)]
mod tests {
    use super::{due_days_label, Schedule, SchedulePatch, MAX_SCHEDULES, NEW_TITLE};
    use crate::actor::Actor;
    use crate::repeat::{Rule, Unit};
    use crate::{DueRule, Store, TaskDefaults};

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn weekly_monday(time: &str) -> Rule {
        // 2026-11-30 は月曜
        Rule {
            time: Some(time.into()),
            ..Rule::weekly("2026-11-30")
        }
    }

    #[test]
    fn a_new_schedule_starts_weekly_at_nine_and_waits() {
        let mut s = store();
        // 2026-11-27 は金曜
        let made = s
            .create_schedule(None, "", None, "2026-11-27 10:00")
            .unwrap();
        assert_eq!(made.title, NEW_TITLE);
        assert_eq!(made.rule.label(), "毎週 金 9:00");
        assert!(made.enabled);
        // 今日の 9:00 はもう過ぎているので、来週
        assert_eq!(made.next_at, "2026-12-04 09:00");
        assert!(s.sweep_schedules("2026-11-27 10:00").unwrap().is_empty());
        assert_eq!(s.schedules(None).unwrap(), vec![made]);
    }

    #[test]
    fn each_time_makes_a_new_task_from_the_template() {
        let mut s = store();
        let ws = s.create_workspace(None, "定例").unwrap();
        let me = s.create_person("自分", true).unwrap();
        let other = s.create_person("佐藤", false).unwrap();
        s.set_task_defaults(&TaskDefaults {
            workspace_id: ws.id.clone(),
            assignee_ids: vec![me.id.clone()],
            categories: vec!["報告".into()],
            priority: Some(3),
            ..Default::default()
        })
        .unwrap();
        let rule = weekly_monday("09:00");
        let made = s
            .create_schedule(
                Some(&ws.id),
                "週次定例の報告",
                Some(&rule),
                "2026-11-27 12:00",
            )
            .unwrap();
        assert_eq!(made.next_at, "2026-11-30 09:00");
        let made = s
            .update_schedule(
                &made.id,
                &SchedulePatch {
                    ai: Some("先週の完了したタスクをまとめる".into()),
                    assignee_ids: Some(vec![other.id.clone()]),
                    due_days: Some(Some(1)),
                    due_time: Some(Some("17:00".into())),
                    ..Default::default()
                },
                "2026-11-27 12:00",
            )
            .unwrap();
        assert_eq!(made.due_label().as_deref(), Some("翌日 17:00"));
        assert!(s.sweep_schedules("2026-11-30 08:59").unwrap().is_empty());

        let first = s.sweep_schedules("2026-11-30 09:00").unwrap();
        assert_eq!(first.len(), 1);
        let task = s.task(&first[0]).unwrap();
        assert_eq!(task.title, "週次定例の報告");
        assert_eq!(task.workspace_id.as_deref(), Some(ws.id.as_str()));
        assert_eq!(task.assignee_ids, vec![other.id.clone()]);
        assert_eq!(task.categories, vec!["報告".to_owned()]);
        assert_eq!(task.priority, 3);
        assert_eq!(task.due.as_deref(), Some("2026-12-01 17:00"));
        assert_eq!(task.created_by, Actor::Schedule);
        let jobs = s.repeat_ai_jobs().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].task_id, task.id);
        assert_eq!(jobs[0].prompt, "先週の完了したタスクをまとめる");
        assert_eq!(s.schedule(&made.id).unwrap().next_at, "2026-12-07 09:00");

        // 次の週は、前のタスクを変えずに、もう 1 件作る
        let second = s.sweep_schedules("2026-12-07 09:00").unwrap();
        assert_eq!(second.len(), 1);
        assert_ne!(second[0], first[0]);
        assert_eq!(
            s.task(&second[0]).unwrap().due.as_deref(),
            Some("2026-12-08 17:00")
        );
        assert_eq!(s.task(&first[0]).unwrap(), task);
    }

    #[test]
    fn missed_times_make_only_the_latest_and_undo_does_not_repeat_it() {
        let mut s = store();
        let rule = Rule::daily("2026-11-27");
        let made = s
            .create_schedule(None, "日報", Some(&rule), "2026-11-27 12:00")
            .unwrap();
        assert_eq!(made.next_at, "2026-11-28 00:00");
        s.undo_mark().unwrap();
        // 3 日閉じていた
        let tasks = s.sweep_schedules("2026-12-01 08:00").unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(s.task(&tasks[0]).unwrap().title, "日報");
        assert_eq!(s.schedule(&made.id).unwrap().next_at, "2026-12-02 00:00");

        // 前の操作（定時タスクの追加より後の変更）を元に戻しても、作ったタスクは残り、同じ回は作らない
        s.update_schedule(
            &made.id,
            &SchedulePatch {
                title: Some("日報を書く".into()),
                ..Default::default()
            },
            "2026-12-01 08:00",
        )
        .unwrap();
        s.undo_mark().unwrap();
        assert!(s.undo().unwrap());
        assert_eq!(s.schedule(&made.id).unwrap().title, "日報");
        assert!(s.task(&tasks[0]).is_ok());
        assert!(s.sweep_schedules("2026-12-01 08:00").unwrap().is_empty());
    }

    #[test]
    fn a_stopped_schedule_makes_nothing_and_restarts_from_now() {
        let mut s = store();
        let rule = Rule::daily("2026-11-27");
        let made = s
            .create_schedule(None, "日報", Some(&rule), "2026-11-27 12:00")
            .unwrap();
        let off = SchedulePatch {
            enabled: Some(false),
            ..Default::default()
        };
        s.update_schedule(&made.id, &off, "2026-11-27 12:00")
            .unwrap();
        assert!(s.sweep_schedules("2026-12-05 00:00").unwrap().is_empty());
        let on = SchedulePatch {
            enabled: Some(true),
            ..Default::default()
        };
        let back = s
            .update_schedule(&made.id, &on, "2026-12-05 00:30")
            .unwrap();
        assert_eq!(back.next_at, "2026-12-06 00:00");
        assert!(s.sweep_schedules("2026-12-05 00:30").unwrap().is_empty());
    }

    #[test]
    fn global_schedules_make_tasks_outside_workspaces() {
        let mut s = store();
        let rule = Rule {
            time: Some("09:00".into()),
            ..Rule::of(Unit::Month, "2026-12-01")
        };
        let made = s
            .create_schedule(None, "月次の請求", Some(&rule), "2026-11-27 12:00")
            .unwrap();
        let tasks = s.sweep_schedules(&made.next_at).unwrap();
        let task = s.task(&tasks[0]).unwrap();
        assert_eq!(task.workspace_id, None);
        assert_eq!(task.due, None);
    }

    #[test]
    fn workspace_due_defaults_apply_unless_the_template_sets_one() {
        let mut s = store();
        let ws = s.create_workspace(None, "イベント").unwrap();
        s.set_workspace_date(&ws.id, Some("2026-12-20")).unwrap();
        s.set_task_defaults(&TaskDefaults {
            workspace_id: ws.id.clone(),
            due: DueRule::Relative { days: -1 },
            ..Default::default()
        })
        .unwrap();
        let rule = Rule::daily("2026-11-27");
        let made = s
            .create_schedule(Some(&ws.id), "準備", Some(&rule), "2026-11-27 12:00")
            .unwrap();
        let tasks = s.sweep_schedules("2026-11-28 00:00").unwrap();
        assert_eq!(
            s.task(&tasks[0]).unwrap().due.as_deref(),
            Some("2026-12-19")
        );
        s.update_schedule(
            &made.id,
            &SchedulePatch {
                due_days: Some(Some(0)),
                ..Default::default()
            },
            "2026-11-28 00:00",
        )
        .unwrap();
        let tasks = s.sweep_schedules("2026-11-29 00:00").unwrap();
        assert_eq!(
            s.task(&tasks[0]).unwrap().due.as_deref(),
            Some("2026-11-29")
        );
    }

    #[test]
    fn limits_and_bad_input_are_refused() {
        let mut s = store();
        let now = "2026-11-27 12:00";
        for _ in 0..MAX_SCHEDULES {
            s.create_schedule(None, "", None, now).unwrap();
        }
        assert!(s.create_schedule(None, "", None, now).is_err());
        let id = s.schedules(None).unwrap()[0].id.clone();
        let bad =
            |patch: SchedulePatch, s: &mut Store| s.update_schedule(&id, &patch, now).is_err();
        assert!(bad(
            SchedulePatch {
                title: Some("  ".into()),
                ..Default::default()
            },
            &mut s
        ));
        assert!(bad(
            SchedulePatch {
                due_days: Some(Some(-1)),
                ..Default::default()
            },
            &mut s
        ));
        assert!(bad(
            SchedulePatch {
                due_days: Some(Some(0)),
                due_time: Some(Some("25:00".into())),
                ..Default::default()
            },
            &mut s
        ));
        assert!(bad(
            SchedulePatch {
                assignee_ids: Some(vec!["nobody".into()]),
                ..Default::default()
            },
            &mut s
        ));
        assert!(bad(
            SchedulePatch {
                ai: Some("あ".repeat(4001)),
                ..Default::default()
            },
            &mut s
        ));
    }

    #[test]
    fn deleting_the_workspace_deletes_its_schedules() {
        let mut s = store();
        let ws = s.create_workspace(None, "消す").unwrap();
        s.create_schedule(Some(&ws.id), "", None, "2026-11-27 12:00")
            .unwrap();
        s.delete_workspace(&ws.id).unwrap();
        assert!(s.all_schedules().unwrap().is_empty());
    }

    #[test]
    fn labels() {
        assert_eq!(due_days_label(0), "当日");
        assert_eq!(due_days_label(1), "翌日");
        assert_eq!(due_days_label(3), "3 日後");
        assert_eq!(due_days_label(7), "1 週間後");
        let mut s: Schedule = serde_json::from_value(serde_json::json!({
            "id": "x", "workspace_id": null, "title": "t",
            "rule": {"unit": "day", "from": "2026-11-27"},
            "enabled": true, "next_at": "2026-11-28 00:00",
            "position": 0, "created_at": 0, "updated_at": 0
        }))
        .unwrap();
        assert_eq!(s.due_label(), None);
        s.due_days = Some(0);
        s.due_time = Some("09:30".into());
        assert_eq!(s.due_label().as_deref(), Some("当日 9:30"));
        assert_eq!(s.due_on("2026-11-27").as_deref(), Some("2026-11-27 09:30"));
    }

    #[test]
    fn 取り込む定時タスクの上限と整え方() {
        use super::{clean, MAX_ASSIGNEES, MAX_TITLE_CHARS};
        let mut st = store();
        let made = st
            .create_schedule(None, "定例", None, "2026-11-27 10:00")
            .unwrap();
        let with = |f: &dyn Fn(&mut Schedule)| {
            let mut s = made.clone();
            f(&mut s);
            s
        };
        assert_eq!(clean(&made).unwrap(), made);
        assert!(clean(&with(&|s| s.id.clear())).is_err());
        assert!(clean(&with(&|s| s.next_at = "あした".into())).is_err());
        assert!(clean(&with(&|s| s.title = "あ".repeat(MAX_TITLE_CHARS))).is_ok());
        assert!(clean(&with(&|s| s.title = "あ".repeat(MAX_TITLE_CHARS + 1))).is_err());
        let ids = |n: usize| (0..n).map(|i| format!("p{i}")).collect::<Vec<_>>();
        assert!(clean(&with(&|s| s.assignee_ids = ids(MAX_ASSIGNEES))).is_ok());
        assert!(clean(&with(&|s| s.assignee_ids = ids(MAX_ASSIGNEES + 1))).is_err());
        let dirty = with(&|s| {
            s.title = "  新しい題 ".into();
            s.ai = " 指示 \n".into();
            s.assignee_ids = vec!["a".into(), "".into(), "a".into(), "b".into()];
            s.due_days = Some(2);
            s.due_time = Some(" 17:00 ".into());
            s.rule = Rule {
                weekdays: vec![2, 0, 2],
                ..Rule::weekly("2026-11-30")
            };
        });
        let cleaned = clean(&dirty).unwrap();
        assert_eq!(cleaned.title, "新しい題");
        assert_eq!(cleaned.ai, "指示");
        assert_eq!(cleaned.assignee_ids, ["a", "b"]);
        assert_eq!(cleaned.due_days, Some(2));
        assert_eq!(cleaned.due_time.as_deref(), Some("17:00"));
        assert_eq!(cleaned.rule.weekdays, [0, 2]);
    }

    #[test]
    fn 新しい定時タスクは同じ場所の末尾に並ぶ() {
        let mut s = store();
        let positions: Vec<i64> = (0..3)
            .map(|_| {
                s.create_schedule(None, "", None, "2026-11-27 10:00")
                    .unwrap()
                    .position
            })
            .collect();
        assert_eq!(positions, [0, 1, 2]);
    }

    #[test]
    fn 決まりを変えたときだけ次の時刻を決め直す() {
        let mut s = store();
        let made = s
            .create_schedule(
                None,
                "定例",
                Some(&weekly_monday("09:00")),
                "2026-11-27 12:00",
            )
            .unwrap();
        assert_eq!(made.next_at, "2026-11-30 09:00");
        // 題だけ変えるなら、あとの時刻で更新しても次の時刻は動かない
        let renamed = s
            .update_schedule(
                &made.id,
                &SchedulePatch {
                    title: Some("改名".into()),
                    ..Default::default()
                },
                "2026-12-20 12:00",
            )
            .unwrap();
        assert_eq!(renamed.title, "改名");
        assert_eq!(renamed.next_at, "2026-11-30 09:00");
        // 決まりを変えたら、更新した時刻から決め直す
        let moved = s
            .update_schedule(
                &made.id,
                &SchedulePatch {
                    rule: Some(weekly_monday("10:00")),
                    ..Default::default()
                },
                "2026-12-20 12:00",
            )
            .unwrap();
        assert_eq!(moved.next_at, "2026-12-21 10:00");
        // 止めている間は動かさず、有効にしたときに決め直す
        let off = s
            .update_schedule(
                &made.id,
                &SchedulePatch {
                    enabled: Some(false),
                    ..Default::default()
                },
                "2026-12-22 12:00",
            )
            .unwrap();
        assert_eq!(off.next_at, "2026-12-21 10:00");
        let on = s
            .update_schedule(
                &made.id,
                &SchedulePatch {
                    enabled: Some(true),
                    ..Default::default()
                },
                "2026-12-22 12:00",
            )
            .unwrap();
        assert_eq!(on.next_at, "2026-12-28 10:00");
        // すでに有効で、決まりも同じなら、そのまま
        let same = s
            .update_schedule(
                &made.id,
                &SchedulePatch {
                    enabled: Some(true),
                    ..Default::default()
                },
                "2027-01-20 12:00",
            )
            .unwrap();
        assert_eq!(same.next_at, "2026-12-28 10:00");
    }

    #[test]
    fn 担当にできるのは登録している人だけ() {
        let mut s = store();
        let me = s.create_person("自分", true).unwrap();
        let made = s
            .create_schedule(None, "定例", None, "2026-11-27 12:00")
            .unwrap();
        let patch = |ids: Vec<String>| SchedulePatch {
            assignee_ids: Some(ids),
            ..Default::default()
        };
        assert!(s
            .update_schedule(
                &made.id,
                &patch(vec!["いない人".into()]),
                "2026-11-27 12:00"
            )
            .is_err());
        let ok = s
            .update_schedule(&made.id, &patch(vec![me.id.clone()]), "2026-11-27 12:00")
            .unwrap();
        assert_eq!(ok.assignee_ids, [me.id.clone()]);
        // 作るタスクの担当は、登録している人
        let rule = weekly_monday("09:00");
        s.update_schedule(
            &made.id,
            &SchedulePatch {
                rule: Some(rule),
                ..Default::default()
            },
            "2026-11-27 12:00",
        )
        .unwrap();
        let tasks = s.sweep_schedules("2026-11-30 09:00").unwrap();
        assert_eq!(s.task(&tasks[0]).unwrap().assignee_ids, [me.id]);
    }

    #[test]
    fn 定時タスクを消すと一覧から外れ二度は消せない() {
        let mut s = store();
        let made = s
            .create_schedule(None, "定例", None, "2026-11-27 12:00")
            .unwrap();
        s.delete_schedule(&made.id).unwrap();
        assert!(s.schedules(None).unwrap().is_empty());
        assert!(s.schedule(&made.id).is_err());
        assert!(matches!(
            s.delete_schedule(&made.id),
            Err(crate::Error::NotFound(_))
        ));
    }

    fn template_store() -> (Store, crate::Workspace, crate::Workspace, Schedule) {
        let mut s = store();
        let ws = s.create_workspace(None, "経理").unwrap();
        let other = s.create_workspace(None, "共有").unwrap();
        let made = s
            .create_schedule(
                Some(&ws.id),
                "請求書の作成",
                Some(&weekly_monday("09:00")),
                "2026-11-27 12:00",
            )
            .unwrap();
        (s, ws, other, made)
    }

    #[test]
    fn 雛形のすべての項目が作るタスクに入る() {
        use super::Template;
        use crate::{Role, Roles, Status, TaskDraft};
        let (mut s, ws, other, made) = template_store();
        let me = s.create_person("自分", true).unwrap();
        let boss = s.create_person("上司", false).unwrap();
        let parent = s
            .create_task(
                Some(&ws.id),
                &TaskDraft {
                    title: "経理".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let task = Template {
            description: Some("先月分をまとめる".into()),
            status: Status::InProcess,
            priority: Some(1),
            start_days: Some(0),
            start_time: Some("10:00".into()),
            percent: 20,
            parent_id: Some(parent.id.clone()),
            roles: vec![Roles::R, Roles::of(&[Role::A, Role::C])],
            categories: Some(vec!["請求".into(), "月次".into()]),
            urls: vec!["epilysi.stargate65535.workers.dev/".into()],
            linked_workspace_ids: Some(vec![other.id.clone()]),
            alerts_timed: None,
            alerts_date: Some(vec![1, 0]),
            subtasks: vec!["金額を確かめる".into(), "送る".into()],
        };
        let made = s
            .update_schedule(
                &made.id,
                &SchedulePatch {
                    assignee_ids: Some(vec![me.id.clone(), boss.id.clone()]),
                    due_days: Some(Some(3)),
                    task: Some(task),
                    ..Default::default()
                },
                "2026-11-27 12:00",
            )
            .unwrap();
        assert_eq!(
            made.task.urls,
            ["https://epilysi.stargate65535.workers.dev/"]
        );
        assert_eq!(made.task.alerts_date, Some(vec![0, 1]));
        assert_eq!(made.start_label().as_deref(), Some("当日 10:00"));
        s.add_schedule_file(&made.id, "見積.txt", b"abc").unwrap();

        let tasks = s.sweep_schedules("2026-11-30 09:00").unwrap();
        assert_eq!(tasks.len(), 1);
        let t = s.task(&tasks[0]).unwrap();
        assert_eq!(t.title, "請求書の作成");
        assert_eq!(t.description, "先月分をまとめる");
        assert_eq!(t.status, Status::InProcess);
        assert_eq!(t.priority, 1);
        assert_eq!(t.start.as_deref(), Some("2026-11-30 10:00"));
        assert_eq!(t.due.as_deref(), Some("2026-12-03"));
        assert_eq!(t.percent, 20);
        assert_eq!(t.parent_id.as_deref(), Some(parent.id.as_str()));
        assert_eq!(t.roles_of(&me.id), Some(Roles::R));
        assert_eq!(t.roles_of(&boss.id), Some(Roles::of(&[Role::A, Role::C])));
        assert_eq!(t.categories, ["月次", "請求"]);
        assert_eq!(t.urls, ["https://epilysi.stargate65535.workers.dev/"]);
        assert_eq!(t.linked_workspace_ids, [other.id.clone()]);
        assert_eq!(t.created_by, Actor::Schedule);
        let alerts = s.task_alerts(&t.id).unwrap();
        assert_eq!(alerts.timed, None);
        assert_eq!(alerts.date, Some(vec![0, 1]));
        let subs: Vec<String> = s
            .subtasks(&t.id)
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect();
        assert_eq!(subs, ["金額を確かめる", "送る"]);
        let files = s.attachments(&t.id).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "見積.txt");
        assert_eq!(s.attachment_data(&files[0].id).unwrap(), b"abc");
    }

    #[test]
    fn 決めていない項目はワークスペースの既定値になる() {
        use crate::TaskDefaults;
        let (mut s, ws, other, made) = template_store();
        s.set_task_defaults(&TaskDefaults {
            workspace_id: ws.id.clone(),
            categories: vec!["既定".into()],
            priority: Some(5),
            description: "既定の説明".into(),
            link_workspace_ids: vec![other.id.clone()],
            ..Default::default()
        })
        .unwrap();
        let tasks = s.sweep_schedules("2026-11-30 09:00").unwrap();
        let t = s.task(&tasks[0]).unwrap();
        assert_eq!(t.description, "既定の説明");
        assert_eq!(t.priority, 5);
        assert_eq!(t.categories, ["既定"]);
        assert_eq!(t.linked_workspace_ids, [other.id.clone()]);
        assert_eq!(t.start, None);
        assert!(s.task_alerts(&t.id).unwrap().is_standard());
        // 空にした項目は、既定値ではなく空
        s.update_schedule(
            &made.id,
            &SchedulePatch {
                task: Some(super::Template {
                    description: Some(String::new()),
                    priority: Some(0),
                    categories: Some(Vec::new()),
                    linked_workspace_ids: Some(Vec::new()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            "2026-11-30 09:00",
        )
        .unwrap();
        let tasks = s.sweep_schedules("2026-12-07 09:00").unwrap();
        let t = s.task(&tasks[0]).unwrap();
        assert_eq!(t.description, "");
        assert_eq!(t.priority, 0);
        assert!(t.categories.is_empty());
        assert!(t.linked_workspace_ids.is_empty());
    }

    #[test]
    fn 担当を変えても残った人の役はそのまま() {
        use crate::{Role, Roles};
        let (mut s, _, _, made) = template_store();
        let a = s.create_person("A", true).unwrap();
        let b = s.create_person("B", false).unwrap();
        let c = s.create_person("C", false).unwrap();
        let now = "2026-11-27 12:00";
        let with = s
            .update_schedule(
                &made.id,
                &SchedulePatch {
                    assignee_ids: Some(vec![a.id.clone(), b.id.clone()]),
                    task: Some(super::Template {
                        roles: vec![Roles::R, Roles::of(&[Role::A])],
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        assert_eq!(with.roles_of(&b.id), Some(Roles::of(&[Role::A])));
        let changed = s
            .update_schedule(
                &made.id,
                &SchedulePatch {
                    assignee_ids: Some(vec![b.id.clone(), c.id.clone()]),
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        assert_eq!(changed.roles_of(&b.id), Some(Roles::of(&[Role::A])));
        assert_eq!(changed.roles_of(&c.id), Some(Roles::R));
        assert_eq!(changed.roles_of(&a.id), None);
        // A は 1 人まで。あとの人の A は外れる
        let two_a = s
            .update_schedule(
                &made.id,
                &SchedulePatch {
                    task: Some(super::Template {
                        roles: vec![Roles::of(&[Role::A]), Roles::of(&[Role::A, Role::I])],
                        ..changed.task.clone()
                    }),
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        assert_eq!(two_a.roles_of(&b.id), Some(Roles::of(&[Role::A])));
        assert_eq!(two_a.roles_of(&c.id), Some(Roles::of(&[Role::I])));
    }

    #[test]
    fn 雛形の項目の正しくない値は断る() {
        use super::{Template, MAX_SUBTASKS};
        let (mut s, ws, _, made) = template_store();
        let unfiled = s
            .create_schedule(None, "未所属", None, "2026-11-27 12:00")
            .unwrap();
        let now = "2026-11-27 12:00";
        let bad = |id: &str, t: Template, s: &mut Store| {
            s.update_schedule(
                id,
                &SchedulePatch {
                    task: Some(t),
                    ..Default::default()
                },
                now,
            )
            .is_err()
        };
        let base = Template::default();
        assert!(bad(
            &made.id,
            Template {
                priority: Some(10),
                ..base.clone()
            },
            &mut s
        ));
        assert!(bad(
            &made.id,
            Template {
                percent: 101,
                ..base.clone()
            },
            &mut s
        ));
        assert!(bad(
            &made.id,
            Template {
                start_days: Some(-1),
                ..base.clone()
            },
            &mut s
        ));
        assert!(bad(
            &made.id,
            Template {
                start_days: Some(0),
                start_time: Some("9時".into()),
                ..base.clone()
            },
            &mut s
        ));
        assert!(bad(
            &made.id,
            Template {
                urls: vec!["リンク".into()],
                ..base.clone()
            },
            &mut s
        ));
        assert!(bad(
            &made.id,
            Template {
                subtasks: vec!["a".into(); MAX_SUBTASKS + 1],
                ..base.clone()
            },
            &mut s
        ));
        assert!(bad(
            &made.id,
            Template {
                parent_id: Some("ない".into()),
                ..base.clone()
            },
            &mut s
        ));
        assert!(bad(
            &made.id,
            Template {
                linked_workspace_ids: Some(vec!["ない".into()]),
                ..base.clone()
            },
            &mut s
        ));
        assert!(bad(
            &unfiled.id,
            Template {
                linked_workspace_ids: Some(vec![ws.id.clone()]),
                ..base.clone()
            },
            &mut s
        ));
        assert!(bad(
            &made.id,
            Template {
                alerts_date: Some(vec![99]),
                ..base.clone()
            },
            &mut s
        ));
        // 断ったときは、何も変わらない
        assert_eq!(s.schedule(&made.id).unwrap(), made);
        // 完了で作るなら、進捗は 100
        let done = s
            .update_schedule(
                &made.id,
                &SchedulePatch {
                    task: Some(Template {
                        status: crate::Status::Completed,
                        ..base
                    }),
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        assert_eq!(done.task.percent, 100);
    }

    #[test]
    fn 親タスクやワークスペースを消したあとも作れる() {
        use crate::TaskDraft;
        let (mut s, ws, other, made) = template_store();
        let parent = s
            .create_task(
                Some(&ws.id),
                &TaskDraft {
                    title: "親".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        s.update_schedule(
            &made.id,
            &SchedulePatch {
                task: Some(super::Template {
                    parent_id: Some(parent.id.clone()),
                    linked_workspace_ids: Some(vec![other.id.clone()]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            "2026-11-27 12:00",
        )
        .unwrap();
        s.delete_task(&parent.id).unwrap();
        s.delete_workspace(&other.id).unwrap();
        let tasks = s.sweep_schedules("2026-11-30 09:00").unwrap();
        let t = s.task(&tasks[0]).unwrap();
        assert_eq!(t.parent_id, None);
        assert!(t.linked_workspace_ids.is_empty());
    }

    #[test]
    fn 雛形の添付の中身はどこからも指されなくなったら消える() {
        let (mut s, _, _, made) = template_store();
        let file = s.add_schedule_file(&made.id, "a.txt", b"hello").unwrap();
        let tasks = s.sweep_schedules("2026-11-30 09:00").unwrap();
        let attached = s.attachments(&tasks[0]).unwrap();
        // タスクの添付を消しても、雛形が指しているので中身は残る
        s.delete_attachment(&attached[0].id).unwrap();
        assert_eq!(s.blob(&file.hash).unwrap(), b"hello");
        let tasks = s.sweep_schedules("2026-12-07 09:00").unwrap();
        let attached = s.attachments(&tasks[0]).unwrap();
        // 雛形から外しても、作ったタスクが指しているので残る
        s.undo_mark().unwrap();
        s.delete_schedule_file(&file.id).unwrap();
        s.undo_mark().unwrap();
        assert!(s.schedule_files(&made.id).unwrap().is_empty());
        assert_eq!(s.attachment_data(&attached[0].id).unwrap(), b"hello");
        // 元に戻すと、雛形の添付も戻る
        assert!(s.undo().unwrap());
        assert_eq!(s.schedule_files(&made.id).unwrap(), vec![file.clone()]);
        // どこからも指されなくなったら、中身も消える
        s.delete_attachment(&attached[0].id).unwrap();
        s.delete_schedule(&made.id).unwrap();
        assert!(s.blob(&file.hash).is_err());
        assert!(matches!(
            s.delete_schedule_file(&file.id),
            Err(crate::Error::NotFound(_))
        ));
    }

    #[test]
    fn 雛形の添付の上限() {
        use super::MAX_FILES;
        let (mut s, _, _, made) = template_store();
        for i in 0..MAX_FILES {
            s.add_schedule_file(&made.id, &format!("{i}.txt"), &[i as u8])
                .unwrap();
        }
        assert!(s.add_schedule_file(&made.id, "over.txt", b"x").is_err());
        let big = vec![0u8; crate::MAX_ATTACHMENT_BYTES as usize + 1];
        let (mut s, _, _, made) = template_store();
        assert!(s.add_schedule_file(&made.id, "big.bin", &big).is_err());
        assert!(s.add_schedule_file("ない", "a.txt", b"x").is_err());
    }

    #[test]
    fn 古い形の定時タスクは雛形の項目がない形で読める() {
        let old: Schedule = serde_json::from_value(serde_json::json!({
            "id": "x", "workspace_id": null, "title": "t",
            "rule": {"unit": "day", "from": "2026-11-27"},
            "enabled": true, "next_at": "2026-11-28 00:00",
            "position": 0, "created_at": 0, "updated_at": 0
        }))
        .unwrap();
        assert_eq!(old.task, super::Template::default());
        assert_eq!(old.task.status, crate::Status::NeedsAction);
        let partial: super::Template =
            serde_json::from_value(serde_json::json!({"urls": ["https://a.example/"]})).unwrap();
        assert_eq!(partial.status, crate::Status::NeedsAction);
        assert_eq!(partial.urls, ["https://a.example/"]);
    }

    #[test]
    fn 雛形の項目と添付は書き出して読み込むと戻る() {
        use crate::{Role, Roles, Status};
        let (mut s, _, other, made) = template_store();
        let me = s.create_person("自分", true).unwrap();
        let made = s
            .update_schedule(
                &made.id,
                &SchedulePatch {
                    assignee_ids: Some(vec![me.id.clone()]),
                    task: Some(super::Template {
                        description: Some("説明".into()),
                        status: Status::InProcess,
                        priority: Some(2),
                        start_days: Some(1),
                        start_time: Some("08:30".into()),
                        percent: 10,
                        roles: vec![Roles::of(&[Role::A])],
                        categories: Some(vec!["月次".into()]),
                        urls: vec!["https://a.example/".into()],
                        linked_workspace_ids: Some(vec![other.id.clone()]),
                        alerts_timed: Some(vec![30]),
                        alerts_date: Some(vec![]),
                        subtasks: vec!["下書き".into()],
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                "2026-11-27 12:00",
            )
            .unwrap();
        let file = s.add_schedule_file(&made.id, "雛形.txt", b"xyz").unwrap();
        let mut zip = std::io::Cursor::new(Vec::new());
        crate::archive::export(&s, &mut zip).unwrap();
        let mut other = store();
        zip.set_position(0);
        crate::archive::import(&mut other, zip).unwrap();
        assert_eq!(other.schedule(&made.id).unwrap(), made);
        assert_eq!(other.schedule_files(&made.id).unwrap(), vec![file.clone()]);
        assert_eq!(other.blob(&file.hash).unwrap(), b"xyz");
        // 読み込んだ先でも、同じタスクを作る
        let tasks = other.sweep_schedules("2026-11-30 09:00").unwrap();
        let t = other.task(&tasks[0]).unwrap();
        assert_eq!(t.urls, ["https://a.example/"]);
        assert_eq!(t.start.as_deref(), Some("2026-12-01 08:30"));
        assert_eq!(other.attachments(&t.id).unwrap()[0].name, "雛形.txt");
    }
}
