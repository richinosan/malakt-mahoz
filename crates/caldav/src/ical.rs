//! iCalendar（RFC 5545）の VTODO の読み書き。
//!
//! サーバーから受け取った ics は、そのまま持っておき、変えた項目の行だけを書き換えて送り返す
//! （[`patch`]）。このアプリが読まない項目（VALARM、RRULE、X- で始まる項目、ATTENDEE の知らない
//! パラメーターなど）は、そのまま残る。

use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// 新しく作る ics の PRODID。
pub const PRODID: &str = "-//Malakt Mahoz//Malakt Mahoz//JA";

/// 1 行の長さ（オクテット）。これより長い行は折り返す。
const FOLD_OCTETS: usize = 75;

/// 説明の末尾の URL の行の区切り。
const LINK_SEPARATOR: &str = "\n\n";

/// タスクの項目のうち、サーバーと同期するもの。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Fields {
    pub summary: String,
    /// 説明。末尾の URL だけの行（[`Fields::links`]）は除いたもの。
    pub body: String,
    /// 説明の末尾にあった、URL だけの行。サーバーがタスクの画面の URL を足すのに使う。
    #[serde(default)]
    pub links: Vec<String>,
    /// URL の項目。
    #[serde(default)]
    pub url: Option<String>,
    /// `NEEDS-ACTION`・`IN-PROCESS`・`COMPLETED`・`CANCELLED`。
    pub status: String,
    /// 0 = なし、1 = 最高 … 9 = 最低。
    pub priority: u8,
    /// 進捗（0〜100）。
    pub percent: u8,
    /// 開始。`YYYY-MM-DD` か `YYYY-MM-DD HH:MM`（この PC の時刻）。
    #[serde(default)]
    pub start: Option<String>,
    /// 期限。形は `start` と同じ。
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    /// 親タスクの UID。
    #[serde(default)]
    pub parent: Option<String>,
    /// 担当者。URI の順。
    #[serde(default)]
    pub attendees: Vec<Attendee>,
}

/// 担当者 1 人（ATTENDEE）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Attendee {
    /// `mailto:` や `https:` などの URI。
    pub uri: String,
    /// 表示名（CN）。
    #[serde(default)]
    pub name: String,
    /// 役。`R`・`A`・`C`・`I` のどれか 1 つ（ROLE）。
    pub role: String,
    /// その人の分を完了にしたか（PARTSTAT=COMPLETED）。タスクが完了のときは `false` にそろえる。
    #[serde(default)]
    pub done: bool,
}

/// VTODO の STATUS。
pub const STATUSES: [&str; 4] = ["NEEDS-ACTION", "IN-PROCESS", "COMPLETED", "CANCELLED"];

impl Fields {
    /// 空のタスク（新しく作るときの元）。
    pub fn empty() -> Self {
        Fields {
            status: "NEEDS-ACTION".into(),
            ..Default::default()
        }
    }

    /// 説明の全文（本文と、末尾の URL の行）。
    pub fn description(&self) -> String {
        join_description(&self.body, &self.links)
    }

    /// 項目の比べ方をそろえる。完了のタスクは、進捗を 100、担当者ごとの完了をなしにする
    /// （完了にするとアプリが全員の分を完了にするので、それを変更と数えないため）。
    pub fn settle(&mut self) {
        if self.status == "COMPLETED" {
            self.percent = 100;
            for a in &mut self.attendees {
                a.done = false;
            }
        }
        self.attendees.sort_by(|a, b| a.uri.cmp(&b.uri));
        self.attendees.dedup_by(|a, b| a.uri == b.uri);
    }
}

/// 役の ROLE の値。
fn role_param(role: &str) -> &'static str {
    match role {
        "A" => "CHAIR",
        "C" => "OPT-PARTICIPANT",
        "I" => "NON-PARTICIPANT",
        _ => "REQ-PARTICIPANT",
    }
}

