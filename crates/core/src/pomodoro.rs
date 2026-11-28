//! ポモドーロタイマー（作業と休憩を交互に測る）と、タスクごとの作業の記録。
//!
//! タイマーの状態は設定（[`TIMER_KEY`]）に JSON で置く。アプリの画面、AI チャット、MCP の
//! どこから動かしても同じ状態を読み書きし、アプリを閉じても続きから戻る。
//! 時刻が来た段は、次に読んだときに進める（[`Timer::advance`]）。作業の段が終わると、
//! 測った時間をタスクの作業の記録（`work_logs`）に残す。
//!
//! 段が終わったら、次の段は止まったまま待つ。「休憩を自動で始める」を選んだときだけ、
//! 作業のあとの休憩をそのまま始める。休憩のあとの作業は、いつもユーザーが始める。
//!
//! 作業の記録は元に戻す相手にしない（タスクを消して元に戻すと、記録も一緒に戻る）。

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::model::{new_id, Id};
use crate::{Error, Result, Store};

/// タイマーの状態を置く設定の名前。
pub const TIMER_KEY: &str = "pomodoro.timer";
/// 長さなどの設定を置く設定の名前。
pub const SETTINGS_KEY: &str = "pomodoro.settings";
/// 段の長さ（分）の範囲。
pub const MIN_MINUTES: u32 = 1;
pub const MAX_MINUTES: u32 = 180;
/// 長い休憩までの作業の回数の範囲。
pub const MIN_ROUNDS: u32 = 1;
pub const MAX_ROUNDS: u32 = 12;
/// これより短い作業は記録しない（秒）。
pub const MIN_LOG_SECS: i64 = 60;
/// 残す作業の記録の数。超えたら古いものから消す。
pub const MAX_LOGS: i64 = 50_000;
/// 1 回に進める段の数。アプリを長く閉じていても、溜めて記録しない。
const MAX_STEPS: usize = 3;

/// 段の種類。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Work,
    ShortBreak,
    LongBreak,
}

impl Phase {
    pub const ALL: [Phase; 3] = [Phase::Work, Phase::ShortBreak, Phase::LongBreak];

    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Work => "work",
            Phase::ShortBreak => "short_break",
            Phase::LongBreak => "long_break",
        }
    }

    pub fn parse(text: &str) -> Option<Phase> {
        Phase::ALL.into_iter().find(|p| p.as_str() == text)
    }

    /// 画面に出す名前。
    pub fn label(self) -> &'static str {
        match self {
            Phase::Work => "作業",
            Phase::ShortBreak => "休憩",
            Phase::LongBreak => "長い休憩",
        }
    }

    pub fn is_break(self) -> bool {
        self != Phase::Work
    }
}

/// 段を始めたときに、OS で再生中のメディア（音楽や動画）をどうするか。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Media {
    /// 何もしない。
    #[default]
    None,
    /// 再生する。
    Play,
    /// 一時停止する。
    Pause,
}

impl Media {
    pub const ALL: [Media; 3] = [Media::None, Media::Play, Media::Pause];

    pub fn as_str(self) -> &'static str {
        match self {
            Media::None => "none",
            Media::Play => "play",
            Media::Pause => "pause",
        }
    }

    pub fn parse(text: &str) -> Option<Media> {
        Media::ALL.into_iter().find(|m| m.as_str() == text)
    }

    pub fn label(self) -> &'static str {
        match self {
            Media::None => "なし",
            Media::Play => "再生",
            Media::Pause => "一時停止",
        }
    }
}

/// タイマーの設定。長さは分。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub work: u32,
    pub short_break: u32,
    pub long_break: u32,
    /// 長い休憩までの作業の回数。
    pub rounds: u32,
    /// 作業が終わったら、休憩をそのまま始める。
    pub auto_break: bool,
    /// 作業を始めたとき（一時停止から戻したときも）のメディア。
    pub work_media: Media,
    /// 休憩を始めたときのメディア。
    pub break_media: Media,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            work: 25,
            short_break: 5,
            long_break: 15,
            rounds: 4,
            auto_break: false,
            work_media: Media::None,
            break_media: Media::None,
        }
    }
}

