//! ターミナル。シェルを PTY（Windows は ConPTY）で起こし、画面の中身を端末として解釈する。
//! 画面には依存しない。描く側は [`Terminal::snapshot`] でマスの並びを受け取る。

pub mod console;
pub mod drop;
pub mod keys;
pub mod profile;
mod settle;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as AlacEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{self, Config, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};

pub use drop::{drop_text, PathStyle};
pub use keys::{Action, Button, Key};
pub use profile::Profile;

/// さかのぼれる行数。メモリの中だけに置き、これより古い行は捨てる。
pub const SCROLLBACK: usize = 5000;

/// 文字の色（前景）と背景の色、カーソルの色。
pub const FOREGROUND: u32 = 0xd4d7dd;
pub const BACKGROUND: u32 = 0x0f1115;
pub const CURSOR: u32 = 0xd4d7dd;
pub const SELECTION: u32 = 0x2f4a73;

/// 16 色（ふつうの 8 色と、明るい 8 色）。
const ANSI: [u32; 16] = [
    0x1c2028, 0xff7b72, 0x7ee787, 0xe3b341, 0x79c0ff, 0xd2a8ff, 0x56d4dd, 0xc9d1d9, 0x6e7681,
    0xffa198, 0xa5f0ad, 0xf2cc60, 0xa5d6ff, 0xe2c5ff, 0x76e3ea, 0xf0f6fc,
];

/// 256 色の番号を RGB にする。
pub fn indexed(index: u8) -> u32 {
    match index {
        0..=15 => ANSI[index as usize],
        16..=231 => {
            let i = u32::from(index) - 16;
            let level = |n: u32| if n == 0 { 0 } else { 55 + n * 40 };
            level(i / 36) << 16 | level(i / 6 % 6) << 8 | level(i % 6)
        }
        _ => {
            let gray = 8 + (u32::from(index) - 232) * 10;
            gray << 16 | gray << 8 | gray
        }
    }
}

fn dim(rgb: u32) -> u32 {
    let scale = |shift: u32| ((rgb >> shift & 0xff) * 2 / 3) << shift;
    scale(16) | scale(8) | scale(0)
}

fn rgb_of(rgb: Rgb) -> u32 {
    u32::from(rgb.r) << 16 | u32::from(rgb.g) << 8 | u32::from(rgb.b)
}

fn named(color: NamedColor) -> u32 {
    use NamedColor::*;
    match color {
        Foreground | BrightForeground => FOREGROUND,
        Background => BACKGROUND,
        Cursor => CURSOR,
        DimForeground => dim(FOREGROUND),
        DimBlack => dim(ANSI[0]),
        DimRed => dim(ANSI[1]),
        DimGreen => dim(ANSI[2]),
        DimYellow => dim(ANSI[3]),
        DimBlue => dim(ANSI[4]),
        DimMagenta => dim(ANSI[5]),
        DimCyan => dim(ANSI[6]),
        DimWhite => dim(ANSI[7]),
        other => ANSI[(other as usize).min(15)],
    }
}

/// 色を RGB にする。端末のプログラムが色を変えていれば（OSC 4 など）、その色。
fn resolve(color: Color, colors: &term::color::Colors) -> u32 {
    match color {
        Color::Spec(rgb) => rgb_of(rgb),
        Color::Named(name) => colors[name].map(rgb_of).unwrap_or_else(|| named(name)),
        Color::Indexed(index) => colors[index as usize]
            .map(rgb_of)
            .unwrap_or_else(|| indexed(index)),
    }
}

/// 番号（0〜268）の色。色を問い合わせる端末のプログラムに答える。
fn color_at(index: usize) -> u32 {
    match index {
        0..=255 => indexed(index as u8),
        256 | 267 => FOREGROUND,
        257 | 268 => BACKGROUND,
        258 => CURSOR,
        _ => dim(FOREGROUND),
    }
}

/// マスの飾り。
pub mod style {
    pub const BOLD: u8 = 1;
    pub const ITALIC: u8 = 1 << 1;
    pub const UNDERLINE: u8 = 1 << 2;
    pub const STRIKE: u8 = 1 << 3;
    /// 全角（2 マス分）の文字。右どなりは [`SPACER`]。
    pub const WIDE: u8 = 1 << 4;
    /// 全角の文字の右半分。何も描かない。
    pub const SPACER: u8 = 1 << 5;
    pub const SELECTED: u8 = 1 << 6;
}