fn role_of(param: Option<&str>) -> &'static str {
    match param.map(str::to_ascii_uppercase).as_deref() {
        Some("CHAIR") => "A",
        Some("OPT-PARTICIPANT") => "C",
        Some("NON-PARTICIPANT") => "I",
        _ => "R",
    }
}

// ---- 説明の末尾の URL ----

fn is_link_line(line: &str) -> bool {
    let line = line.trim();
    (line.starts_with("https://") || line.starts_with("http://"))
        && line.len() > "https://".len()
        && !line.chars().any(char::is_whitespace)
}

/// 説明を、本文と末尾の URL だけの行に分ける。URL は空行で区切られた段落 1 つに 1 つ。
pub fn split_description(text: &str) -> (String, Vec<String>) {
    let text = text.replace("\r\n", "\n");
    let mut paragraphs: Vec<&str> = text.trim_end().split(LINK_SEPARATOR).collect();
    let mut links = Vec::new();
    while let Some(last) = paragraphs.last() {
        let last = last.trim_matches('\n');
        if last.lines().count() == 1 && is_link_line(last) {
            links.push(last.trim().to_owned());
            paragraphs.pop();
        } else {
            break;
        }
    }
    links.reverse();
    let body = paragraphs.join(LINK_SEPARATOR).trim_end().to_owned();
    (body, links)
}

pub fn join_description(body: &str, links: &[String]) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if !body.is_empty() {
        parts.push(body);
    }
    parts.extend(links.iter().map(String::as_str));
    parts.join(LINK_SEPARATOR)
}

// ---- 行 ----

/// 1 行（折り返しを戻したもの）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Line {
    /// 大文字にした名前。
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Line {
    fn new(name: &str, value: impl Into<String>) -> Self {
        Line {
            name: name.into(),
            params: Vec::new(),
            value: value.into(),
        }
    }

    fn with(mut self, name: &str, value: impl Into<String>) -> Self {
        self.params.push((name.into(), value.into()));
        self
    }

    fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn set_param(&mut self, name: &str, value: Option<String>) {
        let at = self
            .params
            .iter()
            .position(|(n, _)| n.eq_ignore_ascii_case(name));
        match (at, value) {
            (Some(i), Some(v)) => self.params[i].1 = v,
            (Some(i), None) => {
                self.params.remove(i);
            }
            (None, Some(v)) => self.params.push((name.into(), v)),
            (None, None) => {}
        }
    }

    fn parse(raw: &str) -> Option<Line> {
        // 名前とパラメーターは、引用符の外の最初の : まで
        let mut in_quote = false;
        let mut colon = None;
        for (i, c) in raw.char_indices() {
            match c {
                '"' => in_quote = !in_quote,
                ':' if !in_quote => {
                    colon = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let colon = colon?;
        let head = &raw[..colon];
        let value = raw[colon + 1..].to_owned();
        let mut parts = split_outside_quotes(head, ';').into_iter();
        let name = parts.next()?.trim().to_ascii_uppercase();
        if name.is_empty() {
            return None;
        }
        let params = parts
            .filter_map(|p| {
                let (n, v) = p.split_once('=')?;
                Some((n.trim().to_ascii_uppercase(), v.to_owned()))
            })
            .collect();
        Some(Line {
            name,
            params,
            value,
        })
    }

    fn render(&self) -> String {
        let mut out = self.name.clone();
        for (n, v) in &self.params {
            out.push(';');
            out.push_str(n);
            out.push('=');
            out.push_str(v);
        }
        out.push(':');
        out.push_str(&self.value);
        out
    }
}

fn split_outside_quotes(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut in_quote = false;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if c == '"' {
            in_quote = !in_quote;
        } else if c == sep && !in_quote {
            out.push(&s[start..i]);
            start = i + c.len_utf8();
        }
    }
    out.push(&s[start..]);
    out
}

fn unquote(v: &str) -> &str {
    v.strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(v)
}

/// パラメーターの値。`:` `;` `,` を含むときは引用符で囲む。引用符と改行は使えないので除く。
fn param_value(v: &str) -> String {
    let v: String = v.chars().filter(|c| *c != '"' && !c.is_control()).collect();
    if v.contains([':', ';', ',']) {
        format!("\"{v}\"")
    } else {
        v
    }
}

/// 折り返しを戻した行の一覧。
fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = raw.strip_prefix([' ', '\t']) {
            if let Some(last) = lines.last_mut() {
                last.push_str(rest);
                continue;
            }
        }
        if !raw.is_empty() {
            lines.push(raw.to_owned());
        }
    }
    lines
}