impl Settings {
    /// 段の長さ（秒）。
    pub fn seconds(&self, phase: Phase) -> i64 {
        let minutes = match phase {
            Phase::Work => self.work,
            Phase::ShortBreak => self.short_break,
            Phase::LongBreak => self.long_break,
        };
        i64::from(minutes) * 60
    }

    pub fn minutes(&self, phase: Phase) -> u32 {
        match phase {
            Phase::Work => self.work,
            Phase::ShortBreak => self.short_break,
            Phase::LongBreak => self.long_break,
        }
    }

    pub fn set_minutes(&mut self, phase: Phase, minutes: u32) {
        match phase {
            Phase::Work => self.work = minutes,
            Phase::ShortBreak => self.short_break = minutes,
            Phase::LongBreak => self.long_break = minutes,
        }
    }

    /// 範囲の外の値があれば、その項目を挙げて断る。
    pub fn validate(&self) -> Result<()> {
        let mut bad = Vec::new();
        for phase in Phase::ALL {
            let m = self.minutes(phase);
            if !(MIN_MINUTES..=MAX_MINUTES).contains(&m) {
                bad.push(format!(
                    "{}の長さは {MIN_MINUTES}〜{MAX_MINUTES} 分です（{m} 分）",
                    phase.label()
                ));
            }
        }
        if !(MIN_ROUNDS..=MAX_ROUNDS).contains(&self.rounds) {
            bad.push(format!(
                "長い休憩までの回数は {MIN_ROUNDS}〜{MAX_ROUNDS} 回です（{} 回）",
                self.rounds
            ));
        }
        if bad.is_empty() {
            Ok(())
        } else {
            Err(Error::Invalid(bad.join("。")))
        }
    }
}

/// 作業の記録 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkLog {
    pub id: Id,
    /// 作業したタスク。タスクを選ばずに測ったときは `None`。
    pub task_id: Option<Id>,
    /// 測り始めた時刻と、終えた時刻（UNIX ミリ秒）。
    pub started_at: i64,
    pub ended_at: i64,
    /// 測った長さ（一時停止していた間は数えない）。
    pub seconds: i64,
}

/// まだデータベースに書いていない記録。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub task_id: Option<Id>,
    pub started_at: i64,
    pub ended_at: i64,
    pub seconds: i64,
}

/// タイマーの状態。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Timer {
    pub phase: Phase,
    /// 作業しているタスク。
    pub task_id: Option<Id>,
    /// 長い休憩のあとから数えた、済んだ作業の数。
    pub done: u32,
    /// この段の長さ（秒）。
    pub length: i64,
    /// 動いているなら、動かし始めた時刻（UNIX ミリ秒）。止まっていれば `None`。
    pub since: Option<i64>,
    /// `since` より前に進んだ分（ミリ秒）。
    pub elapsed: i64,
    /// この段の作業のうち、もう記録した分（ミリ秒）。
    pub logged: i64,
    /// まだ記録していない作業を測り始めた時刻（UNIX ミリ秒）。
    pub from: Option<i64>,
    /// 時刻が来て終わった段の数。画面はこれが増えたら知らせる。
    pub ended: u64,
    /// 最後に時刻が来て終わった段と、その時刻。
    pub last_ended: Option<Phase>,
    pub ended_at: Option<i64>,
}

impl Default for Timer {
    fn default() -> Self {
        Timer::new(&Settings::default())
    }
}

/// 今どうなっているか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Run {
    /// 始める前（段の長さのまま）。
    Ready,
    Running,
    Paused,
}

impl Run {
    pub fn as_str(self) -> &'static str {
        match self {
            Run::Ready => "ready",
            Run::Running => "running",
            Run::Paused => "paused",
        }
    }
}

