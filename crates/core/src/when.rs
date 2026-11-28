//! 期限や開始の入力を読む。
//!
//! 保存する形は `YYYY-MM-DD` か `YYYY-MM-DD HH:MM` の 2 つだけ。入力はそれ以外の書き方
//! （`2026/11/27`、`11/27`、`11月27日`、`27日`、`明日`、`来週金曜`、`3日後`、`14:00`、`14時`、
//! `2pm` など）も受け付け、ここで保存する形に直す。

use chrono::{Datelike, Duration, Months, NaiveDate};

/// 入力を保存する形に直す。読めなければ `None`。
///
/// - `now`: いまの日時（`YYYY-MM-DD HH:MM`。日付だけでもよい）
/// - `base`: 時刻だけを入力したときに使う日付（いまの期限など）。なければ今日
pub fn parse(input: &str, now: &str, base: Option<&str>) -> Option<String> {
    let text = normalize(input);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let today = NaiveDate::parse_from_str(crate::date_of(now), "%Y-%m-%d").ok()?;
    let join = |date: NaiveDate, time: Option<(u32, u32)>| {
        let date = date.format("%Y-%m-%d");
        match time {
            Some((h, m)) => format!("{date} {h:02}:{m:02}"),
            None => date.to_string(),
        }
    };
    if let Some(time) = time(text) {
        let date = base
            .and_then(|b| NaiveDate::parse_from_str(crate::date_of(b), "%Y-%m-%d").ok())
            .unwrap_or(today);
        return Some(join(date, Some(time)));
    }
    if let Some((date, rest)) = date(text, today) {
        let rest = rest.trim_start_matches([' ', ',', 't', 'の']);
        if rest.is_empty() {
            return Some(join(date, None));
        }
        if let Some(time) = time(rest) {
            return Some(join(date, Some(time)));
        }
    }
    // 「14:00 明日」のように、時刻を先に書いた形
    let (first, rest) = text.split_once(' ')?;
    let time = time(first)?;
    match date(rest.trim(), today)? {
        (date, "") => Some(join(date, Some(time))),
        _ => None,
    }
}

/// 時刻だけの入力（`9`、`9:30`、`21時`、`午後3時半`、`2pm` など）を、時と分にする。読めなければ `None`。
pub fn parse_time(input: &str) -> Option<(u32, u32)> {
    time(normalize(input).trim())
}

/// 入力を読めたら保存する形を、読めなければ入力のままを返す。
/// 読めなかった入力は、保存のときに「正しくありません」のエラーになる。
pub fn resolve(input: &str, now: &str, base: Option<&str>) -> String {
    parse(input, now, base).unwrap_or_else(|| input.trim().to_owned())
}

/// 全角の数字と記号を半角に、英字を小文字にそろえる。
fn normalize(input: &str) -> String {
    input
        .chars()
        .map(|c| match c {
            '０'..='９' => char::from(b'0' + (c as u32 - '０' as u32) as u8),
            '：' => ':',
            '／' => '/',
            '．' => '.',
            '＋' => '+',
            '－' | 'ー' | '‐' | '−' => '-',
            '\u{3000}' | '\t' => ' ',
            '、' | '，' => ',',
            c => c.to_ascii_lowercase(),
        })
        .collect()
}

/// 先頭の数字の並びと、その残り。
fn number(s: &str) -> Option<(u32, &str)> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if end == 0 || end > 9 {
        return None;
    }
    Some((s[..end].parse().ok()?, &s[end..]))
}

/// `words` のどれかで始まっていれば、その残り。
fn strip<'a>(s: &'a str, words: &[&str]) -> Option<&'a str> {
    words.iter().find_map(|w| s.strip_prefix(w))
}

