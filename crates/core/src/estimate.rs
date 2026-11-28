//! 予想工数（タスクにかかると見込む時間）と、所要時間（着手から完了までの経過）の見せ方。
//!
//! 予想工数は月・日・時間・分を組み合わせて持ち、書いたとおりの単位で見せる（「90分」は「90分」のまま）。
//! 保存は ISO 8601 の期間（`P1M2DT3H30M`）。並べ替えや所要時間との比べ合いでは、1 か月 = 30 日、
//! 1 日 = 24 時間として分に直す（所要時間は時計の経過なので、同じ数え方にそろえる）。
//!
//! iCalendar では RFC 10051 の `ESTIMATED-DURATION` に当たる。値の型は RFC 5545 の DURATION で月がないので、
//! 月は 30 日にして書く（[`Estimate::to_ical_duration`]）。
//!
//! ポモドーロで記録する「作業時間」（実際に手を動かした時間の合計）とは別のもの。

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

/// 1 つの単位に入れられる数の上限。
pub const MAX_PART: u32 = 999;

/// 1 日・1 か月を分にしたもの（並べ替えと比べ合いで使う）。
const DAY_MINUTES: i64 = 24 * 60;
const MONTH_MINUTES: i64 = 30 * DAY_MINUTES;

/// 予想工数。全部 0 なら「なし」と同じ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Estimate {
    pub months: u32,
    pub days: u32,
    pub hours: u32,
    pub minutes: u32,
}

/// 単位。月・日・時間・分の順。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Month,
    Day,
    Hour,
    Minute,
}

impl Unit {
    pub const ALL: [Unit; 4] = [Unit::Month, Unit::Day, Unit::Hour, Unit::Minute];

    pub fn label(self) -> &'static str {
        match self {
            Unit::Month => "か月",
            Unit::Day => "日",
            Unit::Hour => "時間",
            Unit::Minute => "分",
        }
    }

    /// 要素の名前（テストで押す所）。
    pub fn key(self) -> &'static str {
        match self {
            Unit::Month => "months",
            Unit::Day => "days",
            Unit::Hour => "hours",
            Unit::Minute => "minutes",
        }
    }

    /// ＋・−で動かす幅。
    pub fn step(self) -> u32 {
        match self {
            Unit::Minute => 15,
            _ => 1,
        }
    }
}

impl Estimate {
    pub const fn new(months: u32, days: u32, hours: u32, minutes: u32) -> Self {
        Estimate {
            months,
            days,
            hours,
            minutes,
        }
    }

    pub fn is_zero(&self) -> bool {
        *self == Estimate::default()
    }

    /// 0 なら `None`。
    pub fn non_zero(self) -> Option<Self> {
        (!self.is_zero()).then_some(self)
    }

    pub fn get(&self, unit: Unit) -> u32 {
        match unit {
            Unit::Month => self.months,
            Unit::Day => self.days,
            Unit::Hour => self.hours,
            Unit::Minute => self.minutes,
        }
    }

    /// その単位の数を変えたもの。上限で止める。
    pub fn with(mut self, unit: Unit, value: u32) -> Self {
        let value = value.min(MAX_PART);
        match unit {
            Unit::Month => self.months = value,
            Unit::Day => self.days = value,
            Unit::Hour => self.hours = value,
            Unit::Minute => self.minutes = value,
        }
        self
    }

    /// その単位を `step` 回ぶん増やす・減らす（0 より下と上限より上には行かない）。
    pub fn nudge(self, unit: Unit, step: i32) -> Self {
        let now = self.get(unit) as i64;
        let next = (now + step as i64 * unit.step() as i64).clamp(0, MAX_PART as i64);
        self.with(unit, next as u32)
    }

    /// 分に直したもの（1 か月 = 30 日、1 日 = 24 時間）。
    pub fn total_minutes(&self) -> i64 {
        self.months as i64 * MONTH_MINUTES
            + self.days as i64 * DAY_MINUTES
            + self.hours as i64 * 60
            + self.minutes as i64
    }