impl Timer {
    pub fn new(settings: &Settings) -> Self {
        Timer {
            phase: Phase::Work,
            task_id: None,
            done: 0,
            length: settings.seconds(Phase::Work),
            since: None,
            elapsed: 0,
            logged: 0,
            from: None,
            ended: 0,
            last_ended: None,
            ended_at: None,
        }
    }

    pub fn run(&self) -> Run {
        if self.since.is_some() {
            Run::Running
        } else if self.elapsed > 0 {
            Run::Paused
        } else {
            Run::Ready
        }
    }

    pub fn running(&self) -> bool {
        self.since.is_some()
    }

    /// この段で進んだ時間（ミリ秒）。段の長さを超えない。
    pub fn elapsed_ms(&self, now: i64) -> i64 {
        let running = self.since.map_or(0, |since| (now - since).max(0));
        (self.elapsed + running).min(self.length * 1000)
    }

    /// 残り時間（ミリ秒）。
    pub fn remaining_ms(&self, now: i64) -> i64 {
        (self.length * 1000 - self.elapsed_ms(now)).max(0)
    }

    /// 進んだ割合（0〜1）。
    pub fn progress(&self, now: i64) -> f32 {
        if self.length <= 0 {
            return 0.;
        }
        (self.elapsed_ms(now) as f64 / (self.length * 1000) as f64).clamp(0., 1.) as f32
    }

    /// 時刻が来た段を終わらせて、次の段へ進める。記録する作業を返す。
    pub fn advance(&mut self, settings: &Settings, now: i64) -> Vec<Pending> {
        let mut logs = Vec::new();
        for _ in 0..MAX_STEPS {
            let Some(since) = self.since else { break };
            let end_at = since + (self.length * 1000 - self.elapsed).max(0);
            if now < end_at {
                break;
            }
            logs.extend(self.finish(settings, end_at, true));
        }
        logs
    }

    /// 始める。一時停止していたら続きから。
    pub fn start(&mut self, now: i64) {
        if self.since.is_none() {
            self.since = Some(now);
            if self.phase == Phase::Work {
                self.from.get_or_insert(now);
            }
        }
    }

    pub fn pause(&mut self, now: i64) {
        if let Some(since) = self.since.take() {
            self.elapsed = (self.elapsed + (now - since).max(0)).min(self.length * 1000);
        }
    }

    /// 止めて、最初の作業に戻す。測った作業は記録する。
    pub fn stop(&mut self, settings: &Settings, now: i64) -> Vec<Pending> {
        self.pause(now);
        let log = self.take_log(now);
        self.phase = Phase::Work;
        self.done = 0;
        self.reset(settings);
        log.into_iter().collect()
    }

    /// 今の段を飛ばして、次の段へ進める。測った作業は記録する。
    pub fn skip(&mut self, settings: &Settings, now: i64) -> Vec<Pending> {
        self.pause(now);
        self.finish(settings, now, false)
    }

    /// 作業するタスクを変える。作業の途中なら、それまでの分を前のタスクに記録する。
    pub fn set_task(&mut self, task_id: Option<Id>, now: i64) -> Vec<Pending> {
        if self.task_id == task_id {
            return Vec::new();
        }
        let log = self.take_log(now);
        self.task_id = task_id;
        log.into_iter().collect()
    }

    /// 設定が変わったとき。始める前の段は、新しい長さにする。
    pub fn follow(&mut self, settings: &Settings) {
        if self.run() == Run::Ready {
            self.length = settings.seconds(self.phase);
        }
    }

    /// 作業の段の、まだ記録していない分を記録にする。短すぎる分は次へ持ち越す。
    fn take_log(&mut self, now: i64) -> Option<Pending> {
        if self.phase != Phase::Work {
            return None;
        }
        let elapsed = self.elapsed_ms(now);
        let ms = elapsed - self.logged;
        if ms < MIN_LOG_SECS * 1000 {
            return None;
        }
        let started_at = self.from.unwrap_or(now - ms);
        self.logged = elapsed;
        self.from = self.since.map(|_| now);
        Some(Pending {
            task_id: self.task_id.clone(),
            started_at,
            ended_at: now,
            seconds: ms / 1000,
        })
    }

