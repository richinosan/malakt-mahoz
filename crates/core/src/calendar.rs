//! 日付を選ぶカレンダーの計算。月の並び、日本の祝日、よく使う日、入力の解釈の一行。
//!
//! 画面には依存しない。日付は `YYYY-MM-DD` の文字列ではなく [`NaiveDate`] で扱う。

use std::cell::RefCell;
use std::collections::BTreeMap;

use chrono::{Datelike, Duration, Months, NaiveDate, Weekday};

/// 曜日の名前（日曜はじまり）。
pub const WEEKDAYS: [&str; 7] = ["日", "月", "火", "水", "木", "金", "土"];

/// 時刻のチップ（`HH:MM`）。
pub const TIME_CHIPS: [&str; 4] = ["09:00", "12:00", "18:00", "23:59"];

/// 祝日を計算する年の範囲。今の法律（2020 年以降）の決まりで出せる年だけにする。
pub const HOLIDAY_YEARS: std::ops::RangeInclusive<i32> = 2020..=2099;

/// `YYYY-MM-DD`（後ろに時刻があってもよい）を読む。
pub fn parse_date(when: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(crate::date_of(when), "%Y-%m-%d").ok()
}

/// 保存する形（`YYYY-MM-DD`）。
pub fn ymd(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

/// 曜日の番号（日曜が 0）。
pub fn weekday(date: NaiveDate) -> usize {
    date.weekday().num_days_from_sunday() as usize
}

/// 入力欄に戻す形（2026/11/30）。
pub fn slash(date: NaiveDate) -> String {
    format!("{}/{}/{}", date.year(), date.month(), date.day())
}

/// 曜日つき（2026/11/30(月)）。
pub fn with_weekday(date: NaiveDate) -> String {
    format!("{}({})", slash(date), WEEKDAYS[weekday(date)])
}

/// 今日からの日数の言い方（今日・明日・昨日・あと 3 日・3 日前）。
pub fn relative(today: NaiveDate, date: NaiveDate) -> String {
    match (date - today).num_days() {
        0 => "今日".into(),
        1 => "明日".into(),
        -1 => "昨日".into(),
        n if n > 0 => format!("あと {n} 日"),
        n => format!("{} 日前", -n),
    }
}

/// 月の 1 日。
pub fn first_of(year: i32, month: u32) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(year, month, 1)
}

/// 月を足す（負なら引く）。日がない月では、その月の末日にする（1/31 の 1 か月後は 2/28）。
pub fn shift_months(date: NaiveDate, months: i32) -> NaiveDate {
    let moved = if months >= 0 {
        date.checked_add_months(Months::new(months as u32))
    } else {
        date.checked_sub_months(Months::new(months.unsigned_abs()))
    };
    moved.unwrap_or(date)
}

/// 月（`(年, 月)`）を足す。
pub fn shift_month(year: i32, month: u32, months: i32) -> (i32, u32) {
    let index = year * 12 + month as i32 - 1 + months;
    (index.div_euclid(12), index.rem_euclid(12) as u32 + 1)
}

/// 月のカレンダーの日付（日曜はじまり）。前後の月の日で埋めて、いつも 6 週（42 日）にする
/// （月によって高さが変わって、パネルの位置が動かないように）。
pub fn month_grid(year: i32, month: u32) -> Vec<NaiveDate> {
    let Some(first) = first_of(year, month) else {
        return Vec::new();
    };
    let start = first - Duration::days(weekday(first) as i64);
    (0..42).map(|i| start + Duration::days(i)).collect()
}

/// 月の見出し（2026年11月）。
pub fn month_title(year: i32, month: u32) -> String {
    format!("{year}年{month}月")
}

/// 月を選ぶ一覧に出す月。今年の前の年の 1 月から 5 年後の 12 月まで（表示中の月が外にあれば広げる）。
pub fn month_choices(today: NaiveDate, shown: (i32, u32)) -> Vec<(i32, u32)> {
    let first = (today.year() - 1).min(shown.0);
    let last = (today.year() + 5).max(shown.0);
    (first..=last)
        .flat_map(|year| (1..=12).map(move |month| (year, month)))
        .collect()
}

