//! タスクに付ける URL。

/// 1 つのタスクに付けられる URL の数。
pub const MAX_URLS: usize = 50;
/// URL 1 つの長さ（文字数）。
pub const MAX_URL_CHARS: usize = 2048;

/// 書いた URL を、保存する形にする。`https://` を省いたもの（`github.com/a/b`）には付ける。
/// URL でなければ `None`。
pub fn clean(input: &str) -> Option<String> {
    let text = input.trim();
    if text.is_empty()
        || text.chars().count() > MAX_URL_CHARS
        || text.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    if let Some((scheme, rest)) = text.split_once("://") {
        let mut chars = scheme.chars();
        let scheme_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
        return (scheme_ok && !rest.is_empty()).then(|| text.to_owned());
    }
    let host = text.split(['/', '?', '#']).next().unwrap_or_default();
    let looks_like_host = (host.contains('.') || host.starts_with("localhost"))
        && !host.starts_with('.')
        && !host.ends_with('.')
        && !host.contains("..");
    if !looks_like_host {
        return None;
    }
    let lower = host.to_ascii_lowercase();
    let local = lower.starts_with("localhost") || lower.starts_with("127.");
    Some(format!("{}://{text}", if local { "http" } else { "https" }))
}

/// 入力欄に書いた（貼った）文から、URL を 1 つずつ取り出す。空白と改行で区切る。
pub fn split(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

/// URL の並びを保存する形にする。同じものは最初の 1 つだけを残す。
/// URL でないものや、数が多すぎるときは、その理由を返す。
pub fn clean_all(urls: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let mut bad: Vec<&str> = Vec::new();
    for url in urls {
        if url.trim().is_empty() {
            continue;
        }
        match clean(url) {
            Some(url) if !out.contains(&url) => out.push(url),
            Some(_) => {}
            None => bad.push(url.trim()),
        }
    }
    if !bad.is_empty() {
        let names: Vec<String> = bad.iter().map(|b| format!("「{}」", clip(b))).collect();
        return Err(format!("URL ではありません: {}", names.join("")));
    }
    if out.len() > MAX_URLS {
        return Err(format!("URL は 1 つのタスクに {MAX_URLS} 個までです"));
    }
    Ok(out)
}

/// 長すぎる文を、エラーに入れる長さに切る。
fn clip(text: &str) -> String {
    const MAX: usize = 60;
    if text.chars().count() <= MAX {
        text.to_owned()
    } else {
        let head: String = text.chars().take(MAX).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_keep_their_scheme_and_get_https_when_left_out() {
        assert_eq!(
            clean("  https://github.com/a/b/pull/1 ").as_deref(),
            Some("https://github.com/a/b/pull/1")
        );
        assert_eq!(
            clean("github.com/a/b").as_deref(),
            Some("https://github.com/a/b")
        );
        assert_eq!(
            clean("localhost:3000/x").as_deref(),
            Some("http://localhost:3000/x")
        );
        assert_eq!(
            clean("malakt-mahoz://task/01ABC").as_deref(),
            Some("malakt-mahoz://task/01ABC")
        );
        assert_eq!(
            clean("https://例え.jp/ページ").as_deref(),
            Some("https://例え.jp/ページ")
        );
    }

    #[test]
    fn words_and_broken_urls_are_not_urls() {
        for text in [
            "",
            "   ",
            "メモ",
            "hello",
            "a b.com",
            "://x",
            "1http://x",
            "https://",
            ".com",
            "example.",
            "a..b",
        ] {
            assert_eq!(clean(text), None, "{text:?}");
        }
        let long = format!("https://a.jp/{}", "x".repeat(MAX_URL_CHARS));
        assert_eq!(clean(&long), None);
    }

    #[test]
    fn pasted_text_is_split_on_spaces_and_lines() {
        assert_eq!(
            split(" https://a.jp\nhttps://b.jp\t c.jp "),
            ["https://a.jp", "https://b.jp", "c.jp"]
        );
        assert!(split("  \n").is_empty());
    }

    #[test]
    fn lists_keep_order_drop_duplicates_and_name_every_bad_entry() {
        let urls = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            clean_all(&urls(&["b.jp", "https://a.jp", " ", "https://b.jp"])).unwrap(),
            ["https://b.jp", "https://a.jp"]
        );
        let err = clean_all(&urls(&["https://a.jp", "メモ", "x y"])).unwrap_err();
        assert_eq!(err, "URL ではありません: 「メモ」「x y」");
        let many: Vec<String> = (0..=MAX_URLS)
            .map(|i| format!("https://a.jp/{i}"))
            .collect();
        assert!(clean_all(&many).unwrap_err().contains("50 個まで"));
        assert_eq!(clean_all(&many[..MAX_URLS]).unwrap().len(), MAX_URLS);
    }

    #[test]
    fn リンクの長さは上限ちょうどまで使える() {
        let base = "https://a.example/";
        let pad = |n: usize| format!("{base}{}", "x".repeat(n - base.len()));
        assert!(clean(&pad(MAX_URL_CHARS)).is_some());
        assert!(clean(&pad(MAX_URL_CHARS + 1)).is_none());
    }
}
