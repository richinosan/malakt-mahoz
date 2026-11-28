//! ほかのタスク管理ツール（Notion・Redmine・Backlog・ClickUp・Slack）からの取り込み。
//!
//! - ファイル（CSV、Notion の「Markdown & CSV」の zip）は、拡張子ではなく中身で形式を見分ける
//! - 読めない行が 1 つでもあれば、何も取り込まずに、読めなかった行を全部挙げてエラーにする
//! - 取り込んだものは、元のサービスの ID（[`Origin`]）を残す。同じものをもう一度取り込んでも、
//!   新しいタスクは作らない（取り込み済みとして数える）
//! - 元の URL が分かるものは、タスクの URL 欄に入れる

mod csv;
mod fields;
mod notion;
pub mod slack;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::model::*;
use crate::store::{
    insert_comment, insert_person, insert_task, put_person_workspaces, record_created,
};
use crate::text::{normalize, normalize_line};
use crate::{Error, Result, Store};

pub use csv::decode;

/// 1 回に取り込めるタスクの数。壊れたファイルでデータベースを膨らませないため。
pub const MAX_ITEMS: usize = 20_000;
/// 読むファイルの大きさの上限。
pub const MAX_FILE_BYTES: u64 = 200 * 1024 * 1024;

/// 取り込み元のサービス。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Notion,
    Redmine,
    Backlog,
    #[serde(rename = "clickup")]
    ClickUp,
    /// Slack のリマインダー
    SlackReminder,
    /// Slack リストの項目
    SlackList,
}

impl Source {
    pub const ALL: [Source; 6] = [
        Source::Notion,
        Source::Redmine,
        Source::Backlog,
        Source::ClickUp,
        Source::SlackReminder,
        Source::SlackList,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Source::Notion => "notion",
            Source::Redmine => "redmine",
            Source::Backlog => "backlog",
            Source::ClickUp => "clickup",
            Source::SlackReminder => "slack_reminder",
            Source::SlackList => "slack_list",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Source::Notion => "Notion",
            Source::Redmine => "Redmine",
            Source::Backlog => "Backlog",
            Source::ClickUp => "ClickUp",
            Source::SlackReminder => "Slack のリマインダー",
            Source::SlackList => "Slack リスト",
        }
    }

    /// ファイルから取り込めるサービス。
    pub fn from_file(self) -> bool {
        matches!(
            self,
            Source::Notion | Source::Redmine | Source::Backlog | Source::ClickUp
        )
    }

    /// サーバーの URL を入れると、チケットの URL を作れるサービス（CSV に URL がないもの）。
    pub fn needs_base_url(self) -> bool {
        matches!(self, Source::Redmine | Source::Backlog)
    }
}

/// 取り込んだタスクが、どのサービスのどれから来たか。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// [`Source::as_str`]。サーバーが分かるときは `redmine:ホスト名` のようにホスト名を付ける
    pub source: String,
    /// 元のサービスでの ID（Redmine のチケット番号、Backlog の課題キーなど）
    pub key: String,
    pub task_id: Id,
}

/// 取り込む 1 件のコメント。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemComment {
    pub author: String,
    pub body: String,
    /// 書いた時刻（ミリ秒）。分からなければ取り込んだ時刻。
    pub at: Option<i64>,
}

/// 取り込む 1 件。ファイルの 1 行や、API の 1 件。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Item {
    /// 元のサービスでの ID。重複を見分けるのに使う
    pub key: String,
    /// 何行目か（ファイルのとき）。エラーの文に出す
    pub line: Option<usize>,
    pub title: String,
    pub description: String,
    pub status: Status,
    pub priority: u8,
    pub start: Option<String>,
    pub due: Option<String>,
    pub percent: u8,
    /// 親の `key`
    pub parent: Option<String>,
    /// 担当者の名前
    pub assignees: Vec<String>,
    pub categories: Vec<String>,
    pub urls: Vec<String>,
    pub comments: Vec<ItemComment>,
}

/// 読んだ取り込みの中身。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub source: Source,
    /// サーバーのホスト名（分かるときだけ）。重複を見分けるときに、ほかのサーバーと分ける
    pub host: Option<String>,
    pub items: Vec<Item>,
}

