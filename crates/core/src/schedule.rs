//! 定時タスク。決まった時刻が来るたびに、雛形から新しいタスクを 1 件作る。
//!
//! 繰り返し（`repeat`）が 1 つのタスクを写して次の回を作るのに対して、定時タスクはワークスペース
//! （またはどのワークスペースにも入れない場所）の設定で、雛形（タイトル・担当・期限・AI への指示）
//! を持つ。作ったタスクには、ワークスペースの既定値も入る。AI への指示があれば、作ったタスクを
//! AI の待ち行列（繰り返しと同じ `repeat_ai`）に入れる。
//!
//! アプリが動いていなかった間の回は、いちばん新しい 1 回だけを作る（溜めない）。
//! 設定（`schedules`）は元に戻せる。最後に作った回（`schedule_last`）は元に戻す相手にしない。
//! 元に戻しても、同じ回をもう一度作らない。

use chrono::Duration;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::actor::Actor;
use crate::model::*;
use crate::repeat::{clean_ai, date, ymd, Rule};
use crate::store::{clean_draft, insert_task, record_created};
use crate::text::normalize_line;
use crate::{Error, Result, Store};

/// 1 つの場所（ワークスペース 1 つ、またはワークスペースの外）に置ける定時タスクの数。
pub const MAX_SCHEDULES: usize = 50;
/// タイトルの長さの上限。
pub const MAX_TITLE_CHARS: usize = 200;
/// 担当の数の上限。
pub const MAX_ASSIGNEES: usize = 20;
/// 期限を回の日から何日後にできるか。
pub const MAX_DUE_DAYS: i64 = 365;
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
}

impl Schedule {
    /// 回の日に作るタスクの期限。期限を決めていなければ `None`。
    pub fn due_on(&self, day: &str) -> Option<String> {
        let days = self.due_days?;
        let d = date(day)? + Duration::days(days);
        Some(match &self.due_time {
            Some(time) => format!("{} {time}", ymd(d)),
            None => ymd(d),
        })
    }

    /// 期限の短い表示（「当日」「翌日 17:00」「3 日後」）。決めていなければ `None`。
    pub fn due_label(&self) -> Option<String> {
        let mut text = due_days_label(self.due_days?);
        if let Some(time) = &self.due_time {
            text.push(' ');
            text.push_str(time.strip_prefix('0').unwrap_or(time));
        }
        Some(text)
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
}

const COLUMNS: &str = "id, workspace_id, title, rule, ai, assignees, due_days, due_time, \
                       enabled, next_at, position, created_at, updated_at";

fn json_error(ix: usize, e: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(ix, rusqlite::types::Type::Text, Box::new(e))
}

fn from_row(r: &Row) -> rusqlite::Result<Schedule> {
    let rule: String = r.get(3)?;
    let assignees: String = r.get(5)?;
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
    })
}

/// ID が同じものは上書きし、なければ足す。上書きは UPDATE にして、元に戻すで前の中身に戻るようにする。
pub(crate) fn save(conn: &Connection, s: &Schedule) -> Result<()> {
    conn.prepare_cached(&format!(
        "INSERT INTO schedules({COLUMNS}) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
         ON CONFLICT(id) DO UPDATE SET workspace_id = excluded.workspace_id, \
         title = excluded.title, rule = excluded.rule, ai = excluded.ai, \
         assignees = excluded.assignees, due_days = excluded.due_days, \
         due_time = excluded.due_time, enabled = excluded.enabled, next_at = excluded.next_at, \
         position = excluded.position, created_at = excluded.created_at, \
         updated_at = excluded.updated_at"
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
        s.updated_at
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
    let Some(days) = days else {
        return Ok((None, None));
    };
    if !(0..=MAX_DUE_DAYS).contains(&days) {
        return Err(Error::Invalid(format!(
            "定時タスクの期限は 0 から {MAX_DUE_DAYS} 日後です"
        )));
    }
    let time = match time.map(str::trim) {
        None | Some("") => None,
        Some(t) if is_valid_time(t) => Some(t.to_owned()),
        Some(t) => {
            return Err(Error::Invalid(format!(
                "定時タスクの期限の時刻が正しくありません: {t}"
            )))
        }
    };
    Ok((Some(days), time))
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
    Ok(Schedule {
        title: clean_title(&s.title)?,
        rule: s.rule.clean()?,
        ai: clean_ai(&s.ai)?,
        assignee_ids: clean_assignees(&s.assignee_ids)?,
        due_days,
        due_time,
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
        if let Some(ids) = &patch.assignee_ids {
            let ids = clean_assignees(ids)?;
            let people = self.people()?;
            if let Some(id) = ids.iter().find(|id| !people.iter().any(|p| p.id == **id)) {
                return Err(Error::NotFound(format!("担当者 {id}")));
            }
            s.assignee_ids = ids;
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
        let task = if made_already {
            None
        } else {
            Some(self.scheduled_task(&s, &at)?)
        };
        let now_ms = now_ms();
        let tx = self.conn.transaction()?;
        if let Some(task) = &task {
            insert_task(&tx, task)?;
            record_created(&tx, task, &Actor::Schedule)?;
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
        Ok(task.map(|t| t.id))
    }

    /// 回の日 `at` に作るタスク。ワークスペースの既定値に、雛形の担当と期限を重ねる。
    fn scheduled_task(&self, s: &Schedule, at: &str) -> Result<Task> {
        let workspace = s.workspace_id.as_deref();
        let mut draft = self.draft_for(workspace, &s.title)?;
        let people = self.people()?;
        let assignees: Vec<Id> = s
            .assignee_ids
            .iter()
            .filter(|id| people.iter().any(|p| &p.id == *id))
            .cloned()
            .collect();
        if !assignees.is_empty() {
            draft.assignee_ids = assignees;
        }
        if let Some(due) = s.due_on(at) {
            draft.due = Some(due);
        }
        let draft = clean_draft(&draft)?;
        let position: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM tasks WHERE workspace_id IS ?1",
            params![workspace],
            |r| r.get(0),
        )?;
        let now = now_ms();
        Ok(Task {
            id: new_id(),
            workspace_id: s.workspace_id.clone(),
            title: draft.title,
            description: draft.description,
            status: Status::NeedsAction,
            priority: draft.priority,
            start: draft.start,
            due: draft.due,
            percent: 0,
            parent_id: None,
            assignee_roles: vec![Roles::R; draft.assignee_ids.len()],
            assignee_done: vec![None; draft.assignee_ids.len()],
            assignee_ids: draft.assignee_ids,
            categories: draft.categories,
            urls: draft.urls,
            linked_workspace_ids: Vec::new(),
            position,
            created_at: now,
            updated_at: now,
            completed_at: None,
            created_by: Actor::Schedule,
            updated_by: Actor::Schedule,
        })
    }
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
}
