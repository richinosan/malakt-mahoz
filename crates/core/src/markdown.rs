//! 本文（説明・コメント・完了報告・AI の返答）の Markdown を、画面に出す形にそろえる。
//!
//! - GitHub のコメントと同じく、段落の中の改行はそのまま改行として見せる（marked の
//!   `breaks: true` と同じ）。CommonMark では段落の中の改行は空白になるので、改行の前に
//!   空白 2 つ（強制改行）を足す。コードブロック・インラインコード・表などには足さない。
//! - 文の中にそのまま書いた URL は、GFM では後ろの日本語の文字までリンクになる
//!   （「https://e.x/aを見て」の「を見て」まで）。URL の部分だけを `<…>` で囲む。
//!
//! 足した分だけ位置がずれるので、画面で選んだ範囲を元の本文の位置に戻す
//! [`Rendered::to_source`] を持つ。

use ::markdown::mdast::Node;
use ::markdown::ParseOptions;

/// 画面に出す Markdown と、元の本文の位置への戻し方。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Rendered {
    /// 描く文。
    pub text: String,
    /// 足した文字列と、足した元の本文での位置（小さい順）。
    inserted: Vec<(usize, &'static str)>,
}

impl Rendered {
    /// 描く文での位置を、元の本文での位置にする。足した文字の上なら、足した位置。
    pub fn to_source(&self, at: usize) -> usize {
        let mut added = 0;
        for &(pos, s) in &self.inserted {
            let start = pos + added;
            if at <= start {
                break;
            }
            if at < start + s.len() {
                return pos;
            }
            added += s.len();
        }
        at - added
    }
}

/// 強制改行（行の終わりの空白 2 つ）。
const BREAK: &str = "  ";

/// 説明・コメント・完了報告: 改行をそのまま見せ、URL をリンクにする。
pub fn with_line_breaks(source: &str) -> Rendered {
    render(source, true)
}

/// AI の返答: URL だけをリンクにする（改行は Markdown のまま）。
pub fn with_links(source: &str) -> Rendered {
    render(source, false)
}

fn render(source: &str, breaks: bool) -> Rendered {
    let mut inserted = Vec::new();
    let needed = (breaks && source.contains('\n')) || source.contains("://");
    if needed {
        if let Some(root) = parse(source) {
            collect(&root, source, breaks, false, &mut inserted);
        }
    }
    // 同じ位置に足すものは、集めた順（URL の閉じ `>` のあとに改行）のまま
    inserted.sort_by_key(|&(pos, _)| pos);
    let extra: usize = inserted.iter().map(|(_, s)| s.len()).sum();
    let mut text = String::with_capacity(source.len() + extra);
    let mut from = 0;
    for &(pos, s) in &inserted {
        text.push_str(&source[from..pos]);
        text.push_str(s);
        from = pos;
    }
    text.push_str(&source[from..]);
    Rendered { text, inserted }
}

/// 画面と同じ読み方（GFM と数式）で読む。読めない本文は `None`（そのまま描く）。
fn parse(source: &str) -> Option<Node> {
    let mut options = ParseOptions::gfm();
    options.constructs.math_text = true;
    options.constructs.math_flow = true;
    // markdown 1.0.0 は入力によって panic するので、アプリごと落とさない
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ::markdown::to_mdast(source, &options)
    }))
    .ok()?
    .ok()
}

