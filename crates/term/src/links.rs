//! 画面の中のリンク（`https://…` の文字と、OSC 8 のハイパーリンク）。

use std::ops::Range;

/// 画面の中のリンク 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// 開く URL。`http://` か `https://` で始まる。
    pub url: String,
    /// リンクのマス（桁、行）。画面に見えているマスだけ。折り返して次の行に続くリンクは、両方の行のマスを持つ。
    pub cells: Vec<(usize, usize)>,
}

/// 開いてよい URL か。Web の URL だけを開く（プログラムが送るハイパーリンクで、ほかのアプリを起こさない）。
pub fn openable(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    ["https://", "http://"]
        .iter()
        .any(|scheme| lower.starts_with(scheme) && lower.len() > scheme.len())
}

/// URL に入る文字か。日本語や空白、かっこの一部で URL は終わる。
fn in_url(c: char) -> bool {
    c.is_ascii_graphic()
        && !matches!(
            c,
            '<' | '>' | '"' | '\'' | '`' | '{' | '}' | '|' | '\\' | '^'
        )
}

/// 文字の中の URL の範囲（文字の位置）。文の終わりの句読点や、対になっていない閉じかっこは URL に入れない。
pub fn find_urls(text: &[char]) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let mut ix = 0;
    while ix < text.len() {
        let starts = ["https://", "http://"].iter().find_map(|scheme| {
            let end = ix + scheme.len();
            (end <= text.len()
                && text[ix..end]
                    .iter()
                    .zip(scheme.chars())
                    .all(|(c, s)| c.to_ascii_lowercase() == s))
            .then_some(end)
        });
        // 「xhttps://」のような、単語の途中は URL の始まりにしない
        let word_before = ix > 0 && text[ix - 1].is_ascii_alphanumeric();
        let Some(body) = starts.filter(|_| !word_before) else {
            ix += 1;
            continue;
        };
        let mut end = body;
        while end < text.len() && in_url(text[end]) {
            end += 1;
        }
        while end > body {
            let last = text[end - 1];
            let unpaired = |open: char, close: char| {
                last == close && {
                    let part = &text[ix..end];
                    part.iter().filter(|&&c| c == close).count()
                        > part.iter().filter(|&&c| c == open).count()
                }
            };
            if matches!(last, '.' | ',' | ';' | ':' | '!' | '?')
                || unpaired('(', ')')
                || unpaired('[', ']')
            {
                end -= 1;
            } else {
                break;
            }
        }
        if end > body {
            found.push(ix..end);
        }
        ix = end.max(ix + 1);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        find_urls(&chars)
            .into_iter()
            .map(|range| chars[range].iter().collect())
            .collect()
    }

    #[test]
    fn urls_are_found_in_a_line() {
        assert_eq!(
            urls("see https://example.com/a?b=1#c and http://x.test"),
            ["https://example.com/a?b=1#c", "http://x.test"]
        );
        assert_eq!(urls("HTTPS://Example.com/"), ["HTTPS://Example.com/"]);
    }

    #[test]
    fn trailing_punctuation_and_unpaired_brackets_are_left_out() {
        assert_eq!(urls("https://example.com."), ["https://example.com"]);
        assert_eq!(urls("(https://example.com)"), ["https://example.com"]);
        assert_eq!(
            urls("[a](https://example.com/x)"),
            ["https://example.com/x"]
        );
        assert_eq!(
            urls("https://en.wikipedia.org/wiki/Rust_(language)"),
            ["https://en.wikipedia.org/wiki/Rust_(language)"]
        );
        assert_eq!(urls("<https://example.com>"), ["https://example.com"]);
        assert_eq!(urls("\"https://example.com\","), ["https://example.com"]);
    }

    #[test]
    fn japanese_text_ends_a_url() {
        assert_eq!(urls("https://example.com/です"), ["https://example.com/"]);
        assert_eq!(urls("「https://example.com」"), ["https://example.com"]);
    }

    #[test]
    fn a_scheme_alone_or_in_a_word_is_not_a_url() {
        assert!(urls("https://").is_empty());
        assert!(urls("https://.").is_empty());
        assert!(urls("xhttps://example.com").is_empty());
        assert!(urls("ftp://example.com").is_empty());
    }

    #[test]
    fn only_web_urls_are_opened() {
        assert!(openable("https://example.com"));
        assert!(openable("HTTP://example.com"));
        assert!(!openable("https://"));
        assert!(!openable("file:///etc/passwd"));
        assert!(!openable("javascript:alert(1)"));
    }
}
