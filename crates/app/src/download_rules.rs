//! ダウンロードの保存先の決まり（Chrome の「ダウンロード」の設定と同じ項目に、日付のフォルダを足したもの）。
//!
//! - 保存先: 選んだフォルダ。選んでいなければ OS のダウンロードのフォルダ
//! - 保存先を毎回確認する: ダウンロードと PDF の保存のたびに、保存する場所を聞く
//! - 日付のフォルダ: 保存先の中に `yyyy-mm-dd` のような形式でフォルダを作り、その中に保存する
//!
//! ページからのダウンロードも、ページを PDF に保存したファイルも、この決まりに従う。

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

/// 決まりを覚えておく場所（設定の表のキー）。
pub const SETTING: &str = "browser.download_rules";

/// 日付のフォルダを使い始めたときの形式。
pub const DEFAULT_PATTERN: &str = "yyyy-mm-dd";

/// 形式の中で、日付に置き換える文字（大文字と小文字は区別しない）。
pub const TOKENS: [(&str, &str); 4] = [
    ("yyyy", "年（4 桁）"),
    ("yy", "年（2 桁）"),
    ("mm", "月（2 桁）"),
    ("dd", "日（2 桁）"),
];

/// フォルダの名前に使えない文字（Windows に合わせる）。
const NOT_IN_NAMES: [char; 8] = ['\\', ':', '*', '?', '"', '<', '>', '|'];

/// ダウンロードの保存先の決まり。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rules {
    /// 保存先のフォルダ。`None` なら OS のダウンロードのフォルダ。
    pub folder: Option<PathBuf>,
    /// 保存のたびに、保存する場所を聞く。
    pub ask: bool,
    /// 日付のフォルダの形式（`yyyy-mm-dd` など）。`None` なら作らない。
    pub subfolder: Option<String>,
}

impl Rules {
    /// 保存できる決まりか。できなければ、何がおかしいかを返す。
    pub fn check(&self) -> Result<(), String> {
        if let Some(folder) = &self.folder {
            if !folder.is_absolute() {
                return Err(format!(
                    "保存先「{}」はフォルダの場所の全体で書いてください",
                    folder.display()
                ));
            }
        }
        if let Some(pattern) = &self.subfolder {
            subfolder(pattern, chrono::NaiveDate::default())?;
        }
        Ok(())
    }

    /// 保存するフォルダ。`base` は保存先を選んでいないときのフォルダ、`today` は今日の日付。
    pub fn dir(&self, base: &Path, today: chrono::NaiveDate) -> Result<PathBuf, String> {
        let root = self.folder.as_deref().unwrap_or(base);
        match &self.subfolder {
            Some(pattern) => Ok(root.join(subfolder(pattern, today)?)),
            None => Ok(root.to_path_buf()),
        }
    }
}

/// 形式に今日の日付を入れて、フォルダの場所（`/` で区切れば中のフォルダ）にする。
/// `'...'` で囲んだ所は、そのままの文字にする。
pub fn subfolder(pattern: &str, date: chrono::NaiveDate) -> Result<PathBuf, String> {
    use chrono::Datelike;
    let wrong = |why: &str| format!("日付のフォルダの形式「{pattern}」: {why}");
    let mut text = String::new();
    let chars: Vec<char> = pattern.chars().collect();
    let mut at = 0;
    while at < chars.len() {
        if chars[at] == '\'' {
            let Some(close) = chars[at + 1..].iter().position(|c| *c == '\'') else {
                return Err(wrong("' が閉じていません"));
            };
            text.extend(&chars[at + 1..at + 1 + close]);
            at += close + 2;
            continue;
        }
        let rest: String = chars[at..]
            .iter()
            .take(4)
            .collect::<String>()
            .to_lowercase();
        let (value, used) = if rest.starts_with("yyyy") {
            (format!("{:04}", date.year()), 4)
        } else if rest.starts_with("yy") {
            (format!("{:02}", date.year().rem_euclid(100)), 2)
        } else if rest.starts_with("mm") {
            (format!("{:02}", date.month()), 2)
        } else if rest.starts_with("dd") {
            (format!("{:02}", date.day()), 2)
        } else {
            (chars[at].to_string(), 1)
        };
        text.push_str(&value);
        at += used;
    }
    if let Some(c) = text
        .chars()
        .find(|c| NOT_IN_NAMES.contains(c) || c.is_control())
    {
        return Err(wrong(&format!("「{c}」はフォルダの名前に使えません")));
    }
    let mut path = PathBuf::new();
    for part in text.split('/') {
        // Windows は末尾の点と空白を消すので、先に消しておく
        let part = part.trim().trim_end_matches(['.', ' ']);
        if part.is_empty() || part == "." || part == ".." {
            return Err(wrong("空のフォルダの名前があります"));
        }
        path.push(part);
    }
    Ok(path)
}

/// 今の決まり。ページからのダウンロードの場所は OS の部品の知らせの中で決めるので、そこから読めるように置く。
static CURRENT: RwLock<Option<Rules>> = RwLock::new(None);

/// 今の決まりを置く（読んだとき・変えたとき）。
pub fn set_current(rules: &Rules) {
    // 書いている途中で落ちたスレッドはないので、壊れた印が付いていても中身はそのまま使える
    let mut current = CURRENT.write().unwrap_or_else(|e| e.into_inner());
    *current = Some(rules.clone());
}

/// 今の決まり。まだ読んでいなければ標準。
pub fn current() -> Rules {
    CURRENT
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

/// 今日の日付（この PC の時刻）。
pub fn today() -> chrono::NaiveDate {
    chrono::Local::now().date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day() -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 3, 7).unwrap()
    }

    #[test]
    fn dates_fill_the_pattern() {
        assert_eq!(
            subfolder("yyyy-mm-dd", day()).unwrap(),
            PathBuf::from("2026-03-07")
        );
        assert_eq!(
            subfolder("YYYY/MM", day()).unwrap(),
            PathBuf::from("2026").join("03")
        );
        assert_eq!(
            subfolder("yy年mm月dd日", day()).unwrap(),
            PathBuf::from("26年03月07日")
        );
        assert_eq!(
            subfolder("'summer' yyyy", day()).unwrap(),
            PathBuf::from("summer 2026"),
            "' で囲んだ所はそのまま"
        );
    }

    #[test]
    fn broken_patterns_are_refused() {
        for pattern in ["", "yyyy/", "../yyyy", "a:b", "'yyyy", "yyyy/ . /dd"] {
            assert!(subfolder(pattern, day()).is_err(), "{pattern}");
        }
        let rules = Rules {
            folder: Some(PathBuf::from("relative")),
            ..Default::default()
        };
        assert!(rules.check().is_err());
    }

    #[test]
    fn the_folder_follows_the_rules() {
        let base = Path::new("/home/a/Downloads");
        assert_eq!(Rules::default().dir(base, day()).unwrap(), base);
        let rules = Rules {
            folder: Some(PathBuf::from("/data/dl")),
            ask: false,
            subfolder: Some(DEFAULT_PATTERN.into()),
        };
        assert_eq!(
            rules.dir(base, day()).unwrap(),
            PathBuf::from("/data/dl/2026-03-07")
        );
    }

    #[test]
    fn old_settings_still_load() {
        let rules: Rules = serde_json::from_str(r#"{"ask":true}"#).unwrap();
        assert!(rules.ask);
        assert_eq!(rules.subfolder, None);
    }
}
