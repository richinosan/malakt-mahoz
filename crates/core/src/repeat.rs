//! 繰り返すタスク（定期実行）。
//!
//! 繰り返しは 1 つのタスク（いちばん新しい回）に付く。次の回の時刻が来ると、そのタスクを写した
//! 新しいタスクを作り、繰り返しを新しいほうへ移す。開始日と期限は、回の日付に合わせてずらす。
//! アプリが動いていなかった間の回は、いちばん新しい 1 回だけを作る（溜めない）。
//! AI への指示があれば、作った回を AI の待ち行列に入れる。アプリの AI が実行し、結果をタスクに
//! コメントする。
//!
//! 繰り返しの設定（`task_repeats`）は元に戻せる。最後に作った回（`repeat_last`）と AI の待ち行列
//! （`repeat_ai`）は元に戻す相手にしない。元に戻しても、同じ回をもう一度作らない。

use std::collections::HashSet;

use chrono::{Datelike, Duration, Months, NaiveDate};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::actor::Actor;
use crate::model::*;
use crate::store::{insert_change, insert_task, record_created, touch};
use crate::{Error, Result, Store};

/// 間隔の数の上限（99 日ごと、99 週間ごと…）。
pub const MAX_EVERY: u32 = 99;
/// 繰り返しを付けられるタスクの数。
pub const MAX_REPEATS: usize = 500;
/// AI への指示の長さ（文字）。
pub const MAX_AI_CHARS: usize = 4000;
/// 1 回に写すサブタスクの数。
pub const MAX_COPIED_SUBTASKS: usize = 100;
/// AI の待ち行列（失敗したものを含む）に残す数。超えたら古いものから消す。
pub const MAX_AI_QUEUE: usize = 50;
/// 次の回を探す日数。99 年ごとでも見つかる長さ。
const SEARCH_DAYS: i64 = 366 * 100;

/// 変更履歴の項目の名前。
pub const FIELD: &str = "repeat";
pub const AI_FIELD: &str = "repeat_ai";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Day,
    Week,
    Month,
    Year,
}

impl Unit {
    pub const ALL: [Unit; 4] = [Unit::Day, Unit::Week, Unit::Month, Unit::Year];

    pub fn as_str(self) -> &'static str {
        match self {
            Unit::Day => "day",
            Unit::Week => "week",
            Unit::Month => "month",
            Unit::Year => "year",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|u| u.as_str() == s)
    }

    /// 「2 日ごと」の「日」。
    pub fn label(self) -> &'static str {
        match self {
            Unit::Day => "日",
            Unit::Week => "週間",
            Unit::Month => "か月",
            Unit::Year => "年",
        }
    }

    /// 間隔のメニューに出す数。
    pub fn choices(self) -> &'static [u32] {
        match self {
            Unit::Day => &[1, 2, 3, 4, 5, 6, 7, 10, 14],
            Unit::Week => &[1, 2, 3, 4],
            Unit::Month => &[1, 2, 3, 4, 6],
            Unit::Year => &[1, 2],
        }
    }
}

/// 曜日の名前。0 が月曜。
pub const WEEKDAYS: [&str; 7] = ["月", "火", "水", "木", "金", "土", "日"];
/// 平日（月〜金）。
pub const WORKDAYS: [u8; 5] = [0, 1, 2, 3, 4];

/// いつ繰り返すか。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub unit: Unit,
    /// 何日・何週間・何か月・何年ごとか。
    #[serde(default = "one")]
    pub every: u32,
    /// 毎週のときの曜日（0 = 月 … 6 = 日）。小さい順。
    #[serde(default)]
    pub weekdays: Vec<u8>,
    /// 数え始めの日（`YYYY-MM-DD`）。毎月・毎年の日付と、2 週間ごとなどの数え始めになる。
    pub from: String,
    /// 次の回を作る時刻（`HH:MM`）。`None` は 0:00（日付が変わったとき）。
    #[serde(default)]
    pub time: Option<String>,
}

fn one() -> u32 {
    1
}

fn date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

