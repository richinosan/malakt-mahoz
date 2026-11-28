//! 単純なタイマー。カウントダウン（決めた時間から 0 まで）とストップウォッチ（0 から数える）。
//!
//! 状態は設定（[`STATE_KEY`]）に JSON で置く。アプリの画面、AI bot、MCP のどこから動かしても
//! 同じ状態を読み書きし、アプリを閉じても続きから戻る。カウントダウンの時刻が来たら、次に読んだときに
//! 止めて（[`Timer::advance`]）、通知（[`crate::notice`] の [`NoticeKind::Timer`]）を 1 回作る。
//!
//! 設定の表は元に戻す相手にしないので、タイマーの操作も元に戻す段にならない。

use serde::{Deserialize, Serialize};

use crate::notice::{self, NoticeKind};
use crate::{Error, Result, Store};

/// 状態を置く設定の名前。
pub const STATE_KEY: &str = "timer.state";
/// カウントダウンの長さの範囲（秒）。上は 99:59:59。
pub const MIN_SECS: i64 = 1;
pub const MAX_SECS: i64 = 100 * 3600 - 1;
/// 最初のカウントダウンの長さ（秒）。
pub const DEFAULT_SECS: i64 = 5 * 60;
/// ストップウォッチで数える上限（ミリ秒）。
pub const STOPWATCH_MAX_MS: i64 = (MAX_SECS + 1) * 1000 - 1;

/// どちらで測るか。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Countdown,
    Stopwatch,
}

impl Mode {
    pub const ALL: [Mode; 2] = [Mode::Countdown, Mode::Stopwatch];

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Countdown => "countdown",
            Mode::Stopwatch => "stopwatch",
        }
    }

    pub fn parse(text: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.as_str() == text)
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Countdown => "カウントダウン",
            Mode::Stopwatch => "ストップウォッチ",
        }
    }
}

/// 今どうなっているか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Run {
    /// 始める前。
    Ready,
    Running,
    Paused,
    /// カウントダウンが 0 になって止まった。
    Done,
}

impl Run {
    pub fn as_str(self) -> &'static str {
        match self {
            Run::Ready => "ready",
            Run::Running => "running",
            Run::Paused => "paused",
            Run::Done => "done",
        }
    }
}

/// タイマーの状態。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Timer {
    pub mode: Mode,
    /// カウントダウンの長さ（秒）。
    pub length: i64,
    /// 動いているなら、動かし始めた時刻（UNIX ミリ秒）。
    pub since: Option<i64>,
    /// `since` より前に進んだ分（ミリ秒）。
    pub elapsed: i64,
    /// カウントダウンが 0 になって止まっている。
    pub done: bool,
    /// 時刻が来て終わったカウントダウンの数。画面はこれが増えたら知らせる。
    pub ended: u64,
    /// 最後に終わった時刻（UNIX ミリ秒）。
    pub ended_at: Option<i64>,
}

impl Default for Timer {
    fn default() -> Self {
        Timer {
            mode: Mode::Countdown,
            length: DEFAULT_SECS,
            since: None,
            elapsed: 0,
            done: false,
            ended: 0,
            ended_at: None,
        }
    }
}

/// タイマーへの操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// 始める。一時停止していれば続きから、終わっていれば最初から。
    Start,
    Pause,
    /// 止めて、最初に戻す。
    Reset,
    /// カウントダウンの長さ（秒）を決めて、最初に戻す。
    SetLength(i64),
    /// カウントダウンの長さに足す（秒）。動いていれば、そのまま続く。
    Add(i64),
    /// カウントダウンとストップウォッチを切り替える。最初に戻す。
    SetMode(Mode),
}

impl Timer {
    pub fn run(&self) -> Run {
        if self.since.is_some() {
            Run::Running
        } else if self.done {
            Run::Done
        } else if self.elapsed > 0 {
            Run::Paused
        } else {
            Run::Ready
        }
    }

    pub fn running(&self) -> bool {
        self.since.is_some()
    }

    fn cap(&self) -> i64 {
        match self.mode {
            Mode::Countdown => self.length * 1000,
            Mode::Stopwatch => STOPWATCH_MAX_MS,
        }
    }

    /// 進んだ時間（ミリ秒）。
    pub fn elapsed_ms(&self, now: i64) -> i64 {
        let running = self.since.map_or(0, |since| (now - since).max(0));
        (self.elapsed + running).min(self.cap())
    }

