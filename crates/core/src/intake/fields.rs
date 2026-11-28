//! CSV の列を、タスクの項目に対応付ける。列の名前は、各サービスの英語と日本語の画面の名前。

use super::{
    date_of, percent_of, priority_of, problems_error, range_end, split_names, split_tags,
    status_of, Batch, Item, ItemComment, Source,
};
use crate::text::{normalize, normalize_line};
use crate::{Error, Result};

/// 列の名前をそろえる（大文字小文字・前後の空白・BOM を無視）。
fn key(name: &str) -> String {
    normalize_line(name).trim_matches('\u{feff}').to_lowercase()
}

fn has(header: &[String], names: &[&str]) -> bool {
    header.iter().any(|h| names.contains(&key(h).as_str()))
}

const REDMINE_TITLE: &[&str] = &["subject", "題名", "件名"];
const REDMINE_MARK: &[&str] = &["tracker", "トラッカー"];
const BACKLOG_KEY: &[&str] = &["キー", "課題キー", "key", "issue key"];
const BACKLOG_MARK: &[&str] = &[
    "種別",
    "課題種別",
    "issue type",
    "プロジェクトid",
    "project id",
];

/// 1 行目（列の名前）から、どのサービスの CSV かを見分ける。どれでもなければ `None`。
pub fn detect(header: &[String]) -> Option<Source> {
    if has(header, &["task id"]) && has(header, &["task name"]) {
        return Some(Source::ClickUp);
    }
    let first = header.first().map(|h| key(h));
    if first.as_deref() == Some("#") && (has(header, REDMINE_TITLE) || has(header, REDMINE_MARK)) {
        return Some(Source::Redmine);
    }
    if has(header, BACKLOG_KEY)
        && has(header, &["件名", "subject", "summary"])
        && has(header, BACKLOG_MARK)
    {
        return Some(Source::Backlog);
    }
    // Notion のデータベースの CSV は、1 列目がタイトルで、列の名前は人が付けたもの。
    // 決まった印がないので、タイトルらしい列か、状態・期限らしい列があれば Notion とみなす
    if has(header, NOTION_TITLE) || has(header, NOTION_STATUS) || has(header, NOTION_DUE) {
        return Some(Source::Notion);
    }
    None
}

const NOTION_TITLE: &[&str] = &[
    "name",
    "名前",
    "title",
    "タイトル",
    "タスク名",
    "task name",
    "task",
    "タスク",
];
const NOTION_STATUS: &[&str] = &["status", "ステータス", "状態", "進捗"];
const NOTION_DONE: &[&str] = &["done", "完了", "complete", "completed", "済み", "チェック"];
const NOTION_DUE: &[&str] = &[
    "due",
    "due date",
    "deadline",
    "date",
    "期限",
    "期日",
    "締め切り",
    "締切",
    "日付",
];
const NOTION_START: &[&str] = &["start", "start date", "開始", "開始日"];
const NOTION_PEOPLE: &[&str] = &[
    "assignee",
    "assignees",
    "assign",
    "assigned to",
    "owner",
    "person",
    "people",
    "担当",
    "担当者",
];
const NOTION_TAGS: &[&str] = &[
    "tags",
    "tag",
    "タグ",
    "category",
    "カテゴリ",
    "カテゴリー",
    "type",
    "種類",
    "label",
    "labels",
    "ラベル",
];
const NOTION_PRIORITY: &[&str] = &["priority", "優先度", "重要度"];
const NOTION_TEXT: &[&str] = &[
    "description",
    "説明",
    "詳細",
    "memo",
    "メモ",
    "notes",
    "note",
    "備考",
    "内容",
];
const NOTION_URL: &[&str] = &["url", "link", "リンク", "userdefined:url"];
const NOTION_PARENT: &[&str] = &[
    "parent item",
    "parent",
    "parent task",
    "親",
    "親タスク",
    "親アイテム",
];
const NOTION_PERCENT: &[&str] = &["progress", "進捗率", "% done", "percent"];

