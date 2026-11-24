//! ターミナルの出力（ANSI のエスケープ）を、文字と色に分ける。

use std::ops::Range;

/// ターミナルの 16 色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
}

impl Color {
    const ALL: [Color; 16] = [
        Color::Black,
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
        Color::White,
        Color::BrightBlack,
        Color::BrightRed,
        Color::BrightGreen,
        Color::BrightYellow,
        Color::BrightBlue,
        Color::BrightMagenta,
        Color::BrightCyan,
        Color::BrightWhite,
    ];
}

/// 色の付いた範囲（`text` のバイト位置）。
pub type Span = (Range<usize>, Color);

/// 行の途中で変わる色の状態。次の行に引き継ぐ。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct State {
    color: Option<Color>,
    bright: bool,
}

/// 1 行を、エスケープを除いた文字と、色の範囲に分ける。
///
/// - 色（SGR）の 30〜37・90〜97・39・0・1 を読む。256 色と RGB は標準の色で出す
/// - `\r` は行の頭に戻る。進み具合の表示のように上書きされた行は、最後の内容だけを残す
/// - カーソルの移動や画面の消去など、ほかのエスケープは捨てる
pub fn parse(line: &str, state: &mut State) -> (String, Vec<Span>) {
    let mut text = String::new();
    let mut spans: Vec<Span> = Vec::new();
    let mut chars = line.chars().peekable();
    let push = |text: &mut String, spans: &mut Vec<Span>, c: char, state: &State| {
        let start = text.len();
        text.push(c);
        if let Some(color) = state.color {
            let color = if state.bright {
                Color::ALL[(color as usize % 8) + 8]
            } else {
                color
            };
            match spans.last_mut() {
                Some((range, last)) if *last == color && range.end == start => {
                    range.end = text.len()
                }
                _ => spans.push((start..text.len(), color)),
            }
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                // 行末の \r（CRLF）は何もしない。途中の \r は上書き
                if chars.peek().is_some() {
                    text.clear();
                    spans.clear();
                }
            }
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    let mut params = String::new();
                    let mut last = None;
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            last = Some(c);
                            break;
                        }
                        params.push(c);
                    }
                    if last == Some('m') {
                        apply_sgr(&params, state);
                    }
                }
                Some(']') => {
                    // OSC（タイトルの変更など）。BEL か ST まで捨てる
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' {
                            chars.next();
                            break;
                        }
                    }
                }
                // 2 文字のエスケープ
                Some(_) | None => {}
            },
            '\t' => {
                // タブは 8 桁ごと
                let width = 8 - text.chars().count() % 8;
                for _ in 0..width {
                    push(&mut text, &mut spans, ' ', state);
                }
            }
            c if c.is_control() => {}
            c => push(&mut text, &mut spans, c, state),
        }
    }
    (text, spans)
}

fn apply_sgr(params: &str, state: &mut State) {
    let mut codes = params.split(';').map(|p| p.parse::<u16>().unwrap_or(0));
    while let Some(code) = codes.next() {
        match code {
            0 => *state = State::default(),
            1 => state.bright = true,
            22 => state.bright = false,
            30..=37 => state.color = Some(Color::ALL[usize::from(code - 30)]),
            39 => state.color = None,
            90..=97 => state.color = Some(Color::ALL[usize::from(code - 90) + 8]),
            38 | 48 => {
                // 256 色（5;n）と RGB（2;r;g;b）の引数を読み飛ばす
                match codes.next() {
                    Some(5) => {
                        let n = codes.next().unwrap_or(0);
                        if code == 38 && n < 16 {
                            state.color = Some(Color::ALL[usize::from(n)]);
                        }
                    }
                    Some(2) => {
                        codes.nth(2);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// ターミナルでの幅（桁）。全角は 2 桁。
pub fn width(c: char) -> usize {
    let c = c as u32;
    let wide = matches!(c,
        0x1100..=0x115f | 0x2e80..=0x303e | 0x3041..=0x33ff | 0x3400..=0x4dbf
        | 0x4e00..=0x9fff | 0xa000..=0xa4cf | 0xac00..=0xd7a3 | 0xf900..=0xfaff
        | 0xfe30..=0xfe4f | 0xff00..=0xff60 | 0xffe0..=0xffe6
        | 0x1f300..=0x1f64f | 0x1f900..=0x1f9ff | 0x20000..=0x3fffd);
    if wide {
        2
    } else {
        1
    }
}

/// `text` を `columns` 桁で折り返したときの、各行のバイト範囲。空の行も 1 行になる。
pub fn wrap(text: &str, columns: usize) -> Vec<Range<usize>> {
    let columns = columns.max(2);
    let mut rows = Vec::new();
    let (mut start, mut used) = (0, 0);
    for (ix, c) in text.char_indices() {
        let w = width(c);
        if used + w > columns {
            rows.push(start..ix);
            start = ix;
            used = 0;
        }
        used += w;
    }
    rows.push(start..text.len());
    rows
}

#[cfg(test)]
mod tests {
    use super::{parse, wrap, Color, State};

    #[test]
    fn colors_are_split_from_the_text() {
        let mut state = State::default();
        let (text, spans) = parse("ok \u{1b}[31mエラー\u{1b}[0m done", &mut state);
        assert_eq!(text, "ok エラー done");
        assert_eq!(spans, vec![(3..12, Color::Red)]);
        assert_eq!(state, State::default());

        // 太字は明るい色にし、色は次の行に続く
        let (text, spans) = parse("\u{1b}[1;32mPASS", &mut state);
        assert_eq!(
            (text.as_str(), spans),
            ("PASS", vec![(0..4, Color::BrightGreen)])
        );
        let (_, spans) = parse("next", &mut state);
        assert_eq!(spans, vec![(0..4, Color::BrightGreen)]);
        let (_, spans) = parse("\u{1b}[39;22mplain \u{1b}[94mblue", &mut state);
        assert_eq!(spans, vec![(6..10, Color::BrightBlue)]);
    }

    #[test]
    fn other_escapes_are_dropped() {
        let mut state = State::default();
        // カーソルの移動、行の消去、タイトルの変更、256 色、RGB
        let line = "\u{1b}[2K\u{1b}[1Ga\u{1b}]0;title\u{7}b\u{1b}[38;5;200mc\u{1b}[38;2;1;2;3md\u{1b}[48;5;1me";
        let (text, spans) = parse(line, &mut state);
        assert_eq!(text, "abcde");
        assert!(spans.is_empty());
        // 256 色のうち最初の 16 色は、そのまま使う
        let (_, spans) = parse("\u{1b}[38;5;3my", &mut state);
        assert_eq!(spans, vec![(0..1, Color::Yellow)]);
    }

    #[test]
    fn carriage_returns_overwrite_and_tabs_align() {
        let mut state = State::default();
        assert_eq!(parse("10%\r50%\r100%", &mut state).0, "100%");
        assert_eq!(parse("windows line\r", &mut state).0, "windows line");
        assert_eq!(parse("a\tb", &mut state).0, "a       b");
        assert_eq!(parse("bell\u{7}!", &mut state).0, "bell!");
    }

    #[test]
    fn long_lines_wrap_by_columns() {
        assert_eq!(wrap("", 10), vec![0..0]);
        assert_eq!(wrap("abcdef", 4), vec![0..4, 4..6]);
        // 全角は 2 桁。行末に 1 桁しか残っていなければ、次の行へ送る
        let text = "aあいう";
        assert_eq!(wrap(text, 4), vec![0..4, 4..10]);
        assert_eq!(wrap("abcd", 4), vec![0..4]);
    }
}
