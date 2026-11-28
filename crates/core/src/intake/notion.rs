//! Notion の「Markdown & CSV」のエクスポート（zip）。
//!
//! 中身は、データベースごとの CSV（`名前 <32 桁>.csv` と、フィルターなしの `名前 <32 桁>_all.csv`）と、
//! 1 行 1 ページの Markdown（`タイトル <32 桁>.md`）。大きいエクスポートは、zip の中に
//! `…-Part-1.zip` のような zip がさらに入っている。

use std::collections::HashMap;
use std::io::{Cursor, Read};

use zip::ZipArchive;

use super::fields::{read_table_with, NotionPage};
use super::{csv, problems_error, Batch, Source};
use crate::text::normalize;
use crate::{Error, Result};

/// zip の中の 1 つのファイルの上限。
const MAX_ENTRY: u64 = 64 * 1024 * 1024;
/// zip の中の zip を開く深さ。
const MAX_DEPTH: usize = 2;

/// 文字列の中の、Notion のページの ID（32 桁の 16 進数、ハイフンありも）。最後に出てきたもの。
pub fn page_id(s: &str) -> Option<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut found = None;
    let mut i = 0;
    while i < chars.len() {
        let mut hex = String::new();
        let mut j = i;
        while j < chars.len() && hex.len() < 32 {
            let c = chars[j];
            if c.is_ascii_hexdigit() {
                hex.push(c.to_ascii_lowercase());
            } else if c == '-' && !hex.is_empty() {
            } else {
                break;
            }
            j += 1;
        }
        let ends = j >= chars.len() || !chars[j].is_ascii_hexdigit();
        if hex.len() == 32 && ends && (i == 0 || !chars[i - 1].is_ascii_hexdigit()) {
            found = Some(hex);
            i = j;
        } else {
            i += 1;
        }
    }
    found
}

/// ページの URL。
pub fn page_url(id: &str) -> String {
    format!("https://www.notion.so/{id}")
}

struct Page {
    title: String,
    id: String,
    body: String,
}

/// `タイトル <32 桁>.md` から、タイトルと ID。
fn split_name(file: &str) -> Option<(String, String)> {
    let stem = file.rsplit('/').next()?.strip_suffix(".md")?;
    let (title, id) = stem.rsplit_once(' ')?;
    let id = page_id(id).filter(|i| i.len() == 32 && id.len() >= 32)?;
    Some((title.trim().to_owned(), id))
}

/// Markdown のページから、見出し（タイトル）とプロパティの行を除いた本文。
fn page_body(text: &str) -> String {
    let text = normalize(text);
    let mut lines = text.lines().peekable();
    if lines.peek().is_some_and(|l| l.starts_with("# ")) {
        lines.next();
    }
    while lines.peek().is_some_and(|l| l.trim().is_empty()) {
        lines.next();
    }
    // プロパティは `名前: 値` の行のかたまり
    let mut rest: Vec<&str> = lines.collect();
    let props = rest
        .iter()
        .take_while(|l| {
            let l = l.trim();
            !l.is_empty()
                && l.split_once(": ")
                    .is_some_and(|(k, _)| !k.is_empty() && k.len() < 60)
        })
        .count();
    if props > 0 && rest.get(props).is_none_or(|l| l.trim().is_empty()) {
        rest.drain(..props);
    }
    rest.join("\n").trim().to_owned()
}

struct Files {
    csvs: Vec<(String, Vec<u8>)>,
    pages: Vec<Page>,
}

fn collect(bytes: &[u8], depth: usize, files: &mut Files) -> Result<()> {
    let mut zip = ZipArchive::new(Cursor::new(bytes))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_owned();
        let lower = name.to_lowercase();
        let wanted = lower.ends_with(".csv") || lower.ends_with(".md") || lower.ends_with(".zip");
        if !wanted {
            continue;
        }
        if entry.size() > MAX_ENTRY {
            return Err(Error::Invalid(format!(
                "zip の中の「{name}」が大きすぎます"
            )));
        }
        let mut data = Vec::new();
        (&mut entry).take(MAX_ENTRY + 1).read_to_end(&mut data)?;
        if lower.ends_with(".zip") {
            if depth < MAX_DEPTH {
                collect(&data, depth + 1, files)?;
            }
        } else if lower.ends_with(".csv") {
            files.csvs.push((name, data));
        } else if let Some((title, id)) = split_name(&name) {
            let text = csv::decode(&data).map_err(|e| Error::Invalid(format!("「{name}」{e}")))?;
            files.pages.push(Page {
                title,
                id,
                body: page_body(&text),
            });
        }
    }
    Ok(())
}