fn ymd(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn weekday(d: NaiveDate) -> u8 {
    d.weekday().num_days_from_monday() as u8
}

/// 「9:00」。時の頭の 0 は付けない。
fn time_label(time: &str) -> String {
    time.strip_prefix('0')
        .filter(|rest| rest.len() == 4)
        .unwrap_or(time)
        .to_owned()
}

impl Rule {
    /// 起点の日から作る、よく使う形。
    pub fn daily(from: &str) -> Self {
        Self::of(Unit::Day, from)
    }

    pub fn workdays(from: &str) -> Self {
        Rule {
            weekdays: WORKDAYS.to_vec(),
            ..Self::of(Unit::Week, from)
        }
    }

    pub fn weekly(from: &str) -> Self {
        Self::of(Unit::Week, from)
    }

    pub fn monthly(from: &str) -> Self {
        Self::of(Unit::Month, from)
    }

    pub fn yearly(from: &str) -> Self {
        Self::of(Unit::Year, from)
    }

    /// 間隔 1、曜日は起点の曜日。
    pub fn of(unit: Unit, from: &str) -> Self {
        let weekdays = match (unit, date(from)) {
            (Unit::Week, Some(d)) => vec![weekday(d)],
            _ => Vec::new(),
        };
        Rule {
            unit,
            every: 1,
            weekdays,
            from: from.to_owned(),
            time: None,
        }
    }

    /// 単位を変える。間隔・曜日・時刻は、変えた先で使えるものだけ残す。
    pub fn with_unit(&self, unit: Unit) -> Self {
        let mut rule = Rule {
            time: self.time.clone(),
            ..Self::of(unit, &self.from)
        };
        if unit == self.unit {
            rule.every = self.every;
            rule.weekdays = self.weekdays.clone();
        }
        rule
    }

    /// 正しい形にそろえる。正しくなければエラー。
    pub fn clean(&self) -> Result<Rule> {
        let from = date(&self.from).ok_or_else(|| {
            Error::Invalid(format!("繰り返しの起点が正しくありません: {}", self.from))
        })?;
        if !(1..=MAX_EVERY).contains(&self.every) {
            return Err(Error::Invalid(format!(
                "繰り返しの間隔は 1 から {MAX_EVERY} です"
            )));
        }
        let time = match self.time.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(t) if is_valid_time(t) => Some(t.to_owned()),
            Some(t) => {
                return Err(Error::Invalid(format!(
                    "繰り返しの時刻が正しくありません: {t}"
                )))
            }
        };
        let weekdays = if self.unit == Unit::Week {
            if let Some(bad) = self.weekdays.iter().find(|d| **d > 6) {
                return Err(Error::Invalid(format!("曜日が正しくありません: {bad}")));
            }
            let mut days = self.weekdays.clone();
            days.sort_unstable();
            days.dedup();
            if days.is_empty() {
                days.push(weekday(from));
            }
            days
        } else {
            Vec::new()
        };
        Ok(Rule {
            unit: self.unit,
            every: self.every,
            weekdays,
            from: ymd(from),
            time,
        })
    }

    /// その日が回の日か。
    pub fn occurs(&self, d: NaiveDate) -> bool {
        let Some(from) = date(&self.from) else {
            return false;
        };
        if d < from {
            return false;
        }
        let every = i64::from(self.every.max(1));
        match self.unit {
            Unit::Day => (d - from).num_days() % every == 0,
            Unit::Week => {
                let monday = |x: NaiveDate| x - Duration::days(i64::from(weekday(x)));
                let weeks = (monday(d) - monday(from)).num_days() / 7;
                weeks % every == 0 && self.weekdays.contains(&weekday(d))
            }
            Unit::Month | Unit::Year => {
                let step = if self.unit == Unit::Year {
                    every * 12
                } else {
                    every
                };
                let months = i64::from(d.year() - from.year()) * 12 + i64::from(d.month())
                    - i64::from(from.month());
                if months % step != 0 {
                    return false;
                }
                // 起点が 31 日なら、短い月は月末
                from.checked_add_months(Months::new(months as u32)) == Some(d)
            }
        }
    }

    /// `d` より後の、最初の回の日。
    pub fn next_after(&self, d: NaiveDate) -> Option<NaiveDate> {
        let mut day = d;
        for _ in 0..SEARCH_DAYS {
            day = day.succ_opt()?;
            if self.occurs(day) {
                return Some(day);
            }
        }
        None
    }

    /// その日の回を作る時刻（`YYYY-MM-DD HH:MM`）。
    pub fn when(&self, d: NaiveDate) -> String {
        format!("{} {}", ymd(d), self.time.as_deref().unwrap_or("00:00"))
    }

    /// `at` より後で、作る時刻が `now` より後の、最初の回。
    pub fn upcoming(&self, at: NaiveDate, now: &str) -> Option<NaiveDate> {
        let mut d = self.next_after(at)?;
        // 今日の、もう過ぎた時刻の回は飛ばす（設定したとたんに回を作らない）
        while self.when(d).as_str() <= now {
            d = self.next_after(d)?;
        }
        Some(d)
    }

    /// 曜日の並び（「月・木」）。
    fn weekday_names(&self) -> String {
        self.weekdays
            .iter()
            .filter_map(|d| WEEKDAYS.get(*d as usize))
            .copied()
            .collect::<Vec<_>>()
            .join("・")
    }

    /// 画面に出す名前（「毎週 月」「2 週間ごと 月・木」「毎月 20 日 9:00」）。
    pub fn label(&self) -> String {
        let from = date(&self.from);
        let every = self.every;
        let mut text = match self.unit {
            Unit::Day if every == 1 => "毎日".to_owned(),
            Unit::Day => format!("{every} 日ごと"),
            Unit::Week if every == 1 && self.weekdays == WORKDAYS => "平日".to_owned(),
            Unit::Week if every == 1 => format!("毎週 {}", self.weekday_names()),
            Unit::Week => format!("{every} 週間ごと {}", self.weekday_names()),
            Unit::Month => {
                let day = from.map_or(0, |d| d.day());
                if every == 1 {
                    format!("毎月 {day} 日")
                } else {
                    format!("{every} か月ごと {day} 日")
                }
            }
            Unit::Year => {
                let (m, d) = from.map_or((0, 0), |d| (d.month(), d.day()));
                if every == 1 {
                    format!("毎年 {m}/{d}")
                } else {
                    format!("{every} 年ごと {m}/{d}")
                }
            }
        };
        if let Some(time) = &self.time {
            text.push(' ');
            text.push_str(&time_label(time));
        }
        text
    }
}