impl Batch {
    fn origin_source(&self) -> String {
        match &self.host {
            Some(host) => format!("{}:{host}", self.source.as_str()),
            None => self.source.as_str().to_owned(),
        }
    }
}

/// 取り込みの結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// 新しく作ったタスク
    pub added: usize,
    /// 前に取り込んだので作らなかったタスク
    pub skipped: usize,
    /// 新しく足した担当者の名前
    pub people: Vec<String>,
    /// 作ったタスクの id（取り込んだ順）
    pub task_ids: Vec<Id>,
}

impl Report {
    /// 画面やツールの答えに出す文。
    pub fn summary(&self, source: Source) -> String {
        let mut text = format!(
            "{} から {} 件のタスクを取り込みました",
            source.label(),
            self.added
        );
        if self.skipped > 0 {
            text.push_str(&format!("（{} 件は取り込み済み）", self.skipped));
        }
        if !self.people.is_empty() {
            text.push_str(&format!(
                "。担当者 {} を追加しました",
                self.people
                    .iter()
                    .map(|n| format!("「{n}」"))
                    .collect::<String>()
            ));
        }
        text.push('。');
        text
    }
}

/// ファイルを読むときの指定。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileOptions {
    /// 決めた取り込み元。`None` なら中身で見分ける
    pub source: Option<Source>,
    /// Redmine・Backlog のサーバーの URL。チケットの URL を作るのに使う
    pub base_url: Option<String>,
}

/// ファイルを読んで、取り込む中身にする。形式は中身で見分ける。
pub fn read_file(path: impl AsRef<Path>, options: &FileOptions) -> Result<Batch> {
    let path = path.as_ref();
    let size = std::fs::metadata(path)?.len();
    if size > MAX_FILE_BYTES {
        return Err(Error::Invalid(format!(
            "「{}」が大きすぎます（{} MB まで）",
            file_name(path),
            MAX_FILE_BYTES / 1024 / 1024
        )));
    }
    let bytes = std::fs::read(path)?;
    read_bytes(&bytes, &file_name(path), options)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// ファイルの中身を読む。`name` はエラーの文に出すファイル名。
pub fn read_bytes(bytes: &[u8], name: &str, options: &FileOptions) -> Result<Batch> {
    if options.source.is_some_and(|s| !s.from_file()) {
        return Err(Error::Invalid(format!(
            "{} はファイルから取り込めません",
            options.source.map(Source::label).unwrap_or_default()
        )));
    }
    let base = options
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .map(clean_base_url)
        .transpose()?;
    if bytes.starts_with(b"PK\x03\x04") {
        if options.source.is_some_and(|s| s != Source::Notion) {
            return Err(Error::Invalid(format!(
                "「{name}」は zip です。{} の CSV を選んでください",
                options.source.map(Source::label).unwrap_or_default()
            )));
        }
        return notion::read_zip(bytes, name);
    }
    let text = decode(bytes).map_err(|e| Error::Invalid(format!("「{name}」{e}")))?;
    let table = csv::parse(&text);
    let Some(header) = table.first() else {
        return Err(Error::Invalid(format!("「{name}」は空です")));
    };
    let source = match options.source {
        Some(s) => s,
        None => fields::detect(header).ok_or_else(|| {
            Error::Invalid(format!(
                "「{name}」を読めませんでした。Notion・Redmine・Backlog・ClickUp の CSV か、\
                 Notion の zip、Malakt Mahoz のエクスポートを選んでください"
            ))
        })?,
    };
    let mut batch = fields::read_table(source, &table, base.as_deref(), name)?;
    if source.needs_base_url() {
        batch.host = base.as_deref().and_then(host_of);
    }
    Ok(batch)
}

/// サーバーの URL を `https://host/path` の形にする（末尾の `/` なし）。
pub fn clean_base_url(input: &str) -> Result<String> {
    let input = input.trim().trim_end_matches('/');
    let url = if input.contains("://") {
        input.to_owned()
    } else {
        format!("https://{input}")
    };
    if !(url.starts_with("https://") || url.starts_with("http://")) || host_of(&url).is_none() {
        return Err(Error::Invalid(format!(
            "サーバーの URL「{input}」が正しくありません"
        )));
    }
    Ok(url)
}

/// URL のホスト名。
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit('@').next()?;
    (!host.is_empty() && !host.contains(char::is_whitespace)).then(|| host.to_lowercase())
}