/// 1 つのサービスの、項目ごとの列の名前。
struct Columns {
    key: &'static [&'static str],
    title: &'static [&'static str],
    description: &'static [&'static str],
    status: &'static [&'static str],
    done: &'static [&'static str],
    priority: &'static [&'static str],
    assignees: &'static [&'static str],
    start: &'static [&'static str],
    due: &'static [&'static str],
    percent: &'static [&'static str],
    parent: &'static [&'static str],
    tags: &'static [&'static str],
    url: &'static [&'static str],
    comments: &'static [&'static str],
}

fn columns(source: Source) -> Columns {
    match source {
        Source::Redmine => Columns {
            key: &["#"],
            title: REDMINE_TITLE,
            description: &["description", "説明"],
            status: &["status", "ステータス"],
            done: &[],
            priority: &["priority", "優先度"],
            assignees: &["assignee", "担当者"],
            start: &["start date", "開始日"],
            due: &["due date", "期日"],
            percent: &["% done", "進捗率"],
            parent: &["parent task", "親チケット"],
            tags: &[
                "tracker",
                "トラッカー",
                "category",
                "カテゴリ",
                "カテゴリー",
            ],
            url: &[],
            comments: &["last notes", "最新の注記", "notes", "注記"],
        },
        Source::Backlog => Columns {
            key: BACKLOG_KEY,
            title: &["件名", "subject", "summary"],
            description: &["詳細", "description"],
            status: &["状態", "status"],
            done: &[],
            priority: &["優先度", "priority"],
            assignees: &["担当者", "assignee"],
            start: &["開始日", "start date"],
            due: &["期限日", "due date"],
            percent: &[],
            parent: &["親課題キー", "親課題", "parent issue key", "parent issue"],
            tags: &[
                "種別",
                "課題種別",
                "issue type",
                "カテゴリー名",
                "カテゴリー",
                "category",
            ],
            url: &[],
            comments: &["コメント", "comment", "comments"],
        },
        Source::ClickUp => Columns {
            key: &["task id"],
            title: &["task name"],
            description: &["task content", "task description", "description"],
            status: &["status"],
            done: &[],
            priority: &["priority"],
            assignees: &["assignees", "assignee"],
            start: &["start date text", "start date"],
            due: &["due date text", "due date"],
            percent: &[],
            parent: &["parent id"],
            tags: &["tags"],
            url: &["task url", "url"],
            comments: &["comments"],
        },
        Source::Notion | Source::SlackReminder | Source::SlackList => Columns {
            key: &[],
            title: NOTION_TITLE,
            description: NOTION_TEXT,
            status: NOTION_STATUS,
            done: NOTION_DONE,
            priority: NOTION_PRIORITY,
            assignees: NOTION_PEOPLE,
            start: NOTION_START,
            due: NOTION_DUE,
            percent: NOTION_PERCENT,
            parent: NOTION_PARENT,
            tags: NOTION_TAGS,
            url: NOTION_URL,
            comments: &["comments", "コメント"],
        },
    }
}

/// 列の位置。
fn find(header: &[String], names: &[&str]) -> Option<usize> {
    // 名前の並び順を優先する（ClickUp の `Due Date Text` を `Due Date` より先に読む）
    names
        .iter()
        .find_map(|n| header.iter().position(|h| key(h) == *n))
}

fn find_all(header: &[String], names: &[&str]) -> Vec<usize> {
    header
        .iter()
        .enumerate()
        .filter(|(_, h)| names.contains(&key(h).as_str()))
        .map(|(i, _)| i)
        .collect()
}

/// Notion の CSV の 1 行目に付く、ページへの印。
pub(super) struct NotionPage {
    pub key: String,
    pub url: String,
    pub body: String,
}