/// 次に作る時刻の短い表示（「12/8（月）9:00」）。
pub fn next_label(next_at: &str) -> String {
    let Some(d) = date(date_of(next_at)) else {
        return next_at.to_owned();
    };
    let mut text = format!(
        "{}/{}（{}）",
        d.month(),
        d.day(),
        WEEKDAYS[weekday(d) as usize]
    );
    if let Some(time) = time_of(next_at).filter(|t| *t != "00:00") {
        text.push_str(&time_label(time));
    }
    text
}

/// タスクに付けた繰り返し。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repeat {
    pub id: Id,
    /// 今の回（いちばん新しく作られたタスク）。次の回はこのタスクを写して作る。
    pub task_id: Id,
    pub rule: Rule,
    /// 回を作ったときに AI に頼むこと。空なら頼まない。
    #[serde(default)]
    pub ai: String,
    /// 今の回の日（`YYYY-MM-DD`）。次の回の開始日と期限は、ここからの日数でずらす。
    pub at: String,
    /// 次の回を作る時刻（`YYYY-MM-DD HH:MM`）。
    pub next_at: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// AI の待ち行列の 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiJob {
    pub task_id: Id,
    pub title: String,
    pub prompt: String,
    /// 前に失敗した（もう一度頼まれるまで実行しない）。
    pub failed: bool,
}

/// タスクの、回の日にする日。開始日、なければ期限、どちらもなければ今日。
pub fn anchor(task: &Task, today: &str) -> String {
    task.start
        .as_deref()
        .or(task.due.as_deref())
        .map(date_of)
        .unwrap_or(date_of(today))
        .to_owned()
}

/// 期限や開始を日数でずらす。時刻はそのまま。
fn shift(when: &str, days: i64) -> String {
    let Some(d) = date(date_of(when)) else {
        return when.to_owned();
    };
    let moved = ymd(d + Duration::days(days));
    match time_of(when) {
        Some(time) => format!("{moved} {time}"),
        None => moved,
    }
}

const COLUMNS: &str = "id, task_id, rule, ai, at, next_at, created_at, updated_at";

fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Repeat> {
    let rule: String = r.get(2)?;
    let rule = serde_json::from_str(&rule).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(Repeat {
        id: r.get(0)?,
        task_id: r.get(1)?,
        rule,
        ai: r.get(3)?,
        at: r.get(4)?,
        next_at: r.get(5)?,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
    })
}

pub(crate) fn save(conn: &Connection, r: &Repeat) -> rusqlite::Result<usize> {
    conn.prepare_cached(
        "INSERT INTO task_repeats(id, task_id, rule, ai, at, next_at, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
         ON CONFLICT(id) DO UPDATE SET task_id = excluded.task_id, rule = excluded.rule, \
         ai = excluded.ai, at = excluded.at, next_at = excluded.next_at, \
         updated_at = excluded.updated_at",
    )?
    .execute(params![
        r.id,
        r.task_id,
        serde_json::to_string(&r.rule).expect("文字列と数だけの構造体"),
        r.ai,
        r.at,
        r.next_at,
        r.created_at,
        r.updated_at
    ])
}

/// 取り込んだ繰り返しを確かめる。
pub fn clean_repeat(r: &Repeat) -> Result<Repeat> {
    let bad = || Error::Invalid(format!("タスク {} の繰り返しが正しくありません", r.task_id));
    if r.id.is_empty()
        || r.task_id.is_empty()
        || !is_valid_date(&r.at)
        || !is_valid_when(&r.next_at)
    {
        return Err(bad());
    }
    Ok(Repeat {
        rule: r.rule.clean()?,
        ai: clean_ai(&r.ai)?,
        ..r.clone()
    })
}

fn clean_ai(text: &str) -> Result<String> {
    let text = crate::text::normalize(text).trim().to_owned();
    if text.chars().count() > MAX_AI_CHARS {
        return Err(Error::Invalid(format!(
            "AI への指示は {MAX_AI_CHARS} 文字までです"
        )));
    }
    Ok(text)
}