/// 読めなかった行を、まとめて 1 つのエラーにする。
pub(crate) fn problems_error(name: &str, problems: &[String]) -> Error {
    const SHOWN: usize = 20;
    let mut text = format!("「{name}」の次の行を読めませんでした。何も取り込んでいません。");
    for p in problems.iter().take(SHOWN) {
        text.push('\n');
        text.push_str(p);
    }
    if problems.len() > SHOWN {
        text.push_str(&format!("\nほか {} 行", problems.len() - SHOWN));
    }
    Error::Invalid(text)
}

/// 状態の名前（サービスの標準の名前と、よくある書き方）を、このアプリの状態にする。
pub fn status_of(raw: &str) -> Status {
    let s = normalize_line(raw).to_lowercase();
    if s.is_empty() {
        return Status::NeedsAction;
    }
    const CANCELLED: [&str; 9] = [
        "却下",
        "中止",
        "キャンセル",
        "取り下げ",
        "rejected",
        "cancel",
        "won't",
        "wont fix",
        "対応しない",
    ];
    const DONE: [&str; 12] = [
        "完了",
        "終了",
        "クローズ",
        "済",
        "done",
        "complete",
        "closed",
        "finished",
        "archived",
        "true",
        "yes",
        "✓",
    ];
    const DOING: [&str; 14] = [
        "進行中",
        "処理中",
        "処理済",
        "対応中",
        "作業中",
        "確認",
        "レビュー",
        "解決",
        "フィードバック",
        "in progress",
        "doing",
        "review",
        "resolved",
        "feedback",
    ];
    if CANCELLED.iter().any(|k| s.contains(k)) {
        Status::Cancelled
    } else if s.starts_with("未") || s.starts_with("not ") || s.contains("処理済") {
        // Backlog の「処理済み」は、完了の前の確認待ち
        if s.contains("処理済") {
            return Status::InProcess;
        }
        // 「未完了」「not done」は完了ではない
        Status::NeedsAction
    } else if DONE.iter().any(|k| s.contains(k)) {
        Status::Completed
    } else if DOING.iter().any(|k| s.contains(k)) || s == "active" || s == "started" {
        Status::InProcess
    } else {
        Status::NeedsAction
    }
}

/// 優先度の名前を、1（最高）〜 9（最低）にする。読めなければ 0（なし）。
pub fn priority_of(raw: &str) -> u8 {
    let s = normalize_line(raw).to_lowercase();
    if s.is_empty() {
        return 0;
    }
    if let Ok(n) = s.parse::<u8>() {
        // ClickUp は 1 = Urgent … 4 = Low
        return match n {
            1 => 1,
            2 => 3,
            3 => 5,
            4 => 9,
            _ => 0,
        };
    }
    const TOP: [&str; 9] = [
        "urgent",
        "immediate",
        "highest",
        "critical",
        "緊急",
        "至急",
        "今すぐ",
        "最高",
        "最優先",
    ];
    if TOP.iter().any(|k| s.contains(k)) {
        1
    } else if s.contains("low") || s.contains('低') {
        9
    } else if s.contains("high") || s.contains('高') || s.contains("急いで") {
        3
    } else if ["normal", "medium", "mid", "中", "通常", "普通"]
        .iter()
        .any(|k| s.contains(k))
    {
        5
    } else {
        0
    }
}