/// 時刻。文字列の全体が時刻のときだけ読む。
fn time(s: &str) -> Option<(u32, u32)> {
    let s = s.trim();
    if matches!(s, "正午" | "noon") {
        return Some((12, 0));
    }
    // 前に付く「午前」「午後」
    let (mut half, s) = if let Some(rest) = strip(s, &["午前", "am"]) {
        (Some(false), rest.trim_start())
    } else if let Some(rest) = strip(s, &["午後", "pm"]) {
        (Some(true), rest.trim_start())
    } else {
        (None, s)
    };
    let (hour, rest) = number(s)?;
    let (minute, rest, marked) = if let Some(rest) = rest.strip_prefix(':') {
        let (minute, rest) = number(rest)?;
        // 秒は捨てる
        let rest = match rest.strip_prefix(':') {
            Some(seconds) => number(seconds)?.1,
            None => rest,
        };
        (minute, rest, true)
    } else if let Some(rest) = rest.strip_prefix('時') {
        if let Some(rest) = rest.strip_prefix('半') {
            (30, rest, true)
        } else if let Some((minute, after)) = number(rest) {
            (minute, after.strip_prefix('分')?, true)
        } else {
            (0, rest, true)
        }
    } else {
        (0, rest, false)
    };
    // 後ろに付く am / pm
    let rest = rest.trim_start();
    let rest = match rest {
        "am" | "a.m." => {
            half = Some(false);
            ""
        }
        "pm" | "p.m." => {
            half = Some(true);
            ""
        }
        rest => rest,
    };
    // 数字だけ（「14」）は時刻として読まない
    if !rest.is_empty() || (!marked && half.is_none()) {
        return None;
    }
    let hour = match half {
        Some(true) if hour < 12 => hour + 12,
        Some(false) if hour == 12 => 0,
        _ => hour,
    };
    (hour < 24 && minute < 60).then_some((hour, minute))
}

/// 曜日（月曜を 0 とする）と、その残り。`bare` なら「曜」のない 1 文字も曜日として読む。
fn weekday(s: &str, bare: bool) -> Option<(i64, &str)> {
    const LONG: [&str; 7] = [
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ];
    const SHORT: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    for (i, kanji) in ["月", "火", "水", "木", "金", "土", "日"]
        .iter()
        .enumerate()
    {
        let Some(rest) = s.strip_prefix(kanji) else {
            continue;
        };
        if let Some(rest) = rest.strip_prefix('曜') {
            return Some((i as i64, rest.strip_prefix('日').unwrap_or(rest)));
        }
        return (bare && !rest.starts_with('末')).then_some((i as i64, rest));
    }
    for names in [LONG, SHORT] {
        for (i, name) in names.iter().enumerate() {
            if let Some(rest) = s.strip_prefix(name) {
                if !rest.starts_with(|c: char| c.is_ascii_alphabetic()) {
                    return Some((i as i64, rest));
                }
            }
        }
    }
    None
}

/// その月の、ない日は月末に寄せた日付。
fn clamp(year: i32, month: u32, day: u32) -> Option<NaiveDate> {
    (1..=day.min(31))
        .rev()
        .find_map(|d| NaiveDate::from_ymd_opt(year, month, d))
}

/// 月の最後の日。
fn month_end(date: NaiveDate) -> Option<NaiveDate> {
    clamp(date.year(), date.month(), 31)
}