/// 表を取り込む中身にする。`base` は Redmine・Backlog のサーバーの URL。
pub fn read_table(
    source: Source,
    table: &[Vec<String>],
    base: Option<&str>,
    name: &str,
) -> Result<Batch> {
    read_table_with(source, table, base, name, &mut |_, _| None)
}

/// `page` は、Notion の zip の中のページ（タイトルと行から探す）。
pub(super) fn read_table_with(
    source: Source,
    table: &[Vec<String>],
    base: Option<&str>,
    name: &str,
    page: &mut dyn FnMut(&str, usize) -> Option<NotionPage>,
) -> Result<Batch> {
    let Some((header, rows)) = table.split_first() else {
        return Err(Error::Invalid(format!("「{name}」は空です")));
    };
    let cols = columns(source);
    let title_col = find(header, cols.title).or(match source {
        // Notion はタイトルの列の名前を人が変えられる。1 列目がタイトル
        Source::Notion => Some(0),
        _ => None,
    });
    let Some(title_col) = title_col else {
        return Err(Error::Invalid(format!(
            "「{name}」にタイトルの列（{}）がありません",
            cols.title.first().copied().unwrap_or_default()
        )));
    };
    let key_col = find(header, cols.key);
    if key_col.is_none() && !cols.key.is_empty() {
        return Err(Error::Invalid(format!(
            "「{name}」に ID の列（{}）がありません",
            cols.key.join("・")
        )));
    }
    let at = |cols: &[&str]| find(header, cols);
    let (desc, status, done, prio, people, start, due, percent, parent, url) = (
        at(cols.description),
        at(cols.status),
        at(cols.done),
        at(cols.priority),
        at(cols.assignees),
        at(cols.start),
        at(cols.due),
        at(cols.percent),
        at(cols.parent),
        at(cols.url),
    );
    let tags = find_all(header, cols.tags);
    let comment_cols = find_all(header, cols.comments);
    let mut items = Vec::new();
    let mut problems = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        // 1 行目は列の名前なので、データは 2 行目から
        let line = i + 2;
        let cell = |c: Option<usize>| {
            c.and_then(|c| row.get(c))
                .map(String::as_str)
                .unwrap_or_default()
        };
        let title = normalize_line(cell(Some(title_col)));
        if title.is_empty() && row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        if title.is_empty() {
            problems.push(format!("{line} 行目: タイトルが空です"));
            continue;
        }
        let mut bad = Vec::new();
        let mut date = |what: &str, c: Option<usize>| match date_of(cell(c)) {
            Ok(d) => d,
            Err(_) => {
                bad.push(format!("{what}「{}」", cell(c)));
                None
            }
        };
        let start_value = date("開始", start);
        let mut due_value = date("期限", due);
        // Notion の日付が範囲なら、始まりを開始、終わりを期限にする
        let mut start_value = start_value;
        if source == Source::Notion && start.is_none() {
            if let Ok(Some(end)) = range_end(cell(due)) {
                start_value = due_value.take();
                due_value = Some(end);
            }
        }
        let percent_value = match percent_of(cell(percent)) {
            Ok(p) => p,
            Err(_) => {
                bad.push(format!("進捗「{}」", cell(percent)));
                0
            }
        };
        if !bad.is_empty() {
            problems.push(format!(
                "{line} 行目「{title}」: {} を読めません",
                bad.join("・")
            ));
            continue;
        }
        let mut status_value = status_of(cell(status));
        if status.is_none() && done.is_some() {
            status_value = status_of(cell(done));
        }
        let raw_key = normalize_line(cell(key_col));
        let key = match source {
            Source::Redmine => raw_key.trim_start_matches('#').to_owned(),
            _ => raw_key,
        };
        if key_col.is_some() && key.is_empty() {
            problems.push(format!("{line} 行目「{title}」: ID が空です"));
            continue;
        }
        let mut urls = Vec::new();
        let mut description = normalize(cell(desc)).trim().to_owned();
        let mut key = key;
        if source == Source::Notion {
            if let Some(p) = page(&title, line) {
                key = p.key;
                urls.push(p.url);
                if description.is_empty() {
                    description = p.body;
                }
            } else {
                // ページの ID がない CSV だけのときは、行の中身から ID を作る（同じ行なら同じ ID）
                key = format!(
                    "row:{}",
                    crate::attachment::hash_of(row.join("\u{1f}").as_bytes())
                );
            }
        }
        match (source, base) {
            (Source::Redmine, Some(base)) => urls.push(format!("{base}/issues/{key}")),
            (Source::Backlog, Some(base)) => urls.push(format!("{base}/view/{key}")),
            (Source::ClickUp, _) if cell(url).trim().is_empty() => {
                urls.push(format!("https://app.clickup.com/t/{key}"))
            }
            _ => {}
        }
        for u in split_urls(cell(url)) {
            if !urls.contains(&u) {
                urls.push(u);
            }
        }
        let mut categories = Vec::new();
        for c in &tags {
            for t in split_tags(cell(Some(*c))) {
                if !categories.contains(&t) {
                    categories.push(t);
                }
            }
        }
        let parent_value = parent_key(source, cell(parent));
        let comments = comment_cols
            .iter()
            .flat_map(|c| comments_of(cell(Some(*c))))
            .collect();
        items.push(Item {
            key,
            line: Some(line),
            title,
            description,
            status: status_value,
            priority: priority_of(cell(prio)),
            start: start_value,
            due: due_value,
            percent: percent_value,
            parent: parent_value,
            assignees: split_names(cell(people)),
            categories,
            urls,
            comments,
        });
    }
    if !problems.is_empty() {
        return Err(problems_error(name, &problems));
    }
    Ok(Batch {
        source,
        host: None,
        items,
    })
}