/// 75 オクテットで折り返し、CRLF でつなぐ。文字の途中では切らない。
fn fold(lines: &[String]) -> String {
    let mut out = String::new();
    for line in lines {
        let mut used = 0;
        for c in line.chars() {
            let n = c.len_utf8();
            if used + n > FOLD_OCTETS {
                out.push_str("\r\n ");
                used = 1;
            }
            out.push(c);
            used += n;
        }
        out.push_str("\r\n");
    }
    out
}

/// TEXT の値を読む（RFC 5545 3.3.11）。
fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn escape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// TEXT のリスト（CATEGORIES）を、エスケープしていない `,` で分ける。
fn split_list(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                cur.push(c);
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ',' => out.push(unescape(&std::mem::take(&mut cur))),
            c => cur.push(c),
        }
    }
    out.push(unescape(&cur));
    out.into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

// ---- 日時 ----

/// DUE・DTSTART の値を、`YYYY-MM-DD` か `YYYY-MM-DD HH:MM`（`local` の時刻）にする。
fn read_when(line: &Line, local: &TimeZone) -> Option<String> {
    let v = line.value.trim();
    let date_only = line
        .param("VALUE")
        .is_some_and(|p| p.eq_ignore_ascii_case("DATE"))
        || (v.len() == 8 && v.bytes().all(|b| b.is_ascii_digit()));
    if date_only {
        let d = v.get(..8)?;
        if !d.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        return Some(format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..8]));
    }
    let (body, utc) = match v.strip_suffix(['Z', 'z']) {
        Some(b) => (b, true),
        None => (v, false),
    };
    let dt = DateTime::strptime("%Y%m%dT%H%M%S", body)
        .or_else(|_| DateTime::strptime("%Y%m%dT%H%M", body))
        .ok()?;
    let civil = if utc {
        dt.to_zoned_datetime(TimeZone::UTC)
            .ok()?
            .with_time_zone(local.clone())
            .datetime()
    } else if let Some(tz) = line
        .param("TZID")
        .map(unquote)
        .and_then(|id| TimeZone::get(id.trim_start_matches('/')).ok())
    {
        dt.to_zoned_datetime(tz)
            .ok()?
            .with_time_zone(local.clone())
            .datetime()
    } else {
        // 時刻帯のない時刻と、この PC が知らない時刻帯は、そのままの時刻として読む
        dt
    };
    Some(civil.strftime("%Y-%m-%d %H:%M").to_string())
}

/// `YYYY-MM-DD` は日付、`YYYY-MM-DD HH:MM` は UTC の時刻にして書く。
fn when_line(name: &str, when: &str, local: &TimeZone) -> Option<Line> {
    match when.split_once(' ') {
        None => {
            let d = when.replace('-', "");
            (d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()))
                .then(|| Line::new(name, d).with("VALUE", "DATE"))
        }
        Some(_) => {
            let dt = DateTime::strptime("%Y-%m-%d %H:%M", when).ok()?;
            let utc = dt.to_zoned_datetime(local.clone()).ok()?.timestamp();
            Some(Line::new(name, stamp(utc)))
        }
    }
}

fn stamp(t: Timestamp) -> String {
    t.strftime("%Y%m%dT%H%M%SZ").to_string()
}

// ---- 読む ----

/// 読んだ VTODO。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Todo {
    pub uid: String,
    pub fields: Fields,
}