impl Store {
    /// そのタスクに付いている繰り返し。
    pub fn repeat(&self, task_id: &str) -> Result<Option<Repeat>> {
        Ok(self
            .conn
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM task_repeats WHERE task_id = ?1"
            ))?
            .query_row(params![task_id], from_row)
            .optional()?)
    }

    /// 繰り返しが付いているタスク。一覧に印を出すのに使う。
    pub fn repeating_task_ids(&self) -> Result<HashSet<Id>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT task_id FROM task_repeats")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub(crate) fn all_repeats(&self) -> Result<Vec<Repeat>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM task_repeats ORDER BY task_id"
        ))?;
        let rows = stmt.query_map([], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 繰り返しを付ける・変える。`None` で外す。`now` は今の日時（`YYYY-MM-DD HH:MM`）。
    /// 付けたとたんに回は作らない。次の回は、作る時刻が `now` より後の最初の回。
    pub fn set_repeat(
        &mut self,
        task_id: &str,
        rule: Option<&Rule>,
        now: &str,
    ) -> Result<Option<Repeat>> {
        let task = self.task(task_id)?;
        let old = self.repeat(task_id)?;
        let at = old
            .as_ref()
            .map_or_else(|| anchor(&task, now), |r| r.at.clone());
        let new = match rule {
            None => None,
            Some(rule) => {
                let rule = rule.clean()?;
                let at_day = date(&at)
                    .ok_or_else(|| Error::Invalid(format!("日付が正しくありません: {at}")))?;
                let next = rule
                    .upcoming(at_day, now)
                    .ok_or_else(|| Error::Invalid("次の回が見つかりません".into()))?;
                Some((rule, next))
            }
        };
        if old.as_ref().map(|r| &r.rule) == new.as_ref().map(|(rule, _)| rule) {
            return Ok(old);
        }
        if old.is_none() && new.is_some() {
            let count: i64 = self
                .conn
                .query_row("SELECT COUNT(*) FROM task_repeats", [], |r| r.get(0))?;
            if count as usize >= MAX_REPEATS {
                return Err(Error::Invalid(format!(
                    "繰り返しを付けられるのは {MAX_REPEATS} 件までです"
                )));
            }
        }
        let now_ms = now_ms();
        let actor = self.actor.clone();
        let label = |r: Option<&Rule>| r.map(Rule::label).unwrap_or_default();
        let tx = self.conn.transaction()?;
        let saved = match (&old, new) {
            (_, None) => {
                tx.execute(
                    "DELETE FROM task_repeats WHERE task_id = ?1",
                    params![task_id],
                )?;
                None
            }
            (old, Some((rule, next))) => {
                let repeat = Repeat {
                    id: old.as_ref().map_or_else(new_id, |r| r.id.clone()),
                    task_id: task_id.to_owned(),
                    ai: old.as_ref().map(|r| r.ai.clone()).unwrap_or_default(),
                    at: at.clone(),
                    next_at: rule.when(next),
                    created_at: old.as_ref().map_or(now_ms, |r| r.created_at),
                    updated_at: now_ms,
                    rule,
                };
                save(&tx, &repeat)?;
                Some(repeat)
            }
        };
        insert_change(
            &tx,
            task_id,
            FIELD,
            &label(old.as_ref().map(|r| &r.rule)),
            &label(saved.as_ref().map(|r| &r.rule)),
            now_ms,
            &actor,
        )?;
        touch(&tx, task_id, now_ms, &actor)?;
        crate::store::trim_changes(&tx, task_id)?;
        tx.commit()?;
        Ok(saved)
    }

    /// 回を作ったときに AI に頼むことを変える。空にすると頼まない。
    pub fn set_repeat_ai(&mut self, task_id: &str, text: &str) -> Result<Repeat> {
        let text = clean_ai(text)?;
        let mut repeat = self
            .repeat(task_id)?
            .ok_or_else(|| Error::Invalid("繰り返しのないタスクです".into()))?;
        if repeat.ai == text {
            return Ok(repeat);
        }
        let old = std::mem::replace(&mut repeat.ai, text);
        let now = now_ms();
        repeat.updated_at = now;
        let actor = self.actor.clone();
        let tx = self.conn.transaction()?;
        save(&tx, &repeat)?;
        insert_change(&tx, task_id, AI_FIELD, &old, &repeat.ai, now, &actor)?;
        touch(&tx, task_id, now, &actor)?;
        crate::store::trim_changes(&tx, task_id)?;
        tx.commit()?;
        Ok(repeat)
    }

    /// 作る時刻が来た回を作る。作ったタスクの ID を返す。`now` は今の日時（`YYYY-MM-DD HH:MM`）。
    /// 1 つの繰り返しにつき、作るのは 1 回分だけ（時刻が過ぎた回がいくつもあれば、いちばん新しいもの）。
    /// 作った回は元に戻す相手にしない。
    pub fn sweep_repeats(&mut self, now: &str) -> Result<Vec<Id>> {
        let due: Vec<Repeat> = self
            .conn
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM task_repeats WHERE next_at <= ?1 ORDER BY next_at, id"
            ))?
            .query_map(params![now], from_row)?
            .collect::<rusqlite::Result<_>>()?;
        if due.is_empty() {
            return Ok(Vec::new());
        }
        // 前の変更（AI が変えた分など）は、その前の段にする
        self.undo_mark()?;
        let mut made = Vec::new();
        for repeat in due {
            if let Some(id) = self.make_next(repeat, now)? {
                made.push(id);
            }
        }
        self.undo_skip()?;
        Ok(made)
    }

    /// 1 つの繰り返しの次の回を作る。同じ回をもう作っていれば、次の時刻だけを進める。
    fn make_next(&mut self, mut repeat: Repeat, now: &str) -> Result<Option<Id>> {
        let rule = &repeat.rule;
        // 時刻が過ぎた回のうち、いちばん新しいもの
        let Some(mut day) = date(date_of(&repeat.next_at)) else {
            return Err(Error::Invalid(format!(
                "繰り返しの次の時刻が正しくありません: {}",
                repeat.next_at
            )));
        };
        while let Some(next) = rule
            .next_after(day)
            .filter(|d| rule.when(*d).as_str() <= now)
        {
            day = next;
        }
        let next = rule.next_after(day);
        let last: Option<String> = self
            .conn
            .prepare_cached("SELECT at FROM repeat_last WHERE repeat_id = ?1")?
            .query_row(params![repeat.id], |r| r.get(0))
            .optional()?;
        let made_already = last
            .as_deref()
            .is_some_and(|last| last >= ymd(day).as_str());
        let holder = self.task(&repeat.task_id)?;
        let subtasks = if made_already {
            Vec::new()
        } else {
            let mut subtasks = self.subtasks(&holder.id)?;
            subtasks.truncate(MAX_COPIED_SUBTASKS);
            subtasks
        };
        let days = date(&repeat.at).map_or(0, |at| (day - at).num_days());
        let now_ms = now_ms();
        let tx = self.conn.transaction()?;
        let made = if made_already {
            None
        } else {
            let copy = copy_task(&tx, &holder, None, days, now_ms)?;
            for sub in &subtasks {
                copy_task(&tx, sub, Some(&copy), days, now_ms)?;
            }
            tx.execute(
                "INSERT INTO repeat_last(repeat_id, at) VALUES (?1, ?2) \
                 ON CONFLICT(repeat_id) DO UPDATE SET at = excluded.at",
                params![repeat.id, ymd(day)],
            )?;
            if !repeat.ai.is_empty() {
                tx.execute(
                    "INSERT OR REPLACE INTO repeat_ai(task_id, prompt, failed, created_at) \
                     VALUES (?1, ?2, 0, ?3)",
                    params![copy, repeat.ai, now_ms],
                )?;
                trim_ai_queue(&tx)?;
            }
            repeat.task_id = copy.clone();
            repeat.at = ymd(day);
            Some(copy)
        };
        match next {
            Some(next) => {
                repeat.next_at = repeat.rule.when(next);
                repeat.updated_at = now_ms;
                save(&tx, &repeat)?;
            }
            // 次の回がない（ずっと先）なら、繰り返しは終わり
            None => {
                tx.execute("DELETE FROM task_repeats WHERE id = ?1", params![repeat.id])?;
            }
        }
        tx.commit()?;
        Ok(made)
    }

    /// AI の待ち行列。古い順。
    pub fn repeat_ai_jobs(&self) -> Result<Vec<AiJob>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT q.task_id, t.title, q.prompt, q.failed FROM repeat_ai q \
             JOIN tasks t ON t.id = q.task_id ORDER BY q.created_at, q.task_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(AiJob {
                task_id: r.get(0)?,
                title: r.get(1)?,
                prompt: r.get(2)?,
                failed: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 待ち行列から、失敗していない 1 件を取り出す（行列から消す）。
    pub fn take_repeat_ai(&mut self) -> Result<Option<AiJob>> {
        let Some(job) = self.repeat_ai_jobs()?.into_iter().find(|j| !j.failed) else {
            return Ok(None);
        };
        self.conn.execute(
            "DELETE FROM repeat_ai WHERE task_id = ?1",
            params![job.task_id],
        )?;
        Ok(Some(job))
    }

    /// 失敗した実行を、行列に戻す。もう一度頼まれるまで実行しない。
    pub fn fail_repeat_ai(&mut self, task_id: &str, prompt: &str) -> Result<()> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE id = ?1)",
            params![task_id],
            |r| r.get(0),
        )?;
        // 消えたタスクの分は、やり直す先がない
        if exists {
            self.conn.execute(
                "INSERT OR REPLACE INTO repeat_ai(task_id, prompt, failed, created_at) \
                 VALUES (?1, ?2, 1, ?3)",
                params![task_id, prompt, now_ms()],
            )?;
            trim_ai_queue(&self.conn)?;
        }
        Ok(())
    }

    /// 失敗した実行を、もう一度実行する。行列になければ `false`。
    pub fn retry_repeat_ai(&mut self, task_id: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE repeat_ai SET failed = 0 WHERE task_id = ?1",
            params![task_id],
        )? > 0)
    }
}