    /// カウントダウンの残り時間（ミリ秒）。
    pub fn remaining_ms(&self, now: i64) -> i64 {
        (self.length * 1000 - self.elapsed_ms(now)).max(0)
    }

    /// 画面に出す時間（ミリ秒）。カウントダウンは残り、ストップウォッチは進んだ時間。
    pub fn shown_ms(&self, now: i64) -> i64 {
        match self.mode {
            Mode::Countdown => self.remaining_ms(now),
            Mode::Stopwatch => self.elapsed_ms(now),
        }
    }

    /// 画面に出す時間の文字（「04:59」「1:05:00」）。
    pub fn label(&self, now: i64) -> String {
        match self.mode {
            Mode::Countdown => crate::pomodoro::clock_label(self.remaining_ms(now)),
            Mode::Stopwatch => stopwatch_label(self.elapsed_ms(now)),
        }
    }

    /// 輪に出す割合（0〜1）。カウントダウンは進んだ割合、ストップウォッチは 1 分で 1 周。
    pub fn progress(&self, now: i64) -> f32 {
        let elapsed = self.elapsed_ms(now);
        match self.mode {
            Mode::Countdown if self.length > 0 => {
                (elapsed as f64 / (self.length * 1000) as f64).clamp(0., 1.) as f32
            }
            Mode::Countdown => 0.,
            Mode::Stopwatch => (elapsed % 60_000) as f32 / 60_000.,
        }
    }

    /// カウントダウンの時刻が来ていたら止める。止めたら `true`。
    pub fn advance(&mut self, now: i64) -> bool {
        let Some(since) = self.since else {
            return false;
        };
        let end_at = since + (self.cap() - self.elapsed).max(0);
        if now < end_at {
            return false;
        }
        self.since = None;
        self.elapsed = self.cap();
        if self.mode == Mode::Countdown {
            self.done = true;
            self.ended += 1;
            self.ended_at = Some(end_at);
        }
        true
    }

    pub fn start(&mut self, now: i64) {
        if self.done || (self.mode == Mode::Stopwatch && self.elapsed >= self.cap()) {
            self.reset();
        }
        if self.since.is_none() {
            self.since = Some(now);
        }
    }

    pub fn pause(&mut self, now: i64) {
        if let Some(since) = self.since.take() {
            self.elapsed = (self.elapsed + (now - since).max(0)).min(self.cap());
        }
    }

    pub fn reset(&mut self) {
        self.since = None;
        self.elapsed = 0;
        self.done = false;
    }

    /// 操作する。`now` は今の時刻（UNIX ミリ秒）。
    pub fn apply(&mut self, op: Op, now: i64) -> Result<()> {
        match op {
            Op::Start => self.start(now),
            Op::Pause => self.pause(now),
            Op::Reset => self.reset(),
            Op::SetLength(secs) => {
                check_length(secs)?;
                self.length = secs;
                self.reset();
            }
            Op::Add(secs) => {
                if self.mode != Mode::Countdown {
                    return Err(Error::Invalid(
                        "時間を足せるのはカウントダウンだけです".into(),
                    ));
                }
                if secs <= 0 {
                    return Err(Error::Invalid(format!(
                        "足す時間は 1 秒以上です（{secs} 秒）"
                    )));
                }
                let length = (self.length + secs).min(MAX_SECS);
                if self.done {
                    // 終わったあとに足したら、足した分を残して一時停止にする
                    self.done = false;
                    self.elapsed = self.length * 1000;
                }
                // 動いていれば、そのまま数え続ける
                self.length = length;
            }
            Op::SetMode(mode) => {
                if self.mode != mode {
                    self.mode = mode;
                    self.reset();
                }
            }
        }
        Ok(())
    }
}

/// カウントダウンの長さを確かめる。
pub fn check_length(secs: i64) -> Result<()> {
    if (MIN_SECS..=MAX_SECS).contains(&secs) {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "タイマーの長さは 1 秒から 99 時間 59 分 59 秒までです（{secs} 秒）"
        )))
    }
}