/// 担当者の欄を名前に分ける。`[a, b]` のような括弧と、`、` `;` の区切りも読む。
pub fn split_names(raw: &str) -> Vec<String> {
    let s = normalize_line(raw);
    let s = s.trim().trim_start_matches('[').trim_end_matches(']');
    let mut names: Vec<String> = Vec::new();
    for part in s.split([',', '、', ';', '，']) {
        let name = part.trim().trim_matches(['"', '\'', '@']).trim().to_owned();
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// 値を読めなかった。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unreadable;

/// 日付や日時を `YYYY-MM-DD` か `YYYY-MM-DD HH:MM` にする。空なら `Ok(None)`、読めなければ `Err`。
///
/// 読む書き方: `2024-05-01`、`2024-05-01T10:00:00Z`、`2024/5/1 10:00`、`2024年5月1日`、
/// `May 1, 2024`、`Wednesday, May 1st 2024`、`1 May 2024`、ミリ秒か秒の UNIX 時刻。
pub fn date_of(raw: &str) -> std::result::Result<Option<String>, Unreadable> {
    let s = normalize_line(raw);
    let s = s.trim().trim_start_matches('@').trim();
    if s.is_empty() {
        return Ok(None);
    }
    // 範囲（Notion の `A → B`）なら始まりを読む。終わりは [`range_of`] で
    let s = s.split('→').next().unwrap_or(s).trim();
    // タイムゾーンの注記 `(GMT+9)` は外す
    let s = match s.find('(') {
        Some(i) => s[..i].trim(),
        None => s,
    };
    if s.chars().all(|c| c.is_ascii_digit()) && s.len() >= 9 {
        let n: i64 = s.parse().map_err(|_| Unreadable)?;
        let ms = if s.len() >= 12 { n } else { n * 1000 };
        let dt = chrono::DateTime::from_timestamp_millis(ms).ok_or(Unreadable)?;
        let local = dt.with_timezone(&chrono::Local);
        return Ok(Some(local.format("%Y-%m-%d").to_string()));
    }
    numeric_date(s)
        .or_else(|| english_date(s))
        .map(Some)
        .ok_or(Unreadable)
}

/// 範囲（`A → B`）の終わり。範囲でなければ `None`。
pub fn range_end(raw: &str) -> std::result::Result<Option<String>, Unreadable> {
    match raw.split_once('→') {
        Some((_, end)) => date_of(end),
        None => Ok(None),
    }
}

fn join_date(y: i32, m: u32, d: u32, time: Option<(u32, u32)>) -> Option<String> {
    let date = chrono::NaiveDate::from_ymd_opt(y, m, d)?;
    Some(match time {
        Some((h, min)) if h < 24 && min < 60 && (h, min) != (0, 0) => {
            format!("{} {h:02}:{min:02}", date.format("%Y-%m-%d"))
        }
        _ => date.format("%Y-%m-%d").to_string(),
    })
}

/// `10:00`、`10:00:00`、`10:00 AM`、`午後3:00` を時と分にする。
fn time_of(s: &str) -> Option<(u32, u32)> {
    let s = s.trim().trim_start_matches(['T', ' ']);
    if s.is_empty() {
        return None;
    }
    let lower = s.to_lowercase();
    let (pm, am) = (
        lower.contains("pm") || lower.contains("午後"),
        lower.contains("am") || lower.contains("午前"),
    );
    let digits: String = lower
        .trim_start_matches("午前")
        .trim_start_matches("午後")
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ':')
        .collect();
    let mut parts = digits.split(':');
    let h: u32 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let h = match (pm, am) {
        (true, _) if h < 12 => h + 12,
        (_, true) if h == 12 => 0,
        _ => h,
    };
    Some((h, m))
}

/// `2024-05-01`、`2024/5/1`、`2024.5.1`、`2024年5月1日` と、その後の時刻。
fn numeric_date(s: &str) -> Option<String> {
    let bytes: Vec<char> = s.chars().collect();
    let mut nums: Vec<u32> = Vec::new();
    let mut i = 0;
    while i < bytes.len() && nums.len() < 3 {
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if start == i {
            return None;
        }
        nums.push(bytes[start..i].iter().collect::<String>().parse().ok()?);
        if nums.len() < 3 {
            match bytes.get(i) {
                Some('-' | '/' | '.' | '年' | '月') => i += 1,
                _ => return None,
            }
        } else if bytes.get(i) == Some(&'日') {
            i += 1;
        }
    }
    if nums.len() != 3 || nums[0] < 1000 {
        return None;
    }
    let rest: String = bytes[i..].iter().collect();
    let rest = rest.trim();
    // `Z` や `+09:00` の付いた ISO 8601 の日時は、この PC の時刻にする
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&s.replace(' ', "T")) {
        let local = dt.with_timezone(&chrono::Local);
        return join_date(
            chrono::Datelike::year(&local),
            chrono::Datelike::month(&local),
            chrono::Datelike::day(&local),
            Some((
                chrono::Timelike::hour(&local),
                chrono::Timelike::minute(&local),
            )),
        );
    }
    let rest = rest.trim_start_matches(['(', ')', '（', '）']);
    let rest = rest
        .trim_start_matches(|c: char| "月火水木金土日曜()（） ".contains(c))
        .trim();
    let time = if rest.is_empty() {
        None
    } else {
        Some(time_of(rest)?)
    };
    join_date(nums[0] as i32, nums[1], nums[2], time)
}