fn trim_ai_queue(conn: &Connection) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "DELETE FROM repeat_ai WHERE task_id NOT IN \
         (SELECT task_id FROM repeat_ai ORDER BY created_at DESC, task_id DESC LIMIT ?1)",
    )?
    .execute(params![MAX_AI_QUEUE as i64])?;
    Ok(())
}

/// タスクを写す。状態は未着手に戻し、開始日と期限を `days` 日ずらす。`parent` は写した親。
fn copy_task(
    conn: &Connection,
    from: &Task,
    parent: Option<&str>,
    days: i64,
    now: i64,
) -> Result<Id> {
    let position: i64 = conn.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM tasks WHERE workspace_id IS ?1",
        params![from.workspace_id],
        |r| r.get(0),
    )?;
    let task = Task {
        id: new_id(),
        workspace_id: from.workspace_id.clone(),
        title: from.title.clone(),
        description: from.description.clone(),
        status: Status::NeedsAction,
        priority: from.priority,
        start: from.start.as_deref().map(|s| shift(s, days)),
        due: from.due.as_deref().map(|d| shift(d, days)),
        percent: 0,
        parent_id: parent.map(str::to_owned).or_else(|| from.parent_id.clone()),
        assignee_ids: from.assignee_ids.clone(),
        assignee_roles: from.assignee_roles.clone(),
        assignee_done: vec![None; from.assignee_ids.len()],
        categories: from.categories.clone(),
        urls: from.urls.clone(),
        linked_workspace_ids: from.linked_workspace_ids.clone(),
        position,
        created_at: now,
        updated_at: now,
        completed_at: None,
        created_by: Actor::Repeat,
        updated_by: Actor::Repeat,
    };
    insert_task(conn, &task)?;
    record_created(conn, &task, &Actor::Repeat)?;
    // タスクごとの、期限をいつ知らせるかの上書きも写す
    conn.prepare_cached(
        "INSERT INTO task_alerts(task_id, timed, date) \
         SELECT ?1, timed, date FROM task_alerts WHERE task_id = ?2",
    )?
    .execute(params![task.id, from.id])?;
    Ok(task.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TaskDraft;

    fn d(s: &str) -> NaiveDate {
        date(s).unwrap()
    }

    fn rule(unit: Unit, every: u32, weekdays: &[u8], from: &str) -> Rule {
        Rule {
            unit,
            every,
            weekdays: weekdays.to_vec(),
            from: from.into(),
            time: None,
        }
        .clean()
        .unwrap()
    }

    fn next(r: &Rule, from: &str, n: usize) -> Vec<String> {
        let mut out = Vec::new();
        let mut day = d(from);
        for _ in 0..n {
            day = r.next_after(day).unwrap();
            out.push(ymd(day));
        }
        out
    }

    #[test]
    fn days_and_weeks_follow_the_interval() {
        let r = rule(Unit::Day, 3, &[], "2026-11-27");
        assert_eq!(
            next(&r, "2026-11-27", 3),
            ["2026-11-30", "2026-12-03", "2026-12-06"]
        );
        // 起点より前は回にならない
        assert_eq!(next(&r, "2026-11-01", 1), ["2026-11-27"]);

        // 2026-11-30 は月曜。2 週間ごとの月・木
        let r = rule(Unit::Week, 2, &[3, 0], "2026-11-30");
        assert_eq!(r.weekdays, [0, 3]);
        assert_eq!(
            next(&r, "2026-11-29", 4),
            ["2026-11-30", "2026-12-03", "2026-12-14", "2026-12-17"]
        );
        // 曜日を決めなければ、起点の曜日
        let r = rule(Unit::Week, 1, &[], "2026-11-27");
        assert_eq!(r.weekdays, [4]);
        assert_eq!(next(&r, "2026-11-27", 2), ["2026-12-04", "2026-12-11"]);
    }

    #[test]
    fn months_keep_the_day_and_end_of_month() {
        let r = rule(Unit::Month, 1, &[], "2026-01-31");
        assert_eq!(
            next(&r, "2026-01-31", 4),
            ["2026-02-28", "2026-03-31", "2026-04-30", "2026-05-31"]
        );
        let r = rule(Unit::Month, 3, &[], "2026-11-20");
        assert_eq!(next(&r, "2026-11-20", 2), ["2027-02-20", "2027-05-20"]);
        // うるう年の 2/29 は、ほかの年は 2/28
        let r = rule(Unit::Year, 1, &[], "2028-02-29");
        assert_eq!(next(&r, "2028-02-29", 2), ["2029-02-28", "2030-02-28"]);
        let r = rule(Unit::Year, 2, &[], "2026-11-27");
        assert_eq!(next(&r, "2026-11-27", 1), ["2028-11-27"]);
    }

    #[test]
    fn labels_read_naturally() {
        let from = "2026-11-30";
        assert_eq!(Rule::daily(from).label(), "毎日");
        assert_eq!(Rule::workdays(from).label(), "平日");
        assert_eq!(Rule::weekly(from).label(), "毎週 月");
        assert_eq!(Rule::monthly(from).label(), "毎月 30 日");
        assert_eq!(Rule::yearly(from).label(), "毎年 11/30");
        assert_eq!(rule(Unit::Day, 2, &[], from).label(), "2 日ごと");
        assert_eq!(
            rule(Unit::Week, 2, &[0, 3], from).label(),
            "2 週間ごと 月・木"
        );
        assert_eq!(rule(Unit::Month, 3, &[], from).label(), "3 か月ごと 30 日");
        let timed = Rule {
            time: Some("09:00".into()),
            ..Rule::weekly(from)
        };
        assert_eq!(timed.label(), "毎週 月 9:00");
        let late = Rule {
            time: Some("18:30".into()),
            ..Rule::daily(from)
        };
        assert_eq!(late.label(), "毎日 18:30");
        assert_eq!(next_label("2026-12-07 09:00"), "12/7（月）9:00");
        assert_eq!(next_label("2026-12-07 00:00"), "12/7（月）");
    }

    #[test]
    fn bad_rules_are_refused() {
        let bad = |r: Rule| r.clean().is_err();
        assert!(bad(Rule {
            every: 0,
            ..Rule::daily("2026-11-27")
        }));
        assert!(bad(Rule {
            every: MAX_EVERY + 1,
            ..Rule::daily("2026-11-27")
        }));
        assert!(bad(Rule::daily("2026-02-30")));
        assert!(bad(Rule {
            weekdays: vec![7],
            ..Rule::weekly("2026-11-27")
        }));
        assert!(bad(Rule {
            time: Some("25:00".into()),
            ..Rule::daily("2026-11-27")
        }));
        // 毎週でないときの曜日は捨てる
        let r = Rule {
            weekdays: vec![1],
            ..Rule::daily("2026-11-27")
        };
        assert!(r.clean().unwrap().weekdays.is_empty());
    }

    #[test]
    fn changing_the_unit_keeps_what_still_fits() {
        let r = Rule {
            every: 2,
            weekdays: vec![0, 3],
            time: Some("09:00".into()),
            ..Rule::weekly("2026-11-30")
        };
        let monthly = r.with_unit(Unit::Month);
        assert_eq!((monthly.every, monthly.time.as_deref()), (1, Some("09:00")));
        assert!(monthly.weekdays.is_empty());
        assert_eq!(r.with_unit(Unit::Week), r);
    }

    fn store_with(draft: TaskDraft) -> (Store, Task) {
        let mut store = Store::open_in_memory().unwrap();
        let w = store.create_workspace(None, "定例").unwrap();
        let task = store.create_task(Some(&w.id), &draft).unwrap();
        (store, task)
    }

    fn weekly_report() -> (Store, Task) {
        store_with(TaskDraft {
            title: "週次定例の報告".into(),
            description: "先週の進み".into(),
            // 2026-11-30 は月曜
            start: Some("2026-11-30 09:00".into()),
            due: Some("2026-11-30 10:00".into()),
            priority: 1,
            categories: vec!["定例".into()],
            urls: vec!["https://example.com/notes".into()],
            ..Default::default()
        })
    }

    #[test]
    fn setting_a_repeat_waits_for_the_next_time() {
        let (mut store, task) = weekly_report();
        let r = store
            .set_repeat(
                &task.id,
                Some(&Rule::weekly("2026-11-30")),
                "2026-11-27 12:00",
            )
            .unwrap()
            .unwrap();
        // 今の回は 11/30。次は 12/7 の 0:00
        assert_eq!(
            (r.at.as_str(), r.next_at.as_str()),
            ("2026-11-30", "2026-12-07 00:00")
        );
        assert!(store.sweep_repeats("2026-12-06 23:59").unwrap().is_empty());
        // 履歴に残る
        let changes = store.changes(&task.id).unwrap();
        let last = changes.last().unwrap();
        assert_eq!(
            (
                last.field.as_str(),
                last.old_value.as_str(),
                last.new_value.as_str()
            ),
            (FIELD, "", "毎週 月")
        );
        // 外すと消える
        assert!(store
            .set_repeat(&task.id, None, "2026-11-27 12:00")
            .unwrap()
            .is_none());
        assert!(store.repeat(&task.id).unwrap().is_none());
        assert_eq!(
            store.changes(&task.id).unwrap().last().unwrap().old_value,
            "毎週 月"
        );
    }

    #[test]
    fn an_old_task_does_not_make_a_copy_right_away() {
        // 3 週間前が期限のタスクに付けても、今日より前の回は作らない
        let (mut store, task) = store_with(TaskDraft {
            title: "日報".into(),
            due: Some("2026-11-06".into()),
            ..Default::default()
        });
        let r = store
            .set_repeat(
                &task.id,
                Some(&Rule {
                    time: Some("18:00".into()),
                    ..Rule::daily("2026-11-06")
                }),
                "2026-11-27 12:00",
            )
            .unwrap()
            .unwrap();
        assert_eq!(r.next_at, "2026-11-27 18:00");
        // 時刻を過ぎていれば明日
        let (mut store, task) = store_with(TaskDraft {
            title: "日報".into(),
            ..Default::default()
        });
        let r = store
            .set_repeat(
                &task.id,
                Some(&Rule {
                    time: Some("09:00".into()),
                    ..Rule::daily("2026-11-27")
                }),
                "2026-11-27 12:00",
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            (r.at.as_str(), r.next_at.as_str()),
            ("2026-11-27", "2026-11-28 09:00")
        );
    }

    #[test]
    fn the_next_copy_moves_dates_and_takes_the_repeat() {
        let (mut store, task) = weekly_report();
        store.ensure_self().unwrap();
        let me = store.people().unwrap()[0].id.clone();
        let mut d = TaskDraft::from(&task);
        d.assignee_ids = vec![me.clone()];
        d.status = Status::Completed;
        store.update_task(&task.id, &d).unwrap();
        store.add_subtask_for_test(&task, "資料を集める");
        let rule = Rule {
            time: Some("08:00".into()),
            ..Rule::weekly("2026-11-30")
        };
        store
            .set_repeat(&task.id, Some(&rule), "2026-11-27 12:00")
            .unwrap();
        store
            .set_repeat_ai(&task.id, "先週のコメントから報告の下書きを書く")
            .unwrap();

        let made = store.sweep_repeats("2026-12-07 08:00").unwrap();
        assert_eq!(made.len(), 1);
        let copy = store.task(&made[0]).unwrap();
        assert_eq!(copy.title, "週次定例の報告");
        assert_eq!(copy.description, "先週の進み");
        assert_eq!(copy.status, Status::NeedsAction);
        assert_eq!(copy.percent, 0);
        assert_eq!(copy.start.as_deref(), Some("2026-12-07 09:00"));
        assert_eq!(copy.due.as_deref(), Some("2026-12-07 10:00"));
        assert_eq!(copy.priority, 1);
        assert_eq!(copy.categories, ["定例"]);
        assert_eq!(copy.urls, ["https://example.com/notes"]);
        assert_eq!(copy.workspace_id, task.workspace_id);
        assert_eq!(copy.assignee_ids, [me.clone()]);
        assert!(!copy.is_done_by(&me));
        assert_eq!(copy.created_by, Actor::Repeat);
        // サブタスクも写す
        let subs = store.subtasks(&copy.id).unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].title, "資料を集める");
        // 繰り返しは新しい回へ移る
        assert!(store.repeat(&task.id).unwrap().is_none());
        let r = store.repeat(&copy.id).unwrap().unwrap();
        assert_eq!(
            (r.at.as_str(), r.next_at.as_str()),
            ("2026-12-07", "2026-12-14 08:00")
        );
        assert_eq!(r.ai, "先週のコメントから報告の下書きを書く");
        // AI の待ち行列に入る
        let jobs = store.repeat_ai_jobs().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].task_id, copy.id);
        // 同じ時刻では、もう作らない
        assert!(store.sweep_repeats("2026-12-07 08:01").unwrap().is_empty());
        // 元のタスクはそのまま
        assert_eq!(store.task(&task.id).unwrap().status, Status::Completed);
    }

    #[test]
    fn missed_times_make_only_the_latest_copy() {
        let (mut store, task) = weekly_report();
        store
            .set_repeat(
                &task.id,
                Some(&Rule::weekly("2026-11-30")),
                "2026-11-27 12:00",
            )
            .unwrap();
        // 3 週間アプリを開いていなかった
        let made = store.sweep_repeats("2026-12-23 10:00").unwrap();
        assert_eq!(made.len(), 1);
        let copy = store.task(&made[0]).unwrap();
        assert_eq!(copy.due.as_deref(), Some("2026-12-21 10:00"));
        let r = store.repeat(&copy.id).unwrap().unwrap();
        assert_eq!(r.next_at, "2026-12-28 00:00");
    }

    #[test]
    fn undo_does_not_make_the_same_copy_twice() {
        let (mut store, task) = weekly_report();
        store.undo_mark().unwrap();
        store
            .set_repeat(
                &task.id,
                Some(&Rule::weekly("2026-11-30")),
                "2026-11-27 12:00",
            )
            .unwrap();
        store.undo_mark().unwrap();
        store.set_repeat_ai(&task.id, "下書き").unwrap();
        store.undo_mark().unwrap();
        let made = store.sweep_repeats("2026-12-07 00:00").unwrap();
        assert_eq!(made.len(), 1);
        // 作った回は戻す相手にならない。戻すのは AI の指示の変更
        assert!(store.undo().unwrap());
        assert!(store.task(&made[0]).is_ok());
        // 繰り返しが前の回に戻っても、同じ回はもう作らない
        assert!(store.sweep_repeats("2026-12-07 00:01").unwrap().is_empty());
        let open = store
            .tasks(Scope::All, &TaskFilter::default())
            .unwrap()
            .len();
        assert_eq!(open, 2);
    }

    #[test]
    fn deleting_the_task_stops_the_repeat_and_ai_jobs() {
        let (mut store, task) = weekly_report();
        store
            .set_repeat(
                &task.id,
                Some(&Rule::weekly("2026-11-30")),
                "2026-11-27 12:00",
            )
            .unwrap();
        store.set_repeat_ai(&task.id, "下書き").unwrap();
        let made = store.sweep_repeats("2026-12-07 00:00").unwrap();
        store.delete_task(&made[0]).unwrap();
        assert!(store.repeat_ai_jobs().unwrap().is_empty());
        assert!(store.repeating_task_ids().unwrap().is_empty());
        assert!(store.sweep_repeats("2026-12-14 00:00").unwrap().is_empty());
    }

    #[test]
    fn ai_jobs_can_fail_and_be_retried() {
        let (mut store, task) = weekly_report();
        store
            .set_repeat(
                &task.id,
                Some(&Rule::weekly("2026-11-30")),
                "2026-11-27 12:00",
            )
            .unwrap();
        store.set_repeat_ai(&task.id, "下書き").unwrap();
        let made = store.sweep_repeats("2026-12-07 00:00").unwrap();
        let job = store.take_repeat_ai().unwrap().unwrap();
        assert_eq!(
            (job.task_id.as_str(), job.prompt.as_str()),
            (made[0].as_str(), "下書き")
        );
        assert!(store.take_repeat_ai().unwrap().is_none());
        store.fail_repeat_ai(&job.task_id, &job.prompt).unwrap();
        // 失敗したものは、頼まれるまで取り出さない
        assert!(store.take_repeat_ai().unwrap().is_none());
        assert!(store.repeat_ai_jobs().unwrap()[0].failed);
        assert!(store.retry_repeat_ai(&job.task_id).unwrap());
        assert_eq!(
            store.take_repeat_ai().unwrap().unwrap().task_id,
            job.task_id
        );
        // 消えたタスクの分は戻さない
        store.delete_task(&job.task_id).unwrap();
        store.fail_repeat_ai(&job.task_id, "下書き").unwrap();
        assert!(store.repeat_ai_jobs().unwrap().is_empty());
    }

    #[test]
    fn the_ai_text_needs_a_repeat_and_has_a_limit() {
        let (mut store, task) = weekly_report();
        assert!(store.set_repeat_ai(&task.id, "下書き").is_err());
        store
            .set_repeat(
                &task.id,
                Some(&Rule::weekly("2026-11-30")),
                "2026-11-27 12:00",
            )
            .unwrap();
        assert!(store
            .set_repeat_ai(&task.id, &"あ".repeat(MAX_AI_CHARS + 1))
            .is_err());
        let r = store.set_repeat_ai(&task.id, "  下書き  ").unwrap();
        assert_eq!(r.ai, "下書き");
        let last = store.changes(&task.id).unwrap().pop().unwrap();
        assert_eq!(
            (last.field.as_str(), last.new_value.as_str()),
            (AI_FIELD, "下書き")
        );
    }

    #[test]
    fn shifting_keeps_the_time() {
        assert_eq!(shift("2026-11-30 09:00", 7), "2026-12-07 09:00");
        assert_eq!(shift("2026-12-28", 7), "2027-01-04");
        assert_eq!(shift("2026-12-28", -28), "2026-11-30");
    }

    impl Store {
        fn add_subtask_for_test(&mut self, parent: &Task, title: &str) {
            self.create_task(
                parent.workspace_id.as_deref(),
                &TaskDraft {
                    title: title.into(),
                    parent_id: Some(parent.id.clone()),
                    due: Some("2026-11-29".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        }
    }
}