/// 画面の 1 マス。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub c: char,
    pub fg: u32,
    pub bg: u32,
    pub style: u8,
}

/// カーソルの形。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caret {
    Block,
    Underline,
    Beam,
}

/// 画面の中身。`cells` は上の行から順に `cols` マスずつ。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub cols: usize,
    pub rows: usize,
    pub cells: Vec<Cell>,
    /// マスに重ねる文字（濁点などの結合文字）。`cells` の位置と文字。
    pub combining: Vec<(usize, String)>,
    /// カーソルの場所（桁、行）と形。さかのぼっている間や、隠されている間は `None`。
    pub cursor: Option<(usize, usize, Caret)>,
    /// 入力の場所（桁、行）。カーソルを隠すプログラム（Claude Code など）でも分かる。
    /// IME の変換中の文字と候補を出す場所に使う。さかのぼっている間は `None`。
    pub input_at: Option<(usize, usize)>,
    /// さかのぼっている行数。
    pub offset: usize,
    /// さかのぼれる行数。
    pub history: usize,
}

impl Snapshot {
    /// 1 行の文字。右の空白は除く。
    pub fn line(&self, row: usize) -> String {
        let start = row * self.cols;
        let mut out = String::new();
        for (ix, cell) in self.cells[start..start + self.cols].iter().enumerate() {
            if cell.style & style::SPACER != 0 {
                continue;
            }
            out.push(cell.c);
            if let Ok(at) = self
                .combining
                .binary_search_by_key(&(start + ix), |(at, _)| *at)
            {
                out.push_str(&self.combining[at].1);
            }
        }
        out.truncate(out.trim_end().len());
        out
    }

    /// 画面の文字。下の空の行は除く。
    pub fn text(&self) -> String {
        let mut lines: Vec<String> = (0..self.rows).map(|row| self.line(row)).collect();
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines.join("\n")
    }
}

/// ターミナルから届く知らせ。別のスレッドから届く。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// 画面が変わった。
    Wakeup,
    /// タイトルが変わった。`None` なら元に戻す。
    Title(Option<String>),
    /// プログラムがクリップボードに入れた（OSC 52）。
    Copy(String),
    Bell,
    /// シェルが終わった。シグナルで止まったときは `None`。
    Exit(Option<i32>),
}

/// 起こすシェル。
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub cols: usize,
    pub rows: usize,
}

impl Options {
    /// 一覧のシェルを、`cwd` で始める。
    pub fn of(profile: &Profile, cwd: Option<PathBuf>) -> Self {
        let cwd = profile::start_dir(cwd.as_deref());
        Self {
            program: profile.program.clone(),
            args: profile.command_args(cwd.is_some()),
            cwd,
            env: Vec::new(),
            cols: 80,
            rows: 24,
        }
    }
}

#[derive(Clone, Copy)]
struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

struct Shared {
    sender: Mutex<Option<EventLoopSender>>,
    size: Mutex<WindowSize>,
    exited: AtomicBool,
    on_event: Box<dyn Fn(Event) + Send + Sync>,
}

impl Shared {
    fn write(&self, bytes: Vec<u8>) {
        // 空の書き込みを送ると、PTY への書き込みの列の先頭に残り続けて、あとの入力がすべて届かなくなる
        // （IME は、変換を取り消したときなどに空の文字を確定してくる）
        if bytes.is_empty() {
            return;
        }
        if let Some(sender) = self.sender.lock().unwrap().as_ref() {
            // シェルが終わったあとは送れない。終わったことは Exit で伝えてある
            let _ = sender.send(Msg::Input(bytes.into()));
        }
    }

    fn exit(&self, code: Option<i32>) {
        if !self.exited.swap(true, Ordering::SeqCst) {
            (self.on_event)(Event::Exit(code));
        }
    }
}

#[derive(Clone)]
struct Listener(Arc<Shared>);