/// 先頭の日付と、その残り。
fn date(s: &str, today: NaiveDate) -> Option<(NaiveDate, &str)> {
    let days = |n: i64| today + Duration::days(n);
    // 決まった言葉
    for (words, offset) in [
        (&["明後日", "あさって"][..], 2),
        (&["今日", "きょう", "本日", "today"][..], 0),
        (&["明日", "あした", "あす", "tomorrow", "tmr"][..], 1),
        (&["昨日", "きのう", "yesterday"][..], -1),
    ] {
        if let Some(rest) = strip(s, words) {
            return Some((days(offset), rest));
        }
    }
    // 今週・来週・再来週（曜日が続けばその曜日、なければ同じ曜日）
    for (words, weeks) in [
        (&["再来週", "さ来週"][..], 2),
        (&["来週", "next week", "next "][..], 1),
        (&["今週", "this "][..], 0),
    ] {
        let Some(rest) = strip(s, words) else {
            continue;
        };
        let after = rest.strip_prefix('の').unwrap_or(rest).trim_start();
        let monday = days(weeks * 7 - i64::from(today.weekday().num_days_from_monday()));
        return match weekday(after, true) {
            Some((day, rest)) => Some((monday + Duration::days(day), rest)),
            // 「next 」「this 」は曜日が続くときだけ
            None if s.starts_with("next ") && !s.starts_with("next week") => None,
            None if s.starts_with("this ") => None,
            None => Some((days(weeks * 7), rest)),
        };
    }
    // 月末、今月・来月・再来月（「末」か日が続けばその日、なければ同じ日）
    if let Some(rest) = s.strip_prefix("月末") {
        return Some((month_end(today)?, rest));
    }
    for (words, months) in [(&["再来月"][..], 2), (&["来月"][..], 1), (&["今月"][..], 0)] {
        let Some(rest) = strip(s, words) else {
            continue;
        };
        let moved = today.checked_add_months(Months::new(months))?;
        let after = rest.strip_prefix('の').unwrap_or(rest);
        if let Some(rest) = after.strip_prefix('末') {
            return Some((month_end(moved)?, rest));
        }
        if let Some((day, rest)) = number(after) {
            let rest = rest.strip_prefix('日')?;
            return Some((
                NaiveDate::from_ymd_opt(moved.year(), moved.month(), day)?,
                rest,
            ));
        }
        return Some((moved, rest));
    }
    // 曜日だけ。今日を含めて、次に来るその曜日
    if let Some((day, rest)) = weekday(s, false) {
        let ahead = (day - i64::from(today.weekday().num_days_from_monday())).rem_euclid(7);
        return Some((days(ahead), rest));
    }
    // 数字で始まるもの
    let digits = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if digits == 8 {
        // 20261127
        let part = |r: std::ops::Range<usize>| s[r].parse::<u32>().ok();
        let date = NaiveDate::from_ymd_opt(part(0..4)? as i32, part(4..6)?, part(6..8)?)?;
        return Some((date, &s[8..]));
    }
    let (first, rest) = number(s)?;
    // 3日後、2週間後、1か月後
    if let Some(rest) = rest.strip_prefix("日後") {
        return Some((
            today.checked_add_signed(Duration::try_days(i64::from(first))?)?,
            rest,
        ));
    }
    if let Some(rest) = strip(rest, &["週間後", "週後"]) {
        return Some((
            today.checked_add_signed(Duration::try_weeks(i64::from(first))?)?,
            rest,
        ));
    }
    if let Some(rest) = strip(rest, &["か月後", "ヶ月後", "カ月後", "ヵ月後", "ケ月後"])
    {
        return Some((today.checked_add_months(Months::new(first))?, rest));
    }
    // 27日
    if let Some(rest) = rest.strip_prefix('日') {
        let this = NaiveDate::from_ymd_opt(today.year(), today.month(), first);
        return match this {
            Some(date) if date >= today => Some((date, rest)),
            _ => {
                let next =
                    clamp(today.year(), today.month(), 1)?.checked_add_months(Months::new(1))?;
                Some((
                    NaiveDate::from_ymd_opt(next.year(), next.month(), first)?,
                    rest,
                ))
            }
        };
    }
    // 年のない月日は、今日より前なら来年
    let upcoming = |month: u32, day: u32| {
        let this = NaiveDate::from_ymd_opt(today.year(), month, day);
        match this {
            Some(date) if date >= today => Some(date),
            _ => NaiveDate::from_ymd_opt(today.year() + 1, month, day).or(this),
        }
    };
    // 2026年11月27日
    if let Some(rest) = rest.strip_prefix('年') {
        let (month, rest) = number(rest)?;
        let (day, rest) = number(rest.strip_prefix('月')?)?;
        let date = NaiveDate::from_ymd_opt(first as i32, month, day)?;
        return Some((date, rest.strip_prefix('日').unwrap_or(rest)));
    }
    // 11月27日
    if let Some(rest) = rest.strip_prefix('月') {
        let (day, rest) = number(rest)?;
        return Some((
            upcoming(first, day)?,
            rest.strip_prefix('日').unwrap_or(rest),
        ));
    }
    // 2026/11/27、2026-11-27、2026.11.27、26/11/27、11/27、11-27
    let sep = rest
        .chars()
        .next()
        .filter(|c| matches!(c, '/' | '-' | '.'))?;
    let (second, rest) = number(&rest[1..])?;
    if let Some(third) = rest.strip_prefix(sep) {
        if let Some((third, rest)) = number(third) {
            let year = if first < 100 { 2000 + first } else { first };
            return Some((NaiveDate::from_ymd_opt(year as i32, second, third)?, rest));
        }
    }
    Some((upcoming(first, second)?, rest))
}