/// 親の欄を、親の ID にする。Redmine は `#12` や `機能 #12: 題名`、Notion は
/// `題名 (https://www.notion.so/題名-<32 桁>)`。
fn parent_key(source: Source, raw: &str) -> Option<String> {
    let s = normalize_line(raw);
    if s.is_empty() {
        return None;
    }
    match source {
        Source::Redmine => {
            let after = s.rsplit('#').next().unwrap_or(&s);
            let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
            (!digits.is_empty()).then_some(digits)
        }
        Source::Notion => super::notion::page_id(&s),
        _ => Some(s),
    }
}

/// URL の欄。空白か改行かカンマで区切ったいくつか。
fn split_urls(raw: &str) -> Vec<String> {
    raw.split(|c: char| c.is_whitespace() || c == ',')
        .map(str::trim)
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
        .map(str::to_owned)
        .collect()
}

/// コメントの欄。ClickUp は JSON の配列（`[{"text": …, "by": …, "date": …}]`）のことがある。
fn comments_of(raw: &str) -> Vec<ItemComment> {
    let s = raw.trim();
    if s.is_empty() || s == "[]" {
        return Vec::new();
    }
    if let Ok(serde_json::Value::Array(list)) = serde_json::from_str::<serde_json::Value>(s) {
        let text = |v: &serde_json::Value, keys: &[&str]| {
            keys.iter()
                .find_map(|k| v.get(*k).and_then(|x| x.as_str()).map(str::to_owned))
                .unwrap_or_default()
        };
        return list
            .iter()
            .filter_map(|v| {
                let body = text(v, &["text", "comment", "comment_text", "body"]);
                (!body.trim().is_empty()).then(|| ItemComment {
                    author: text(v, &["by", "user", "author", "username"]),
                    body,
                    at: v
                        .get("date")
                        .and_then(|d| d.as_i64().or_else(|| d.as_str()?.parse().ok())),
                })
            })
            .collect();
    }
    vec![ItemComment {
        author: String::new(),
        body: s.to_owned(),
        at: None,
    }]
}