/// Notion の zip を読む。データベースが いくつあっても、全部の行を取り込む。
pub fn read_zip(bytes: &[u8], name: &str) -> Result<Batch> {
    let mut files = Files {
        csvs: Vec::new(),
        pages: Vec::new(),
    };
    collect(bytes, 0, &mut files)
        .map_err(|e| Error::Invalid(format!("「{name}」を zip として読めませんでした: {e}")))?;
    // `_all.csv` があるデータベースは、フィルターのかかった方を読まない
    let all: Vec<String> = files
        .csvs
        .iter()
        .filter_map(|(n, _)| n.strip_suffix("_all.csv").map(|s| format!("{s}.csv")))
        .collect();
    files.csvs.retain(|(n, _)| !all.contains(n));
    if files.csvs.is_empty() {
        if files.pages.is_empty() {
            return Err(Error::Invalid(format!(
                "「{name}」に、Notion のデータベースの CSV がありません。\
                 Notion で「Markdown & CSV」を選んで書き出してください"
            )));
        }
        return Err(Error::Invalid(format!(
            "「{name}」はページだけのエクスポートです。データベースを開いて書き出してください"
        )));
    }
    // タイトルごとのページ。同じタイトルのページが複数あれば、行の順で割り当てる
    let mut by_title: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, p) in files.pages.iter().enumerate() {
        by_title
            .entry(crate::text::normalize_line(&p.title))
            .or_default()
            .push(i);
    }
    let mut used = vec![false; files.pages.len()];
    let mut items = Vec::new();
    let mut problems = Vec::new();
    for (csv_name, data) in &files.csvs {
        let text = match csv::decode(data) {
            Ok(t) => t,
            Err(e) => {
                problems.push(format!("「{csv_name}」{e}"));
                continue;
            }
        };
        let table = csv::parse(&text);
        let short = csv_name.rsplit('/').next().unwrap_or(csv_name);
        let mut page = |title: &str, _line: usize| {
            // Notion はファイル名に使えない文字を除いたり、長いタイトルを切ったりする
            let key = crate::text::normalize_line(title);
            let candidates = by_title.get(&key).or_else(|| {
                let cut: String = key
                    .chars()
                    .filter(|c| !"/\\:*?\"<>|".contains(*c))
                    .collect();
                by_title.get(&cut)
            })?;
            let ix = *candidates.iter().find(|i| !used[**i])?;
            used[ix] = true;
            let p = &files.pages[ix];
            Some(NotionPage {
                key: p.id.clone(),
                url: page_url(&p.id),
                body: p.body.clone(),
            })
        };
        match read_table_with(Source::Notion, &table, None, short, &mut page) {
            Ok(batch) => items.extend(batch.items),
            Err(Error::Invalid(e)) => problems.push(e),
            Err(e) => return Err(e),
        }
    }
    if !problems.is_empty() {
        return Err(problems_error(name, &problems));
    }
    Ok(Batch {
        source: Source::Notion,
        host: None,
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_ids_are_found_in_names_and_urls() {
        let id = "0123456789abcdef0123456789abcdef";
        assert_eq!(page_id(&format!("Task {id}.md")), Some(id.into()));
        assert_eq!(
            page_id("親 (https://www.notion.so/Parent-0123456789ABCDEF0123456789ABCDEF)"),
            Some(id.into())
        );
        assert_eq!(
            page_id("01234567-89ab-cdef-0123-456789abcdef"),
            Some(id.into())
        );
        assert_eq!(page_id("abc"), None);
        assert_eq!(
            split_name(&format!("Export/Tasks/買い物 {id}.md")),
            Some(("買い物".into(), id.into()))
        );
    }

    #[test]
    fn page_body_drops_the_title_and_properties() {
        let md = "# 買い物\n\nStatus: Done\nDue: May 1, 2024\n\n牛乳を買う\n\n- 卵\n";
        assert_eq!(page_body(md), "牛乳を買う\n\n- 卵");
        assert_eq!(page_body("# A\n\n本文だけ"), "本文だけ");
    }
}