/// 文の中の改行と、日本語の文字まで続いてしまった URL の位置を集める。
fn collect(
    node: &Node,
    source: &str,
    breaks: bool,
    in_link: bool,
    out: &mut Vec<(usize, &'static str)>,
) {
    let Some(pos) = node.position() else { return };
    let (start, end) = (pos.start.offset, pos.end.offset);
    let Some(slice) = source.get(start..end) else {
        return;
    };
    match node {
        Node::Text(_) => {
            if breaks {
                for (i, _) in slice.match_indices('\n') {
                    let at = start + i;
                    // CRLF なら CR の前に足す
                    let at = if at > start && source.as_bytes()[at - 1] == b'\r' {
                        at - 1
                    } else {
                        at
                    };
                    out.push((at, BREAK));
                }
            }
            return;
        }
        // 文の中にそのまま書いた URL（`<…>` や `[…](…)` でないもの）は、GFM では
        // 「詳細はhttps://e.x/aを見て」の「を見て」までリンクになる。日本語の前で切る
        Node::Link(_) if !in_link && find_scheme(slice) == Some(0) => {
            let url = url_end(slice);
            if url < slice.len() {
                out.push((start, "<"));
                out.push((start + url, ">"));
            }
            return;
        }
        _ => {}
    }
    // リンクの文字の中は、リンクにし直さない
    let in_link = in_link || matches!(node, Node::Link(_) | Node::LinkReference(_));
    if let Some(children) = node.children() {
        for child in children {
            collect(child, source, breaks, in_link, out);
        }
    }
}

/// `http://` `https://` で始まる文の、URL の終わり。空白・日本語の文字・`<` の前で終わり、
/// 終わりの句読点と、対になっていない `)` は含めない。
fn url_end(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut end = 0;
    while end < bytes.len() && is_url_byte(bytes[end]) {
        end += 1;
    }
    while let Some(&last) = bytes[..end].last() {
        let url = &text[..end];
        let unbalanced = last == b')' && url.matches('(').count() < url.matches(')').count();
        if b".,:;!?'\"*_~".contains(&last) || unbalanced {
            end -= 1;
        } else {
            break;
        }
    }
    end
}

fn find_scheme(text: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    match (lower.find("https://"), lower.find("http://")) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// URL に含める文字。ASCII の、空白・`<` `>` `"` `` ` `` `\` 以外。
fn is_url_byte(b: u8) -> bool {
    b.is_ascii_graphic() && !matches!(b, b'<' | b'>' | b'"' | b'`' | b'\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(source: &str) -> String {
        with_line_breaks(source).text
    }

    #[test]
    fn breaks_lines_inside_a_paragraph() {
        assert_eq!(text("1 行目\n2 行目\n3 行目"), "1 行目  \n2 行目  \n3 行目");
    }

    #[test]
    fn leaves_text_without_newlines_alone() {
        assert_eq!(text("1 行だけ **太字**"), "1 行だけ **太字**");
        assert_eq!(text(""), "");
    }

    #[test]
    fn leaves_paragraph_breaks_alone() {
        assert_eq!(text("段落 1\n\n段落 2\n"), "段落 1\n\n段落 2\n");
    }

    #[test]
    fn breaks_lines_inside_emphasis_links_lists_and_quotes() {
        assert_eq!(text("*a\nb*"), "*a  \nb*");
        assert_eq!(text("[a\nb](https://e.x)"), "[a  \nb](https://e.x)");
        assert_eq!(text("- a\n  b\n- c"), "- a  \n  b\n- c");
        assert_eq!(text("> a\n> b"), "> a  \n> b");
        assert_eq!(text("a\n**b**"), "a  \n**b**");
    }

    #[test]
    fn leaves_code_tables_and_headings_alone() {
        let fenced = "```\na\nb\n```";
        assert_eq!(text(fenced), fenced);
        let inline = "`a\nb`";
        assert_eq!(text(inline), inline);
        let table = "| a | b |\n| - | - |\n| 1 | 2 |";
        assert_eq!(text(table), table);
        assert_eq!(text("# 見出し\n本文"), "# 見出し\n本文");
        let setext = "見出し\n===\n本文";
        assert_eq!(text(setext), setext);
        let rule = "a\n\n---\nb";
        assert_eq!(text(rule), rule);
    }

    #[test]
    fn keeps_existing_hard_breaks() {
        assert_eq!(text("a\\\nb"), "a\\\nb");
        assert_eq!(text("a  \nb"), "a  \nb");
    }

    #[test]
    fn keeps_an_escaped_backslash_at_the_end_of_a_line() {
        assert_eq!(text("C:\\\\\nD"), "C:\\\\  \nD");
    }

    #[test]
    fn puts_the_break_before_crlf() {
        assert_eq!(text("a\r\nb"), "a  \r\nb");
    }

    #[test]
    fn task_lists_keep_their_items() {
        assert_eq!(text("- [ ] やる\n- [x] やった"), "- [ ] やる\n- [x] やった");
    }

    #[test]
    fn urls_end_before_japanese_text() {
        assert_eq!(
            text("詳細はhttps://example.com/a?b=1を見てください。"),
            "詳細は<https://example.com/a?b=1>を見てください。"
        );
        assert_eq!(text("（https://e.x/a）"), "（<https://e.x/a>）");
        assert_eq!(text("先は https://e.x/a。"), "先は <https://e.x/a>。");
        assert_eq!(text("先はhttps://e.x/(a)です"), "先は<https://e.x/(a)>です");
    }

    #[test]
    fn leaves_urls_that_end_by_themselves_alone() {
        for source in [
            "見て https://example.com",
            "https://example.com",
            "資料:http://e.x",
            "先はhttps://e.x/a.",
            "先は(https://e.x/a)",
            "先はhttps://e.x/wiki/A_(b)",
            "[資料](https://example.com)",
            "[https://example.comを見て](https://example.com)",
            "<https://example.com>",
            "`https://example.comを見て`",
            "```\nhttps://example.comを見て\n```",
            "![図](https://example.com/a.png)",
        ] {
            assert_eq!(text(source), source, "{source}");
        }
    }

    #[test]
    fn links_and_breaks_together() {
        assert_eq!(
            text("資料はhttps://e.x\n次の行"),
            "資料はhttps://e.x  \n次の行"
        );
        assert_eq!(
            text("1: https://e.xです\n2: 資料"),
            "1: <https://e.x>です  \n2: 資料"
        );
    }

    #[test]
    fn ai_text_keeps_line_breaks_as_markdown() {
        let r = with_links("一行目\n二行目はhttps://e.xです");
        assert_eq!(r.text, "一行目\n二行目は<https://e.x>です");
        assert_eq!(with_links("ふつうの文").text, "ふつうの文");
    }

    #[test]
    fn maps_positions_back_to_the_source() {
        let source = "ab\ncd\nef";
        let r = with_line_breaks(source);
        assert_eq!(r.text, "ab  \ncd  \nef");
        let cd = r.text.find("cd").unwrap();
        assert_eq!(&source[r.to_source(cd)..r.to_source(cd + 2)], "cd");
        let ef = r.text.find("ef").unwrap();
        assert_eq!(&source[r.to_source(ef)..r.to_source(ef + 2)], "ef");
        // 足した空白の上は、元の改行の位置
        assert_eq!(r.to_source(3), 2);
        assert_eq!(r.to_source(0), 0);
        assert_eq!(r.to_source(r.text.len()), source.len());
    }

    #[test]
    fn maps_positions_around_wrapped_urls() {
        let source = "先はhttps://e.xです\n次";
        let r = with_line_breaks(source);
        let url = r.text.find("https").unwrap();
        let back = r.to_source(url)..r.to_source(url + "https://e.x".len());
        assert_eq!(&source[back], "https://e.x");
        let tail = r.text.find("です").unwrap();
        assert_eq!(&source[r.to_source(tail)..], "です\n次");
    }

    #[test]
    fn maps_positions_without_changes_unchanged() {
        let r = with_line_breaks("本文");
        assert_eq!(r.to_source(3), 3);
    }

    #[test]
    fn multibyte_lines_stay_on_char_boundaries() {
        let source = "日本語\n漢字";
        let r = with_line_breaks(source);
        let at = r.text.find("漢字").unwrap();
        let back = r.to_source(at);
        assert!(source.is_char_boundary(back));
        assert_eq!(&source[back..], "漢字");
    }
}