impl EventListener for Listener {
    fn send_event(&self, event: AlacEvent) {
        let shared = &self.0;
        match event {
            AlacEvent::Wakeup => (shared.on_event)(Event::Wakeup),
            AlacEvent::Title(title) => (shared.on_event)(Event::Title(Some(title))),
            AlacEvent::ResetTitle => (shared.on_event)(Event::Title(None)),
            AlacEvent::ClipboardStore(_, text) => (shared.on_event)(Event::Copy(text)),
            AlacEvent::Bell => (shared.on_event)(Event::Bell),
            AlacEvent::PtyWrite(text) => shared.write(text.into_bytes()),
            AlacEvent::ColorRequest(index, format) => {
                let rgb = color_at(index);
                let rgb = Rgb {
                    r: (rgb >> 16) as u8,
                    g: (rgb >> 8) as u8,
                    b: rgb as u8,
                };
                shared.write(format(rgb).into_bytes());
            }
            AlacEvent::TextAreaSizeRequest(format) => {
                let size = *shared.size.lock().unwrap();
                shared.write(format(size).into_bytes());
            }
            AlacEvent::ChildExit(status) => shared.exit(status.code()),
            AlacEvent::Exit => shared.exit(None),
            // クリップボードの中身は、プログラムには渡さない
            AlacEvent::ClipboardLoad(..)
            | AlacEvent::MouseCursorDirty
            | AlacEvent::CursorBlinkingChange => {}
        }
    }
}

/// 選ぶ単位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Select {
    /// 文字ごと。
    Chars,
    /// 単語ごと（ダブルクリック）。
    Words,
    /// 行ごと（トリプルクリック）。
    Lines,
}

/// 動いているターミナル。捨てるとシェルも終わる。
pub struct Terminal {
    term: Arc<FairMutex<Term<Listener>>>,
    shared: Arc<Shared>,
}

fn window_size(cols: usize, rows: usize, cell: (u16, u16)) -> WindowSize {
    WindowSize {
        num_lines: rows as u16,
        num_cols: cols as u16,
        cell_width: cell.0,
        cell_height: cell.1,
    }
}