    /// 今の段を `at` で終わらせて、次の段にする。`timed` は時刻が来て終わったか。
    fn finish(&mut self, settings: &Settings, at: i64, timed: bool) -> Vec<Pending> {
        let mut logs = Vec::new();
        let ended = self.phase;
        // 終わった時刻までの分で数える
        if let Some(since) = self.since.take() {
            self.elapsed = (self.elapsed + (at - since).max(0)).min(self.length * 1000);
        }
        let next = match ended {
            Phase::Work => {
                logs.extend(self.take_log(at));
                self.done += 1;
                if self.done.is_multiple_of(settings.rounds.max(1)) {
                    Phase::LongBreak
                } else {
                    Phase::ShortBreak
                }
            }
            Phase::LongBreak => {
                self.done = 0;
                Phase::Work
            }
            Phase::ShortBreak => Phase::Work,
        };
        self.phase = next;
        self.reset(settings);
        if timed {
            self.ended += 1;
            self.last_ended = Some(ended);
            self.ended_at = Some(at);
        }
        if next.is_break() && settings.auto_break {
            self.since = Some(at);
        }
        logs
    }

    fn reset(&mut self, settings: &Settings) {
        self.length = settings.seconds(self.phase);
        self.since = None;
        self.elapsed = 0;
        self.logged = 0;
        self.from = None;
    }
}

/// タイマーへの操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// 始める（一時停止していれば続きから）。`task` があれば、先にタスクを変える
    /// （`Some(None)` はタスクを選ばない）。
    Start {
        task: Option<Option<Id>>,
    },
    Pause,
    Stop,
    Skip,
    SetTask(Option<Id>),
}

/// 何分何秒を「1 時間 5 分」のように書く。1 分に満たなければ「1 分未満」。
pub fn duration_label(seconds: i64) -> String {
    let minutes = seconds / 60;
    match (minutes / 60, minutes % 60) {
        (0, 0) => "1 分未満".to_owned(),
        (0, m) => format!("{m} 分"),
        (h, 0) => format!("{h} 時間"),
        (h, m) => format!("{h} 時間 {m} 分"),
    }
}

/// 残り時間を「24:59」（1 時間以上は「1:05:00」）の形で書く。秒は切り上げる。
pub fn clock_label(ms: i64) -> String {
    let total = (ms.max(0) + 999) / 1000;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// その日（`YYYY-MM-DD`、この PC の時刻）の始まりの UNIX ミリ秒。
pub fn day_start(date: &str) -> Option<i64> {
    use chrono::{Local, NaiveDate, TimeZone};
    let day = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0)?)
        .earliest()
        .map(|t| t.timestamp_millis())
}

fn log_from_row(row: &rusqlite::Row) -> rusqlite::Result<WorkLog> {
    Ok(WorkLog {
        id: row.get(0)?,
        task_id: row.get(1)?,
        started_at: row.get(2)?,
        ended_at: row.get(3)?,
        seconds: row.get(4)?,
    })
}

