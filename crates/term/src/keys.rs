//! キーとマウスを、端末に送るバイト列に変える（xterm の形）。

/// 押したキー。`name` は小文字の名前（`enter`、`up`、`f5`、`a` など）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Key<'a> {
    pub name: &'a str,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// 修飾キーの番号（xterm）。何も押していなければ `None`。
fn modifier(key: &Key) -> Option<u8> {
    let bits = u8::from(key.shift) | u8::from(key.alt) << 1 | u8::from(key.ctrl) << 2;
    (bits != 0).then_some(bits + 1)
}

/// `ESC [ 1 ; m X` か、修飾なしなら `ESC [ X`（アプリケーションモードなら `ESC O X`）。
fn letter(key: &Key, last: char, app: bool) -> Vec<u8> {
    match modifier(key) {
        Some(m) => format!("\x1b[1;{m}{last}"),
        None if app => format!("\x1bO{last}"),
        None => format!("\x1b[{last}"),
    }
    .into_bytes()
}

/// `ESC [ n ~` か `ESC [ n ; m ~`。
fn tilde(key: &Key, number: u8) -> Vec<u8> {
    match modifier(key) {
        Some(m) => format!("\x1b[{number};{m}~"),
        None => format!("\x1b[{number}~"),
    }
    .into_bytes()
}

fn with_alt(key: &Key, mut bytes: Vec<u8>) -> Vec<u8> {
    if key.alt {
        bytes.insert(0, 0x1b);
    }
    bytes
}

/// キーを端末に送るバイト列にする。文字として入力するキー（修飾なしの `a` など）は `None`。
/// `app_cursor` は、矢印キーをアプリケーションモードで送るかどうか。
pub fn encode(key: &Key, app_cursor: bool) -> Option<Vec<u8>> {
    let bytes = match key.name {
        // Shift+Enter は改行として送る（Claude Code などが、送信と区別できる）
        "enter" if key.shift => b"\x1b\r".to_vec(),
        "enter" => with_alt(key, b"\r".to_vec()),
        "tab" if key.shift => b"\x1b[Z".to_vec(),
        "tab" => with_alt(key, b"\t".to_vec()),
        "backspace" if key.ctrl => with_alt(key, vec![0x08]),
        "backspace" => with_alt(key, vec![0x7f]),
        "escape" => with_alt(key, vec![0x1b]),
        "space" if key.ctrl => with_alt(key, vec![0]),
        "up" => letter(key, 'A', app_cursor),
        "down" => letter(key, 'B', app_cursor),
        "right" => letter(key, 'C', app_cursor),
        "left" => letter(key, 'D', app_cursor),
        "home" => letter(key, 'H', app_cursor),
        "end" => letter(key, 'F', app_cursor),
        "insert" => tilde(key, 2),
        "delete" => tilde(key, 3),
        "pageup" => tilde(key, 5),
        "pagedown" => tilde(key, 6),
        "f1" => letter(key, 'P', true),
        "f2" => letter(key, 'Q', true),
        "f3" => letter(key, 'R', true),
        "f4" => letter(key, 'S', true),
        "f5" => tilde(key, 15),
        "f6" => tilde(key, 17),
        "f7" => tilde(key, 18),
        "f8" => tilde(key, 19),
        "f9" => tilde(key, 20),
        "f10" => tilde(key, 21),
        "f11" => tilde(key, 23),
        "f12" => tilde(key, 24),
        name => {
            let mut chars = name.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            if key.ctrl {
                let code = match c.to_ascii_lowercase() {
                    c @ 'a'..='z' => c as u8 - b'a' + 1,
                    '@' | '2' => 0,
                    '[' | '3' => 0x1b,
                    '\\' | '4' => 0x1c,
                    ']' | '5' => 0x1d,
                    '^' | '6' => 0x1e,
                    '_' | '/' | '-' | '7' => 0x1f,
                    '?' | '8' => 0x7f,
                    _ => return None,
                };
                with_alt(key, vec![code])
            } else if key.alt {
                let c = if key.shift { c.to_ascii_uppercase() } else { c };
                with_alt(key, c.to_string().into_bytes())
            } else {
                return None;
            }
        }
    };
    Some(bytes)
}

/// マウスのボタン。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
    /// ボタンを押さずに動かした。
    None,
    WheelUp,
    WheelDown,
}

/// マウスの出来事。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Press,
    Release,
    /// 動かした（ボタンを押したままなら、そのボタン）。
    Move,
}

/// マウスの報告を作る。`col` と `row` は 0 から。`sgr` でなければ古い形（223 桁まで）。
pub fn mouse(
    button: Button,
    action: Action,
    (col, row): (usize, usize),
    key: &Key,
    sgr: bool,
) -> Option<Vec<u8>> {
    let mut code = match button {
        Button::Left => 0,
        Button::Middle => 1,
        Button::Right => 2,
        Button::None => 3,
        Button::WheelUp => 64,
        Button::WheelDown => 65,
    };
    if action == Action::Move {
        code += 32;
    }
    code += u32::from(key.shift) * 4 + u32::from(key.alt) * 8 + u32::from(key.ctrl) * 16;
    if sgr {
        let last = if action == Action::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{};{}{last}", col + 1, row + 1).into_bytes());
    }
    if action == Action::Release {
        code = code & !3 | 3;
    }
    if col >= 223 || row >= 223 {
        return None;
    }
    Some(vec![
        0x1b,
        b'[',
        b'M',
        32 + code as u8,
        33 + col as u8,
        33 + row as u8,
    ])
}