    /// ミリ秒に直したもの。所要時間と比べるのに使う。
    pub fn total_ms(&self) -> i64 {
        self.total_minutes() * 60_000
    }

    /// 「1か月2日3時間30分」。0 の単位は書かない。全部 0 なら空。
    pub fn label(&self) -> String {
        Unit::ALL
            .into_iter()
            .filter(|&u| self.get(u) > 0)
            .map(|u| format!("{}{}", self.get(u), u.label()))
            .collect()
    }

    /// ISO 8601 の期間（`P1M2DT3H30M`）。全部 0 なら `PT0M`。
    pub fn to_iso(&self) -> String {
        let mut out = String::from("P");
        if self.months > 0 {
            out.push_str(&format!("{}M", self.months));
        }
        if self.days > 0 {
            out.push_str(&format!("{}D", self.days));
        }
        if self.hours > 0 || self.minutes > 0 || out == "P" {
            out.push('T');
            if self.hours > 0 {
                out.push_str(&format!("{}H", self.hours));
            }
            if self.minutes > 0 || self.hours == 0 {
                out.push_str(&format!("{}M", self.minutes));
            }
        }
        out
    }

    /// ISO 8601 の期間を読む。年・秒・小数は読まない（`P1W` は 7 日）。
    pub fn from_iso(text: &str) -> Option<Self> {
        let rest = text.trim().strip_prefix(['P', 'p'])?;
        let (date, time) = match rest.split_once(['T', 't']) {
            Some((date, time)) => (date, Some(time)),
            None => (rest, None),
        };
        let mut out = Estimate::default();
        let mut any = false;
        for (n, unit) in iso_parts(date)? {
            any = true;
            match unit {
                'M' => out.months = add(out.months, n)?,
                'W' => out.days = add(out.days, n.checked_mul(7)?)?,
                'D' => out.days = add(out.days, n)?,
                _ => return None,
            }
        }
        if let Some(time) = time {
            let parts = iso_parts(time)?;
            if parts.is_empty() {
                return None;
            }
            for (n, unit) in parts {
                any = true;
                match unit {
                    'H' => out.hours = add(out.hours, n)?,
                    'M' => out.minutes = add(out.minutes, n)?,
                    // 秒は分に切り上げる
                    'S' => out.minutes = add(out.minutes, n.div_ceil(60))?,
                    _ => return None,
                }
            }
        }
        any.then_some(out)
    }

    /// iCalendar（RFC 5545 の DURATION）の形。月は 30 日にする。全部 0 なら `PT0M`。
    pub fn to_ical_duration(&self) -> String {
        Estimate {
            months: 0,
            days: 0,
            ..*self
        }
        .with_days(self.months as u64 * 30 + self.days as u64)
    }

    fn with_days(&self, days: u64) -> String {
        let mut out = String::from("P");
        if days > 0 {
            out.push_str(&format!("{days}D"));
        }
        if self.hours > 0 || self.minutes > 0 || days == 0 {
            out.push('T');
            if self.hours > 0 {
                out.push_str(&format!("{}H", self.hours));
            }
            if self.minutes > 0 || self.hours == 0 {
                out.push_str(&format!("{}M", self.minutes));
            }
        }
        out
    }

    /// iCalendar の DURATION を読む（`P2W`・`P1DT3H`・`-` 付きは読まない）。
    pub fn from_ical_duration(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix('+').unwrap_or(text);
        if text.contains('M') && !text.contains('T') {
            // DURATION に月はない
            return None;
        }
        Self::from_iso(text)
    }
}

/// `1M2D` のような並びを（数, 単位）に分ける。
fn iso_parts(text: &str) -> Option<Vec<(u32, char)>> {
    let mut out = Vec::new();
    let mut number = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            number.push(c);
        } else {
            if number.is_empty() {
                return None;
            }
            out.push((number.parse().ok()?, c.to_ascii_uppercase()));
            number.clear();
        }
    }
    number.is_empty().then_some(out)
}