pub(crate) fn insert_log(conn: &rusqlite::Connection, log: &WorkLog) -> Result<()> {
    conn.prepare_cached(
        "INSERT OR REPLACE INTO work_logs(id, task_id, started_at, ended_at, seconds) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?
    .execute(params![
        log.id,
        log.task_id,
        log.started_at,
        log.ended_at,
        log.seconds
    ])?;
    Ok(())
}

/// 取り込む記録を確かめる。
pub fn clean_log(log: &WorkLog) -> Result<WorkLog> {
    if log.id.is_empty() || log.seconds < 0 || log.ended_at < log.started_at {
        return Err(Error::Invalid("作業の記録が正しくありません".into()));
    }
    Ok(log.clone())
}

impl Store {
    /// タイマーの設定。
    pub fn pomodoro_settings(&self) -> Result<Settings> {
        match self.setting(SETTINGS_KEY)? {
            Some(text) => Ok(serde_json::from_str(&text)?),
            None => Ok(Settings::default()),
        }
    }

    /// タイマーの設定を変える。始める前の段の長さも合わせる。
    pub fn set_pomodoro_settings(&mut self, settings: &Settings) -> Result<Timer> {
        settings.validate()?;
        let mut timer = self.pomodoro_timer()?;
        timer.follow(settings);
        let tx = self.conn.transaction()?;
        put(&tx, SETTINGS_KEY, &serde_json::to_string(settings)?)?;
        put(&tx, TIMER_KEY, &serde_json::to_string(&timer)?)?;
        tx.commit()?;
        Ok(timer)
    }

    /// 保存してあるタイマー（時刻が来た段は進めていない）。
    pub fn pomodoro_timer(&self) -> Result<Timer> {
        match self.setting(TIMER_KEY)? {
            Some(text) => Ok(serde_json::from_str(&text)?),
            None => Ok(Timer::new(&self.pomodoro_settings()?)),
        }
    }

    /// 今のタイマー。時刻が来た段を進め、終わった作業を記録する。
    pub fn pomodoro_now(&mut self, now: i64) -> Result<Timer> {
        self.pomodoro_apply(None, now)
    }

    /// タイマーを操作する。先に時刻が来た段を進める。
    pub fn pomodoro(&mut self, op: Op, now: i64) -> Result<Timer> {
        self.pomodoro_apply(Some(op), now)
    }

    fn pomodoro_apply(&mut self, op: Option<Op>, now: i64) -> Result<Timer> {
        let settings = self.pomodoro_settings()?;
        let before = self.pomodoro_timer()?;
        let mut timer = before.clone();
        // 消したタスクは選んでいないことにする
        if let Some(id) = timer.task_id.clone() {
            if !self.task_exists(&id)? {
                timer.task_id = None;
            }
        }
        let mut logs = timer.advance(&settings, now);
        match op {
            None => {}
            Some(Op::Start { task }) => {
                if let Some(task) = task {
                    self.check_task(task.as_deref())?;
                    logs.extend(timer.set_task(task, now));
                }
                timer.start(now);
            }
            Some(Op::Pause) => timer.pause(now),
            Some(Op::Stop) => logs.extend(timer.stop(&settings, now)),
            Some(Op::Skip) => logs.extend(timer.skip(&settings, now)),
            Some(Op::SetTask(task)) => {
                self.check_task(task.as_deref())?;
                logs.extend(timer.set_task(task, now));
            }
        }
        if timer == before && logs.is_empty() {
            return Ok(timer);
        }
        // ここまでの変更は区切り、記録は元に戻す相手にしない
        self.undo_mark()?;
        let tx = self.conn.transaction()?;
        for log in logs {
            // 測っている間に消したタスクの分は、タスクなしで残す
            let task_id = match log.task_id {
                Some(id) => {
                    let known: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM tasks WHERE id = ?1)",
                        params![id],
                        |r| r.get(0),
                    )?;
                    known.then_some(id)
                }
                None => None,
            };
            insert_log(
                &tx,
                &WorkLog {
                    id: new_id(),
                    task_id,
                    started_at: log.started_at,
                    ended_at: log.ended_at,
                    seconds: log.seconds,
                },
            )?;
        }
        tx.execute(
            "DELETE FROM work_logs WHERE id IN (SELECT id FROM work_logs \
             ORDER BY ended_at DESC, id DESC LIMIT -1 OFFSET ?1)",
            params![MAX_LOGS],
        )?;
        put(&tx, TIMER_KEY, &serde_json::to_string(&timer)?)?;
        tx.commit()?;
        self.undo_skip()?;
        Ok(timer)
    }

    fn task_exists(&self, id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE id = ?1)",
            params![id],
            |r| r.get(0),
        )?)
    }

    fn check_task(&self, id: Option<&str>) -> Result<()> {
        match id {
            Some(id) if !self.task_exists(id)? => Err(Error::NotFound(format!("タスク {id}"))),
            _ => Ok(()),
        }
    }

    /// 作業の記録。新しい順。`task_id` を渡すとそのタスクだけ、`since` 以降（UNIX ミリ秒）に終えたものだけ。
    pub fn work_logs(
        &self,
        task_id: Option<&str>,
        since: Option<i64>,
        limit: usize,
    ) -> Result<Vec<WorkLog>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, task_id, started_at, ended_at, seconds FROM work_logs \
             WHERE (?1 IS NULL OR task_id = ?1) AND (?2 IS NULL OR ended_at >= ?2) \
             ORDER BY ended_at DESC, id DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![task_id, since, limit as i64], log_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// タスクの作業の回数と、合計の秒数。
    pub fn work_total(&self, task_id: &str) -> Result<(usize, i64)> {
        let (count, seconds): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(seconds), 0) FROM work_logs WHERE task_id = ?1",
            params![task_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((count as usize, seconds))
    }

    /// `since` 以降（UNIX ミリ秒）に終えた作業の回数と、合計の秒数（タスクなしも含む）。
    pub fn work_total_since(&self, since: i64) -> Result<(usize, i64)> {
        let (count, seconds): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(seconds), 0) FROM work_logs WHERE ended_at >= ?1",
            params![since],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((count as usize, seconds))
    }

    pub(crate) fn all_work_logs(&self) -> Result<Vec<WorkLog>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, task_id, started_at, ended_at, seconds FROM work_logs ORDER BY ended_at, id",
        )?;
        let rows = stmt.query_map([], log_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 1 件の記録（テスト用）。
    pub fn work_log(&self, id: &str) -> Result<Option<WorkLog>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, task_id, started_at, ended_at, seconds FROM work_logs WHERE id = ?1",
                params![id],
                log_from_row,
            )
            .optional()?)
    }
}