/// 貼り付ける文字列。改行は Enter にそろえ、`bracketed` なら貼り付けの印で囲む。
pub fn paste(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        // 貼り付けの終わりの印を、貼る文字列の中から取り除く
        let text = text.replace("\x1b[201~", "").replace('\x1b', "");
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> Key<'_> {
        Key {
            name,
            ..Default::default()
        }
    }

    #[test]
    fn plain_text_keys_are_left_to_text_input() {
        assert_eq!(encode(&key("a"), false), None);
        assert_eq!(encode(&key("space"), false), None);
        assert_eq!(
            encode(
                &Key {
                    shift: true,
                    ..key("a")
                },
                false
            ),
            None
        );
    }

    #[test]
    fn control_and_alt_keys() {
        let ctrl = |name| Key {
            ctrl: true,
            ..key(name)
        };
        assert_eq!(encode(&ctrl("c"), false), Some(vec![3]));
        assert_eq!(encode(&ctrl("d"), false), Some(vec![4]));
        assert_eq!(encode(&ctrl("["), false), Some(vec![0x1b]));
        assert_eq!(encode(&ctrl("space"), false), Some(vec![0]));
        assert_eq!(encode(&ctrl("backspace"), false), Some(vec![8]));
        assert_eq!(encode(&ctrl("1"), false), None);
        let alt = Key {
            alt: true,
            ..key("b")
        };
        assert_eq!(encode(&alt, false), Some(b"\x1bb".to_vec()));
        let both = Key {
            alt: true,
            ..ctrl("c")
        };
        assert_eq!(encode(&both, false), Some(vec![0x1b, 3]));
    }

    #[test]
    fn named_keys() {
        assert_eq!(encode(&key("enter"), false), Some(b"\r".to_vec()));
        let shift = |name| Key {
            shift: true,
            ..key(name)
        };
        assert_eq!(encode(&shift("enter"), false), Some(b"\x1b\r".to_vec()));
        assert_eq!(encode(&shift("tab"), false), Some(b"\x1b[Z".to_vec()));
        assert_eq!(encode(&key("backspace"), false), Some(vec![0x7f]));
        assert_eq!(encode(&key("up"), false), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode(&key("up"), true), Some(b"\x1bOA".to_vec()));
        assert_eq!(encode(&shift("left"), true), Some(b"\x1b[1;2D".to_vec()));
        let ctrl_right = Key {
            ctrl: true,
            ..key("right")
        };
        assert_eq!(encode(&ctrl_right, false), Some(b"\x1b[1;5C".to_vec()));
        assert_eq!(encode(&key("delete"), false), Some(b"\x1b[3~".to_vec()));
        assert_eq!(encode(&shift("pageup"), false), Some(b"\x1b[5;2~".to_vec()));
        assert_eq!(encode(&key("f1"), false), Some(b"\x1bOP".to_vec()));
        assert_eq!(encode(&shift("f1"), false), Some(b"\x1b[1;2P".to_vec()));
        assert_eq!(encode(&key("f5"), false), Some(b"\x1b[15~".to_vec()));
        assert_eq!(encode(&key("f12"), false), Some(b"\x1b[24~".to_vec()));
        assert_eq!(encode(&key("f13"), false), None);
    }

    #[test]
    fn mouse_reports() {
        let none = Key::default();
        assert_eq!(
            mouse(Button::Left, Action::Press, (0, 0), &none, true),
            Some(b"\x1b[<0;1;1M".to_vec())
        );
        assert_eq!(
            mouse(Button::Right, Action::Release, (9, 4), &none, true),
            Some(b"\x1b[<2;10;5m".to_vec())
        );
        assert_eq!(
            mouse(Button::Left, Action::Move, (2, 3), &none, true),
            Some(b"\x1b[<32;3;4M".to_vec())
        );
        let ctrl = Key {
            ctrl: true,
            ..Default::default()
        };
        assert_eq!(
            mouse(Button::WheelDown, Action::Press, (0, 0), &ctrl, true),
            Some(b"\x1b[<81;1;1M".to_vec())
        );
        // 古い形
        assert_eq!(
            mouse(Button::Left, Action::Press, (0, 0), &none, false),
            Some(vec![0x1b, b'[', b'M', 32, 33, 33])
        );
        assert_eq!(
            mouse(Button::Left, Action::Release, (1, 2), &none, false),
            Some(vec![0x1b, b'[', b'M', 35, 34, 35])
        );
        assert_eq!(
            mouse(Button::Left, Action::Press, (300, 0), &none, false),
            None
        );
    }

    #[test]
    fn pasted_text() {
        assert_eq!(paste("a\r\nb\nc", false), b"a\rb\rc".to_vec());
        assert_eq!(paste("x\x1b[201~y", true), b"\x1b[200~xy\x1b[201~".to_vec());
    }
}