fn add(a: u32, b: u32) -> Option<u32> {
    a.checked_add(b).filter(|v| *v <= MAX_PART)
}

impl std::fmt::Display for Estimate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label())
    }
}

impl Serialize for Estimate {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_iso())
    }
}

impl<'de> Deserialize<'de> for Estimate {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        parse(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("予想工数が読めません: {text}")))
    }
}

/// 入力を読んだ結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    /// 空（なしにする）
    Empty,
    Value(Estimate),
    Unreadable,
}

/// 入力を読む。空・0 は [`Reading::Empty`]。
pub fn read(input: &str) -> Reading {
    if input.trim().is_empty() {
        return Reading::Empty;
    }
    match parse(input) {
        Some(e) if e.is_zero() => Reading::Empty,
        Some(e) => Reading::Value(e),
        None => Reading::Unreadable,
    }
}

/// 単位の書き方。長いものから先に見る。
const UNITS: &[(&str, Unit, u32)] = &[
    ("ヶ月", Unit::Month, 1),
    ("ケ月", Unit::Month, 1),
    ("カ月", Unit::Month, 1),
    ("ヵ月", Unit::Month, 1),
    ("か月", Unit::Month, 1),
    ("箇月", Unit::Month, 1),
    ("months", Unit::Month, 1),
    ("month", Unit::Month, 1),
    ("mo", Unit::Month, 1),
    ("月", Unit::Month, 1),
    ("週間", Unit::Day, 7),
    ("週", Unit::Day, 7),
    ("weeks", Unit::Day, 7),
    ("week", Unit::Day, 7),
    ("wk", Unit::Day, 7),
    ("w", Unit::Day, 7),
    ("日間", Unit::Day, 1),
    ("日", Unit::Day, 1),
    ("days", Unit::Day, 1),
    ("day", Unit::Day, 1),
    ("d", Unit::Day, 1),
    ("時間", Unit::Hour, 1),
    ("時", Unit::Hour, 1),
    ("hours", Unit::Hour, 1),
    ("hour", Unit::Hour, 1),
    ("hrs", Unit::Hour, 1),
    ("hr", Unit::Hour, 1),
    ("h", Unit::Hour, 1),
    ("分間", Unit::Minute, 1),
    ("分", Unit::Minute, 1),
    ("minutes", Unit::Minute, 1),
    ("minute", Unit::Minute, 1),
    ("mins", Unit::Minute, 1),
    ("min", Unit::Minute, 1),
    ("m", Unit::Minute, 1),
];