/// よく使う日のチップ（名前と日付）。今日・明日・今週末（土）・来週月曜・1 週間後。
/// `base`（ワークスペースの基準日）があれば、基準日とその前日も足す。
pub fn quick_dates(today: NaiveDate, base: Option<NaiveDate>) -> Vec<(&'static str, NaiveDate)> {
    // これから来る土曜（今日が土曜なら今日）
    let saturday = today + Duration::days(((6 - weekday(today) as i64) + 7) % 7);
    // 来週の月曜（月曜はじまりの週）
    let monday = today + Duration::days(7 - today.weekday().num_days_from_monday() as i64);
    let mut out = vec![
        ("今日", today),
        ("明日", today + Duration::days(1)),
        ("今週末(土)", saturday),
        ("来週月曜", monday),
        ("1週間後", today + Duration::days(7)),
    ];
    if let Some(base) = base {
        out.push(("基準日", base));
        out.push(("基準日の前日", base - Duration::days(1)));
    }
    out
}

/// 日付の入力の解釈の一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hint {
    /// まだ何も書いていない（文は案内）。
    Empty(&'static str),
    /// 読めた（区切りごと。区切りの中では折り返さない）。
    Ok(Vec<String>),
    /// 読めない。
    Invalid,
}

/// 日付の入力欄の文字の解釈。`now` は `YYYY-MM-DD HH:MM`。
/// 返すのは、読めた日付（時刻を書いていればその時刻も）と一行。
pub fn date_hint(text: &str, now: &str, has_value: bool) -> (Option<String>, Hint) {
    if text.trim().is_empty() {
        return (
            None,
            Hint::Empty(if has_value {
                "Enter で日付をなくします"
            } else {
                "日付を選んでください"
            }),
        );
    }
    let Some(value) = crate::when::parse(text, now, None) else {
        return (None, Hint::Invalid);
    };
    let (Some(date), Some(today)) = (parse_date(&value), parse_date(now)) else {
        return (None, Hint::Invalid);
    };
    let mut parts = vec![format!("→ {}", with_weekday(date))];
    if let Some(time) = crate::time_of(&value) {
        parts.push(time.to_owned());
    }
    parts.push(relative(today, date));
    if let Some(name) = holiday_name(date) {
        parts.push(format!("・{name}"));
    }
    (Some(value), Hint::Ok(parts))
}

/// 時刻の入力欄の文字を読む（`HH:MM`）。日付を書いたものは読まない。
pub fn parse_time(text: &str, now: &str) -> Option<String> {
    let value = crate::when::parse(text, now, Some("2000-01-01"))?;
    (crate::date_of(&value) == "2000-01-01")
        .then(|| crate::time_of(&value).map(str::to_owned))
        .flatten()
}

/// 時刻の入力欄の解釈の一行。`has_date` が偽なら、日付がまだない。
pub fn time_hint(text: &str, now: &str, has_date: bool) -> (Option<String>, Hint) {
    if !has_date {
        return (None, Hint::Empty("先に日付を"));
    }
    if text.trim().is_empty() {
        return (None, Hint::Empty("時刻なし（終日）"));
    }
    match parse_time(text, now) {
        Some(time) => (Some(time.clone()), Hint::Ok(vec![format!("→ {time}")])),
        None => (None, Hint::Invalid),
    }
}

/// 月の第 `nth` 月曜日。
fn nth_monday(year: i32, month: u32, nth: u8) -> u32 {
    NaiveDate::from_weekday_of_month_opt(year, month, Weekday::Mon, nth).map_or(1, |d| d.day())
}

/// 春分の日（3 月の何日か）。1980〜2099 年で正しい近似式。
fn spring_equinox(year: i32) -> u32 {
    let y = (year - 1980) as f64;
    (20.8431 + 0.242194 * y - (y / 4.).floor()).floor() as u32
}

/// 秋分の日（9 月の何日か）。
fn autumn_equinox(year: i32) -> u32 {
    let y = (year - 1980) as f64;
    (23.2488 + 0.242194 * y - (y / 4.).floor()).floor() as u32
}

thread_local! {
    static HOLIDAYS: RefCell<BTreeMap<i32, BTreeMap<NaiveDate, &'static str>>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// その年の祝日（振替休日と国民の休日を含む）。日付の順。範囲の外の年は空。
pub fn holidays_of(year: i32) -> BTreeMap<NaiveDate, &'static str> {
    HOLIDAYS.with(|cache| {
        cache
            .borrow_mut()
            .entry(year)
            .or_insert_with(|| {
                if HOLIDAY_YEARS.contains(&year) {
                    compute_holidays(year)
                } else {
                    BTreeMap::new()
                }
            })
            .clone()
    })
}