fn put(tx: &rusqlite::Transaction, key: &str, value: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TaskDraft;

    const MIN: i64 = 60_000;

    fn draft(title: &str) -> TaskDraft {
        TaskDraft {
            title: title.into(),
            ..TaskDraft::default()
        }
    }

    fn short() -> Settings {
        Settings {
            work: 25,
            short_break: 5,
            long_break: 15,
            rounds: 2,
            ..Settings::default()
        }
    }

    #[test]
    fn 作業が終わると休憩が止まったまま待つ() {
        let s = short();
        let mut t = Timer::new(&s);
        assert_eq!(t.run(), Run::Ready);
        t.start(0);
        assert_eq!(t.remaining_ms(10 * MIN), 15 * MIN);
        assert!(t.advance(&s, 24 * MIN).is_empty());
        let logs = t.advance(&s, 30 * MIN);
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].seconds, 25 * 60);
        assert_eq!((logs[0].started_at, logs[0].ended_at), (0, 25 * MIN));
        assert_eq!(t.phase, Phase::ShortBreak);
        assert_eq!(t.run(), Run::Ready);
        assert_eq!((t.ended, t.last_ended), (1, Some(Phase::Work)));
        assert_eq!(t.ended_at, Some(25 * MIN));
        assert_eq!(t.remaining_ms(40 * MIN), 5 * MIN);
    }

    #[test]
    fn 休憩を自動で始めると終わった時刻から数える() {
        let s = Settings {
            auto_break: true,
            ..short()
        };
        let mut t = Timer::new(&s);
        t.start(0);
        // 作業 25 分のあと休憩 5 分も過ぎていたら、作業で止まって待つ（作業は自動で始めない）
        let logs = t.advance(&s, 40 * MIN);
        assert_eq!(logs.len(), 1);
        assert_eq!(t.phase, Phase::Work);
        assert_eq!(t.run(), Run::Ready);
        assert_eq!(t.ended, 2);
        assert_eq!(t.done, 1);
        // 休憩の途中なら、休憩が動いている
        let mut t = Timer::new(&s);
        t.start(0);
        t.advance(&s, 27 * MIN);
        assert_eq!(t.phase, Phase::ShortBreak);
        assert_eq!(t.remaining_ms(27 * MIN), 3 * MIN);
    }

    #[test]
    fn 決めた回数ごとに長い休憩になり_そのあと数え直す() {
        let s = short();
        let mut t = Timer::new(&s);
        let mut at = 0;
        let mut phases = Vec::new();
        for _ in 0..6 {
            t.start(at);
            at += t.length * 1000;
            t.advance(&s, at);
            phases.push(t.phase);
        }
        use Phase::*;
        assert_eq!(
            phases,
            [ShortBreak, Work, LongBreak, Work, ShortBreak, Work]
        );
        assert_eq!(t.done, 1);
    }

    #[test]
    fn 一時停止の間は数えず_止めると測った分だけ記録する() {
        let s = short();
        let mut t = Timer::new(&s);
        t.start(0);
        t.pause(10 * MIN);
        assert_eq!(t.run(), Run::Paused);
        assert_eq!(t.remaining_ms(100 * MIN), 15 * MIN);
        t.start(20 * MIN);
        let logs = t.stop(&s, 25 * MIN);
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].seconds, 15 * 60);
        assert_eq!(logs[0].started_at, 0);
        assert_eq!(t.run(), Run::Ready);
        assert_eq!(t.phase, Phase::Work);
        // 1 分に満たない作業は記録しない
        t.start(0);
        assert!(t.stop(&s, 30_000).is_empty());
    }

    #[test]
    fn 途中でタスクを変えると前のタスクにそれまでの分を記録する() {
        let s = short();
        let mut t = Timer::new(&s);
        t.set_task(Some("a".into()), 0);
        t.start(0);
        let logs = t.set_task(Some("b".into()), 10 * MIN);
        assert_eq!(logs[0].task_id.as_deref(), Some("a"));
        assert_eq!(logs[0].seconds, 10 * 60);
        let logs = t.advance(&s, 25 * MIN);
        assert_eq!(logs[0].task_id.as_deref(), Some("b"));
        assert_eq!(logs[0].seconds, 15 * 60);
        assert_eq!(logs[0].started_at, 10 * MIN);
    }

    #[test]
    fn 飛ばすと次の段にして_知らせる数は増やさない() {
        let s = short();
        let mut t = Timer::new(&s);
        t.start(0);
        let logs = t.skip(&s, 5 * MIN);
        assert_eq!(logs[0].seconds, 5 * 60);
        assert_eq!(t.phase, Phase::ShortBreak);
        assert_eq!(t.ended, 0);
        t.skip(&s, 6 * MIN);
        assert_eq!(t.phase, Phase::Work);
    }

    #[test]
    fn 設定の範囲と始める前の長さ() {
        let mut s = Settings::default();
        assert!(s.validate().is_ok());
        s.work = 0;
        s.rounds = 13;
        let error = s.validate().unwrap_err().to_string();
        assert!(error.contains("作業の長さ"));
        assert!(error.contains("長い休憩までの回数"));
        let mut t = Timer::new(&Settings::default());
        let s = Settings {
            work: 50,
            ..Settings::default()
        };
        t.follow(&s);
        assert_eq!(t.length, 50 * 60);
        t.start(0);
        t.follow(&Settings::default());
        assert_eq!(t.length, 50 * 60);
    }

    #[test]
    fn 書き方() {
        assert_eq!(clock_label(25 * MIN), "25:00");
        assert_eq!(clock_label(59_001), "01:00");
        assert_eq!(clock_label(65 * MIN), "1:05:00");
        assert_eq!(duration_label(30), "1 分未満");
        assert_eq!(duration_label(25 * 60), "25 分");
        assert_eq!(duration_label(120 * 60), "2 時間");
        assert_eq!(duration_label(75 * 60), "1 時間 15 分");
    }

    #[test]
    fn 保存したタイマーは時刻が来ると記録を残して進む() {
        let mut store = Store::open_in_memory().unwrap();
        let task = store.create_task(None, &draft("資料を作る")).unwrap();
        let t = store
            .pomodoro(
                Op::Start {
                    task: Some(Some(task.id.clone())),
                },
                0,
            )
            .unwrap();
        assert_eq!(t.run(), Run::Running);
        assert_eq!(store.pomodoro_timer().unwrap(), t);
        let t = store.pomodoro_now(26 * MIN).unwrap();
        assert_eq!(t.phase, Phase::ShortBreak);
        assert_eq!(store.work_total(&task.id).unwrap(), (1, 25 * 60));
        let logs = store.work_logs(Some(&task.id), None, 10).unwrap();
        assert_eq!(logs[0].ended_at, 25 * MIN);
        // ないタスクは選べない
        assert!(store.pomodoro(Op::SetTask(Some("ない".into())), 0).is_err());
        assert_eq!(store.work_total_since(0).unwrap(), (1, 25 * 60));
        assert_eq!(store.work_total_since(26 * MIN).unwrap(), (0, 0));
        // 測ったことは元に戻す段にならない（戻すのはタスクの作成だけ）
        assert!(store.undo().unwrap());
        assert!(store.task(&task.id).is_err());
        assert!(!store.can_undo());
    }

    #[test]
    fn タスクを消して元に戻すと記録も戻る() {
        let mut store = Store::open_in_memory().unwrap();
        let task = store.create_task(None, &draft("資料を作る")).unwrap();
        store
            .pomodoro(
                Op::Start {
                    task: Some(Some(task.id.clone())),
                },
                0,
            )
            .unwrap();
        store.pomodoro(Op::Stop, 10 * MIN).unwrap();
        store.undo_mark().unwrap();
        store.delete_task(&task.id).unwrap();
        store.undo_mark().unwrap();
        assert_eq!(store.work_total(&task.id).unwrap().0, 0);
        assert!(store.undo().unwrap());
        assert_eq!(store.work_total(&task.id).unwrap(), (1, 10 * 60));
    }

    #[test]
    fn 測っている間に消したタスクの分はタスクなしで残る() {
        let mut store = Store::open_in_memory().unwrap();
        let task = store.create_task(None, &draft("資料を作る")).unwrap();
        store
            .pomodoro(
                Op::Start {
                    task: Some(Some(task.id.clone())),
                },
                0,
            )
            .unwrap();
        store.delete_task(&task.id).unwrap();
        let t = store.pomodoro(Op::Stop, 10 * MIN).unwrap();
        assert_eq!(t.task_id, None);
        let logs = store.work_logs(None, None, 10).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].task_id, None);
    }

    #[test]
    fn 設定を変えると始める前の段の長さも変わる() {
        let mut store = Store::open_in_memory().unwrap();
        let s = Settings {
            work: 50,
            work_media: Media::Play,
            ..Settings::default()
        };
        let t = store.set_pomodoro_settings(&s).unwrap();
        assert_eq!(t.length, 50 * 60);
        assert_eq!(store.pomodoro_settings().unwrap(), s);
        let bad = Settings { work: 0, ..s };
        assert!(store.set_pomodoro_settings(&bad).is_err());
        assert_eq!(store.pomodoro_settings().unwrap(), s);
    }

    #[test]
    fn 作業の記録はエクスポートして読み込むと戻る() {
        let mut store = Store::open_in_memory().unwrap();
        let task = store.create_task(None, &draft("資料を作る")).unwrap();
        store
            .pomodoro(
                Op::Start {
                    task: Some(Some(task.id.clone())),
                },
                0,
            )
            .unwrap();
        store.pomodoro(Op::SetTask(None), 10 * MIN).unwrap();
        store.pomodoro(Op::Stop, 15 * MIN).unwrap();
        let before = store.work_logs(None, None, 10).unwrap();
        assert_eq!(before.len(), 2);
        let mut zip = std::io::Cursor::new(Vec::new());
        crate::archive::export(&store, &mut zip).unwrap();
        let mut other = Store::open_in_memory().unwrap();
        zip.set_position(0);
        crate::archive::import(&mut other, zip).unwrap();
        assert_eq!(other.work_logs(None, None, 10).unwrap(), before);
        assert_eq!(other.work_total(&task.id).unwrap(), (1, 10 * 60));
    }
}