/// ストップウォッチの時間を「04:59」（1 時間以上は「1:05:00」）の形で書く。秒は切り捨てる。
pub fn stopwatch_label(ms: i64) -> String {
    let total = ms.max(0) / 1000;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// 長さ（秒）を「1 時間 5 分 30 秒」のように書く。
pub fn length_label(secs: i64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    let mut parts = Vec::new();
    if h > 0 {
        parts.push(format!("{h} 時間"));
    }
    if m > 0 {
        parts.push(format!("{m} 分"));
    }
    if s > 0 || parts.is_empty() {
        parts.push(format!("{s} 秒"));
    }
    parts.join(" ")
}

/// 入力した長さを読む。数字だけなら分、「1:30」は分と秒、「1:00:00」は時・分・秒。
/// 「1時間30分」「90秒」「5分」のようにも書ける。全角の数字とコロンも読む。
pub fn parse_length(text: &str) -> Option<i64> {
    let text: String = text
        .trim()
        .chars()
        .map(|c| match c {
            '０'..='９' => char::from_u32(c as u32 - '０' as u32 + '0' as u32).unwrap_or(c),
            '：' => ':',
            c => c,
        })
        .filter(|c| !c.is_whitespace())
        .collect();
    if text.is_empty() {
        return None;
    }
    let number = |part: &str| -> Option<i64> {
        (!part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
            .then(|| part.parse::<i64>().ok())
            .flatten()
    };
    if text.contains(':') {
        let parts: Vec<&str> = text.split(':').collect();
        let values: Option<Vec<i64>> = parts.iter().map(|p| number(p)).collect();
        let values = values?;
        return match values.as_slice() {
            [m, s] if *s < 60 => Some(m * 60 + s),
            [h, m, s] if *m < 60 && *s < 60 => Some(h * 3600 + m * 60 + s),
            _ => None,
        };
    }
    if let Some(minutes) = number(&text) {
        return minutes.checked_mul(60);
    }
    // 「1時間30分」「90秒」
    let mut total = 0i64;
    let mut digits = String::new();
    let mut rest = text.as_str();
    let mut any = false;
    while let Some(c) = rest.chars().next() {
        if c.is_ascii_digit() {
            digits.push(c);
            rest = &rest[c.len_utf8()..];
            continue;
        }
        let (unit, len) = if rest.starts_with("時間") {
            (3600, "時間".len())
        } else if rest.starts_with('分') {
            (60, '分'.len_utf8())
        } else if rest.starts_with('秒') {
            (1, '秒'.len_utf8())
        } else {
            return None;
        };
        let value = number(&digits)?;
        total = total.checked_add(value.checked_mul(unit)?)?;
        digits.clear();
        rest = &rest[len..];
        any = true;
    }
    (any && digits.is_empty()).then_some(total)
}

impl Store {
    /// 保存してあるタイマー（時刻が来たカウントダウンは止めていない）。
    pub fn timer_state(&self) -> Result<Timer> {
        match self.setting(STATE_KEY)? {
            Some(text) => Ok(serde_json::from_str(&text)?),
            None => Ok(Timer::default()),
        }
    }

    /// 今のタイマー。時刻が来たカウントダウンは止めて、通知を作る。
    pub fn timer_now(&mut self, now: i64) -> Result<Timer> {
        self.timer_apply(None, now)
    }

    /// タイマーを操作する。先に時刻が来たカウントダウンを止める。
    pub fn timer(&mut self, op: Op, now: i64) -> Result<Timer> {
        self.timer_apply(Some(op), now)
    }

    fn timer_apply(&mut self, op: Option<Op>, now: i64) -> Result<Timer> {
        let before = self.timer_state()?;
        let mut timer = before.clone();
        timer.advance(now);
        if let Some(op) = op {
            timer.apply(op, now)?;
        }
        if timer == before {
            return Ok(timer);
        }
        let tx = self.conn.transaction()?;
        if timer.ended > before.ended {
            let at = timer.ended_at.unwrap_or(now);
            notice::insert_plain(
                &tx,
                NoticeKind::Timer,
                "タイマーが終わりました",
                &length_label(before.length),
                &format!("timer:{at}:{}", timer.ended),
                at,
            )?;
        }
        tx.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![STATE_KEY, serde_json::to_string(&timer)?],
        )?;
        tx.commit()?;
        Ok(timer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_down_and_stops_at_zero() {
        let mut t = Timer::default();
        t.apply(Op::SetLength(90), 0).unwrap();
        assert_eq!((t.run(), t.label(0)), (Run::Ready, "01:30".into()));
        t.apply(Op::Start, 0).unwrap();
        assert_eq!(t.label(500), "01:30");
        assert_eq!(t.label(1_000), "01:29");
        t.apply(Op::Pause, 30_000).unwrap();
        assert_eq!((t.run(), t.label(99_000)), (Run::Paused, "01:00".into()));
        t.apply(Op::Start, 100_000).unwrap();
        assert!(!t.advance(159_999));
        assert!(t.advance(170_000));
        assert_eq!(
            (t.run(), t.ended, t.ended_at),
            (Run::Done, 1, Some(160_000))
        );
        assert_eq!(t.label(200_000), "00:00");
        // もう一度始めると、最初から
        t.apply(Op::Start, 200_000).unwrap();
        assert_eq!(t.label(200_000), "01:30");
        t.apply(Op::Reset, 210_000).unwrap();
        assert_eq!((t.run(), t.label(210_000)), (Run::Ready, "01:30".into()));
    }

    #[test]
    fn adds_time_while_running_and_after_the_end() {
        let mut t = Timer::default();
        t.apply(Op::SetLength(60), 0).unwrap();
        t.apply(Op::Start, 0).unwrap();
        t.apply(Op::Add(60), 30_000).unwrap();
        // 動いている間に足しても、止まらずに数え続ける
        assert_eq!(t.run(), Run::Running);
        assert_eq!(t.label(30_000), "01:30");
        assert!(!t.advance(119_000));
        assert!(t.advance(120_000));
        // 終わったあとに足すと、足した分が残って一時停止になる
        t.apply(Op::Add(60), 130_000).unwrap();
        assert_eq!((t.run(), t.label(130_000)), (Run::Paused, "01:00".into()));
        t.apply(Op::Start, 130_000).unwrap();
        assert!(t.advance(190_000));
        assert_eq!(t.ended, 2);
    }

    #[test]
    fn the_stopwatch_counts_up_and_switching_resets() {
        let mut t = Timer::default();
        t.apply(Op::SetMode(Mode::Stopwatch), 0).unwrap();
        t.apply(Op::Start, 0).unwrap();
        assert_eq!(t.label(61_999), "01:01");
        assert!(!t.advance(3_600_000));
        assert_eq!(t.label(3_725_000), "1:02:05");
        assert!((t.progress(90_000) - 0.5).abs() < 1e-6);
        assert!(t.apply(Op::Add(60), 0).is_err());
        t.apply(Op::SetMode(Mode::Countdown), 4_000_000).unwrap();
        assert_eq!((t.run(), t.label(4_000_000)), (Run::Ready, "05:00".into()));
    }

    #[test]
    fn reads_typed_lengths() {
        assert_eq!(parse_length("5"), Some(300));
        assert_eq!(parse_length(" 1:30 "), Some(90));
        assert_eq!(parse_length("1:00:00"), Some(3600));
        assert_eq!(parse_length("１：３０"), Some(90));
        assert_eq!(parse_length("1時間30分"), Some(5400));
        assert_eq!(parse_length("90秒"), Some(90));
        assert_eq!(parse_length("2分 30秒"), Some(150));
        assert_eq!(parse_length("1:75"), None);
        assert_eq!(parse_length("abc"), None);
        assert_eq!(parse_length("5分x"), None);
        assert_eq!(parse_length(""), None);
        assert!(check_length(0).is_err());
        assert!(check_length(MAX_SECS + 1).is_err());
        assert_eq!(length_label(5400), "1 時間 30 分");
        assert_eq!(length_label(90), "1 分 30 秒");
        assert_eq!(length_label(0), "0 秒");
    }

    #[test]
    fn the_end_makes_one_notice() {
        let mut s = Store::open_in_memory().unwrap();
        s.timer(Op::SetLength(60), 0).unwrap();
        s.timer(Op::Start, 0).unwrap();
        assert_eq!(s.timer_now(59_000).unwrap().run(), Run::Running);
        assert!(s.notices(10).unwrap().is_empty());
        let t = s.timer_now(61_000).unwrap();
        assert_eq!((t.run(), t.ended), (Run::Done, 1));
        // 読み直しても、通知は 1 回だけ
        s.timer_now(62_000).unwrap();
        let notices = s.notices(10).unwrap();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].kind, NoticeKind::Timer);
        assert_eq!(notices[0].task_id, None);
        assert_eq!(notices[0].headline(), "タイマーが終わりました");
        assert_eq!(notices[0].excerpt, "1 分");
        assert_eq!(notices[0].created_at, 60_000);
        assert_eq!(s.undelivered_notices().unwrap().len(), 1);
        // 止めたあとの操作は、通知を作らない
        s.timer(Op::Reset, 70_000).unwrap();
        assert_eq!(s.notices(10).unwrap().len(), 1);
        assert!(s.timer(Op::SetLength(0), 0).is_err());
    }
}