/// `May 1, 2024`、`Wednesday, May 1st 2024 10:00 am`、`1 May 2024`。
fn english_date(s: &str) -> Option<String> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let lower = s.to_lowercase().replace(',', " ");
    let words: Vec<&str> = lower.split_whitespace().collect();
    let mut month = None;
    let mut day = None;
    let mut year = None;
    let mut used = 0;
    for (i, w) in words.iter().enumerate() {
        if month.is_none() {
            if let Some(m) = MONTHS.iter().position(|m| w.starts_with(m)) {
                month = Some(m as u32 + 1);
                used = i + 1;
                continue;
            }
        }
        let n = w.trim_end_matches(|c: char| c.is_ascii_alphabetic());
        if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) && !w.contains(':') {
            let v: u32 = n.parse().ok()?;
            if v >= 1000 && year.is_none() {
                year = Some(v as i32);
                used = i + 1;
            } else if day.is_none() && v <= 31 {
                day = Some(v);
                used = i + 1;
            }
        }
        if month.is_some() && day.is_some() && year.is_some() {
            break;
        }
    }
    let (month, day, year) = (month?, day?, year?);
    let rest = words[used..].join(" ");
    let time = if rest.trim().is_empty() {
        None
    } else {
        Some(time_of(&rest)?)
    };
    join_date(year, month, day, time)
}

/// 進捗（`50`、`50%`、`0.5`）を 0〜100 にする。
pub fn percent_of(raw: &str) -> std::result::Result<u8, Unreadable> {
    let s = normalize_line(raw);
    let s = s.trim().trim_end_matches('%').trim();
    if s.is_empty() {
        return Ok(0);
    }
    let v: f64 = s.parse().map_err(|_| Unreadable)?;
    let v = if v > 0.0 && v < 1.0 && s.contains('.') {
        v * 100.0
    } else {
        v
    };
    if !(0.0..=100.0).contains(&v) {
        return Err(Unreadable);
    }
    Ok(v.round() as u8)
}

/// 1 行の文字列の、タグの区切り。
pub fn split_tags(raw: &str) -> Vec<String> {
    split_names(raw)
}