impl Terminal {
    /// シェルを起こす。知らせは別のスレッドから `on_event` に届き、終わると [`Event::Exit`] が 1 回届く。
    pub fn spawn(
        options: &Options,
        on_event: impl Fn(Event) + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let cols = options.cols.max(2);
        let rows = options.rows.max(1);
        let size = window_size(cols, rows, (8, 16));
        let mut env: HashMap<String, String> = options.env.iter().cloned().collect();
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());
        env.insert("TERM_PROGRAM".into(), "malakt-mahoz".into());
        let pty = tty::new(
            &tty::Options {
                shell: Some(tty::Shell::new(
                    options.program.clone(),
                    options.args.clone(),
                )),
                working_directory: options.cwd.clone(),
                drain_on_exit: true,
                env,
                #[cfg(target_os = "windows")]
                escape_args: true,
            },
            size,
            0,
        )?;
        let shared = Arc::new(Shared {
            sender: Mutex::new(None),
            size: Mutex::new(size),
            exited: AtomicBool::new(false),
            on_event: Box::new(on_event),
        });
        let config = Config {
            scrolling_history: SCROLLBACK,
            ..Config::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(
            config,
            &Size { cols, rows },
            Listener(shared.clone()),
        )));
        let event_loop = EventLoop::new(
            term.clone(),
            Listener(shared.clone()),
            settle::Pty::new(pty)?,
            true,
            false,
        )?;
        *shared.sender.lock().unwrap() = Some(event_loop.channel());
        event_loop.spawn();
        Ok(Self { term, shared })
    }

    /// シェルが終わったか。
    pub fn exited(&self) -> bool {
        self.shared.exited.load(Ordering::SeqCst)
    }

    /// バイト列をそのまま送る。
    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        self.shared.write(bytes.into());
    }

    /// 入力した文字を送る。さかのぼっていたら、いちばん下に戻す。
    pub fn input(&self, text: &str) {
        self.to_bottom();
        self.write(text.as_bytes());
    }

    /// キーを送る。文字として入力するキーなら `false`（何も送らない）。
    pub fn key(&self, key: &Key) -> bool {
        let app_cursor = self.mode().contains(TermMode::APP_CURSOR);
        match keys::encode(key, app_cursor) {
            Some(bytes) => {
                self.to_bottom();
                self.write(bytes);
                true
            }
            None => false,
        }
    }

    /// 貼り付ける。
    pub fn paste(&self, text: &str) {
        let bracketed = self.mode().contains(TermMode::BRACKETED_PASTE);
        self.to_bottom();
        self.write(keys::paste(text, bracketed));
    }

    fn mode(&self) -> TermMode {
        *self.term.lock().mode()
    }

    /// 動いているプログラムが、マウスを受け取るか。
    pub fn mouse_mode(&self) -> bool {
        self.mode().intersects(TermMode::MOUSE_MODE)
    }

    /// 画面全体を使うプログラム（代替画面）が動いているか。
    pub fn alt_screen(&self) -> bool {
        self.mode().contains(TermMode::ALT_SCREEN)
    }

    /// マウスの出来事を、動いているプログラムに送る。プログラムが受け取らないなら `false`。
    pub fn mouse(&self, button: Button, action: Action, at: (usize, usize), key: &Key) -> bool {
        let mode = self.mode();
        if !mode.intersects(TermMode::MOUSE_MODE) {
            return false;
        }
        let wanted = match action {
            Action::Press | Action::Release => true,
            Action::Move if button == Button::None => mode.contains(TermMode::MOUSE_MOTION),
            Action::Move => mode.intersects(TermMode::MOUSE_MOTION | TermMode::MOUSE_DRAG),
        };
        if wanted {
            if let Some(bytes) =
                keys::mouse(button, action, at, key, mode.contains(TermMode::SGR_MOUSE))
            {
                self.write(bytes);
            }
        }
        true
    }

    /// ホイール。`lines` が正なら上（古い方）へ。プログラムがマウスを受け取るならそこへ送り、
    /// 画面全体を使うプログラムには ↑↓ として送り、どちらでもなければ出力をさかのぼる。
    pub fn wheel(&self, lines: i32, at: (usize, usize), key: &Key) {
        if lines == 0 {
            return;
        }
        let mode = self.mode();
        let count = lines.unsigned_abs().min(50) as usize;
        if mode.intersects(TermMode::MOUSE_MODE) && !key.shift {
            let button = if lines > 0 {
                Button::WheelUp
            } else {
                Button::WheelDown
            };
            let sgr = mode.contains(TermMode::SGR_MOUSE);
            if let Some(bytes) = keys::mouse(button, Action::Press, at, key, sgr) {
                self.write(bytes.repeat(count));
            }
        } else if mode.contains(TermMode::ALT_SCREEN) && !key.shift {
            let arrow = Key {
                name: if lines > 0 { "up" } else { "down" },
                ..Default::default()
            };
            if let Some(bytes) = keys::encode(&arrow, mode.contains(TermMode::APP_CURSOR)) {
                self.write(bytes.repeat(count));
            }
        } else {
            self.term.lock().scroll_display(Scroll::Delta(lines));
        }
    }

    /// ページ単位でさかのぼる・戻る。
    pub fn page(&self, up: bool) {
        self.term
            .lock()
            .scroll_display(if up { Scroll::PageUp } else { Scroll::PageDown });
    }

    /// いちばん下（今の出力）に戻す。
    pub fn to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    /// 画面の大きさを変える。`cell` は 1 マスの幅と高さ（ピクセル）。
    pub fn resize(&self, cols: usize, rows: usize, cell: (u16, u16)) {
        let (cols, rows) = (cols.max(2), rows.max(1));
        let size = window_size(cols, rows, cell);
        {
            let mut current = self.shared.size.lock().unwrap();
            if (current.num_cols, current.num_lines) == (size.num_cols, size.num_lines) {
                *current = size;
                return;
            }
            *current = size;
        }
        if let Some(sender) = self.shared.sender.lock().unwrap().as_ref() {
            // シェルが終わったあとは送れない。画面の側の大きさだけ変える
            let _ = sender.send(Msg::Resize(size));
        }
        self.term.lock().resize(Size { cols, rows });
    }

    /// 今の大きさ（桁、行）。
    pub fn size(&self) -> (usize, usize) {
        let size = self.shared.size.lock().unwrap();
        (size.num_cols as usize, size.num_lines as usize)
    }

    fn point(term: &Term<Listener>, (col, row): (usize, usize)) -> Point {
        let col = col.min(term.columns().saturating_sub(1));
        let row = row.min(term.screen_lines().saturating_sub(1));
        term::viewport_to_point(term.grid().display_offset(), Point::new(row, Column(col)))
    }

    /// 選び始める。`right` は、マスの右半分を押したか。
    pub fn select(&self, unit: Select, at: (usize, usize), right: bool) {
        let mut term = self.term.lock();
        let point = Self::point(&term, at);
        let ty = match unit {
            Select::Chars => SelectionType::Simple,
            Select::Words => SelectionType::Semantic,
            Select::Lines => SelectionType::Lines,
        };
        let side = if right { Side::Right } else { Side::Left };
        term.selection = Some(Selection::new(ty, point, side));
    }

    /// 選ぶ範囲を、ここまで広げる。
    pub fn select_to(&self, at: (usize, usize), right: bool) {
        let mut term = self.term.lock();
        let point = Self::point(&term, at);
        let side = if right { Side::Right } else { Side::Left };
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    /// さかのぼれる分も含めて、すべて選ぶ。
    pub fn select_all(&self) {
        let mut term = self.term.lock();
        let start = Point::new(term.topmost_line(), Column(0));
        let end = Point::new(term.bottommost_line(), term.last_column());
        let mut selection = Selection::new(SelectionType::Simple, start, Side::Left);
        selection.update(end, Side::Right);
        term.selection = Some(selection);
    }

    pub fn clear_selection(&self) {
        self.term.lock().selection = None;
    }

    /// 選んでいる文字。選んでいなければ `None`。
    pub fn selected_text(&self) -> Option<String> {
        self.term
            .lock()
            .selection_to_string()
            .filter(|text| !text.is_empty())
    }

    /// 画面の中身を写し取る。
    pub fn snapshot(&self) -> Snapshot {
        let term = self.term.lock();
        let (cols, rows) = (term.columns(), term.screen_lines());
        let content = term.renderable_content();
        let blank = Cell {
            c: ' ',
            fg: FOREGROUND,
            bg: BACKGROUND,
            style: 0,
        };
        let mut cells = vec![blank; cols * rows];
        let mut combining = Vec::new();
        let offset = content.display_offset;
        for indexed in content.display_iter {
            let Some(at) = term::point_to_viewport(offset, indexed.point) else {
                continue;
            };
            if at.line >= rows || at.column.0 >= cols {
                continue;
            }
            let cell = indexed.cell;
            let flags = cell.flags;
            let mut fg = resolve(cell.fg, content.colors);
            let mut bg = resolve(cell.bg, content.colors);
            if flags.contains(Flags::DIM) {
                fg = dim(fg);
            }
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            let mut bits = 0;
            for (flag, bit) in [
                (Flags::BOLD, style::BOLD),
                (Flags::ITALIC, style::ITALIC),
                (Flags::ALL_UNDERLINES, style::UNDERLINE),
                (Flags::STRIKEOUT, style::STRIKE),
                (Flags::WIDE_CHAR, style::WIDE),
                (
                    Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER,
                    style::SPACER,
                ),
            ] {
                if flags.intersects(flag) {
                    bits |= bit;
                }
            }
            if content
                .selection
                .is_some_and(|range| range.contains(indexed.point))
            {
                bits |= style::SELECTED;
            }
            let ix = at.line * cols + at.column.0;
            cells[ix] = Cell {
                c: if cell.c == '\t' { ' ' } else { cell.c },
                fg,
                bg,
                style: bits,
            };
            if let Some(extra) = cell.zerowidth() {
                combining.push((ix, extra.iter().collect()));
            }
        }
        let input_at = term::point_to_viewport(offset, content.cursor.point)
            .filter(|at| at.line < rows && at.column.0 < cols);
        let cursor = input_at
            .filter(|_| content.cursor.shape != CursorShape::Hidden)
            .map(|at| {
                let caret = match content.cursor.shape {
                    CursorShape::Underline => Caret::Underline,
                    CursorShape::Beam => Caret::Beam,
                    _ => Caret::Block,
                };
                (at.column.0, at.line, caret)
            });
        Snapshot {
            cols,
            rows,
            cells,
            combining,
            cursor,
            input_at: input_at.map(|at| (at.column.0, at.line)),
            offset,
            history: term.grid().history_size(),
        }
    }

    /// シェルを終わらせる。
    pub fn kill(&self) {
        if let Some(sender) = self.shared.sender.lock().unwrap().take() {
            // もう終わっていれば送れない
            let _ = sender.send(Msg::Shutdown);
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.kill();
    }
}

/// 罫線の文字（U+2500〜U+257F）の、左・右・上・下に伸びる線の太さ（0 = なし、1 = 細い、2 = 太い）。
/// 2 ビットずつ、下の桁から左・右・上・下。書体の字形に頼らず、マスいっぱいに線を引くために使う。
const BOX: [u8; 128] = [
    0x05, 0x0a, 0x50, 0xa0, 0x05, 0x0a, 0x50, 0xa0, 0x05, 0x0a, 0x50, 0xa0, 0x44, 0x48, 0x84, 0x88,
    0x41, 0x42, 0x81, 0x82, 0x14, 0x18, 0x24, 0x28, 0x11, 0x12, 0x21, 0x22, 0x54, 0x58, 0x64, 0x94,
    0xa4, 0x68, 0x98, 0xa8, 0x51, 0x52, 0x61, 0x91, 0xa1, 0x62, 0x92, 0xa2, 0x45, 0x46, 0x49, 0x4a,
    0x85, 0x86, 0x89, 0x8a, 0x15, 0x16, 0x19, 0x1a, 0x25, 0x26, 0x29, 0x2a, 0x55, 0x56, 0x59, 0x5a,
    0x65, 0x95, 0xa5, 0x66, 0x69, 0x96, 0x99, 0x6a, 0x9a, 0xa6, 0xa9, 0xaa, 0x05, 0x0a, 0x50, 0xa0,
    0x0a, 0xa0, 0x48, 0x84, 0x88, 0x42, 0x81, 0x82, 0x18, 0x24, 0x28, 0x12, 0x21, 0x22, 0x58, 0xa4,
    0xa8, 0x52, 0xa1, 0xa2, 0x4a, 0x85, 0x8a, 0x1a, 0x25, 0x2a, 0x5a, 0xa5, 0xaa, 0x44, 0x41, 0x11,
    0x14, 0x00, 0x00, 0x00, 0x01, 0x10, 0x04, 0x40, 0x02, 0x20, 0x08, 0x80, 0x09, 0x90, 0x06, 0x60,
];

/// 罫線の文字なら、左・右・上・下の線の太さ。斜めの線は `None`（書体の字形で描く）。
pub fn box_arms(c: char) -> Option<[u8; 4]> {
    let at = (c as u32).checked_sub(0x2500)?;
    let bits = *BOX.get(at as usize)?;
    (bits != 0).then_some([bits & 3, bits >> 2 & 3, bits >> 4 & 3, bits >> 6 & 3])
}

/// ブロックの文字（U+2580〜U+259F）なら、塗る四角（マスを 1 とした x・y・幅・高さ）と濃さ。
pub fn block_rects(c: char) -> Option<(Vec<[f32; 4]>, f32)> {
    let at = (c as u32).checked_sub(0x2580).filter(|at| *at < 0x20)?;
    let eighth = |n: u32| n as f32 / 8.;
    let quad = |bits: u8| {
        let mut out = Vec::new();
        for (bit, x, y) in [(1, 0., 0.), (2, 0.5, 0.), (4, 0., 0.5), (8, 0.5, 0.5)] {
            if bits & bit != 0 {
                out.push([x, y, 0.5, 0.5]);
            }
        }
        out
    };
    let full = vec![[0., 0., 1., 1.]];
    Some(match at {
        0x00 => (vec![[0., 0., 1., 0.5]], 1.),
        // 下から n/8
        0x01..=0x08 => (vec![[0., 1. - eighth(at), 1., eighth(at)]], 1.),
        // 左から n/8
        0x09..=0x0f => (vec![[0., 0., eighth(0x10 - at), 1.]], 1.),
        0x10 => (vec![[0.5, 0., 0.5, 1.]], 1.),
        0x11 => (full, 0.25),
        0x12 => (full, 0.5),
        0x13 => (full, 0.75),
        0x14 => (vec![[0., 0., 1., eighth(1)]], 1.),
        0x15 => (vec![[1. - eighth(1), 0., eighth(1), 1.]], 1.),
        // 4 つに分けたマス。1 = 左上、2 = 右上、4 = 左下、8 = 右下
        0x16 => (quad(4), 1.),
        0x17 => (quad(8), 1.),
        0x18 => (quad(1), 1.),
        0x19 => (quad(1 | 4 | 8), 1.),
        0x1a => (quad(1 | 8), 1.),
        0x1b => (quad(1 | 2 | 4), 1.),
        0x1c => (quad(1 | 2 | 8), 1.),
        0x1d => (quad(2), 1.),
        0x1e => (quad(2 | 4), 1.),
        _ => (quad(2 | 4 | 8), 1.),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_drawing_characters_know_their_lines() {
        // 左・右・上・下
        assert_eq!(box_arms('─'), Some([1, 1, 0, 0]));
        assert_eq!(box_arms('│'), Some([0, 0, 1, 1]));
        assert_eq!(box_arms('┌'), Some([0, 1, 0, 1]));
        // 丸い角も、同じ向きの線として引く
        assert_eq!(box_arms('╭'), Some([0, 1, 0, 1]));
        assert_eq!(box_arms('╯'), Some([1, 0, 1, 0]));
        assert_eq!(box_arms('┼'), Some([1, 1, 1, 1]));
        assert_eq!(box_arms('━'), Some([2, 2, 0, 0]));
        assert_eq!(box_arms('═'), Some([2, 2, 0, 0]));
        assert_eq!(box_arms('┡'), Some([0, 2, 2, 1]));
        assert_eq!(box_arms('╴'), Some([1, 0, 0, 0]));
        assert_eq!(box_arms('┄'), Some([1, 1, 0, 0]));
        assert_eq!(box_arms('╱'), None);
        assert_eq!(box_arms('a'), None);
        assert_eq!(box_arms('▀'), None);
    }

    #[test]
    fn block_characters_fill_parts_of_the_cell() {
        assert_eq!(block_rects('█'), Some((vec![[0., 0., 1., 1.]], 1.)));
        assert_eq!(block_rects('▀'), Some((vec![[0., 0., 1., 0.5]], 1.)));
        assert_eq!(block_rects('▄'), Some((vec![[0., 0.5, 1., 0.5]], 1.)));
        assert_eq!(block_rects('▌'), Some((vec![[0., 0., 0.5, 1.]], 1.)));
        assert_eq!(block_rects('▐'), Some((vec![[0.5, 0., 0.5, 1.]], 1.)));
        assert_eq!(block_rects('▁'), Some((vec![[0., 0.875, 1., 0.125]], 1.)));
        assert_eq!(block_rects('▏'), Some((vec![[0., 0., 0.125, 1.]], 1.)));
        assert_eq!(block_rects('▒'), Some((vec![[0., 0., 1., 1.]], 0.5)));
        assert_eq!(
            block_rects('▚'),
            Some((vec![[0., 0., 0.5, 0.5], [0.5, 0.5, 0.5, 0.5]], 1.))
        );
        assert_eq!(block_rects('─'), None);
        assert_eq!(block_rects('a'), None);
    }

    #[test]
    fn indexed_colors_cover_the_cube_and_the_grays() {
        assert_eq!(indexed(1), 0xff7b72);
        assert_eq!(indexed(16), 0x000000);
        assert_eq!(indexed(21), 0x0000ff);
        assert_eq!(indexed(208), 0xff8700);
        assert_eq!(indexed(231), 0xffffff);
        assert_eq!(indexed(232), 0x080808);
        assert_eq!(indexed(255), 0xeeeeee);
    }
}