/// 主な VTODO の範囲（BEGIN の行と END の行の位置）。繰り返しの 1 回分だけを直したもの
/// （RECURRENCE-ID のあるもの）より、元のものを選ぶ。
fn main_todo(lines: &[String]) -> Option<(usize, usize)> {
    let mut depth = 0usize;
    let mut begin = None;
    let mut override_of = false;
    let mut first = None;
    for (i, raw) in lines.iter().enumerate() {
        let upper = raw.to_ascii_uppercase();
        if upper.starts_with("BEGIN:") {
            depth += 1;
            if depth == 2 && upper == "BEGIN:VTODO" {
                begin = Some(i);
                override_of = false;
            }
        } else if upper.starts_with("END:") {
            if depth == 2 && upper == "END:VTODO" {
                if let Some(b) = begin.take() {
                    if !override_of {
                        return Some((b, i));
                    }
                    first.get_or_insert((b, i));
                }
            }
            depth = depth.saturating_sub(1);
        } else if depth == 2 && begin.is_some() && upper.starts_with("RECURRENCE-ID") {
            override_of = true;
        }
    }
    first
}

/// VTODO の直下の行（中の VALARM などの行は除く）の位置。
fn top_lines(lines: &[String], begin: usize, end: usize) -> Vec<usize> {
    let mut depth = 0usize;
    let mut out = Vec::new();
    for (i, raw) in lines.iter().enumerate().take(end).skip(begin + 1) {
        let upper = raw.to_ascii_uppercase();
        if upper.starts_with("BEGIN:") {
            depth += 1;
        } else if upper.starts_with("END:") {
            depth = depth.saturating_sub(1);
        } else if depth == 0 {
            out.push(i);
        }
    }
    out
}

/// ics を読む。VTODO がなければ（予定など）`Ok(None)`。
pub fn parse(ics: &str, local: &TimeZone) -> Result<Option<Todo>, String> {
    let lines = unfold(ics);
    if !lines
        .first()
        .is_some_and(|l| l.eq_ignore_ascii_case("BEGIN:VCALENDAR"))
    {
        return Err("iCalendar の形ではありません".into());
    }
    let Some((begin, end)) = main_todo(&lines) else {
        return Ok(None);
    };
    let mut uid = String::new();
    let mut f = Fields::empty();
    let mut description = String::new();
    let mut completed = false;
    for i in top_lines(&lines, begin, end) {
        let Some(line) = Line::parse(&lines[i]) else {
            continue;
        };
        match line.name.as_str() {
            "UID" => uid = line.value.trim().to_owned(),
            "SUMMARY" => f.summary = unescape(&line.value),
            "DESCRIPTION" => description = unescape(&line.value),
            "URL" => f.url = Some(line.value.trim().to_owned()).filter(|u| !u.is_empty()),
            "STATUS" => {
                let s = line.value.trim().to_ascii_uppercase();
                if STATUSES.contains(&s.as_str()) {
                    f.status = s;
                }
            }
            "COMPLETED" => completed = true,
            "PRIORITY" => f.priority = line.value.trim().parse::<u8>().unwrap_or(0).min(9),
            "PERCENT-COMPLETE" => {
                f.percent = line.value.trim().parse::<u8>().unwrap_or(0).min(100);
            }
            "DTSTART" => f.start = read_when(&line, local),
            "DUE" => f.due = read_when(&line, local),
            "CATEGORIES" => f.categories.extend(split_list(&line.value)),
            "RELATED-TO" => {
                let parent = line
                    .param("RELTYPE")
                    .is_none_or(|t| t.eq_ignore_ascii_case("PARENT"));
                if parent && f.parent.is_none() {
                    f.parent = Some(line.value.trim().to_owned()).filter(|v| !v.is_empty());
                }
            }
            "ATTENDEE" => {
                let uri = line.value.trim().to_owned();
                if uri.is_empty() {
                    continue;
                }
                f.attendees.push(Attendee {
                    name: line.param("CN").map(unquote).unwrap_or_default().to_owned(),
                    role: role_of(line.param("ROLE").map(unquote)).into(),
                    done: line
                        .param("PARTSTAT")
                        .is_some_and(|p| unquote(p).eq_ignore_ascii_case("COMPLETED")),
                    uri,
                });
            }
            _ => {}
        }
    }
    if uid.is_empty() {
        return Err("UID がありません".into());
    }
    // STATUS がなくても COMPLETED があれば完了
    if completed && f.status == "NEEDS-ACTION" {
        f.status = "COMPLETED".into();
    }
    let (body, links) = split_description(&description);
    f.body = body;
    f.links = links;
    f.settle();
    Ok(Some(Todo { uid, fields: f }))
}