/// いろいろな書き方を読む。「1日3時間」「1d3h」「90分」「1.5時間」「2時間半」「1:30」「2週間」「P1DT3H」。
/// 単位のない数は時間。小数は下の単位に回す（1.5 日 = 1 日 12 時間、0.5 か月 = 15 日）。読めなければ `None`。
pub fn parse(input: &str) -> Option<Estimate> {
    let text: String = input.nfkc().collect::<String>().to_lowercase();
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.starts_with('p') {
        return Estimate::from_iso(text);
    }
    // 1:30 は 1 時間 30 分
    if let Some((h, m)) = text.split_once(':') {
        let (h, m) = (h.trim(), m.trim());
        if !h.is_empty() && h.chars().all(|c| c.is_ascii_digit()) && m.len() == 2 {
            let (h, m) = (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?);
            return (h <= MAX_PART && m < 60).then_some(Estimate::new(0, 0, h, m));
        }
        return None;
    }
    // 分ごとに足していく（小数を下の単位に回すため）。最後に単位ごとへ戻す
    let mut parts = [0f64; 4];
    let mut rest = text;
    let mut any = false;
    let skip = |s: &str| {
        s.trim_start_matches([' ', '　', ',', '、', '+', 'と', '\t'])
            .to_owned()
    };
    let mut owned = skip(rest);
    rest = &owned;
    while !rest.is_empty() {
        let digits: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if digits.is_empty() || digits.chars().filter(|c| *c == '.').count() > 1 {
            return None;
        }
        let value: f64 = digits.parse().ok()?;
        let after = rest[digits.len()..].trim_start();
        let found = UNITS.iter().find(|(word, _, _)| after.starts_with(word));
        let (unit, times, after) = match found {
            Some((word, unit, times)) => (*unit, *times, &after[word.len()..]),
            // 単位のない数は時間。後ろにまだ何かあれば読めない
            None if after.trim().is_empty() => (Unit::Hour, 1, ""),
            None => return None,
        };
        // 「2時間半」の半は、その単位の半分
        let (value, after) = match after.strip_prefix('半') {
            Some(after) => (value + 0.5, after),
            None => (value, after),
        };
        let index = Unit::ALL.iter().position(|u| *u == unit).unwrap_or(1);
        parts[index] += value * times as f64;
        any = true;
        owned = skip(after);
        rest = &owned;
    }
    if !any {
        return None;
    }
    // 小数を下の単位へ回す（月→日は 30、日→時間は 24、時間→分は 60）
    const CARRY: [f64; 3] = [30., 24., 60.];
    let mut out = [0u32; 4];
    for i in 0..4 {
        let whole = if i == 3 {
            parts[i].round()
        } else {
            parts[i].trunc()
        };
        if i < 3 {
            parts[i + 1] += (parts[i] - whole) * CARRY[i];
        }
        if !(0.0..=MAX_PART as f64).contains(&whole) {
            return None;
        }
        out[i] = whole as u32;
    }
    Some(Estimate::new(out[0], out[1], out[2], out[3]))
}

/// よく使う予想工数（メニューのチップ）。
pub fn common() -> [Estimate; 10] {
    [
        Estimate::new(0, 0, 0, 15),
        Estimate::new(0, 0, 0, 30),
        Estimate::new(0, 0, 1, 0),
        Estimate::new(0, 0, 2, 0),
        Estimate::new(0, 0, 4, 0),
        Estimate::new(0, 1, 0, 0),
        Estimate::new(0, 2, 0, 0),
        Estimate::new(0, 3, 0, 0),
        Estimate::new(0, 7, 0, 0),
        Estimate::new(1, 0, 0, 0),
    ]
}