#[cfg(test)]
mod tests {
    use super::{parse, resolve};

    // 2026-11-25 は水曜
    const NOW: &str = "2026-11-25 10:00";

    fn p(input: &str) -> Option<String> {
        parse(input, NOW, None)
    }

    #[test]
    fn dates_in_many_spellings() {
        for (input, want) in [
            ("2026-11-27", "2026-11-27"),
            ("2026/11/27", "2026-11-27"),
            ("2026.11.27", "2026-11-27"),
            ("2026/1/3", "2026-01-03"),
            ("26/11/27", "2026-11-27"),
            ("20261127", "2026-11-27"),
            ("２０２６／１１／２７", "2026-11-27"),
            ("2026年11月27日", "2026-11-27"),
            ("11/27", "2026-11-27"),
            ("11-27", "2026-11-27"),
            ("11月27日", "2026-11-27"),
            ("１１月２７日", "2026-11-27"),
            ("11/25", "2026-11-25"),
            // 今日より前の月日は来年
            ("11/24", "2027-11-24"),
            ("1月5日", "2027-01-05"),
            ("27日", "2026-11-27"),
            ("25日", "2026-11-25"),
            // 今月はもう過ぎた日は来月
            ("3日", "2026-12-03"),
        ] {
            assert_eq!(p(input).as_deref(), Some(want), "{input}");
        }
    }

    #[test]
    fn relative_words() {
        for (input, want) in [
            ("今日", "2026-11-25"),
            ("本日", "2026-11-25"),
            ("today", "2026-11-25"),
            ("明日", "2026-11-26"),
            ("あした", "2026-11-26"),
            ("Tomorrow", "2026-11-26"),
            ("明後日", "2026-11-27"),
            ("あさって", "2026-11-27"),
            ("昨日", "2026-11-24"),
            ("3日後", "2026-11-28"),
            ("2週間後", "2026-12-09"),
            ("1か月後", "2026-12-25"),
            ("3ヶ月後", "2027-02-25"),
            ("来週", "2026-12-02"),
            ("再来週", "2026-12-09"),
            ("来週金曜", "2026-12-04"),
            ("来週の金曜日", "2026-12-04"),
            ("来週金", "2026-12-04"),
            ("来週月曜", "2026-11-30"),
            ("今週金曜", "2026-11-27"),
            ("今週月曜", "2026-11-23"),
            ("再来週火曜", "2026-12-08"),
            ("金曜", "2026-11-27"),
            ("金曜日", "2026-11-27"),
            // 今日の曜日は今日
            ("水曜", "2026-11-25"),
            ("火曜", "2026-12-01"),
            ("friday", "2026-11-27"),
            ("Fri", "2026-11-27"),
            ("next friday", "2026-12-04"),
            ("月末", "2026-11-30"),
            ("今月末", "2026-11-30"),
            ("来月末", "2026-12-31"),
            ("来月", "2026-12-25"),
            ("来月5日", "2026-12-05"),
            ("再来月10日", "2027-01-10"),
        ] {
            assert_eq!(p(input).as_deref(), Some(want), "{input}");
        }
        // 月末からの 1 か月後は、ない日なら月末
        assert_eq!(
            parse("1か月後", "2027-01-31", None).as_deref(),
            Some("2027-02-28")
        );
    }