// ---- 書く ----

/// 新しい ics を作る。
pub fn build(uid: &str, fields: &Fields, local: &TimeZone, now: Timestamp) -> String {
    let skeleton = [
        "BEGIN:VCALENDAR".to_owned(),
        "VERSION:2.0".to_owned(),
        format!("PRODID:{PRODID}"),
        "BEGIN:VTODO".to_owned(),
        format!("UID:{uid}"),
        format!("CREATED:{}", stamp(now)),
        "END:VTODO".to_owned(),
        "END:VCALENDAR".to_owned(),
    ]
    .join("\r\n");
    let blank = Fields {
        // 状態は必ず書く
        status: String::new(),
        ..Fields::empty()
    };
    patch_lines(&skeleton, &blank, fields, local, now, false)
}

/// `ics` の項目のうち、`old` から `new` で変わったものの行だけを書き換える。
/// `old` は `ics` を読んだときの項目。変わったものがなければ `ics` のまま。
pub fn patch(ics: &str, old: &Fields, new: &Fields, local: &TimeZone, now: Timestamp) -> String {
    if old == new {
        return ics.to_owned();
    }
    patch_lines(ics, old, new, local, now, true)
}

fn patch_lines(
    ics: &str,
    old: &Fields,
    new: &Fields,
    local: &TimeZone,
    now: Timestamp,
    bump: bool,
) -> String {
    let mut lines = unfold(ics);
    let Some((begin, end)) = main_todo(&lines) else {
        return ics.to_owned();
    };
    let top = top_lines(&lines, begin, end);
    let mut remove: Vec<usize> = Vec::new();
    let mut add: Vec<Line> = Vec::new();
    let parsed: Vec<(usize, Line)> = top
        .iter()
        .filter_map(|&i| Line::parse(&lines[i]).map(|l| (i, l)))
        .collect();
    let drop_named = |names: &[&str], remove: &mut Vec<usize>| {
        for (i, l) in &parsed {
            if names.contains(&l.name.as_str()) {
                remove.push(*i);
            }
        }
    };

    if old.summary != new.summary {
        drop_named(&["SUMMARY"], &mut remove);
        add.push(Line::new("SUMMARY", escape(&new.summary)));
    }
    if old.body != new.body || old.links != new.links {
        drop_named(&["DESCRIPTION"], &mut remove);
        let text = new.description();
        if !text.is_empty() {
            add.push(Line::new("DESCRIPTION", escape(&text)));
        }
    }
    if old.url != new.url {
        drop_named(&["URL"], &mut remove);
        if let Some(url) = &new.url {
            add.push(Line::new("URL", url.clone()));
        }
    }
    if old.status != new.status {
        drop_named(&["STATUS", "COMPLETED"], &mut remove);
        add.push(Line::new("STATUS", new.status.clone()));
        if new.status == "COMPLETED" {
            add.push(Line::new("COMPLETED", stamp(now)));
        }
    }
    if old.priority != new.priority {
        drop_named(&["PRIORITY"], &mut remove);
        if new.priority > 0 {
            add.push(Line::new("PRIORITY", new.priority.to_string()));
        }
    }
    if old.percent != new.percent {
        drop_named(&["PERCENT-COMPLETE"], &mut remove);
        if new.percent > 0 {
            add.push(Line::new("PERCENT-COMPLETE", new.percent.to_string()));
        }
    }
    if old.start != new.start {
        drop_named(&["DTSTART"], &mut remove);
        if let Some(line) = new
            .start
            .as_deref()
            .and_then(|w| when_line("DTSTART", w, local))
        {
            add.push(line);
        }
    }
    if old.due != new.due {
        // DUE と DURATION は同時に持てない
        drop_named(&["DUE", "DURATION"], &mut remove);
        if let Some(line) = new.due.as_deref().and_then(|w| when_line("DUE", w, local)) {
            add.push(line);
        }
    }
    if old.categories != new.categories {
        drop_named(&["CATEGORIES"], &mut remove);
        if !new.categories.is_empty() {
            let v: Vec<String> = new.categories.iter().map(|c| escape(c)).collect();
            add.push(Line::new("CATEGORIES", v.join(",")));
        }
    }
    if old.parent != new.parent {
        for (i, l) in &parsed {
            if l.name == "RELATED-TO"
                && l.param("RELTYPE")
                    .is_none_or(|t| t.eq_ignore_ascii_case("PARENT"))
            {
                remove.push(*i);
            }
        }
        if let Some(parent) = &new.parent {
            add.push(Line::new("RELATED-TO", parent.clone()).with("RELTYPE", "PARENT"));
        }
    }
    if old.attendees != new.attendees {
        // 変わらない人の行はそのまま残し、変わった人の行だけを書き直す
        for (i, l) in &parsed {
            if l.name != "ATTENDEE" {
                continue;
            }
            let uri = l.value.trim();
            let before = old.attendees.iter().find(|a| a.uri == uri);
            let after = new.attendees.iter().find(|a| a.uri == uri);
            if before.is_none() || before != after {
                remove.push(*i);
            }
        }
        for a in &new.attendees {
            let before = old.attendees.iter().find(|b| b.uri == a.uri);
            if before == Some(a)
                && parsed
                    .iter()
                    .any(|(_, l)| l.name == "ATTENDEE" && l.value.trim() == a.uri)
            {
                continue;
            }
            // 元の行のパラメーター（RSVP など）を残し、名前・役・完了だけを変える
            let mut line = parsed
                .iter()
                .find(|(_, l)| l.name == "ATTENDEE" && l.value.trim() == a.uri)
                .map(|(_, l)| l.clone())
                .unwrap_or_else(|| Line::new("ATTENDEE", a.uri.clone()));
            let role_changed = before.is_none_or(|b| b.role != a.role);
            if role_changed {
                // 役が変わった人は、ROLE のほかの役の書き方（X- のパラメーター）を残さない
                line.params.retain(|(n, _)| !n.starts_with("X-"));
                line.set_param("ROLE", Some(role_param(&a.role).into()));
            }
            line.set_param("CN", (!a.name.is_empty()).then(|| param_value(&a.name)));
            let partstat = if a.done {
                Some("COMPLETED".to_owned())
            } else if line
                .param("PARTSTAT")
                .is_some_and(|p| unquote(p).eq_ignore_ascii_case("COMPLETED"))
            {
                Some("NEEDS-ACTION".to_owned())
            } else {
                line.param("PARTSTAT").map(str::to_owned)
            };
            line.set_param("PARTSTAT", partstat);
            add.push(line);
        }
    }

    if bump && (add.is_empty() && remove.is_empty()) {
        return ics.to_owned();
    }
    // 書いた時刻と版
    drop_named(&["DTSTAMP", "LAST-MODIFIED"], &mut remove);
    add.push(Line::new("DTSTAMP", stamp(now)));
    add.push(Line::new("LAST-MODIFIED", stamp(now)));
    if bump {
        let sequence = parsed
            .iter()
            .find(|(_, l)| l.name == "SEQUENCE")
            .and_then(|(_, l)| l.value.trim().parse::<u32>().ok());
        drop_named(&["SEQUENCE"], &mut remove);
        add.push(Line::new(
            "SEQUENCE",
            (sequence.unwrap_or(0) + 1).to_string(),
        ));
    }

    remove.sort_unstable();
    remove.dedup();
    let added: Vec<String> = add.iter().map(Line::render).collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + added.len());
    for (i, line) in lines.drain(..).enumerate() {
        if i == end {
            out.extend(added.iter().cloned());
        }
        if remove.binary_search(&i).is_err() {
            out.push(line);
        }
    }
    fold(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokyo() -> TimeZone {
        TimeZone::fixed(jiff::tz::offset(9))
    }

    fn now() -> Timestamp {
        "2026-11-28T04:00:00Z".parse().unwrap()
    }

    const SAMPLE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//EN\r\nBEGIN:VTODO\r\nUID:abc@example.com\r\nDTSTAMP:20261101T000000Z\r\nSUMMARY:会場を押さえる\\, 2 日分\r\nDESCRIPTION:下見も\\n\\nhttps://tasks.example.com/t/1\r\nSTATUS:IN-PROCESS\r\nPRIORITY:1\r\nDUE;TZID=Asia/Tokyo:20261201T180000\r\nDTSTART;VALUE=DATE:20261125\r\nCATEGORIES:会場,下見\r\nRELATED-TO;RELTYPE=PARENT:parent@example.com\r\nATTENDEE;CN=アリス;ROLE=CHAIR;PARTSTAT=COMPLETED;X-OTHER=R,A:https://id.example.com/users/1\r\nATTENDEE;CN=\"Bob, Jr\":mailto:bob@example.com\r\nX-EXTRA:keep me\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:alarm\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";

    #[test]
    fn reads_the_fields() {
        let todo = parse(SAMPLE, &tokyo()).unwrap().unwrap();
        assert_eq!(todo.uid, "abc@example.com");
        let f = todo.fields;
        assert_eq!(f.summary, "会場を押さえる, 2 日分");
        assert_eq!(f.body, "下見も");
        assert_eq!(f.links, vec!["https://tasks.example.com/t/1".to_owned()]);
        assert_eq!(f.status, "IN-PROCESS");
        assert_eq!(f.priority, 1);
        assert_eq!(f.due.as_deref(), Some("2026-12-01 18:00"));
        assert_eq!(f.start.as_deref(), Some("2026-11-25"));
        assert_eq!(f.categories, vec!["会場", "下見"]);
        assert_eq!(f.parent.as_deref(), Some("parent@example.com"));
        assert_eq!(f.attendees.len(), 2);
        let alice = f.attendees.iter().find(|a| a.name == "アリス").unwrap();
        assert_eq!(alice.role, "A");
        assert!(alice.done);
        let bob = f
            .attendees
            .iter()
            .find(|a| a.uri == "mailto:bob@example.com")
            .unwrap();
        assert_eq!(bob.name, "Bob, Jr");
        assert_eq!(bob.role, "R");
    }

    #[test]
    fn utc_times_read_in_the_local_zone() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:x\r\nDUE:20261201T090000Z\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
        let f = parse(ics, &tokyo()).unwrap().unwrap().fields;
        assert_eq!(f.due.as_deref(), Some("2026-12-01 18:00"));
    }

    #[test]
    fn events_are_not_tasks() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        assert_eq!(parse(ics, &tokyo()).unwrap(), None);
        assert!(parse("hello", &tokyo()).is_err());
    }

    #[test]
    fn unchanged_fields_keep_the_original_text() {
        let tz = tokyo();
        let old = parse(SAMPLE, &tz).unwrap().unwrap().fields;
        assert_eq!(patch(SAMPLE, &old, &old, &tz, now()), SAMPLE);
        let mut new = old.clone();
        new.summary = "会場を押さえる".into();
        let out = patch(SAMPLE, &old, &new, &tz, now());
        // 読まない項目と、変えていない項目の書き方はそのまま
        assert!(out.contains("X-EXTRA:keep me"));
        assert!(out.contains("TRIGGER:-PT15M"));
        assert!(out.contains("DUE;TZID=Asia/Tokyo:20261201T180000"));
        assert!(out.contains("X-OTHER=R,A"));
        assert!(out.contains("SEQUENCE:1"));
        let back = parse(&out, &tz).unwrap().unwrap().fields;
        assert_eq!(back, new);
    }

    #[test]
    fn every_field_round_trips() {
        let tz = tokyo();
        let old = parse(SAMPLE, &tz).unwrap().unwrap().fields;
        let mut new = Fields {
            summary: "新しい名前".into(),
            body: "1 行目\n2 行目; カンマ, も".into(),
            links: vec!["https://tasks.example.com/t/1".into()],
            url: Some("https://example.com/doc".into()),
            status: "NEEDS-ACTION".into(),
            priority: 5,
            percent: 40,
            start: Some("2026-11-26 09:30".into()),
            due: Some("2026-12-02".into()),
            categories: vec!["a,b".into(), "c".into()],
            parent: None,
            attendees: vec![
                Attendee {
                    uri: "https://id.example.com/users/1".into(),
                    name: "アリス".into(),
                    role: "R".into(),
                    done: false,
                },
                Attendee {
                    uri: "urn:uuid:00000000-0000-0000-0000-000000000001".into(),
                    name: "キャロル".into(),
                    role: "I".into(),
                    done: true,
                },
            ],
        };
        new.settle();
        let out = patch(SAMPLE, &old, &new, &tz, now());
        assert!(
            !out.contains("X-OTHER"),
            "役を変えた人の古い役の書き方は残さない"
        );
        assert!(out.contains("DTSTART:20261126T003000Z"));
        let back = parse(&out, &tz).unwrap().unwrap().fields;
        assert_eq!(back, new);
        // 新しく作ったものも同じに読める
        let built = build("new-1", &new, &tz, now());
        let read = parse(&built, &tz).unwrap().unwrap();
        assert_eq!(read.uid, "new-1");
        assert_eq!(read.fields, new);
    }

    #[test]
    fn completing_drops_the_per_person_done_and_writes_the_time() {
        let tz = tokyo();
        let old = parse(SAMPLE, &tz).unwrap().unwrap().fields;
        let mut new = old.clone();
        new.status = "COMPLETED".into();
        new.settle();
        let out = patch(SAMPLE, &old, &new, &tz, now());
        assert!(out.contains("COMPLETED:20261128T040000Z"));
        assert_eq!(parse(&out, &tz).unwrap().unwrap().fields, new);
    }

    #[test]
    fn long_lines_fold_on_character_boundaries() {
        let tz = tokyo();
        let mut f = Fields::empty();
        f.summary = "あ".repeat(100);
        let out = build("u", &f, &tz, now());
        for line in out.split("\r\n") {
            assert!(line.len() <= FOLD_OCTETS, "{line}");
        }
        assert_eq!(parse(&out, &tz).unwrap().unwrap().fields.summary, f.summary);
    }

    #[test]
    fn description_links() {
        let (body, links) =
            split_description("本文\n\nhttps://a.example/1\n\nhttps://b.example/2\n");
        assert_eq!(body, "本文");
        assert_eq!(links, vec!["https://a.example/1", "https://b.example/2"]);
        assert_eq!(
            join_description(&body, &links),
            "本文\n\nhttps://a.example/1\n\nhttps://b.example/2"
        );
        // 文の中の URL は本文のまま
        let (body, links) = split_description("見て https://a.example/1");
        assert_eq!(body, "見て https://a.example/1");
        assert!(links.is_empty());
        let (body, links) = split_description("https://a.example/1");
        assert_eq!(body, "");
        assert_eq!(links, vec!["https://a.example/1"]);
    }
}