impl Store {
    /// 取り込みの中身を `workspace_id`（`None` はどこにも入れない）に入れる。
    /// 全部入れるか、何も入れないか（1 つのトランザクション）。
    pub fn import_batch(&mut self, workspace_id: Option<&str>, batch: &Batch) -> Result<Report> {
        if batch.items.len() > MAX_ITEMS {
            return Err(Error::Invalid(format!(
                "一度に取り込めるのは {MAX_ITEMS} 件までです（{} 件あります）",
                batch.items.len()
            )));
        }
        let root = match workspace_id {
            Some(w) => {
                self.workspace(w)?;
                Some(self.workspace_root(w)?)
            }
            None => None,
        };
        let source = batch.origin_source();
        let people = self.people()?;
        let before = self.open_tasks_of_self()?;
        let actor = self.actor.clone();
        let now = now_ms();

        // 担当者は、このワークスペースで使える同じ名前の人。いなければ足す
        let usable = |p: &Person| {
            p.workspace_ids.is_empty()
                || root
                    .as_ref()
                    .is_some_and(|r| p.workspace_ids.iter().any(|w| w == r))
        };
        let mut by_name: HashMap<String, Id> = HashMap::new();
        for p in people.iter().filter(|p| usable(p)) {
            by_name
                .entry(normalize_line(&p.name).to_lowercase())
                .or_insert_with(|| p.id.clone());
        }

        let tx = self.conn.transaction()?;
        tx.pragma_update(None, "defer_foreign_keys", "ON")?;
        let known = |key: &str| -> rusqlite::Result<Option<Id>> {
            tx.prepare_cached(
                "SELECT i.task_id FROM import_keys i JOIN tasks t ON t.id = i.task_id \
                 WHERE i.source = ?1 AND i.key = ?2",
            )?
            .query_row(params![source, key], |r| r.get(0))
            .optional()
        };
        let mut report = Report::default();
        let mut ids: HashMap<String, Id> = HashMap::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut new_people: Vec<Person> = Vec::new();
        let mut fresh: Vec<(&Item, Id)> = Vec::new();
        for item in &batch.items {
            if !item.key.is_empty() && !seen.insert(item.key.as_str()) {
                continue;
            }
            if !item.key.is_empty() {
                if let Some(id) = known(&item.key)? {
                    ids.insert(item.key.clone(), id);
                    report.skipped += 1;
                    continue;
                }
            }
            let id = new_id();
            if !item.key.is_empty() {
                ids.insert(item.key.clone(), id.clone());
            }
            fresh.push((item, id));
        }
        let first_position: i64 = tx.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM tasks WHERE workspace_id IS ?1",
            params![workspace_id],
            |r| r.get(0),
        )?;
        let mut person_of = |name: &str, new_people: &mut Vec<Person>| -> Result<Id> {
            let name = normalize_line(name);
            let lookup = name.to_lowercase();
            if let Some(id) = by_name.get(&lookup) {
                return Ok(id.clone());
            }
            let person = Person {
                id: new_id(),
                name: crate::store::required_line(&name, "担当者の名前")?,
                is_self: false,
                note: String::new(),
                created_at: now,
                updated_at: now,
                workspace_ids: root.iter().cloned().collect(),
            };
            by_name.insert(lookup, person.id.clone());
            let id = person.id.clone();
            new_people.push(person);
            Ok(id)
        };
        let mut tasks: Vec<(Task, &Item)> = Vec::new();
        for (n, (item, id)) in fresh.iter().enumerate() {
            let mut assignee_ids: Vec<Id> = Vec::new();
            for name in &item.assignees {
                let pid = person_of(name, &mut new_people)?;
                if !assignee_ids.contains(&pid) {
                    assignee_ids.push(pid);
                }
            }
            let draft = TaskDraft {
                title: item.title.clone(),
                description: item.description.clone(),
                status: item.status,
                priority: item.priority,
                start: item.start.clone(),
                due: item.due.clone(),
                percent: if item.status == Status::Completed {
                    100
                } else {
                    item.percent
                },
                parent_id: None,
                assignee_ids,
                categories: item.categories.clone(),
                urls: item.urls.clone(),
                estimate: None,
                time_basis: None,
            };
            let draft = crate::store::clean_draft(&draft).map_err(|e| {
                Error::Invalid(format!(
                    "{}「{}」: {e}。何も取り込んでいません",
                    item.line.map(|l| format!("{l} 行目")).unwrap_or_default(),
                    item.title
                ))
            })?;
            let parent_id = item
                .parent
                .as_deref()
                .filter(|p| *p != item.key)
                .and_then(|p| ids.get(p).cloned());
            let task = Task {
                id: id.clone(),
                workspace_id: workspace_id.map(str::to_owned),
                title: draft.title,
                description: draft.description,
                status: draft.status,
                priority: draft.priority,
                start: draft.start,
                due: draft.due,
                percent: draft.percent,
                parent_id,
                assignee_roles: vec![Roles::R; draft.assignee_ids.len()],
                assignee_done: vec![
                    (draft.status == Status::Completed).then_some(now);
                    draft.assignee_ids.len()
                ],
                assigned_at: (!draft.assignee_ids.is_empty()).then_some(now),
                assignee_ids: draft.assignee_ids,
                categories: draft.categories,
                urls: draft.urls,
                linked_workspace_ids: Vec::new(),
                position: first_position + n as i64,
                created_at: now,
                updated_at: now,
                completed_at: (draft.status == Status::Completed).then_some(now),
                created_by: actor.clone(),
                updated_by: actor.clone(),
                estimate: None,
                time_basis: None,
                started_at: (draft.status == Status::InProcess).then_some(now),
                pr: None,
            };
            tasks.push((task, item));
        }
        // 親子が輪になっていたら、輪を切る（先に出てきたほうを親のないタスクにする）
        let parent_of: HashMap<Id, Option<Id>> = tasks
            .iter()
            .map(|(t, _)| (t.id.clone(), t.parent_id.clone()))
            .collect();
        for (t, _) in &mut tasks {
            let mut at = t.parent_id.clone();
            let mut steps = 0;
            while let Some(p) = at {
                if p == t.id || steps > parent_of.len() {
                    t.parent_id = None;
                    break;
                }
                at = parent_of.get(&p).cloned().flatten();
                steps += 1;
            }
        }
        for p in &new_people {
            insert_person(&tx, p)?;
            put_person_workspaces(&tx, p)?;
            report.people.push(p.name.clone());
        }
        for (task, item) in &tasks {
            insert_task(&tx, task)?;
            record_created(&tx, task, &actor)?;
            if !item.key.is_empty() {
                tx.prepare_cached(
                    "INSERT OR REPLACE INTO import_keys(source, key, task_id) VALUES (?1, ?2, ?3)",
                )?
                .execute(params![source, item.key, task.id])?;
            }
            for c in &item.comments {
                let mut body = normalize(&c.body).trim().to_owned();
                if body.is_empty() {
                    continue;
                }
                let author = normalize_line(&c.author);
                let author_id = (!author.is_empty())
                    .then(|| by_name.get(&author.to_lowercase()).cloned())
                    .flatten();
                if author_id.is_none() && !author.is_empty() {
                    body = format!("{author}:\n{body}");
                }
                let at = c.at.unwrap_or(now);
                let comment = Comment {
                    id: new_id(),
                    task_id: task.id.clone(),
                    author_id,
                    mentions: Vec::new(),
                    body,
                    pinned: false,
                    created_at: at,
                    updated_at: at,
                    status: None,
                    review: false,
                    anchor: None,
                    reply_to: None,
                    resolved_at: None,
                    actor: actor.clone(),
                };
                insert_comment(&tx, &comment)?;
            }
            report.task_ids.push(task.id.clone());
        }
        report.added = tasks.len();
        tx.commit()?;
        self.notify_newly_assigned(&before)?;
        Ok(report)
    }

    /// 取り込んだタスクの元（エクスポート用）。
    pub fn import_origins(&self) -> Result<Vec<Origin>> {
        let mut stmt = self
            .conn
            .prepare("SELECT source, key, task_id FROM import_keys ORDER BY source, key")?;
        let rows = stmt.query_map([], |r| {
            Ok(Origin {
                source: r.get(0)?,
                key: r.get(1)?,
                task_id: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// タスクの元。取り込んだものでなければ `None`。
    pub fn task_origin(&self, task_id: &str) -> Result<Option<Origin>> {
        Ok(self
            .conn
            .query_row(
                "SELECT source, key, task_id FROM import_keys WHERE task_id = ?1 LIMIT 1",
                params![task_id],
                |r| {
                    Ok(Origin {
                        source: r.get(0)?,
                        key: r.get(1)?,
                        task_id: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }
}

/// エクスポートの取り込みで、元の記録を入れる。
pub(crate) fn save_origin(conn: &rusqlite::Connection, o: &Origin) -> rusqlite::Result<usize> {
    conn.prepare_cached(
        "INSERT OR REPLACE INTO import_keys(source, key, task_id) VALUES (?1, ?2, ?3)",
    )?
    .execute(params![o.source, o.key, o.task_id])
}