    #[test]
    fn times_with_and_without_a_date() {
        for (input, want) in [
            ("2026-11-27 14:00", "2026-11-27 14:00"),
            ("2026-11-27T14:00:00", "2026-11-27 14:00"),
            ("2026/11/27 9:05", "2026-11-27 09:05"),
            ("11/27 14時", "2026-11-27 14:00"),
            ("11月27日14時30分", "2026-11-27 14:30"),
            ("27日 9時半", "2026-11-27 09:30"),
            ("明日14時", "2026-11-26 14:00"),
            ("明日の14時", "2026-11-26 14:00"),
            ("明日 午後2時", "2026-11-26 14:00"),
            ("来週金曜の15時", "2026-12-04 15:00"),
            ("来週金曜 2pm", "2026-12-04 14:00"),
            ("tomorrow 2:30PM", "2026-11-26 14:30"),
            ("明日 １４：００", "2026-11-26 14:00"),
            ("明日 正午", "2026-11-26 12:00"),
            ("14:00 明日", "2026-11-26 14:00"),
            ("今日 0:00", "2026-11-25 00:00"),
            // 時刻だけなら今日
            ("14:00", "2026-11-25 14:00"),
            ("14時", "2026-11-25 14:00"),
            ("2pm", "2026-11-25 14:00"),
            ("2 pm", "2026-11-25 14:00"),
            ("12am", "2026-11-25 00:00"),
            ("12pm", "2026-11-25 12:00"),
            ("午前9時", "2026-11-25 09:00"),
            ("午後11時59分", "2026-11-25 23:59"),
        ] {
            assert_eq!(p(input).as_deref(), Some(want), "{input}");
        }
        // 時刻だけなら、いまの期限の日付を残す
        assert_eq!(
            parse("18時", NOW, Some("2026-12-24 09:00")).as_deref(),
            Some("2026-12-24 18:00")
        );
        assert_eq!(
            parse("18:30", NOW, Some("2026-12-24")).as_deref(),
            Some("2026-12-24 18:30")
        );
        // 日付を書いたら、いまの期限の時刻は残さない
        assert_eq!(
            parse("明日", NOW, Some("2026-12-24 09:00")).as_deref(),
            Some("2026-11-26")
        );
    }

    #[test]
    fn unreadable_input_is_refused() {
        for input in [
            "",
            " ",
            "14",
            "あとで",
            "2026-02-30",
            "2026-13-01",
            "13/1",
            "32日",
            "25:00",
            "14:60",
            "24時",
            "明日 25時",
            "明日 あとで",
            "11/27 14",
            "来月31日x",
            "99999999999999999999",
        ] {
            assert_eq!(p(input), None, "{input}");
        }
        assert_eq!(resolve(" あとで ", NOW, None), "あとで");
        assert_eq!(resolve("明日", NOW, None), "2026-11-26");
    }

    #[test]
    fn 先頭の数字は9桁まで読む() {
        use super::number;
        assert_eq!(number("123456789x"), Some((123_456_789, "x")));
        assert_eq!(number("1234567890"), None);
        assert_eq!(number("x1"), None);
    }

    #[test]
    fn 曜日は曜をつけたときだけ読み月末は曜日にしない() {
        use super::weekday;
        assert_eq!(weekday("水", false), None);
        assert_eq!(weekday("水", true), Some((2, "")));
        assert_eq!(weekday("水曜", false), Some((2, "")));
        assert_eq!(weekday("月末", true), None);
        assert_eq!(weekday("月の", true), Some((0, "の")));
    }

    #[test]
    fn nextとthisは曜日が続くときだけ読む() {
        use super::date;
        use chrono::NaiveDate;
        let today = NaiveDate::from_ymd_opt(2026, 11, 25).unwrap();
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        assert_eq!(date("next foo", today), None);
        assert_eq!(date("this foo", today), None);
        assert_eq!(date("next friday", today), Some((d(2026, 12, 4), "")));
        assert_eq!(date("this friday", today), Some((d(2026, 11, 27), "")));
        assert_eq!(date("next week", today), Some((d(2026, 12, 2), "")));
    }

    #[test]
    fn 年は3桁以上ならそのまま使う() {
        use super::date;
        use chrono::NaiveDate;
        let today = NaiveDate::from_ymd_opt(2026, 11, 25).unwrap();
        assert_eq!(
            date("100/1/2", today),
            Some((NaiveDate::from_ymd_opt(100, 1, 2).unwrap(), ""))
        );
        assert_eq!(
            date("99/1/2", today),
            Some((NaiveDate::from_ymd_opt(2099, 1, 2).unwrap(), ""))
        );
    }
}