/// 経過したミリ秒を「3分」「2時間5分」「1日3時間」「1か月5日」の形にする（1 か月 = 30 日）。
/// 上の 2 つの単位だけを出す。1 分に満たなければ「1分未満」。
pub fn elapsed_label(ms: i64) -> String {
    let minutes = ms.max(0) / 60_000;
    if minutes == 0 {
        return "1分未満".into();
    }
    let both = |a: i64, ua: &str, b: i64, ub: &str| {
        if b == 0 {
            format!("{a}{ua}")
        } else {
            format!("{a}{ua}{b}{ub}")
        }
    };
    if minutes < 60 {
        format!("{minutes}分")
    } else if minutes < DAY_MINUTES {
        both(minutes / 60, "時間", minutes % 60, "分")
    } else if minutes < MONTH_MINUTES {
        both(
            minutes / DAY_MINUTES,
            "日",
            minutes % DAY_MINUTES / 60,
            "時間",
        )
    } else {
        both(
            minutes / MONTH_MINUTES,
            "か月",
            minutes % MONTH_MINUTES / DAY_MINUTES,
            "日",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(months: u32, days: u32, hours: u32, minutes: u32) -> Option<Estimate> {
        Some(Estimate::new(months, days, hours, minutes))
    }

    #[test]
    fn 日本語と英語の書き方を読む() {
        assert_eq!(parse("1日3時間"), e(0, 1, 3, 0));
        assert_eq!(parse("1日 3時間"), e(0, 1, 3, 0));
        assert_eq!(parse("1d3h"), e(0, 1, 3, 0));
        assert_eq!(parse("1D 3H 20M"), e(0, 1, 3, 20));
        assert_eq!(parse("90分"), e(0, 0, 0, 90));
        assert_eq!(parse("90min"), e(0, 0, 0, 90));
        assert_eq!(parse("2か月"), e(2, 0, 0, 0));
        assert_eq!(parse("2ヶ月10日"), e(2, 10, 0, 0));
        assert_eq!(parse("1カ月と3日"), e(1, 3, 0, 0));
        assert_eq!(parse("3mo"), e(3, 0, 0, 0));
        assert_eq!(parse("2週間"), e(0, 14, 0, 0));
        assert_eq!(parse("1w2d"), e(0, 9, 0, 0));
        assert_eq!(parse("１日３時間"), e(0, 1, 3, 0));
        assert_eq!(parse("1か月2日3時間30分"), e(1, 2, 3, 30));
        assert_eq!(parse("3時間、30分"), e(0, 0, 3, 30));
        assert_eq!(parse("1h+30m"), e(0, 0, 1, 30));
    }

    #[test]
    fn 小数と半は下の単位へ回す() {
        assert_eq!(parse("1.5時間"), e(0, 0, 1, 30));
        assert_eq!(parse("1.5日"), e(0, 1, 12, 0));
        assert_eq!(parse("0.5か月"), e(0, 15, 0, 0));
        assert_eq!(parse("2時間半"), e(0, 0, 2, 30));
        assert_eq!(parse("1日半"), e(0, 1, 12, 0));
        assert_eq!(parse("0.25h"), e(0, 0, 0, 15));
    }

    #[test]
    fn 単位のない数は時間で_コロンは時と分() {
        assert_eq!(parse("3"), e(0, 0, 3, 0));
        assert_eq!(parse("1.5"), e(0, 0, 1, 30));
        assert_eq!(parse("1:30"), e(0, 0, 1, 30));
        assert_eq!(parse("10:05"), e(0, 0, 10, 5));
        assert_eq!(parse("1:3"), None);
        assert_eq!(parse("1:75"), None);
    }

    #[test]
    fn 読めないものは読まない() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("あした"), None);
        assert_eq!(parse("3時間くらい"), None);
        assert_eq!(parse("日"), None);
        assert_eq!(parse("1..5h"), None);
        assert_eq!(parse("1000日"), None);
        assert_eq!(parse("3 4"), None);
        assert_eq!(read("0分"), Reading::Empty);
        assert_eq!(read("  "), Reading::Empty);
        assert_eq!(read("x"), Reading::Unreadable);
        assert_eq!(read("2h"), Reading::Value(Estimate::new(0, 0, 2, 0)));
    }

    #[test]
    fn 書いた単位のまま見せる() {
        assert_eq!(Estimate::new(0, 0, 0, 90).label(), "90分");
        assert_eq!(Estimate::new(1, 2, 3, 30).label(), "1か月2日3時間30分");
        assert_eq!(Estimate::new(0, 1, 3, 0).label(), "1日3時間");
        assert_eq!(Estimate::default().label(), "");
    }

    #[test]
    fn iso_の期間で往復する() {
        for est in [
            Estimate::new(1, 2, 3, 30),
            Estimate::new(0, 0, 0, 90),
            Estimate::new(0, 1, 0, 0),
            Estimate::new(0, 0, 5, 0),
            Estimate::new(2, 0, 0, 0),
            Estimate::default(),
        ] {
            let iso = est.to_iso();
            assert_eq!(Estimate::from_iso(&iso), Some(est), "{iso}");
            assert_eq!(parse(&iso), Some(est), "{iso}");
        }
        assert_eq!(Estimate::new(1, 2, 3, 30).to_iso(), "P1M2DT3H30M");
        assert_eq!(Estimate::new(0, 0, 0, 90).to_iso(), "PT90M");
        assert_eq!(Estimate::new(0, 3, 0, 0).to_iso(), "P3D");
        assert_eq!(Estimate::default().to_iso(), "PT0M");
        assert_eq!(Estimate::from_iso("P1W"), e(0, 7, 0, 0));
        assert_eq!(Estimate::from_iso("PT90S"), e(0, 0, 0, 2));
        assert_eq!(Estimate::from_iso("P"), None);
        assert_eq!(Estimate::from_iso("PT"), None);
        assert_eq!(Estimate::from_iso("P1Y"), None);
        assert_eq!(Estimate::from_iso("P1.5D"), None);
        let json = serde_json::to_string(&Estimate::new(0, 1, 3, 0)).unwrap();
        assert_eq!(json, "\"P1DT3H\"");
        assert_eq!(
            serde_json::from_str::<Estimate>(&json).unwrap(),
            Estimate::new(0, 1, 3, 0)
        );
        assert!(serde_json::from_str::<Estimate>("\"x\"").is_err());
    }

    #[test]
    fn icalendar_では月を三十日にする() {
        assert_eq!(Estimate::new(1, 2, 3, 30).to_ical_duration(), "P32DT3H30M");
        assert_eq!(Estimate::new(0, 0, 0, 45).to_ical_duration(), "PT45M");
        assert_eq!(Estimate::new(0, 2, 0, 0).to_ical_duration(), "P2D");
        assert_eq!(Estimate::default().to_ical_duration(), "PT0M");
        assert_eq!(Estimate::from_ical_duration("P2W"), e(0, 14, 0, 0));
        assert_eq!(Estimate::from_ical_duration("P1DT3H"), e(0, 1, 3, 0));
        assert_eq!(Estimate::from_ical_duration("+PT15M"), e(0, 0, 0, 15));
        assert_eq!(Estimate::from_ical_duration("-PT15M"), None);
        assert_eq!(Estimate::from_ical_duration("P1M"), None);
    }

    #[test]
    fn 分に直すと一か月は三十日() {
        assert_eq!(Estimate::new(0, 0, 1, 30).total_minutes(), 90);
        assert_eq!(Estimate::new(0, 1, 0, 0).total_minutes(), 1440);
        assert_eq!(Estimate::new(1, 0, 0, 0).total_minutes(), 30 * 1440);
        assert_eq!(Estimate::new(0, 0, 0, 1).total_ms(), 60_000);
    }

    #[test]
    fn 単位ごとに増やし減らす() {
        let est = Estimate::default();
        assert_eq!(est.nudge(Unit::Minute, 1), Estimate::new(0, 0, 0, 15));
        assert_eq!(est.nudge(Unit::Hour, 2), Estimate::new(0, 0, 2, 0));
        assert_eq!(est.nudge(Unit::Day, -1), est);
        assert_eq!(
            Estimate::new(MAX_PART, 0, 0, 0)
                .nudge(Unit::Month, 1)
                .months,
            MAX_PART
        );
        assert_eq!(est.with(Unit::Day, 5000).days, MAX_PART);
        assert_eq!(Estimate::new(0, 3, 0, 0).get(Unit::Day), 3);
        assert_eq!(Estimate::default().non_zero(), None);
    }

    #[test]
    fn 経過は上の二つの単位で見せる() {
        let m = 60_000;
        assert_eq!(elapsed_label(30_000), "1分未満");
        assert_eq!(elapsed_label(-5), "1分未満");
        assert_eq!(elapsed_label(3 * m), "3分");
        assert_eq!(elapsed_label(60 * m), "1時間");
        assert_eq!(elapsed_label(125 * m), "2時間5分");
        assert_eq!(elapsed_label((24 * 60 + 3 * 60 + 10) * m), "1日3時間");
        assert_eq!(elapsed_label(2 * 24 * 60 * m), "2日");
        assert_eq!(elapsed_label(35 * 24 * 60 * m), "1か月5日");
        assert_eq!(elapsed_label(60 * 24 * 60 * m), "2か月");
    }

    #[test]
    fn よく使う予想工数は短い順() {
        let list = common();
        assert!(list
            .windows(2)
            .all(|w| w[0].total_minutes() < w[1].total_minutes()));
    }
}