/// 祝日ならその名前。
pub fn holiday_name(date: NaiveDate) -> Option<&'static str> {
    holidays_of(date.year()).get(&date).copied()
}

/// 月の祝日（日付と名前）。
pub fn month_holidays(year: i32, month: u32) -> Vec<(NaiveDate, &'static str)> {
    holidays_of(year)
        .into_iter()
        .filter(|(d, _)| d.month() == month)
        .collect()
}

fn compute_holidays(year: i32) -> BTreeMap<NaiveDate, &'static str> {
    let mut base = BTreeMap::new();
    let mut add = |month: u32, day: u32, name: &'static str| {
        if let Some(date) = NaiveDate::from_ymd_opt(year, month, day) {
            base.insert(date, name);
        }
    };
    add(1, 1, "元日");
    add(1, nth_monday(year, 1, 2), "成人の日");
    add(2, 11, "建国記念の日");
    add(2, 23, "天皇誕生日");
    add(3, spring_equinox(year), "春分の日");
    add(4, 29, "昭和の日");
    add(5, 3, "憲法記念日");
    add(5, 4, "みどりの日");
    add(5, 5, "こどもの日");
    add(9, nth_monday(year, 9, 3), "敬老の日");
    add(9, autumn_equinox(year), "秋分の日");
    add(11, 3, "文化の日");
    add(11, 23, "勤労感謝の日");
    // オリンピックの特例（2020 年は 7/23・7/24・8/10、2021 年は 7/22・7/23・8/8）
    match year {
        2020 => {
            add(7, 23, "海の日");
            add(7, 24, "スポーツの日");
            add(8, 10, "山の日");
        }
        2021 => {
            add(7, 22, "海の日");
            add(7, 23, "スポーツの日");
            add(8, 8, "山の日");
        }
        _ => {
            add(7, nth_monday(year, 7, 3), "海の日");
            add(8, 11, "山の日");
            add(10, nth_monday(year, 10, 2), "スポーツの日");
        }
    }
    // 国民の休日: 前の日と次の日がどちらも祝日で、日曜でも祝日でもない日。振替休日より先に決める
    let mut holidays = base.clone();
    let mut day = NaiveDate::from_ymd_opt(year, 1, 2);
    while let Some(d) = day.filter(|d| d.year() == year) {
        let one = Duration::days(1);
        if !base.contains_key(&d)
            && d.weekday() != Weekday::Sun
            && base.contains_key(&(d - one))
            && base.contains_key(&(d + one))
        {
            holidays.insert(d, "国民の休日");
        }
        day = d.succ_opt();
    }
    // 振替休日: 祝日が日曜なら、その後で最初の祝日でない日
    let sundays: Vec<NaiveDate> = holidays
        .iter()
        .filter(|(d, name)| d.weekday() == Weekday::Sun && **name != "国民の休日")
        .map(|(d, _)| *d)
        .collect();
    for sunday in sundays {
        let mut sub = sunday + Duration::days(1);
        while holidays.contains_key(&sub) {
            sub += Duration::days(1);
        }
        holidays.insert(sub, "振替休日");
    }
    holidays
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        parse_date(s).unwrap()
    }

    #[test]
    fn grid_is_six_weeks_from_sunday() {
        let grid = month_grid(2026, 11);
        assert_eq!(grid.len(), 42);
        // 2026/11/1 は日曜
        assert_eq!(grid[0], d("2026-11-01"));
        let grid = month_grid(2026, 12);
        assert_eq!(grid[0], d("2026-11-29"));
        assert_eq!(grid[41], d("2027-01-09"));
    }

    #[test]
    fn months_shift_and_clamp() {
        assert_eq!(shift_month(2026, 12, 1), (2027, 1));
        assert_eq!(shift_month(2026, 1, -1), (2025, 12));
        assert_eq!(shift_month(2026, 1, -13), (2024, 12));
        assert_eq!(shift_months(d("2027-01-31"), 1), d("2027-02-28"));
        assert_eq!(shift_months(d("2027-03-31"), -1), d("2027-02-28"));
    }

    #[test]
    fn holidays_2026() {
        let names: Vec<(String, &str)> = holidays_of(2026)
            .into_iter()
            .map(|(d, n)| (ymd(d), n))
            .collect();
        let has = |date: &str, name: &str| {
            assert!(
                names.contains(&(date.to_owned(), name)),
                "{date} {name}: {names:?}"
            )
        };
        has("2026-01-01", "元日");
        has("2026-01-12", "成人の日");
        has("2026-03-20", "春分の日");
        // 5/3 が日曜なので 5/6 が振替休日
        has("2026-05-06", "振替休日");
        has("2026-09-21", "敬老の日");
        // 敬老の日と秋分の日に挟まれた国民の休日
        has("2026-09-22", "国民の休日");
        has("2026-09-23", "秋分の日");
        has("2026-10-12", "スポーツの日");
        has("2026-11-23", "勤労感謝の日");
        assert_eq!(holiday_name(d("2026-11-24")), None);
        assert!(holidays_of(2019).is_empty());
    }

    #[test]
    fn olympic_years_and_substitutes() {
        assert_eq!(holiday_name(d("2021-07-23")), Some("スポーツの日"));
        assert_eq!(holiday_name(d("2021-08-09")), Some("振替休日"));
        // 2025/2/23（日）→ 2/24 が振替休日
        assert_eq!(holiday_name(d("2025-02-24")), Some("振替休日"));
    }

    #[test]
    fn quick_dates_from_saturday_and_monday() {
        // 2026/11/28 は土曜
        let chips = quick_dates(d("2026-11-28"), None);
        assert_eq!(chips[2], ("今週末(土)", d("2026-11-28")));
        assert_eq!(chips[3], ("来週月曜", d("2026-11-30")));
        assert_eq!(chips[4], ("1週間後", d("2026-12-05")));
        // 月曜から見た来週月曜は 7 日後
        let chips = quick_dates(d("2026-11-30"), Some(d("2026-12-10")));
        assert_eq!(chips[3], ("来週月曜", d("2026-12-07")));
        assert_eq!(chips[5], ("基準日", d("2026-12-10")));
        assert_eq!(chips[6], ("基準日の前日", d("2026-12-09")));
    }

    #[test]
    fn hints() {
        let now = "2026-11-28 10:00";
        let (value, hint) = date_hint("明日", now, false);
        assert_eq!(value.as_deref(), Some("2026-11-29"));
        assert_eq!(
            hint,
            Hint::Ok(vec!["→ 2026/11/29(日)".into(), "明日".into()])
        );
        let (value, hint) = date_hint("2027/1/11 14時", now, false);
        assert_eq!(value.as_deref(), Some("2027-01-11 14:00"));
        assert_eq!(
            hint,
            Hint::Ok(vec![
                "→ 2027/1/11(月)".into(),
                "14:00".into(),
                "あと 44 日".into(),
                "・成人の日".into()
            ])
        );
        let (_, hint) = date_hint("2026/11/23", now, false);
        assert_eq!(
            hint,
            Hint::Ok(vec![
                "→ 2026/11/23(月)".into(),
                "5 日前".into(),
                "・勤労感謝の日".into()
            ])
        );
        assert_eq!(date_hint("あいう", now, false), (None, Hint::Invalid));
        assert_eq!(
            date_hint("", now, true),
            (None, Hint::Empty("Enter で日付をなくします"))
        );
        assert_eq!(parse_time("18時", now).as_deref(), Some("18:00"));
        assert_eq!(parse_time("2pm", now).as_deref(), Some("14:00"));
        assert_eq!(parse_time("明日", now), None);
        assert_eq!(parse_time("25時", now), None);
        assert_eq!(time_hint("", now, false), (None, Hint::Empty("先に日付を")));
    }

    #[test]
    fn month_choices_cover_shown() {
        let choices = month_choices(d("2026-11-28"), (2026, 11));
        assert_eq!(choices.first(), Some(&(2025, 1)));
        assert_eq!(choices.last(), Some(&(2031, 12)));
        let choices = month_choices(d("2026-11-28"), (2040, 3));
        assert_eq!(choices.last(), Some(&(2040, 12)));
    }
}
