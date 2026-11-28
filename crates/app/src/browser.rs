//! アプリ内ブラウザ。ターミナルと同じ、シェルの場所のタブとして開く。
//!
//! ページは OS のブラウザの部品（[`crate::webview`]）で、ウィンドウの上に重なる。
//! 描いたときに場所を合わせ、描かなかったとき（ほかのタブ、閉じたシェル）と、
//! アプリのメニューやダイアログが上に出ているときは隠す。メニューやダイアログの間は、
//! 隠す前に撮ったページの画像を同じ場所に出すので、後ろのページは消えずに見えたままになる。
//!
//! ログインの情報（Cookie）は OS の部品がデータベースの隣の `browser` に保存する
//! （macOS は OS が決めた場所）。キャッシュは上限を超えたら、最初のページを開く前に消す。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::press::button;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Disableable, Placement, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::{json, Value};

use crate::feel;
use crate::render::{app_icon, col, row};
use crate::term::{self, Terms};

/// 一度に開けるページの数。小さなウィンドウのページも数える。
pub const MAX_PAGES: usize = 12;
/// ホイールで押したリンクを小さなウィンドウで開いたあと、同じリンクの「新しいタブで開く」を捨てる時間。
/// ホイールでの押下を止められない部品では、両方が届く。
const POPUP_ECHO: Duration = Duration::from_secs(1);
/// 閉じたタブを開き直せる数（Ctrl+Shift+T）。
const MAX_CLOSED: usize = 20;
/// 何も開いていないページ。
pub const BLANK: &str = "about:blank";
/// 開いているページの場所を残す設定。次に起動したときに、同じタブを並べる。
const TABS_SETTING: &str = "browser.tabs";
/// キャッシュがこれより大きくなったら、最初のページを開く前に消す。ログインの情報は消さない。
pub const CACHE_LIMIT: u64 = 128 * 1024 * 1024;
/// AI がページの文字を読むツールの名前。読み終わるのを待つので、`ai.rs` で動かす。
pub const READ_TOOL: &str = "read_browser";
/// AI に渡すページの文字の上限。
const MAX_READ_CHARS: usize = 20_000;
/// 文字の検索に使うページ。URL でない言葉を URL の欄に書いたときに開く。
const SEARCH: &str = "https://www.google.com/search?q=";
/// メニューを開いてから、ページの画像が撮れるのを待つ長さ。撮れなければ画像なしで隠す。
const SHOT_WAIT: Duration = Duration::from_millis(300);

/// ページの画像を受け取る関数。OS の部品が撮り終わったら呼ぶ。
pub type Shot = Box<dyn FnOnce(Result<Arc<RenderImage>, String>) + Send + 'static>;

/// ページの拡大率の段階。Ctrl+ホイールや Ctrl++ / Ctrl+- で 1 段ずつ動く。
pub const ZOOM_LEVELS: &[f64] = &[
    0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 5.0,
];
/// 拡大率を変えたあと、拡大率の表示を目立たせておく時間。100% に戻したときは、このあと消える。
pub const ZOOM_SHOWN: Duration = Duration::from_millis(1500);
/// 100% に戻した表示が消えるまでの時間。
const ZOOM_FADE: Duration = Duration::from_millis(200);

/// 拡大率の変え方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zoom {
    In,
    Out,
    /// 100% に戻す。
    Reset,
}

impl Zoom {
    /// ページから届いた知らせ（`zoom:in` など）を読む。
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "zoom:in" => Some(Zoom::In),
            "zoom:out" => Some(Zoom::Out),
            "zoom:reset" => Some(Zoom::Reset),
            _ => None,
        }
    }
}

/// `now` から 1 段動かした拡大率。端ではそのまま。
pub fn zoom_step(now: f64, step: Zoom) -> f64 {
    let first = ZOOM_LEVELS[0];
    let last = ZOOM_LEVELS[ZOOM_LEVELS.len() - 1];
    match step {
        Zoom::Reset => 1.0,
        Zoom::In => ZOOM_LEVELS
            .iter()
            .copied()
            .find(|level| *level > now + 0.001)
            .unwrap_or(last),
        Zoom::Out => ZOOM_LEVELS
            .iter()
            .rev()
            .copied()
            .find(|level| *level < now - 0.001)
            .unwrap_or(first),
    }
}

/// 拡大率の表示（`1.1` なら `110%`）。
pub fn zoom_label(zoom: f64) -> String {
    format!("{}%", (zoom * 100.).round() as i64)
}

fn is_actual_size(zoom: f64) -> bool {
    (zoom - 1.0).abs() < 0.001
}

/// OS のブラウザの部品からの知らせ。
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// 読み込みを始めた（`done` が `false`）、終わった。
    Load { done: bool, url: String },
    /// ページの題名が変わった。
    Title(String),
    /// ページを読み込まずに場所が変わった（`history.pushState`、戻る・進む、`#` の移動）。
    /// 場所は OS の部品から読む（ページが書いた文字は信じない）。
    Moved,
    /// ページが新しいウィンドウで開こうとした。アプリの新しいタブで開く。
    NewTab(String),
    /// ページの中のリンクをホイールで押した。小さなウィンドウで開く。
    Popup(String),
    /// アプリの中で開かないリンク（mailto: など）。OS に渡す。
    External(String),
    /// ページを押した、ページの中に入力が移った。
    Focused,
    /// マウスがページに入った（`true`）、出た（`false`）。
    Pointer(bool),
    /// ページの画像を撮り終わった。`gen` は撮り始めたときの [`Page`] の画像の回。
    Snapshot {
        gen: u64,
        image: Result<Arc<RenderImage>, String>,
    },
    /// ページの上で Ctrl+ホイールや Ctrl++ / Ctrl+- / Ctrl+0 を押した。
    Zoom(Zoom),
    /// ページの上でロックのキー（Ctrl+Shift+L / ⇧⌘L）を押した。
    Lock,
    /// ページの中でブラウザやアプリのキー（`ctrl-r` など、[`crate::shell_keys::page_keys`]）を押した。
    Key(String),
    /// ページの中を探し終わった。`turn` は探し始めたときの [`Page`] の探した回。
    Found { turn: u64, found: bool },
    /// ファイルのダウンロードが始まった・進んだ・終わった。
    Download(crate::downloads::Note),
    /// ページを PDF に保存し終わった。`item` はダウンロードの一覧の番号。
    PdfSaved {
        item: u64,
        result: Result<(), String>,
        /// 保存した場所と、使った設定。仕上げ（ページを選ぶ・フォントを確かめる）に使う。
        path: std::path::PathBuf,
        setup: Box<crate::print_setup::PrintSetup>,
        /// ページの数を数えるための 1 回目か（ページを選んで 1 枚に何ページも並べるとき）。
        counting: bool,
    },
}

/// PDF に保存し終わったときに呼ぶ関数。
pub type PdfDone = Box<dyn FnOnce(Result<(), String>) + Send + 'static>;

/// 開いているページを動かす口。本物は [`crate::webview`]、テストでは差し替える。
pub trait Engine {
    fn load(&self, url: &str) -> Result<(), String>;
    fn back(&self) -> Result<(), String>;
    fn forward(&self) -> Result<(), String>;
    fn reload(&self) -> Result<(), String>;
    fn stop(&self) -> Result<(), String>;
    /// ウィンドウの中の場所に置く。
    fn place(&self, bounds: Bounds<Pixels>) -> Result<(), String>;
    fn show(&self, visible: bool) -> Result<(), String>;
    fn focus(&self) -> Result<(), String>;
    /// キーの入力先を、アプリの画面へ戻す。
    fn focus_parent(&self) -> Result<(), String>;
    fn url(&self) -> Option<String>;
    /// 戻れるか、進めるか。
    fn history(&self) -> (bool, bool);
    /// ページの文字を読む。`done` には JSON の文字列が届く。
    fn read(&self, done: Box<dyn Fn(String) + Send + 'static>) -> Result<(), String>;
    /// ログインの情報とキャッシュを消す。
    fn clear_data(&self) -> Result<(), String>;
    /// 今見えている部分の画像を撮る。メニューやダイアログが上に出ている間、隠したページの代わりに出す。
    fn snapshot(&self, done: Shot) -> Result<(), String>;
    /// ページの拡大率を変える（`1.0` が 100%）。
    fn zoom(&self, factor: f64) -> Result<(), String>;
    /// 印刷のダイアログを出す。
    fn print(&self) -> Result<(), String>;
    /// ページを `setup` の設定で PDF にして `path` に保存する。終わったら `done` を呼ぶ。
    /// `pages` があればそのページ（1 から数える）だけ、なければすべてのページを出す。
    /// `setup` のページの選び方は見ない（呼ぶ側が `pages` にする）。
    fn save_pdf(
        &self,
        path: &std::path::Path,
        setup: &crate::print_setup::PrintSetup,
        pages: Option<&[u32]>,
        done: PdfDone,
    ) -> Result<(), String>;
    /// ページの中で `query` を探して選ぶ。`again` なら今選んでいる所の次から、でなければ頭から。
    /// `done` には見つかったかが届く。
    fn find(
        &self,
        query: &str,
        backwards: bool,
        again: bool,
        done: Box<dyn Fn(bool) + Send + 'static>,
    ) -> Result<(), String>;
}

/// ページを作るときに渡すもの。
pub struct Setup {
    pub id: u64,
    pub url: String,
    pub bounds: Bounds<Pixels>,
    pub events: async_channel::Sender<(u64, Event)>,
}

type MakeEngine = dyn Fn(&mut Window, Setup) -> Result<Rc<dyn Engine>, String>;

/// ページを作る関数。アプリは OS のブラウザの部品、テストでは偽物を使う。
pub struct Factory(pub Rc<MakeEngine>);

impl Global for Factory {}

/// OS のブラウザの部品でページを開くようにする。アプリの起動のときに呼ぶ。
pub fn use_system_browser(cx: &mut App) {
    cx.set_global(Factory(Rc::new(crate::webview::create)));
}

/// 開いているページ 1 つ。
pub struct Page {
    pub id: u64,
    /// 今の場所。
    pub url: String,
    /// ページの題名。
    pub title: Option<SharedString>,
    pub loading: bool,
    pub can_back: bool,
    pub can_forward: bool,
    /// 開けなかった理由。
    pub error: Option<SharedString>,
    /// タブではなく、小さなウィンドウで開いている。タブの並びには出さない。
    pub popup: bool,
    /// 小さなウィンドウ。開き終わるまでは `None`。
    pub(crate) popup_window: Option<AnyWindowHandle>,
    pub(crate) engine: Option<Rc<dyn Engine>>,
    /// ページを置いているウィンドウ。
    pub(crate) window: Option<WindowId>,
    /// 今見せているか。
    pub(crate) shown: bool,
    pub(crate) bounds: Option<Bounds<Pixels>>,
    /// 最後に描いたウィンドウと、その描いた回。
    pub(crate) painted: Option<(WindowId, u64)>,
    /// 作っている途中。
    pub(crate) creating: bool,
    /// キーの入力先がページにある。
    pub(crate) focused: bool,
    /// 今のページの画像。隠している間、ページの代わりに出す。ページが変わったら捨てる。
    pub shot: Option<Arc<RenderImage>>,
    /// 画像の回。ページが変わるたびに増やし、撮っている途中の古い画像を捨てる。
    pub(crate) shot_gen: u64,
    /// 画像を撮っている途中なら、撮り始めた時刻。
    pub(crate) shooting: Option<Instant>,
    /// 画像を撮れなかった。このページでは、もう撮らずに画像なしで隠す。
    pub(crate) shot_failed: bool,
    /// 隠している間にページが変わった。今の画像は、ページを見せたら捨てる。
    pub(crate) shot_stale: bool,
    /// 拡大率（`1.0` が 100%）。
    pub zoom: f64,
    /// 拡大率を変えた回数。表示の動きを、変えるたびに始め直す。
    pub(crate) zoom_turn: u64,
    /// 拡大率の表示が出てきたときの `zoom_turn`。この回だけ、ふわっと出す。
    pub(crate) zoom_entered: u64,
    /// 拡大率を変えたばかりで、表示を目立たせている。
    pub zoom_recent: bool,
    /// 100% に戻した表示が、消えていく途中。
    pub(crate) zoom_leaving: bool,
    /// ページの中を探した回。古い結果を捨てる。
    pub(crate) find_turn: u64,
    /// 最後に探した言葉が、ページの中になかった。
    pub find_missing: bool,
    /// 印刷のダイアログやダウンロードの一覧を、ページの上に描いたウィンドウと、その描いた回。
    /// 描いている間は、ページを隠して画像を出す。描かなくなったら（閉じた、ダイアログの部品ごと
    /// なくなった）、次に描くときにはもう隠さない。
    pub(crate) covered: Option<(WindowId, u64)>,
}

impl Page {
    fn new(id: u64, url: String) -> Self {
        Self {
            id,
            url,
            title: None,
            loading: false,
            can_back: false,
            can_forward: false,
            error: None,
            popup: false,
            popup_window: None,
            engine: None,
            window: None,
            shown: false,
            bounds: None,
            painted: None,
            creating: false,
            focused: false,
            shot: None,
            shot_gen: 0,
            shooting: None,
            shot_failed: false,
            shot_stale: false,
            zoom: 1.0,
            zoom_turn: 0,
            zoom_entered: 0,
            zoom_recent: false,
            zoom_leaving: false,
            find_turn: 0,
            find_missing: false,
            covered: None,
        }
    }

    /// ページが変わった（マウスが入った、読み込んだ、場所や大きさが変わった）。撮った画像を捨てる。
    /// 隠している間（メニューやダイアログの間）は、出している画像を見せたときまで残す。
    fn forget_shot(&mut self) {
        if self.shown {
            self.shot = None;
        } else {
            self.shot_stale = true;
        }
        self.shot_gen += 1;
        self.shooting = None;
    }

    /// 見せているページの画像を撮り始める。撮れたら `events` に届く。
    fn shoot(&mut self, events: &async_channel::Sender<(u64, Event)>) -> Result<(), String> {
        let Some(engine) = self.engine.clone() else {
            return Ok(());
        };
        if !self.shown || self.shot.is_some() || self.shooting.is_some() || self.shot_failed {
            return Ok(());
        }
        let (id, gen) = (self.id, self.shot_gen);
        let events = events.clone();
        self.shooting = Some(Instant::now());
        let started = engine.snapshot(Box::new(move |image| {
            // アプリを閉じている途中なら、受け取る先がない
            let _ = events.try_send((id, Event::Snapshot { gen, image }));
        }));
        if started.is_err() {
            self.shooting = None;
            self.shot_failed = true;
        }
        started
    }

    /// 拡大率の表示を出すか。100% でないあいだと、変えたばかりのとき。
    pub fn zoom_badge(&self) -> bool {
        !is_actual_size(self.zoom) || self.zoom_recent || self.zoom_leaving
    }

    /// タブに出す名前。題名、なければ場所。
    pub fn label(&self) -> SharedString {
        if let Some(title) = &self.title {
            return title.clone();
        }
        if self.url == BLANK || self.url.is_empty() {
            return "新しいタブ".into();
        }
        host(&self.url).unwrap_or(&self.url).to_owned().into()
    }

    /// AI に渡す、ページの説明。
    fn json(&self, active: bool) -> Value {
        json!({
            "tab": self.id,
            "url": self.url,
            "title": self.title.as_ref().map(|t| t.to_string()),
            "loading": self.loading,
            "shown": active,
            "window": self.popup,
            "zoom": zoom_label(self.zoom),
        })
    }
}

/// URL のホストの部分（`https://github.com/a` なら `github.com`）。
pub fn host(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..end];
    // ユーザー名は出さない
    let host = host.rsplit('@').next().unwrap_or(host);
    (!host.is_empty()).then_some(host)
}

/// アプリの中で開ける場所（http と https）か。
pub fn is_web(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// URL の欄に書いたものを、開く場所にする。URL らしくなければ検索する。
pub fn normalize(input: &str) -> String {
    let text = input.trim();
    if text.is_empty() {
        return BLANK.to_owned();
    }
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("about:")
        || lower.starts_with("data:")
    {
        return text.to_owned();
    }
    let looks_like_host = !text.contains(char::is_whitespace)
        && (text.contains('.') || text.starts_with("localhost"))
        && !text.starts_with('.')
        && !text.ends_with('.');
    if looks_like_host {
        let local = lower.starts_with("localhost") || lower.starts_with("127.");
        return format!("{}://{text}", if local { "http" } else { "https" });
    }
    let mut query = String::new();
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                query.push(*byte as char)
            }
            b' ' => query.push('+'),
            _ => query.push_str(&format!("%{byte:02X}")),
        }
    }
    format!("{SEARCH}{query}")
}

// ---- 開いているページ（ターミナルと同じ一覧に入る） ----

impl Terms {
    pub fn page(&self, id: u64) -> Option<&Page> {
        self.pages.iter().find(|page| page.id == id)
    }

    pub(crate) fn page_mut(&mut self, id: u64) -> Option<&mut Page> {
        self.pages.iter_mut().find(|page| page.id == id)
    }

    /// 見ているページ。ターミナルか AI のタブを見ていれば `None`。
    pub fn active_page(&self) -> Option<&Page> {
        self.active.and_then(|id| self.page(id))
    }

    /// ページを新しいタブで開いて、そのタブを見せる。同じ場所のタブがあれば、それを見せる。
    pub fn open_page(&mut self, url: &str, cx: &mut Context<Self>) -> Option<u64> {
        self.error = None;
        if !self.db.read(cx).unlocked {
            return None;
        }
        let url = normalize(url);
        if url != BLANK {
            let same = self.pages.iter().find(|p| !p.popup && p.url == url);
            if let Some(id) = same.map(|p| p.id) {
                self.activate(Some(id), cx);
                return Some(id);
            }
        }
        if self.pages.len() >= MAX_PAGES {
            self.fail(
                format!(
                    "ブラウザのタブは {MAX_PAGES} 個までです。タブを閉じてから開いてください。"
                ),
                cx,
            );
            return None;
        }
        let id = self.next;
        self.next += 1;
        self.pages.push(Page::new(id, url.clone()));
        self.active = Some(id);
        self.focus_wanted = false;
        // 何も開いていないタブは、すぐに場所を書けるようにする
        self.focus_bar = url == BLANK;
        self.save_pages(cx);
        cx.notify();
        Some(id)
    }

    /// ページを小さなウィンドウ用に作る。タブの並びには出さず、場所も残さない。
    /// ウィンドウは [`crate::browser_popup::open`] が開く。
    pub(crate) fn open_popup_page(&mut self, url: &str, cx: &mut Context<Self>) -> Option<u64> {
        self.error = None;
        if !self.db.read(cx).unlocked {
            return None;
        }
        if self.pages.len() >= MAX_PAGES {
            self.fail(
                format!(
                    "ブラウザのページは {MAX_PAGES} 個までです。タブか小さなウィンドウを閉じてから開いてください。"
                ),
                cx,
            );
            return None;
        }
        let id = self.next;
        self.next += 1;
        let mut page = Page::new(id, normalize(url));
        page.popup = true;
        self.pages.push(page);
        cx.notify();
        Some(id)
    }

    /// 小さなウィンドウのページを、ブラウザのタブに移して見せる。ウィンドウは閉じる。
    /// 戻すウィンドウは `None`。開いていたウィンドウ（閉じたあとに呼んだときは `None`）を返す。
    pub(crate) fn dock_popup(
        &mut self,
        id: u64,
        cx: &mut Context<Self>,
    ) -> Option<AnyWindowHandle> {
        let page = self.page_mut(id).filter(|p| p.popup)?;
        page.popup = false;
        let window = page.popup_window.take();
        // ウィンドウを閉じる前に部品を捨てる。タブで見せるときに、そのウィンドウで作り直す
        page.engine = None;
        page.window = None;
        page.shown = false;
        page.focused = false;
        page.bounds = None;
        page.forget_shot();
        self.activate(Some(id), cx);
        self.save_pages(cx);
        cx.notify();
        window
    }

    /// ウィンドウを閉じる前に、そこに置いたページの部品を捨てる。
    /// タブのページは、ほかのウィンドウで見せるときに作り直す。
    pub(crate) fn drop_engines_in(&mut self, window: WindowId) {
        for page in &mut self.pages {
            if page.window != Some(window) {
                continue;
            }
            page.engine = None;
            page.window = None;
            page.shown = false;
            page.focused = false;
            page.bounds = None;
            page.forget_shot();
        }
    }

    /// 見ているページか、`id` のページ。
    fn target_page(&self, id: Option<u64>) -> Option<u64> {
        id.or(self.active).filter(|id| self.page(*id).is_some())
    }

    /// 場所を開く。URL らしくなければ検索する。
    pub fn navigate(&mut self, id: u64, input: &str, cx: &mut Context<Self>) {
        let url = normalize(input);
        let Some(page) = self.page_mut(id) else {
            return;
        };
        page.url = url.clone();
        page.title = None;
        let failed = match page.engine.clone() {
            Some(engine) => {
                page.loading = true;
                engine.load(&url).err()
            }
            // まだ作っていなければ、作るときにこの場所を開く
            None => None,
        };
        if let Some(error) = failed {
            self.fail(format!("{url} を開けませんでした: {error}"), cx);
        }
        self.save_pages(cx);
        cx.notify();
    }

    /// 戻る・進む・読み込み直す・止める。
    pub fn page_step(&mut self, id: u64, step: Step, cx: &mut Context<Self>) {
        let Some(page) = self.page_mut(id) else {
            return;
        };
        if step == Step::Reload && page.engine.is_none() {
            // 開けなかったページは、もう一度作る
            page.error = None;
            cx.notify();
            return;
        }
        let Some(engine) = page.engine.clone() else {
            return;
        };
        let result = match step {
            Step::Back => engine.back(),
            Step::Forward => engine.forward(),
            Step::Reload => engine.reload(),
            Step::Stop => engine.stop(),
        };
        match result {
            Ok(()) => {
                if step == Step::Stop {
                    page.loading = false;
                }
            }
            Err(e) => self.fail(format!("{}できませんでした: {e}", step.label()), cx),
        }
        cx.notify();
    }

    /// ページの拡大率を 1 段変える・100% に戻す。変えたら、拡大率を少しのあいだ目立たせて出す。
    pub fn zoom_page(&mut self, id: u64, step: Zoom, cx: &mut Context<Self>) {
        let Some(page) = self.page_mut(id) else {
            return;
        };
        let to = zoom_step(page.zoom, step);
        if let Some(engine) = page.engine.clone() {
            if let Err(e) = engine.zoom(to) {
                self.fail(format!("ページの拡大率を変更できませんでした: {e}"), cx);
                return;
            }
        }
        let entering = !page.zoom_badge();
        page.zoom = to;
        // 拡大率が変わると見た目も変わるので、撮った画像を捨てる
        page.forget_shot();
        page.zoom_turn += 1;
        if entering {
            page.zoom_entered = page.zoom_turn;
        }
        page.zoom_recent = true;
        page.zoom_leaving = false;
        let turn = page.zoom_turn;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ZOOM_SHOWN).await;
            // タブを閉じていれば、何もしない
            let leaving = this
                .update(cx, |this, cx| {
                    let page = this.page_mut(id).filter(|p| p.zoom_turn == turn)?;
                    page.zoom_recent = false;
                    page.zoom_leaving = is_actual_size(page.zoom);
                    cx.notify();
                    Some(page.zoom_leaving)
                })
                .ok()
                .flatten()
                .unwrap_or(false);
            if !leaving {
                return;
            }
            cx.background_executor().timer(ZOOM_FADE).await;
            // タブを閉じていれば、何もしない
            let _ = this.update(cx, |this, cx| {
                if let Some(page) = this.page_mut(id).filter(|p| p.zoom_turn == turn) {
                    page.zoom_leaving = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// ページの中で押したキーを、そのページを置いているウィンドウのブラウザの画面で押したものとして扱う。
    /// アプリのキー（F11、Ctrl+J など）も、ここからいつもどおりに働く。
    fn page_key(&mut self, id: u64, key: &str, cx: &mut Context<Self>) {
        // ページのスクリプトは、どんな文字でも送れる。知らせると決めたキーだけを受ける
        if !crate::shell_keys::is_page_key(key) {
            return;
        }
        let Some(keystroke) = crate::shell_keys::keystroke(key) else {
            return;
        };
        let Some(here) = self.page(id).and_then(|p| p.window) else {
            return;
        };
        self.browser_focus
            .retain(|(_, focus)| focus.upgrade().is_some());
        let focus = self
            .browser_focus
            .iter()
            .find(|(window, _)| *window == here)
            .and_then(|(_, focus)| focus.upgrade());
        let Some(window) = cx.windows().into_iter().find(|w| w.window_id() == here) else {
            return;
        };
        // 押したキーの働きは、この一覧（Terms）を書き換えることがあるので、書き換え終わってから動かす
        cx.defer(move |cx| {
            // ウィンドウを閉じたばかりなら、押した先がない
            let _ = window.update(cx, |_, window, cx| {
                if let Some(focus) = &focus {
                    if !focus.contains_focused(window, cx) {
                        window.focus(focus, cx);
                    }
                }
                window.dispatch_keystroke(keystroke, cx);
            });
        });
    }

    /// ブラウザの画面のキーの入力先を覚える。ページの中で押したキーを、ここに届ける。
    pub(crate) fn note_browser_focus(&mut self, window: WindowId, focus: &FocusHandle) {
        self.browser_focus
            .retain(|(w, f)| *w != window && f.upgrade().is_some());
        self.browser_focus.push((window, focus.downgrade()));
    }

    /// ページにあるキーの入力先を、アプリの画面へ戻す（URL の欄や検索の欄に書くとき）。
    pub(crate) fn release_page(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(page) = self.page_mut(id) else {
            return;
        };
        if !std::mem::take(&mut page.focused) {
            return;
        }
        if let Some(engine) = page.engine.clone() {
            if let Err(e) = engine.focus_parent() {
                self.fail(format!("キーの入力先を戻せませんでした: {e}"), cx);
            }
        }
    }

    /// キーの入力先をページに移す。まだ見せていなければ、見せたときに移す。
    pub(crate) fn focus_page(&mut self, id: u64, cx: &mut Context<Self>) {
        self.focus_page = Some(id);
        cx.notify();
    }

    /// 最後に閉じたブラウザのタブを開き直す。
    pub fn reopen_page(&mut self, cx: &mut Context<Self>) -> Option<u64> {
        let url = self.closed_pages.pop()?;
        let opened = self.open_page(&url, cx);
        match opened {
            // 閉じる前と同じく、ページの中でキーを使える
            Some(id) => self.focus_page = Some(id),
            // 開けなかったら（タブが多すぎるなど）、次にまた開き直せるように戻す
            None => self.closed_pages.push(url),
        }
        opened
    }

    /// ページの中で `query` を探す。見つからなければ、検索の欄に出す。
    pub fn find_in_page(
        &mut self,
        id: u64,
        query: &str,
        backwards: bool,
        again: bool,
        cx: &mut Context<Self>,
    ) {
        let events = self.page_events.clone();
        let Some(page) = self.page_mut(id) else {
            return;
        };
        page.find_turn += 1;
        page.find_missing = false;
        let Some(engine) = page.engine.clone().filter(|_| !query.is_empty()) else {
            cx.notify();
            return;
        };
        let turn = page.find_turn;
        let done = Box::new(move |found| {
            // アプリを閉じている途中なら、受け取る先がない
            let _ = events.try_send((id, Event::Found { turn, found }));
        });
        if let Err(e) = engine.find(query, backwards, again, done) {
            self.fail(format!("ページの中を検索できませんでした: {e}"), cx);
        }
        cx.notify();
    }

    /// ページを印刷する（OS の印刷のダイアログ）。
    pub fn print_page(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(engine) = self.page(id).and_then(|p| p.engine.clone()) else {
            return;
        };
        if let Err(e) = engine.print() {
            self.fail(format!("ページを印刷できませんでした: {e}"), cx);
        }
    }

    /// ページを PDF にして、ダウンロードの保存先（設定の決まりのフォルダ）に `name` で保存する。
    /// 同じ名前があれば番号を付ける。保存したファイルは、ダウンロードの一覧に出る。
    pub fn save_page_pdf(
        &mut self,
        id: u64,
        name: &str,
        setup: &crate::print_setup::PrintSetup,
        cx: &mut Context<Self>,
    ) {
        let dir = match crate::downloads::download_dir(cx) {
            Ok(dir) => dir,
            Err(e) => {
                self.fail(format!("PDF を保存できませんでした: {e}"), cx);
                return;
            }
        };
        let path = crate::downloads::unique_path(&dir, &crate::downloads::pdf_name(name));
        self.save_page_pdf_at(id, path, setup, cx);
    }

    /// ページを PDF にして `path` に保存する（保存先を聞いて選ばれた場所など）。
    pub fn save_page_pdf_at(
        &mut self,
        id: u64,
        path: PathBuf,
        setup: &crate::print_setup::PrintSetup,
        cx: &mut Context<Self>,
    ) {
        let Some((engine, url)) = self
            .page(id)
            .and_then(|p| Some((p.engine.clone()?, p.url.clone())))
        else {
            return;
        };
        if let Some(dir) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                self.fail(
                    format!("{} に PDF を保存できませんでした: {e}", dir.display()),
                    cx,
                );
                return;
            }
        }
        let downloads = crate::downloads::shared(cx);
        let item = downloads.update(cx, |d, cx| d.pdf_started(url, path.clone(), cx));
        // ページを選んで 1 枚に何ページも並べるときは、並べる前のページの数が要るので、先に 1 回数える。
        // それ以外は 1 回で作り、ページは保存したあとで選ぶ
        let counting = !setup.every_page() && setup.per_sheet() > 1;
        if let Err(e) = self.pdf_pass(&engine, id, item, path, setup, counting, None) {
            downloads.update(cx, |d, cx| d.pdf_done(item, Err(e), cx));
        }
    }

    /// PDF を 1 回作る。終わると [`Event::PdfSaved`] が届く。
    #[allow(clippy::too_many_arguments)]
    fn pdf_pass(
        &self,
        engine: &Rc<dyn Engine>,
        id: u64,
        item: u64,
        path: std::path::PathBuf,
        setup: &crate::print_setup::PrintSetup,
        counting: bool,
        pages: Option<&[u32]>,
    ) -> Result<(), String> {
        let events = self.page_events.clone();
        let (saved, kept) = (path.clone(), Box::new(setup.clone()));
        let done: PdfDone = Box::new(move |result| {
            // アプリを閉じている途中なら、受け取る先がない
            let _ = events.try_send((
                id,
                Event::PdfSaved {
                    item,
                    result,
                    path: saved,
                    setup: kept,
                    counting,
                },
            ));
        });
        let mut pass = setup.clone();
        if counting {
            pass.per_sheet = 1;
        }
        engine.save_pdf(&path, &pass, pages, done)
    }

    /// 印刷のダイアログやダウンロードの一覧を、ページの上に描く。描く前（prepaint）に、描くたびに呼ぶ。
    /// 描いている間は、ページを隠して画像を出す。
    fn cover_page(&mut self, id: u64, window: &Window) {
        let here = window.window_handle().window_id();
        let frame = *self.frames.entry(here).or_default();
        if let Some(page) = self.page_mut(id) {
            page.covered = Some((here, frame));
        }
    }

    /// ページを閉じる（[`Terms::close`] から呼ぶ）。
    pub(crate) fn remove_page(&mut self, id: u64, cx: &mut Context<Self>) -> bool {
        let Some(at) = self.pages.iter().position(|page| page.id == id) else {
            return false;
        };
        // 部品を捨てると、ウィンドウからも消える
        let page = self.pages.remove(at);
        if is_web(&page.url) {
            self.closed_pages.push(page.url);
            let over = self.closed_pages.len().saturating_sub(MAX_CLOSED);
            self.closed_pages.drain(..over);
        }
        self.save_pages(cx);
        true
    }

    /// 開いているページの場所を残す。
    fn save_pages(&mut self, cx: &mut Context<Self>) {
        let urls: Vec<&str> = self
            .pages
            .iter()
            .filter(|p| !p.popup)
            .map(|p| p.url.as_str())
            .filter(|url| *url != BLANK && is_web(url))
            .collect();
        let value = (!urls.is_empty()).then(|| json!(urls).to_string());
        if value == self.saved_pages {
            return;
        }
        let saved = self.db.update(cx, |db, _| {
            db.store.set_setting(TABS_SETTING, value.as_deref())
        });
        match saved {
            Ok(()) => self.saved_pages = value,
            Err(e) => self.fail(format!("ブラウザのタブを保存できませんでした: {e}"), cx),
        }
    }

    /// 前に開いていたページを並べる。ページは見せるときに作る。
    pub(crate) fn restore_pages(&mut self, cx: &mut Context<Self>) {
        let saved: Option<String> = match self.db.read(cx).store.setting(TABS_SETTING) {
            Ok(saved) => saved,
            Err(e) => {
                self.fail(format!("ブラウザのタブを読めませんでした: {e}"), cx);
                return;
            }
        };
        let Some(saved) = saved else {
            return;
        };
        let urls: Vec<String> = match serde_json::from_str(&saved) {
            Ok(urls) => urls,
            Err(e) => {
                self.fail(format!("ブラウザのタブを読めませんでした: {e}"), cx);
                return;
            }
        };
        for url in urls.into_iter().filter(|url| is_web(url)).take(MAX_PAGES) {
            let id = self.next;
            self.next += 1;
            self.pages.push(Page::new(id, url));
        }
        self.saved_pages = Some(saved);
    }

    /// OS の部品からの知らせ。
    pub(crate) fn on_page_event(&mut self, id: u64, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::NewTab(url) => {
                let echo = self
                    .popup_sent
                    .take()
                    .is_some_and(|(sent, at)| sent == url && at.elapsed() < POPUP_ECHO);
                if !echo {
                    self.open_page(&url, cx);
                }
                return;
            }
            Event::Popup(url) => {
                if !is_web(&url) {
                    return;
                }
                self.popup_sent = Some((url.clone(), Instant::now()));
                let from = self.page(id).and_then(|p| p.window);
                // ウィンドウを開くと、その中の画面がこの一覧を読むので、更新を終えてから開く
                cx.defer(move |cx| crate::browser_popup::open(&url, from, cx));
                return;
            }
            Event::External(url) => {
                cx.open_url(&url);
                return;
            }
            Event::Zoom(step) => {
                self.zoom_page(id, step, cx);
                return;
            }
            // ページに入力が移っているとキーはアプリに届かないので、ページから知らせを受ける
            Event::Lock => {
                // 設定を開くときはこの一覧も読むので、更新を終えてから
                cx.defer(crate::lock_now_or_settings);
                return;
            }
            Event::Key(key) => {
                self.page_key(id, &key, cx);
                return;
            }
            Event::Found { turn, found } => {
                if let Some(page) = self.page_mut(id).filter(|p| p.find_turn == turn) {
                    page.find_missing = !found;
                    cx.notify();
                }
                return;
            }
            // ページを閉じたあとに終わったものも、一覧には出す
            Event::Download(note) => {
                crate::downloads::shared(cx).update(cx, |d, cx| d.on_note(note, cx));
                return;
            }
            Event::PdfSaved {
                item,
                result,
                path,
                setup,
                counting,
            } => {
                let downloads = crate::downloads::shared(cx);
                if let Err(e) = result {
                    if counting {
                        crate::print_setup::discard(&path);
                    }
                    downloads.update(cx, |d, cx| d.pdf_done(item, Err(e), cx));
                    return;
                }
                if counting {
                    // 数えたページの数から選んだページを決めて、もう 1 回作る
                    let counted = {
                        let (path, setup) = (path.clone(), setup.clone());
                        cx.background_spawn(async move {
                            let count = crate::print_setup::page_count(&path);
                            crate::print_setup::discard(&path);
                            setup.selected(count?)
                        })
                    };
                    cx.spawn(async move |this, cx| {
                        let pages = counted.await;
                        let again = this.update(cx, |this, _| {
                            let engine = this
                                .page(id)
                                .and_then(|p| p.engine.clone())
                                .ok_or_else(|| "PDF にするページが閉じられました".to_owned())?;
                            this.pdf_pass(&engine, id, item, path, &setup, false, Some(&pages?))
                        });
                        let failed = match again {
                            Ok(Ok(())) => return,
                            Ok(Err(e)) => e,
                            Err(_) => "PDF にするページが閉じられました".to_owned(),
                        };
                        downloads.update(cx, |d, cx| d.pdf_done(item, Err(failed), cx));
                    })
                    .detach();
                    return;
                }
                // ページを選び、フォントを埋め込んでいるかを確かめてから、できたことにする
                let select = !setup.every_page() && setup.per_sheet() == 1;
                let finished = cx.background_spawn(async move {
                    crate::print_setup::finish(&path, &setup, select)
                });
                cx.spawn(async move |_, cx| {
                    let result = finished.await;
                    downloads.update(cx, |d, cx| d.pdf_done(item, result, cx));
                })
                .detach();
                return;
            }
            _ => {}
        }
        let events = self.page_events.clone();
        let Some(page) = self.page_mut(id) else {
            return;
        };
        let failed = match event {
            Event::Pointer(true) => {
                page.forget_shot();
                return;
            }
            // メニューを開くときはマウスがページの外にあるので、出たときに撮っておく
            Event::Pointer(false) => page.shoot(&events).err(),
            Event::Snapshot { gen, image } => {
                if gen != page.shot_gen {
                    // 撮っている間にページが変わった
                    return;
                }
                page.shooting = None;
                match image {
                    Ok(image) => {
                        page.shot = Some(image);
                        None
                    }
                    Err(e) => {
                        page.shot_failed = true;
                        Some(e)
                    }
                }
            }
            event => {
                self.on_page_change(id, event, cx);
                return;
            }
        };
        if let Some(e) = failed {
            self.fail(
                format!("メニューの後ろに出すページの画像を作れませんでした: {e}"),
                cx,
            );
        }
        cx.notify();
    }

    /// ページの場所・題名・読み込みの知らせ。
    fn on_page_change(&mut self, id: u64, event: Event, cx: &mut Context<Self>) {
        let Some(page) = self.page_mut(id) else {
            return;
        };
        match event {
            Event::Load { done, url } => {
                page.forget_shot();
                page.loading = !done;
                if !url.is_empty() {
                    page.url = url;
                }
            }
            Event::Title(title) => {
                let title = title.trim();
                page.title = (!title.is_empty()).then(|| title.to_owned().into());
            }
            Event::Focused => {
                page.focused = true;
                page.forget_shot();
            }
            // 場所は下で OS の部品から読む
            Event::Moved => {}
            Event::NewTab(_)
            | Event::Popup(_)
            | Event::External(_)
            | Event::Zoom(_)
            | Event::Lock
            | Event::Key(_)
            | Event::Found { .. }
            | Event::Download(_)
            | Event::PdfSaved { .. }
            | Event::Pointer(_)
            | Event::Snapshot { .. } => {}
        }
        if let Some(engine) = page.engine.clone() {
            // ページの中だけで場所が変わるもの（GitHub など）は、読み込みの知らせが来ない
            if let Some(url) = engine.url().filter(|url| !url.is_empty()) {
                page.url = url;
            }
            (page.can_back, page.can_forward) = engine.history();
        }
        self.save_pages(cx);
        cx.notify();
    }

    /// 描いたページの場所。描く前（prepaint）に呼ぶ。まだ部品がなければ、描き終わってから作る。
    fn place_page(&mut self, id: u64, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let here = window.window_handle().window_id();
        let frame = *self.frames.entry(here).or_default();
        let Some(page) = self.page_mut(id) else {
            return;
        };
        page.painted = Some((here, frame));
        if page.engine.is_some() && page.window != Some(here) {
            // ほかのウィンドウに置いていたページを、このウィンドウへ移す（作り直す）
            if let Some(engine) = page.engine.take() {
                if page.shown {
                    // 隠せなくても、捨てれば消える
                    let _ = engine.show(false);
                }
            }
            page.shown = false;
            page.window = None;
            page.forget_shot();
        }
        match page.engine.clone() {
            Some(engine) => {
                if page.bounds != Some(bounds) {
                    page.bounds = Some(bounds);
                    page.forget_shot();
                    if let Err(e) = engine.place(bounds) {
                        page.error = Some(format!("ページを置けませんでした: {e}").into());
                    }
                }
            }
            None if !page.creating && page.error.is_none() => {
                page.creating = true;
                page.bounds = Some(bounds);
                let terms = term::shared(cx);
                window.defer(cx, move |window, cx| {
                    terms.update(cx, |terms, cx| terms.create_engine(id, window, cx));
                });
            }
            None => {}
        }
    }

    /// ページの部品を作って、ウィンドウに置く。
    fn create_engine(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let events = self.page_events.clone();
        let Some(page) = self.page_mut(id) else {
            return;
        };
        page.creating = false;
        if page.engine.is_some() {
            return;
        }
        let setup = Setup {
            id,
            url: page.url.clone(),
            bounds: page.bounds.unwrap_or_default(),
            events,
        };
        let made = match cx.try_global::<Factory>() {
            Some(factory) => {
                let make = factory.0.clone();
                make(window, setup)
            }
            None => Err("このアプリではブラウザを使えません".to_owned()),
        };
        let Some(page) = self.page_mut(id) else {
            return;
        };
        match made {
            Ok(engine) => {
                // ほかのウィンドウから移したページは、前の拡大率のまま出す
                if !is_actual_size(page.zoom) {
                    if let Err(e) = engine.zoom(page.zoom) {
                        page.error =
                            Some(format!("ページの拡大率を変更できませんでした: {e}").into());
                    }
                }
                page.engine = Some(engine);
                page.window = Some(window.window_handle().window_id());
                page.shown = false;
                page.loading = page.url != BLANK;
                page.error = None;
                self.pump(cx);
            }
            Err(e) => page.error = Some(e.into()),
        }
        // 見せるかどうかは、次に描くときに決まる
        window.refresh();
        cx.notify();
    }

    /// Linux の GTK の知らせを進め続ける。ページがなくなったら止める。
    fn pump(&mut self, cx: &mut Context<Self>) {
        if self.pumping || !cfg!(target_os = "linux") {
            return;
        }
        self.pumping = true;
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(12))
                .await;
            crate::webview::pump();
            let more = this
                .update(cx, |this, _| {
                    let more = this.pages.iter().any(|p| p.engine.is_some());
                    this.pumping = more;
                    more
                })
                .unwrap_or(false);
            if !more {
                break;
            }
        })
        .detach();
    }

    /// ウィンドウを描き終わる直前（いちばん最後に描く部品）に呼ぶ。
    /// このウィンドウで描かなかったページと、上にメニューやダイアログがあるときは隠す。
    fn end_frame(&mut self, covered: bool, window: &mut Window, cx: &mut App) {
        let here = window.window_handle().window_id();
        let frame = self.frames.entry(here).or_default();
        let now = *frame;
        *frame += 1;
        let covered = covered || menus_open(cx);
        let events = self.page_events.clone();
        let mut waiting = false;
        let mut failed: Vec<String> = Vec::new();
        let focus_page = self.focus_page;
        let mut focused_page = false;
        for page in &mut self.pages {
            if page.window != Some(here) {
                continue;
            }
            let Some(engine) = page.engine.clone() else {
                continue;
            };
            let wanted = page.painted == Some((here, now));
            // 印刷のダイアログは、そのページの上にだけ出る。この回に描いたものだけで隠す
            let covered = covered || page.covered == Some((here, now));
            let mut visible = wanted && !covered;
            if wanted && covered && page.shown && page.shot.is_none() && !page.shot_failed {
                // 隠す前に画像を撮る。撮れるまで（長くても SHOT_WAIT）ページを見せておく
                if let Err(e) = page.shoot(&events) {
                    failed.push(format!(
                        "メニューの後ろに出すページの画像を作れませんでした: {e}"
                    ));
                }
                visible = page
                    .shooting
                    .is_some_and(|started| started.elapsed() < SHOT_WAIT);
            }
            waiting |= wanted && covered;
            if visible && page.shown && focus_page == Some(page.id) {
                // もう見せているページ
                focused_page = true;
                match engine.focus() {
                    Ok(()) => page.focused = true,
                    Err(e) => failed.push(format!("ページにキーの入力先を移せませんでした: {e}")),
                }
            }
            if visible == page.shown {
                continue;
            }
            if !visible && page.focused {
                // 隠したページに、キーの入力先を残さない
                page.focused = false;
                if let Err(e) = engine.focus_parent() {
                    failed.push(format!("キーの入力先を戻せませんでした: {e}"));
                }
            }
            match engine.show(visible) {
                Ok(()) => {
                    page.shown = visible;
                    if visible && std::mem::take(&mut page.shot_stale) {
                        page.shot = None;
                    }
                }
                Err(e) => failed.push(format!("ページを切り替えられませんでした: {e}")),
            }
            if page.shown && focus_page == Some(page.id) {
                // タブを切り替えたら、続けてページの中でキーを使えるようにする
                focused_page = true;
                match engine.focus() {
                    Ok(()) => page.focused = true,
                    Err(e) => failed.push(format!("ページにキーの入力先を移せませんでした: {e}")),
                }
            }
            // 隠している間に動かした場所は、OS によっては見せたときに戻る。見せたら置き直す
            if let (true, Some(bounds)) = (page.shown, page.bounds) {
                if let Err(e) = engine.place(bounds) {
                    failed.push(format!("ページを置けませんでした: {e}"));
                }
            }
        }
        if focused_page {
            self.focus_page = None;
        }
        if let Some(error) = failed.into_iter().next() {
            self.error = Some(error.into());
        }
        // メニューを閉じたら、すぐに見せる。閉じたことはここでは分からないので、少したってから描き直す
        if waiting && !self.rechecking.contains(&here) {
            self.rechecking.push(here);
            let terms = term::shared(cx);
            window
                .spawn(cx, async move |cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(120))
                        .await;
                    // ウィンドウを閉じていれば、描く先がない
                    let _ = cx.update(|window, cx| {
                        terms.update(cx, |terms, _| terms.rechecking.retain(|w| *w != here));
                        window.refresh();
                    });
                })
                .detach();
        }
    }

    /// アプリの画面を押した。ページにあるキーの入力先を、アプリへ戻す。
    fn blur_pages(&mut self, window: WindowId) {
        for page in &mut self.pages {
            if page.window == Some(window) && std::mem::take(&mut page.focused) {
                if let Some(engine) = &page.engine {
                    if let Err(e) = engine.focus_parent() {
                        self.error = Some(format!("キーの入力先を戻せませんでした: {e}").into());
                    }
                }
            }
        }
    }

    /// ログインの情報とキャッシュを消す。開いているページがあれば OS の部品に頼み、
    /// なければ置き場所ごと消す。
    pub fn clear_browser_data(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let engine = self.pages.iter().find_map(|p| p.engine.clone());
        let result = match engine {
            Some(engine) => engine.clear_data(),
            None => remove_profile(),
        };
        self.usage = None;
        cx.notify();
        result.map_err(|e| format!("ブラウザのデータを消せませんでした: {e}"))
    }
}

/// 戻る・進む・読み込み直す・止める。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Back,
    Forward,
    Reload,
    Stop,
}

impl Step {
    fn label(self) -> &'static str {
        match self {
            Step::Back => "戻る操作が",
            Step::Forward => "進む操作が",
            Step::Reload => "読み込み直しが",
            Step::Stop => "読み込みの中止が",
        }
    }
}

// ---- メニュー ----

/// 開いているメニュー（右クリックのメニュー）。ページはメニューより手前に出るので、開いている間は隠す。
#[derive(Default)]
struct OpenMenus(Vec<NotedMenu>);

impl Global for OpenMenus {}

/// 開いたメニュー。右クリックのメニューの部品は、閉じたあとも押した所に残るので、
/// 部品が生きているかではなく、閉じた知らせ（DismissEvent）で閉じたことにする。
struct NotedMenu {
    menu: WeakEntity<PopupMenu>,
    closed: Rc<Cell<bool>>,
    _dismiss: Subscription,
}

impl NotedMenu {
    fn open(&self) -> bool {
        !self.closed.get() && self.menu.upgrade().is_some()
    }
}

/// メニューを開いた。メニューを作るとき（`scroll::Menus` やリンクのメニュー）に、その中で呼ぶ。
pub fn note_menu(cx: &mut Context<PopupMenu>) {
    let closed = Rc::new(Cell::new(false));
    let dismiss = {
        let closed = closed.clone();
        cx.subscribe_self(move |_, _: &DismissEvent, cx| {
            closed.set(true);
            // 閉じたらすぐにページを見せる
            cx.refresh_windows();
        })
    };
    let menu = cx.weak_entity();
    let menus = cx.default_global::<OpenMenus>();
    menus.0.retain(NotedMenu::open);
    menus.0.push(NotedMenu {
        menu,
        closed,
        _dismiss: dismiss,
    });
}

/// メニューやドロップダウンが開いているか。
pub fn menus_open(cx: &App) -> bool {
    cx.try_global::<OpenMenus>()
        .is_some_and(|menus| menus.0.iter().any(NotedMenu::open))
        || gpui_kit::component::GlobalState::is_in_deferred_context(cx)
}

/// ウィンドウの中身のいちばん最後に置く部品。ページを見せるか決め、リンクのメニューを出す。
/// `covered` は、ダイアログなどがページの上に出ているとき。
pub fn frame_end(covered: bool, window: &Window, cx: &App) -> AnyElement {
    let here = window.window_handle().window_id();
    let menu = cx
        .try_global::<LinkMenu>()
        .filter(|menu| menu.window == here)
        .map(|menu| (menu.menu.clone(), menu.at));
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_0()
        .child(
            canvas(
                |_, _, _| {},
                move |_, _, window, cx| {
                    if cx.try_global::<term::SharedTerms>().is_none() {
                        return;
                    }
                    let terms = term::shared(cx);
                    terms.update(cx, |terms, cx| terms.end_frame(covered, window, cx));
                    window.on_mouse_event(move |_: &MouseDownEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture {
                            term::shared(cx).update(cx, |terms, _| terms.blur_pages(here));
                        }
                    });
                },
            )
            .size_0(),
        )
        .children(menu.map(|(menu, at)| {
            deferred(
                anchored()
                    .position(at)
                    .snap_to_window_with_margin(gpui_kit::px(8.))
                    .child(menu),
            )
            .with_priority(gpui_kit::base::POPUP_PRIORITY)
        }))
        .into_any_element()
}

// ---- リンク ----

/// 右クリックで開いた、リンクのメニュー。
struct LinkMenu {
    window: WindowId,
    menu: Entity<PopupMenu>,
    at: Point<Pixels>,
    _dismiss: Subscription,
}

impl Global for LinkMenu {}

/// 文章の中のリンクを押した。左とホイールはアプリ内ブラウザで開き、右はメニューを出す。
/// Web のページでないもの（タスクへのリンクなど）は、これまでどおり OS に渡す。
pub fn on_link(url: &SharedString, event: &ClickEvent, window: &mut Window, cx: &mut App) {
    let (button, position) = match event {
        ClickEvent::Mouse(click) => (click.up.button, click.up.position),
        _ => (MouseButton::Left, window.mouse_position()),
    };
    if !is_web(url) {
        if matches!(button, MouseButton::Left | MouseButton::Middle) {
            cx.open_url(url);
        }
        return;
    }
    match button {
        MouseButton::Right => show_link_menu(url.to_string(), position, window, cx),
        MouseButton::Left | MouseButton::Middle => open(url, window, cx),
        _ => {}
    }
}

fn show_link_menu(url: String, at: Point<Pixels>, window: &mut Window, cx: &mut App) {
    let menu = PopupMenu::build(window, cx, move |menu, _, cx| {
        note_menu(cx);
        let menu = menu.scrollable(true);
        let (inside, outside, copy) = (url.clone(), url.clone(), url.clone());
        menu.item(
            PopupMenuItem::new("アプリ内ブラウザで開く")
                .icon(app_icon("app-window"))
                .on_click(move |_, window, cx| open(&inside, window, cx)),
        )
        .item(
            PopupMenuItem::new("ブラウザで開く")
                .icon(app_icon("external-link"))
                .on_click(move |_, _, cx| cx.open_url(&outside)),
        )
        .item(
            PopupMenuItem::new("リンクをコピー")
                .icon(app_icon("copy"))
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                }),
        )
    });
    let dismiss = cx.subscribe(&menu, |_, _: &DismissEvent, cx| {
        cx.remove_global::<LinkMenu>();
        cx.refresh_windows();
    });
    menu.read(cx).focus_handle(cx).focus(window, cx);
    cx.set_global(LinkMenu {
        window: window.window_handle().window_id(),
        menu,
        at,
        _dismiss: dismiss,
    });
    window.refresh();
}

/// アプリ内ブラウザで開いて、シェルの場所を見える所に出す。
pub fn open(url: &str, window: &mut Window, cx: &mut App) {
    let opened = term::shared(cx).update(cx, |terms, cx| terms.open_page(url, cx));
    if opened.is_some() {
        crate::ai_tools::reveal_shell(window, cx);
    }
}

// ---- ブラウザの画面 ----

/// ブラウザのタブの中身。上に戻る・進む・場所の欄、下にページ。
pub struct BrowserPane {
    terms: Entity<Terms>,
    /// ブラウザの画面のキーの入力先。ページの中で押したキーも、ここで受ける。
    focus: FocusHandle,
    bar: Entity<InputState>,
    /// ページの中を探す欄。
    find: Entity<FindBar>,
    /// 印刷のダイアログ。
    print: Entity<PrintDialog>,
    /// ダウンロードの一覧。ツールバーの右のボタンの下に出す。
    downloads: Entity<crate::downloads_ui::DownloadsBubble>,
    /// 場所の欄に出しているページと場所。
    shown: Option<(u64, String)>,
    /// URL の欄の外から押し始めた。離したときに、書いてある URL をすべて選ぶ（Chrome と同じ）。
    select_bar_on_up: bool,
    _subscriptions: Vec<Subscription>,
}

impl BrowserPane {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let terms = term::shared(cx);
        let bar = cx.new(|cx| InputState::new(window, cx).placeholder("URL を入力、または検索"));
        let find = cx.new(|cx| {
            FindBar::new(
                |cx| term::shared(cx).read(cx).active_page().map(|p| p.id),
                window,
                cx,
            )
        });
        let print = cx.new(|cx| {
            PrintDialog::new(
                |cx| term::shared(cx).read(cx).active_page().map(|p| p.id),
                window,
                cx,
            )
        });
        let downloads = cx.new(|cx| {
            crate::downloads_ui::DownloadsBubble::new(
                |cx| term::shared(cx).read(cx).active_page().map(|p| p.id),
                cx,
            )
        });
        let focus = cx.focus_handle();
        let here = window.window_handle().window_id();
        terms.update(cx, |terms, _| terms.note_browser_focus(here, &focus));
        let subscriptions = vec![
            cx.observe(&terms, |_, _, cx| cx.notify()),
            cx.subscribe_in(&bar, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.go(window, cx);
                }
            }),
        ];
        Self {
            terms,
            focus,
            bar,
            find,
            print,
            downloads,
            shown: None,
            select_bar_on_up: false,
            _subscriptions: subscriptions,
        }
    }

    /// 見ているページ。
    fn page_id(&self, cx: &App) -> Option<u64> {
        self.terms.read(cx).active_page().map(|p| p.id)
    }

    /// URL の欄に移って、書いてある場所をすべて選ぶ（Ctrl+L）。
    fn focus_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.page_id(cx) {
            self.terms
                .update(cx, |terms, cx| terms.release_page(id, cx));
        }
        self.bar.update(cx, |bar, cx| {
            bar.focus(window, cx);
            bar.select_all(window, cx);
        });
        cx.notify();
    }

    fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find.update(cx, |find, cx| find.show(window, cx));
    }

    fn close_find(&mut self, cx: &mut Context<Self>) {
        if !self.find.update(cx, |find, cx| find.hide(cx)) {
            cx.propagate();
        }
    }

    fn find_step(&mut self, backwards: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.find
            .update(cx, |find, cx| find.step(backwards, window, cx));
    }

    /// ブラウザのキー（[`crate::shell_keys`]）を受ける。
    fn with_keys(&self, d: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        use crate::shell_keys::*;
        let page_step =
            |this: &mut Self, step: Step, window: &mut Window, cx: &mut Context<Self>| {
                if let Some(id) = this.page_id(cx) {
                    this.step(id, step, window, cx);
                }
            };
        let zoom = |this: &mut Self, zoom: Zoom, cx: &mut Context<Self>| {
            if let Some(id) = this.page_id(cx) {
                this.terms
                    .update(cx, |terms, cx| terms.zoom_page(id, zoom, cx));
            }
        };
        d.on_action(cx.listener(move |this, _: &BrowserReload, window, cx| {
            page_step(this, Step::Reload, window, cx)
        }))
        .on_action(cx.listener(move |this, _: &BrowserBack, window, cx| {
            page_step(this, Step::Back, window, cx)
        }))
        .on_action(cx.listener(move |this, _: &BrowserForward, window, cx| {
            page_step(this, Step::Forward, window, cx)
        }))
        .on_action(cx.listener(|this, _: &FocusAddress, window, cx| this.focus_address(window, cx)))
        .on_action(cx.listener(|this, _: &NewBrowserTab, _, cx| {
            this.terms.update(cx, |terms, cx| {
                terms.open_page(BLANK, cx);
            })
        }))
        .on_action(cx.listener(|this, _: &ReopenTab, _, cx| {
            this.terms.update(cx, |terms, cx| {
                terms.reopen_page(cx);
            })
        }))
        .on_action(cx.listener(|this, _: &FindInPage, window, cx| this.open_find(window, cx)))
        .on_action(cx.listener(|this, _: &FindNext, window, cx| this.find_step(false, window, cx)))
        .on_action(
            cx.listener(|this, _: &FindPrevious, window, cx| this.find_step(true, window, cx)),
        )
        .on_action(cx.listener(|this, _: &CloseFind, _, cx| this.close_find(cx)))
        .on_action(cx.listener(|this, _: &PrintPage, window, cx| {
            this.print.update(cx, |print, cx| print.show(window, cx))
        }))
        .on_action(
            cx.listener(|this, _: &crate::shell_keys::ShowDownloads, window, cx| {
                this.downloads
                    .update(cx, |downloads, cx| downloads.toggle(window, cx))
            }),
        )
        .on_action(cx.listener(move |this, _: &BrowserZoomIn, _, cx| zoom(this, Zoom::In, cx)))
        .on_action(cx.listener(move |this, _: &BrowserZoomOut, _, cx| zoom(this, Zoom::Out, cx)))
        .on_action(
            cx.listener(move |this, _: &BrowserZoomReset, _, cx| zoom(this, Zoom::Reset, cx)),
        )
    }

    /// URL の欄に書いている途中か。欄にキーの入力先があり、出した URL から書き換えているときだけ。
    /// 押して選んだだけのときや、ページに移ったあとは、ページの場所が変われば欄も変える。
    fn editing(&self, id: u64, window: &Window, cx: &App) -> bool {
        let bar = self.bar.read(cx);
        bar.focus_handle(cx).is_focused(window)
            && self
                .shown
                .as_ref()
                .is_some_and(|(shown, url)| *shown == id && bar.value() != bar_text(url))
    }

    /// URL の欄から離れる。書きかけの URL は捨てて、次に描くときにページの場所を出し直す。
    fn leave_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bar.read(cx).focus_handle(cx).is_focused(window) {
            window.focus(&self.focus, cx);
            self.shown = None;
        }
    }

    /// 場所の欄に書いた所を開く。
    fn go(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.terms.read(cx).active_page().map(|p| p.id) else {
            return;
        };
        let text = self.bar.read(cx).value().to_string();
        self.terms
            .update(cx, |terms, cx| terms.navigate(id, &text, cx));
        // 開いた場所を、欄に出し直す。続けてページの中でキーを使えるように、ページに移る
        self.shown = None;
        window.focus(&self.focus, cx);
        self.terms.update(cx, |terms, cx| terms.focus_page(id, cx));
        cx.notify();
    }

    /// 戻る・進む・読み込み直す・止める。URL の欄に書きかけの URL は捨てて、ページの場所に戻す。
    fn step(&mut self, id: u64, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_bar(window, cx);
        self.terms
            .update(cx, |terms, cx| terms.page_step(id, step, cx));
    }
}

/// URL の欄に出す文字。何もないタブは空にする。
fn bar_text(url: &str) -> String {
    if url == BLANK {
        String::new()
    } else {
        url.to_owned()
    }
}

/// 探すページを返す。
pub(crate) type PageOf = Box<dyn Fn(&App) -> Option<u64>>;

/// ページの中を探す欄（Ctrl+F）。ブラウザのタブと小さなウィンドウで使う。
pub(crate) struct FindBar {
    terms: Entity<Terms>,
    /// 探すページ。タブの画面では、見ているページが変わる。
    page: PageOf,
    input: Entity<InputState>,
    /// 欄を出している。
    open: bool,
    _subscriptions: Vec<Subscription>,
}

impl FindBar {
    pub(crate) fn new(
        page: impl Fn(&App) -> Option<u64> + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let terms = term::shared(cx);
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("ページ内を検索"));
        let subscriptions = vec![
            // 見つからなかったことを出し直す
            cx.observe(&terms, |_, _, cx| cx.notify()),
            cx.subscribe(&input, |this, _, event: &InputEvent, cx| match event {
                // 書き換えたら、ページの頭から探し直す
                InputEvent::Change => this.search(false, false, cx),
                InputEvent::PressEnter { shift, .. } => this.search(*shift, true, cx),
                _ => {}
            }),
        ];
        Self {
            terms,
            page: Box::new(page),
            input,
            open: false,
            _subscriptions: subscriptions,
        }
    }

    /// 欄を出して、そこに移る（Ctrl+F）。前に探した言葉は選んだまま残す。
    pub(crate) fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = (self.page)(cx) else {
            return;
        };
        self.terms
            .update(cx, |terms, cx| terms.release_page(id, cx));
        self.open = true;
        self.input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        cx.notify();
    }

    /// 欄を閉じて、ページに戻る（Esc）。出していなければ false。
    pub(crate) fn hide(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.open {
            return false;
        }
        self.open = false;
        if let Some(id) = (self.page)(cx) {
            self.terms.update(cx, |terms, cx| terms.focus_page(id, cx));
        }
        cx.notify();
        true
    }

    /// 次（前）を探す。欄を出していなければ、出す（F3 / Ctrl+G）。
    pub(crate) fn step(&mut self, backwards: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open || self.input.read(cx).value().is_empty() {
            self.show(window, cx);
            return;
        }
        self.search(backwards, true, cx);
    }

    /// 欄の言葉を、ページの中で探す。
    fn search(&mut self, backwards: bool, again: bool, cx: &mut Context<Self>) {
        let Some(id) = (self.page)(cx) else {
            return;
        };
        let query = self.input.read(cx).value().to_string();
        self.terms.update(cx, |terms, cx| {
            terms.find_in_page(id, &query, backwards, again, cx)
        });
    }
}

impl Render for FindBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().into_any_element();
        }
        let theme = cx.theme().clone();
        let find_missing = (self.page)(cx)
            .and_then(|id| self.terms.read(cx).page(id).map(|p| p.find_missing))
            .unwrap_or(false);
        let query_empty = self.input.read(cx).value().is_empty();
        let tool = |id: &'static str, icon: &str, tip: &'static str| {
            button(id)
                .icon(app_icon(icon))
                .tooltip(tip)
                // ページは下に重なるので、名前はボタンの上に出す
                .tooltip_placement(Placement::Top)
                .ghost()
                .small()
        };
        div()
            .id("browser-find")
            .key_context(crate::shell_keys::FIND)
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .h_9()
            .px_2()
            .gap_0p5()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .id("browser-find-input")
                    .flex_1()
                    .min_w_0()
                    .mx_1()
                    .child(
                        Input::new(&self.input)
                            .small()
                            .prefix(app_icon("search").size_3p5())
                            .aria_label("ページ内を検索"),
                    )
                    .test_support(),
            )
            .when(find_missing && !query_empty, |d| {
                d.child(
                    div()
                        .id("browser-find-missing")
                        .flex_none()
                        .px_1()
                        .text_xs()
                        .text_color(theme.danger)
                        .child("見つかりません")
                        .test_support(),
                )
            })
            .child(feel::gloss(
                tool(
                    "browser-find-previous",
                    "chevron-up",
                    if cfg!(target_os = "macos") {
                        "前を検索（⇧Enter）"
                    } else {
                        "前を検索（Shift+Enter）"
                    },
                )
                .on_click(cx.listener(|this, _, window, cx| this.step(true, window, cx))),
            ))
            .child(feel::gloss(
                tool("browser-find-next", "chevron-down", "次を検索（Enter）")
                    .on_click(cx.listener(|this, _, window, cx| this.step(false, window, cx))),
            ))
            .child(feel::gloss(
                tool("browser-find-close", "x", "閉じる（Esc）").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.hide(cx);
                    },
                )),
            ))
            .test_support()
            .into_any_element()
    }
}

/// 印刷の設定のメニューの項目: 名前、選んでいるか、選んだときに設定をどう変えるか。
type SetupChoice = (
    String,
    bool,
    Box<dyn Fn(&mut crate::print_setup::PrintSetup)>,
);

/// 印刷のダイアログ（Ctrl+P）。Chrome と同じく、送信先に「PDF に保存」と「プリンター」がある。
/// PDF はダウンロードのフォルダに保存し、ダウンロードの一覧に出す。プリンターは OS の印刷のダイアログを出す。
/// ブラウザのタブと小さなウィンドウで使う。出している間は、ページを隠してページの画像を後ろに出す。
pub(crate) struct PrintDialog {
    terms: Entity<Terms>,
    /// 印刷するページ。タブの画面では、見ているページが変わる。
    page: PageOf,
    /// PDF のファイル名（`.pdf` の前まで）。
    name: Entity<InputState>,
    /// 出しているページ。
    open: Option<u64>,
    /// 選んでいる設定。PDF に保存したときに覚える。
    setup: crate::print_setup::PrintSetup,
    /// ページの範囲（カスタム）、倍率（%）、余白（mm、上右下左）の欄。
    ranges: Entity<InputState>,
    scale: Entity<InputState>,
    margins: [Entity<InputState>; 4],
    /// 設定の中のおかしい所。あれば保存できない。
    problem: Option<String>,
    /// 設定を覚えられなかった。
    error: Option<String>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl PrintDialog {
    pub(crate) fn new(
        page: impl Fn(&App) -> Option<u64> + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let terms = term::shared(cx);
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("ファイル名"));
        let ranges = cx.new(|cx| InputState::new(window, cx).placeholder("例: 1-5, 8, 11-13"));
        let scale = cx.new(|cx| InputState::new(window, cx));
        let margins = [(); 4].map(|_| cx.new(|cx| InputState::new(window, cx)));
        let mut subscriptions = vec![
            cx.observe(&terms, |this, terms, cx| {
                // 出しているページを閉じたら、ダイアログも閉じる
                if this
                    .open
                    .is_some_and(|id| terms.read(cx).page(id).is_none())
                {
                    this.open = None;
                }
                cx.notify();
            }),
            cx.subscribe_in(&name, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit(window, cx);
                }
            }),
        ];
        // 設定の欄は、書くたびに読み直す。Enter で保存する
        for input in [&ranges, &scale].into_iter().chain(margins.iter()) {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => this.read_inputs(cx),
                    InputEvent::PressEnter { .. } => this.submit(window, cx),
                    _ => {}
                },
            ));
        }
        Self {
            terms,
            page: Box::new(page),
            name,
            open: None,
            setup: Default::default(),
            ranges,
            scale,
            margins,
            problem: None,
            error: None,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// 設定を欄に書き出す。
    fn write_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let setup = self.setup.clone();
        self.ranges.update(cx, |input, cx| {
            input.set_value(setup.ranges.clone(), window, cx)
        });
        self.scale.update(cx, |input, cx| {
            input.set_value(setup.scale.unwrap_or(100).to_string(), window, cx)
        });
        for (input, side) in self.margins.iter().zip(crate::print_setup::Side::ALL) {
            let mm = setup.custom_margins.get(side);
            input.update(cx, |input, cx| input.set_value(mm_text(mm), window, cx));
        }
        self.read_inputs(cx);
    }

    /// 欄に書いたものを設定に読み込み、おかしい所を探す。
    fn read_inputs(&mut self, cx: &mut Context<Self>) {
        use crate::print_setup::{Margins, Side};
        self.setup.ranges = self.ranges.read(cx).value().trim().to_string();
        let mut problem = None;
        if self.setup.scale.is_some() {
            let text = to_ascii_digits(self.scale.read(cx).value().trim());
            match text.trim_end_matches('%').trim().parse::<u16>() {
                Ok(n) => self.setup.scale = Some(n),
                Err(_) => problem = Some(format!("倍率「{text}」を読めません")),
            }
        }
        if self.setup.margins == Margins::Custom {
            for (input, side) in self.margins.iter().zip(Side::ALL) {
                let text = to_ascii_digits(input.read(cx).value().trim());
                match text.parse::<f32>() {
                    Ok(mm) => self.setup.custom_margins.set(side, mm),
                    Err(_) => {
                        problem.get_or_insert_with(|| {
                            format!("{}の余白「{text}」を読めません", side.label())
                        });
                    }
                }
            }
        }
        self.problem = problem.or_else(|| self.setup.check().err());
        cx.notify();
    }

    /// 設定を 1 つ変える（メニューやチェックから）。カスタムを選んだら、その欄に書けるようにする。
    fn change(
        &mut self,
        apply: impl FnOnce(&mut crate::print_setup::PrintSetup),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::print_setup::{Margins, Pages};
        let before = self.setup.clone();
        apply(&mut self.setup);
        let after = &self.setup;
        let input = if after.pages == Pages::Custom && before.pages != Pages::Custom {
            Some(self.ranges.clone())
        } else if after.margins == Margins::Custom && before.margins != Margins::Custom {
            Some(self.margins[0].clone())
        } else if after.scale.is_some() && before.scale.is_none() {
            Some(self.scale.clone())
        } else {
            None
        };
        if let Some(input) = input {
            input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
        }
        self.read_inputs(cx);
    }

    /// ダイアログを出す（Ctrl+P）。ファイル名はページの題名にする。
    pub(crate) fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = (self.page)(cx) else {
            return;
        };
        let Some(page) = self.terms.read(cx).page(id) else {
            return;
        };
        if page.engine.is_none() || page.error.is_some() {
            return;
        }
        let file = crate::downloads::pdf_name(&page.label());
        let stem = file
            .get(..file.len() - ".pdf".len())
            .unwrap_or(&file)
            .to_owned();
        self.terms
            .update(cx, |terms, cx| terms.release_page(id, cx));
        self.open = Some(id);
        self.error = None;
        self.name
            .update(cx, |name, cx| name.set_value(stem, window, cx));
        // 前に PDF に保存したときの設定で始める
        self.setup = self.terms.read(cx).print_setup.clone();
        self.write_inputs(window, cx);
        self.focus_now(window, cx);
        cx.notify();
    }

    /// 送信先に合わせて、キーの入力先を移す。PDF ならファイル名の欄。
    fn focus_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terms.read(cx).print_pdf {
            self.name.update(cx, |name, cx| {
                name.focus(window, cx);
                name.select_all(window, cx);
            });
        } else {
            window.focus(&self.focus, cx);
        }
    }

    /// 閉じてページに戻る（Esc）。出していなければ false。
    pub(crate) fn hide(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(id) = self.open.take() else {
            return false;
        };
        self.terms.update(cx, |terms, cx| terms.focus_page(id, cx));
        cx.notify();
        true
    }

    fn choose(&mut self, pdf: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.terms.update(cx, |terms, cx| {
            terms.print_pdf = pdf;
            cx.notify();
        });
        self.focus_now(window, cx);
    }

    /// 保存する・印刷する。
    pub(crate) fn submit(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.open else {
            return;
        };
        let pdf = self.terms.read(cx).print_pdf;
        if pdf {
            self.read_inputs(cx);
            if self.problem.is_some() {
                return;
            }
            // 覚えられなければ、保存しないでダイアログに失敗を出す
            let setup = self.setup.clone();
            if let Err(e) = self
                .terms
                .update(cx, |terms, cx| terms.set_print_setup(setup, cx))
            {
                self.error = Some(e);
                cx.notify();
                return;
            }
        }
        let name = self.name.read(cx).value().trim().to_string();
        let setup = self.setup.clone();
        if pdf && crate::downloads::shared(cx).read(cx).rules.ask {
            // 設定の「保存先を毎回確認する」: 保存する場所を聞いてから作る
            let dir = match crate::downloads::download_dir(cx) {
                Ok(dir) if dir.is_dir() => dir,
                Ok(_) => crate::downloads::root_dir(cx),
                Err(e) => {
                    self.error = Some(e);
                    cx.notify();
                    return;
                }
            };
            let chosen = cx.prompt_for_new_path(&dir, Some(&crate::downloads::pdf_name(&name)));
            self.hide(cx);
            let terms = self.terms.clone();
            cx.spawn(async move |_, cx| match chosen.await {
                Ok(Ok(Some(path))) => {
                    terms.update(cx, |terms, cx| terms.save_page_pdf_at(id, path, &setup, cx))
                }
                // キャンセルした
                Ok(Ok(None)) => {}
                Ok(Err(e)) => terms.update(cx, |terms, cx| {
                    terms.fail(format!("PDF の保存先を選べませんでした: {e}"), cx)
                }),
                Err(_) => terms.update(cx, |terms, cx| {
                    terms.fail("PDF の保存先を選ぶ画面が閉じられました".to_owned(), cx)
                }),
            })
            .detach();
            return;
        }
        self.hide(cx);
        self.terms.update(cx, |terms, cx| {
            if pdf {
                terms.save_page_pdf(id, &name, &setup, cx);
            } else {
                terms.print_page(id, cx);
            }
        });
    }

    /// PDF の設定。Chrome の印刷と同じ並び（ページ・レイアウト、詳細設定の中に用紙サイズ・
    /// 1 枚あたりのページ数・余白・倍率・オプション）。
    fn settings(&self, cx: &mut Context<Self>) -> Div {
        use crate::print_setup::{Margins, Pages, Paper, Side, HEADER_FOOTER, N_UP, PER_SHEET};
        let theme = cx.theme().clone();
        let setup = &self.setup;
        let this = cx.entity().downgrade();
        let pick =
            move |id: &'static str, shown: String, options: Vec<SetupChoice>, muted: Hsla| {
                let this = this.clone();
                let options = Rc::new(options);
                button(id)
                    .outline()
                    .small()
                    .w_full()
                    .child(
                        row()
                            .w_full()
                            .min_w_0()
                            .gap_2()
                            .child(div().flex_1().min_w_0().truncate().child(shown))
                            .child(app_icon("chevron-down").small().text_color(muted)),
                    )
                    .pop_menu(move |mut menu, _, _| {
                        for ix in 0..options.len() {
                            let (this, options) = (this.clone(), options.clone());
                            let (label, checked) = (options[ix].0.clone(), options[ix].1);
                            menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                                move |_, window, cx| {
                                    // ダイアログを閉じたあとなら、変える先がない
                                    let _ = this.update(cx, |d, cx| {
                                        d.change(|s| (options[ix].2)(s), window, cx)
                                    });
                                },
                            ));
                        }
                        menu
                    })
            };
        let line = |label: &'static str, control: AnyElement| {
            row()
                .gap_3()
                .child(
                    div()
                        .flex_none()
                        .w(gpui_kit::px(150.))
                        .whitespace_nowrap()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(div().flex_1().min_w_0().child(control))
        };
        let muted = theme.muted_foreground;
        let pages = pick(
            "browser-print-pages",
            setup.pages.label().to_owned(),
            Pages::ALL
                .into_iter()
                .map(|p| {
                    let f: Box<dyn Fn(&mut crate::print_setup::PrintSetup)> =
                        Box::new(move |s| s.pages = p);
                    (p.label().to_owned(), setup.pages == p, f)
                })
                .collect(),
            muted,
        );
        let layout = pick(
            "browser-print-layout",
            if setup.landscape { "横" } else { "縦" }.to_owned(),
            [(false, "縦"), (true, "横")]
                .into_iter()
                .map(|(landscape, label)| {
                    let f: Box<dyn Fn(&mut crate::print_setup::PrintSetup)> =
                        Box::new(move |s| s.landscape = landscape);
                    (label.to_owned(), setup.landscape == landscape, f)
                })
                .collect(),
            muted,
        );
        let mut body = col()
            .gap_2()
            .child(line("ページ", pages.into_any_element()))
            .when(setup.pages == Pages::Custom, |d| {
                d.child(line(
                    "",
                    div()
                        .id("browser-print-ranges")
                        .child(Input::new(&self.ranges).small().aria_label("ページの範囲"))
                        .test_support()
                        .into_any_element(),
                ))
            })
            .child(line("レイアウト", layout.into_any_element()))
            .child(
                row()
                    .id("browser-print-more")
                    .gap_1()
                    .py_1()
                    .cursor_pointer()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .hover(|s| s.text_color(theme.foreground))
                    .child(if setup.more {
                        "詳細設定を閉じる"
                    } else {
                        "詳細設定"
                    })
                    .child(
                        app_icon(if setup.more {
                            "chevron-up"
                        } else {
                            "chevron-down"
                        })
                        .small(),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.change(|s| s.more = !s.more, window, cx)
                    }))
                    .test_support(),
            );
        if setup.more {
            let paper = pick(
                "browser-print-paper",
                setup.paper.label().to_owned(),
                Paper::ALL
                    .into_iter()
                    .map(|p| {
                        let f: Box<dyn Fn(&mut crate::print_setup::PrintSetup)> =
                            Box::new(move |s| s.paper = p);
                        (p.label().to_owned(), setup.paper == p, f)
                    })
                    .collect(),
                muted,
            );
            let per_sheet = pick(
                "browser-print-per-sheet",
                setup.per_sheet().to_string(),
                PER_SHEET
                    .into_iter()
                    .map(|n| {
                        let f: Box<dyn Fn(&mut crate::print_setup::PrintSetup)> =
                            Box::new(move |s| s.per_sheet = n);
                        (n.to_string(), setup.per_sheet() == n, f)
                    })
                    .collect(),
                muted,
            );
            let margins = pick(
                "browser-print-margins",
                setup.margins.label().to_owned(),
                Margins::ALL
                    .into_iter()
                    .map(|m| {
                        let f: Box<dyn Fn(&mut crate::print_setup::PrintSetup)> =
                            Box::new(move |s| s.margins = m);
                        (m.label().to_owned(), setup.margins == m, f)
                    })
                    .collect(),
                muted,
            );
            let scale = pick(
                "browser-print-scale",
                if setup.scale.is_some() {
                    "カスタム"
                } else {
                    "標準"
                }
                .to_owned(),
                [(false, "標準"), (true, "カスタム")]
                    .into_iter()
                    .map(|(custom, label)| {
                        let f: Box<dyn Fn(&mut crate::print_setup::PrintSetup)> =
                            Box::new(move |s| {
                                s.scale = custom.then(|| s.scale.unwrap_or(100));
                            });
                        (label.to_owned(), setup.scale.is_some() == custom, f)
                    })
                    .collect(),
                muted,
            );
            let edges = row()
                .gap_1()
                .children(self.margins.iter().zip(Side::ALL).map(|(input, side)| {
                    col()
                        .id(SharedString::from(format!(
                            "browser-print-margin-{}",
                            side.key()
                        )))
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(side.label()),
                        )
                        .child(
                            Input::new(input)
                                .small()
                                .suffix(div().text_xs().text_color(muted).child("mm"))
                                .aria_label(side.label()),
                        )
                        .test_support()
                }));
            let backgrounds = Checkbox::new("browser-print-backgrounds")
                .label("背景のグラフィック")
                .checked(setup.backgrounds)
                .on_click(cx.listener(|this, on: &bool, window, cx| {
                    let on = *on;
                    this.change(|s| s.backgrounds = on, window, cx)
                }));
            let header = Checkbox::new("browser-print-header-footer")
                .label("ヘッダーとフッター")
                .checked(setup.header_footer)
                .on_click(cx.listener(|this, on: &bool, window, cx| {
                    let on = *on;
                    this.change(|s| s.header_footer = on, window, cx)
                }));
            body = body
                .child(line("用紙サイズ", paper.into_any_element()))
                .when(N_UP, |d| {
                    d.child(line("1 枚あたりのページ数", per_sheet.into_any_element()))
                })
                .child(line("余白", margins.into_any_element()))
                .when(setup.margins == Margins::Custom, |d| {
                    d.child(line("", edges.into_any_element()))
                })
                .child(line("倍率", scale.into_any_element()))
                .when(setup.scale.is_some(), |d| {
                    d.child(line(
                        "",
                        div()
                            .id("browser-print-scale-value")
                            .child(
                                Input::new(&self.scale)
                                    .small()
                                    .suffix(div().text_xs().text_color(muted).child("%"))
                                    .aria_label("倍率"),
                            )
                            .test_support()
                            .into_any_element(),
                    ))
                })
                .child(line(
                    "オプション",
                    col()
                        .gap_1p5()
                        .when(HEADER_FOOTER, |d| d.child(header))
                        .child(backgrounds)
                        .into_any_element(),
                ));
        }
        body
    }
}

impl Render for PrintDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(id) = self.open else {
            return div().into_any_element();
        };
        if (self.page)(cx) != Some(id) {
            // ほかのタブに切り替えた。ダイアログはそのページのものなので閉じる
            self.open = None;
            return div().into_any_element();
        }
        let theme = cx.theme().clone();
        let pdf = self.terms.read(cx).print_pdf;
        let target = |id: &'static str, icon: &str, label: &'static str, on: bool| {
            let b = button(id).icon(app_icon(icon)).label(label).small();
            if on {
                b.primary()
            } else {
                b.ghost()
            }
        };
        let card = col()
            .id("browser-print")
            .key_context(crate::shell_keys::PRINT)
            .track_focus(&self.focus)
            .on_action(
                cx.listener(|this, _: &crate::shell_keys::ClosePrint, _, cx| {
                    this.hide(cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::shell_keys::SubmitPrint, window, cx| {
                    this.submit(window, cx)
                }),
            )
            .occlude()
            .w(gpui_kit::px(440.))
            .max_w(relative(0.94))
            .max_h(relative(0.92))
            .p_4()
            .gap_3()
            .rounded(feel::lg())
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .shadow_lg()
            .child(
                row()
                    .child(div().flex_1().text_lg().child("印刷"))
                    .child(feel::gloss(
                        button("browser-print-close")
                            .icon(app_icon("x"))
                            .tooltip("閉じる（Esc）")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.hide(cx);
                            })),
                    )),
            )
            .child(
                row()
                    .gap_1()
                    .child(feel::gloss(
                        target("browser-print-pdf", "file-down", "PDF に保存", pdf).on_click(
                            cx.listener(|this, _, window, cx| this.choose(true, window, cx)),
                        ),
                    ))
                    .child(feel::gloss(
                        target("browser-print-printer", "printer", "プリンター", !pdf).on_click(
                            cx.listener(|this, _, window, cx| this.choose(false, window, cx)),
                        ),
                    )),
            )
            .when(pdf, |d| {
                d.child(
                    div().id("browser-print-name").child(
                        Input::new(&self.name)
                            .suffix(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(".pdf"),
                            )
                            .aria_label("ファイル名"),
                    ),
                )
            })
            .when(pdf, |d| {
                d.child(
                    div()
                        .id("browser-print-settings")
                        .flex_shrink(1.)
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .child(self.settings(cx)),
                )
            })
            .children(
                pdf.then(|| self.problem.clone().or_else(|| self.error.clone()))
                    .flatten()
                    .map(|text| {
                        div()
                            .id("browser-print-problem")
                            .text_sm()
                            .text_color(theme.danger)
                            .child(text)
                            .test_support()
                    }),
            )
            .child(
                row()
                    .gap_2()
                    .justify_end()
                    .child(feel::gloss(
                        button("browser-print-cancel")
                            .label("キャンセル")
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.hide(cx);
                            })),
                    ))
                    .child(feel::gloss(
                        button("browser-print-submit")
                            .label(if pdf { "保存" } else { "印刷" })
                            .primary()
                            .disabled(pdf && self.problem.is_some())
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    )),
            )
            .test_support()
            // ふわっと出す
            .with_animation(
                "browser-print-in",
                Animation::new(Duration::from_millis(180)).with_easing(ease_out_quint()),
                // 薄い所から出す（出た瞬間から押せる）
                |d, t| d.opacity(0.3 + 0.7 * t).mt(gpui_kit::px(12. * (1. - t))),
            );
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.overlay)
            .occlude()
            // 外を押したら閉じる
            .child(
                div()
                    .id("browser-print-backdrop")
                    .absolute()
                    .inset_0()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.hide(cx);
                    })),
            )
            .child(card)
            // 出している間は、ページを隠して画像を出す
            .child(cover(id))
            .into_any_element()
    }
}

/// 余白の mm を欄に書く形にする（`10`、`4.5`）。
fn mm_text(mm: f32) -> String {
    let text = format!("{mm:.1}");
    text.strip_suffix(".0").unwrap_or(&text).to_owned()
}

/// 全角の数字と小数点を半角にする（欄に全角で書いても読む）。
fn to_ascii_digits(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '０'..='９' => char::from_u32(c as u32 - '０' as u32 + '0' as u32).unwrap_or(c),
            '．' => '.',
            '％' => '%',
            c => c,
        })
        .collect()
}

/// 場所の欄の右に出す、ページの拡大率。押すと 100% に戻す。
pub(crate) struct ZoomBadge {
    zoom: f64,
    turn: u64,
    /// 出てきたばかり。
    entering: bool,
    /// 変えたばかり。
    recent: bool,
    /// 100% に戻して、消えていく途中。
    leaving: bool,
}

impl ZoomBadge {
    /// ページの拡大率の表示。出さないときは `None`。
    pub(crate) fn of(page: &Page) -> Option<Self> {
        page.zoom_badge().then_some(ZoomBadge {
            zoom: page.zoom,
            turn: page.zoom_turn,
            entering: page.zoom_turn == page.zoom_entered,
            recent: page.zoom_recent,
            leaving: page.zoom_leaving,
        })
    }

    pub(crate) fn render(
        self,
        still: bool,
        accent: Hsla,
        reset: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        let icon = if self.zoom < 1.0 {
            "zoom-out"
        } else {
            "zoom-in"
        };
        let button = button("browser-zoom")
            .icon(app_icon(icon))
            .label(zoom_label(self.zoom))
            .tooltip("100% に戻す")
            .tooltip_placement(Placement::Top)
            .ghost()
            .small()
            .on_click(reset);
        let badge = div()
            .flex_none()
            .rounded(feel::md())
            .child(feel::gloss(button));
        if still {
            return badge
                .when(self.recent, |d| d.bg(accent.opacity(0.18)))
                .into_any_element();
        }
        let ZoomBadge {
            turn,
            entering,
            leaving,
            ..
        } = self;
        if leaving {
            return badge
                .with_animation(
                    ("browser-zoom-leave", turn as usize),
                    Animation::new(ZOOM_FADE).with_easing(ease_in_out),
                    |d, t| d.opacity(1. - t),
                )
                .into_any_element();
        }
        // 変えるたびに金色がさっと乗って、ゆっくり引く。初めて出るときは、ふわっと出す
        badge
            .with_animation(
                ("browser-zoom", turn as usize),
                Animation::new(ZOOM_SHOWN).with_easing(ease_out_quint()),
                move |d, t| {
                    let d = d.bg(accent.opacity(0.32 * (1. - t)));
                    if entering {
                        d.opacity((t * 8.).min(1.))
                    } else {
                        d
                    }
                },
            )
            .into_any_element()
    }
}

/// ページを置く場所。描くたびに、OS の部品をここに合わせる。
/// 撮った画像はページの下に置く。メニューやダイアログでページを隠している間だけ見える。
pub(crate) fn page_view(id: u64, shot: Option<Arc<RenderImage>>, background: Hsla) -> AnyElement {
    div()
        .id("browser-page")
        .relative()
        .flex_1()
        .min_h_0()
        .w_full()
        .bg(background)
        .children(shot.map(|shot| {
            img(shot)
                .id("browser-shot")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .object_fit(ObjectFit::Fill)
                .test_support()
        }))
        .child(
            canvas(
                move |bounds, window, cx| {
                    let terms = term::shared(cx);
                    terms.update(cx, |terms, cx| terms.place_page(id, bounds, window, cx));
                },
                |_, _, _, _| {},
            )
            .size_full(),
        )
        .test_support()
        .into_any_element()
}

/// ページの上に出すもの（印刷のダイアログ、ダウンロードの一覧）に添える部品。描いている間だけページを隠す。
/// 閉じ忘れても、描かなくなればページはまた見える。
pub(crate) fn cover(id: u64) -> AnyElement {
    canvas(
        move |_, window, cx| {
            term::shared(cx).update(cx, |terms, _| terms.cover_page(id, window));
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_0()
    .into_any_element()
}

/// ウィンドウを閉じる前に呼ぶ。そこに置いたページの部品を、ウィンドウより先に捨てる。
pub fn window_closing(window: &Window, cx: &mut App) {
    if cx.try_global::<term::SharedTerms>().is_none() {
        return;
    }
    let here = window.window_handle().window_id();
    term::shared(cx).update(cx, |terms, _| terms.drop_engines_in(here));
    crate::webview::settle();
}

/// ページを読み込んでいる間、上の端を流れる線。
pub(crate) fn loading_line(color: Hsla) -> AnyElement {
    div()
        .relative()
        .h(gpui_kit::px(2.))
        .w_full()
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .top_0()
                .h_full()
                .w(relative(0.3))
                .bg(color)
                .with_animation(
                    "browser-loading",
                    Animation::new(Duration::from_millis(1100))
                        .repeat()
                        .with_easing(ease_in_out),
                    |d, t| d.left(relative(-0.3 + 1.3 * t)),
                ),
        )
        .into_any_element()
}

impl Render for BrowserPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let here = window.window_handle().window_id();
        let (page, focus_bar) = {
            let terms = self.terms.read(cx);
            let page = terms.active_page().map(|p| {
                // ほかのウィンドウで見せているページは、ここには置かない
                let elsewhere = p.window.is_some_and(|w| w != here) && p.shown;
                (
                    p.id,
                    p.url.clone(),
                    p.title.clone(),
                    p.loading,
                    (p.can_back, p.can_forward),
                    p.error.clone(),
                    elsewhere,
                    p.shot.clone(),
                    ZoomBadge::of(p),
                    p.focused,
                )
            });
            (page, terms.focus_bar)
        };
        let Some((
            id,
            url,
            title,
            loading,
            (can_back, can_forward),
            error,
            elsewhere,
            shot,
            zoom,
            page_focused,
        )) = page
        else {
            return div().into_any_element();
        };
        // ページの中にキーの入力先があるときも、アプリではブラウザの画面にいることにする。
        // ページの中で押したキー（Ctrl+R など）は、ここで受ける。URL の欄や検索の欄にいたら、
        // そこから離れる（離れないと、欄に書いている途中とみなして URL を出し直さない）
        if page_focused && !elsewhere && !self.focus.is_focused(window) {
            self.leave_bar(window, cx);
            window.focus(&self.focus, cx);
        }
        // 場所の欄は、ページの場所に合わせる。書いている間だけは書き換えない
        if self.shown.as_ref() != Some(&(id, url.clone())) && !self.editing(id, window, cx) {
            let text = bar_text(&url);
            self.bar.update(cx, |bar, cx| {
                // 欄を押して選んだまま場所が変わったら、新しい URL を選び直す（打つとそのまま置き換わる）
                let selected = bar.focus_handle(cx).is_focused(window);
                bar.set_value(text, window, cx);
                if selected {
                    bar.select_all(window, cx);
                }
            });
            self.shown = Some((id, url.clone()));
        }
        if focus_bar {
            self.terms.update(cx, |terms, _| terms.focus_bar = false);
            self.bar.update(cx, |bar, cx| bar.focus(window, cx));
        }
        if self.terms.read(cx).show_downloads && !elsewhere {
            self.terms
                .update(cx, |terms, _| terms.show_downloads = false);
            window.activate_window();
            self.downloads
                .update(cx, |downloads, cx| downloads.show(window, cx));
        }
        let tool = |id: &'static str, icon: &str, tip: &'static str| {
            button(id)
                .icon(app_icon(icon))
                .tooltip(tip)
                // ページは上に重なるので、名前はボタンの上に出す
                .tooltip_placement(Placement::Top)
                .ghost()
                .small()
        };
        let toolbar = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .h_9()
            .px_2()
            .gap_0p5()
            .child(feel::gloss_when(
                can_back,
                tool("browser-back", "arrow-left", "戻る")
                    .disabled(!can_back)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.step(id, Step::Back, window, cx)
                    })),
            ))
            .child(feel::gloss_when(
                can_forward,
                tool("browser-forward", "arrow-right", "進む")
                    .disabled(!can_forward)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.step(id, Step::Forward, window, cx)
                    })),
            ))
            .child(feel::gloss(if loading {
                tool("browser-stop", "x", "読み込みを止める").on_click(
                    cx.listener(move |this, _, window, cx| this.step(id, Step::Stop, window, cx)),
                )
            } else {
                tool("browser-reload", "rotate-cw", "読み込み直す").on_click(
                    cx.listener(move |this, _, window, cx| this.step(id, Step::Reload, window, cx)),
                )
            }))
            .child(
                div()
                    .id("browser-url")
                    .flex_1()
                    .min_w_0()
                    .mx_1()
                    // ほかの場所から URL の欄を押したら、書いてある URL をすべて選ぶ。
                    // 欄の中にいるときに押したとき、ドラッグで範囲を選んだときは、そのまま
                    .capture_any_mouse_down(cx.listener(
                        |this, event: &MouseDownEvent, window, cx| {
                            this.select_bar_on_up = event.button == MouseButton::Left
                                && event.click_count == 1
                                && !this.bar.read(cx).focus_handle(cx).is_focused(window);
                        },
                    ))
                    .capture_any_mouse_up(cx.listener(|this, _: &MouseUpEvent, window, cx| {
                        if std::mem::take(&mut this.select_bar_on_up)
                            && this.bar.read(cx).selected_range().is_empty()
                        {
                            this.bar.update(cx, |bar, cx| bar.select_all(window, cx));
                        }
                    }))
                    .child(Input::new(&self.bar).small().aria_label("URL"))
                    .test_support(),
            )
            .children(zoom.map(|zoom| {
                let terms = self.terms.clone();
                zoom.render(cx.reduce_motion(), theme.primary, move |_, _, cx| {
                    terms.update(cx, |terms, cx| terms.zoom_page(id, Zoom::Reset, cx))
                })
            }))
            .child(feel::gloss_when(
                is_web(&url),
                tool("browser-external", "external-link", "ブラウザで開く")
                    .disabled(!is_web(&url))
                    .on_click({
                        let url = url.clone();
                        move |_, _, cx| cx.open_url(&url)
                    }),
            ))
            // Chrome と同じく、ツールバーのいちばん右にダウンロード
            .children(crate::downloads_ui::toolbar_button(&self.downloads, cx));
        let body = if let Some(error) = error {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap_2()
                .p_4()
                .text_sm()
                .text_color(theme.danger)
                .child(div().id("browser-error").child(error).test_support())
                .child(feel::gloss(
                    button("browser-retry")
                        .icon(app_icon("rotate-cw"))
                        .label("もう一度開く")
                        .ghost()
                        .small()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.step(id, Step::Reload, window, cx)
                        })),
                ))
                .into_any_element()
        } else if elsewhere {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap_2()
                .p_4()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(app_icon("globe").size_8())
                .child(
                    div()
                        .max_w_full()
                        .truncate()
                        .child(title.unwrap_or_else(|| url.clone().into())),
                )
                .child(feel::gloss(
                    button("browser-here")
                        .icon(app_icon("monitor"))
                        .label("このウィンドウに表示")
                        .ghost()
                        .small()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let here = window.window_handle().window_id();
                            this.terms.update(cx, |terms, cx| {
                                if let Some(page) = terms.page_mut(id) {
                                    // 前のウィンドウのページを捨てて、ここで作り直す
                                    page.engine = None;
                                    page.shown = false;
                                    page.window = Some(here);
                                }
                                cx.notify();
                            });
                        })),
                ))
                .into_any_element()
        } else {
            div()
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(page_view(id, shot, theme.background))
                .child(self.print.clone())
                .into_any_element()
        };
        let root = div()
            .id("browser")
            .key_context(crate::shell_keys::BROWSER)
            .track_focus(&self.focus);
        self.with_keys(root, cx)
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .bg(theme.background)
            .child(toolbar)
            .child(self.find.clone())
            .child(
                div()
                    .flex_none()
                    .h(gpui_kit::px(2.))
                    .w_full()
                    .when(loading, |d| d.child(loading_line(theme.primary))),
            )
            .child(body)
            .child(self.downloads.clone())
            .test_support()
            .into_any_element()
    }
}

// ---- AI ----

/// AI のツール `browser` の中身。メインのウィンドウで動かす。
/// `reveal` が `true` になったら、呼んだ側でシェルの場所を見える所に出す。
pub fn run_tool(args: &Value, reveal: &mut bool, cx: &mut App) -> Result<Value, String> {
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .ok_or("action が必要です")?;
    let tab = match args.get("tab") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .ok_or_else(|| format!("tab が正しくありません: {v}"))?,
        ),
    };
    let url_arg = || {
        args.get("url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .ok_or("url が必要です".to_owned())
    };
    let terms = term::shared(cx);
    let target = |cx: &App| -> Result<u64, String> {
        let terms = terms.read(cx);
        terms.target_page(tab).ok_or_else(|| match tab {
            Some(tab) => format!("タブ {tab} はありません"),
            None => "ブラウザのタブを見ていません。tab を渡してください".to_owned(),
        })
    };
    let list = |cx: &App| -> Value {
        let terms = terms.read(cx);
        json!(terms
            .pages
            .iter()
            .map(|p| p.json(terms.active == Some(p.id)))
            .collect::<Vec<_>>())
    };
    match action {
        "list" => Ok(json!({"tabs": list(cx)})),
        "open" => {
            let url = url_arg()?;
            let id = terms
                .update(cx, |terms, cx| terms.open_page(url, cx))
                .ok_or_else(|| {
                    terms
                        .read(cx)
                        .error
                        .as_ref()
                        .map_or("開けませんでした".to_owned(), |e| e.to_string())
                })?;
            *reveal = true;
            Ok(json!({"tab": id, "url": normalize(url)}))
        }
        "show" => {
            let id = target(cx)?;
            // 小さなウィンドウのページは、そのウィンドウを手前に出す
            if let Some(popup) = terms.read(cx).page(id).filter(|p| p.popup) {
                let shown = popup
                    .popup_window
                    .and_then(|w| w.update(cx, |_, window, _| window.activate_window()).ok());
                return match shown {
                    Some(()) => Ok(json!({"tab": id, "window": true})),
                    None => Err(format!("タブ {id} の小さなウィンドウを開いていません")),
                };
            }
            terms.update(cx, |terms, cx| terms.activate(Some(id), cx));
            *reveal = true;
            Ok(json!({"tab": id}))
        }
        "navigate" => {
            let id = target(cx)?;
            let url = url_arg()?;
            terms.update(cx, |terms, cx| terms.navigate(id, url, cx));
            Ok(json!({"tab": id, "url": normalize(url)}))
        }
        "back" | "forward" | "reload" => {
            let id = target(cx)?;
            let step = match action {
                "back" => Step::Back,
                "forward" => Step::Forward,
                _ => Step::Reload,
            };
            if terms.read(cx).page(id).is_some_and(|p| p.engine.is_none()) {
                return Err(format!(
                    "タブ {id} はまだ表示していません。show で表示してから操作してください"
                ));
            }
            terms.update(cx, |terms, cx| {
                terms.error = None;
                terms.page_step(id, step, cx);
            });
            match terms.read(cx).error.clone() {
                Some(error) => Err(error.to_string()),
                None => Ok(json!({"tab": id})),
            }
        }
        "zoom_in" | "zoom_out" | "zoom_reset" => {
            let id = target(cx)?;
            let step = match action {
                "zoom_in" => Zoom::In,
                "zoom_out" => Zoom::Out,
                _ => Zoom::Reset,
            };
            terms.update(cx, |terms, cx| {
                terms.error = None;
                terms.zoom_page(id, step, cx);
            });
            let terms = terms.read(cx);
            match &terms.error {
                Some(error) => Err(error.to_string()),
                None => Ok(json!({
                    "tab": id,
                    "zoom": terms.page(id).map(|p| zoom_label(p.zoom)),
                })),
            }
        }
        "close" => {
            let id = target(cx)?;
            terms.update(cx, |terms, cx| terms.close(id, cx));
            Ok(json!({"closed": id}))
        }
        "open_external" => {
            let url = match url_arg() {
                Ok(url) => url.to_owned(),
                Err(_) => {
                    let id = target(cx)?;
                    terms
                        .read(cx)
                        .page(id)
                        .map(|p| p.url.clone())
                        .unwrap_or_default()
                }
            };
            if !is_web(&url) {
                return Err(format!("Web のページではありません: {url}"));
            }
            cx.open_url(&url);
            Ok(json!({"opened": url}))
        }
        other => Err(format!("action が正しくありません: {other}")),
    }
}

/// AI がページの文字を読む。読み終わると、AI に渡す文字列が届く。
pub fn read_page(args: &Value, cx: &mut App) -> Result<async_channel::Receiver<String>, String> {
    let tab = match args.get("tab") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .ok_or_else(|| format!("tab が正しくありません: {v}"))?,
        ),
    };
    let max = args
        .get("max_chars")
        .and_then(Value::as_u64)
        .map_or(MAX_READ_CHARS, |n| (n as usize).clamp(500, MAX_READ_CHARS));
    let terms = term::shared(cx).read(cx);
    let id = terms.target_page(tab).ok_or_else(|| match tab {
        Some(tab) => format!("タブ {tab} はありません"),
        None => {
            "ブラウザのタブを見ていません。browser の list でタブを調べて、tab を渡してください"
                .to_owned()
        }
    })?;
    let page = terms.page(id).ok_or("タブがありません")?;
    let engine = page.engine.clone().ok_or_else(|| {
        format!("タブ {id} はまだ表示していません。browser の show で表示してから読んでください")
    })?;
    let (tx, rx) = async_channel::bounded(1);
    let loading = page.loading;
    engine.read(Box::new(move |raw: String| {
        // 待っている AI がもういなければ、渡す先がない
        let _ = tx.try_send(page_text(id, &raw, loading, max));
    }))?;
    Ok(rx)
}

/// ページを読んだ結果（JSON の文字列）を、AI に渡す形にする。
pub fn page_text(id: u64, raw: &str, loading: bool, max: usize) -> String {
    // スクリプトの結果は、文字列を JSON にしたもの（さらに JSON の文字列に包まれる）
    let inner: Value = match serde_json::from_str::<Value>(raw) {
        Ok(Value::String(text)) => serde_json::from_str(&text).unwrap_or(Value::Null),
        Ok(value) => value,
        Err(_) => Value::Null,
    };
    let Value::Object(mut page) = inner else {
        return json!({"tab": id, "error": "ページの文字を読めませんでした"}).to_string();
    };
    let text = page
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let total = text.chars().count();
    let clipped: String = text.chars().take(max).collect();
    page.insert("text".into(), json!(clipped));
    page.insert("tab".into(), json!(id));
    page.insert("loading".into(), json!(loading));
    if total > max {
        page.insert("truncated".into(), json!(total));
    }
    Value::Object(page).to_string()
}

/// AI に伝える、ユーザーが見ているページ。見ていなければ `None`。
pub fn ai_place(cx: &App) -> Option<String> {
    let terms = cx.try_global::<term::SharedTerms>()?.0.read(cx);
    let page = terms.active_page()?;
    Some(format!(
        "ユーザーはアプリ内ブラウザでタブ {}「{}」（{}）を見ています。ページの中身が要るときは {READ_TOOL} で読めます。",
        page.id,
        page.title.as_deref().unwrap_or(""),
        page.url
    ))
}

// ---- 置き場所とディスク ----

/// ログインの情報とキャッシュを置く場所（Windows と Linux）。データベースの隣。
pub fn profile_dir() -> PathBuf {
    crate::database_path()
        .parent()
        .map(|dir| dir.join("browser"))
        .unwrap_or_else(|| PathBuf::from("browser"))
}

/// macOS の WKWebView がデータを置く場所。
#[cfg(target_os = "macos")]
fn mac_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let lib = home.join("Library");
    let mut names = vec![crate::APP_ID.to_owned()];
    // 開発中（.app にしていない）は、実行ファイルの名前になる
    if let Some(name) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_name().map(|n| n.to_string_lossy().into_owned()))
    {
        names.push(name);
    }
    names
        .iter()
        .flat_map(|name| {
            [
                lib.join("WebKit").join(name),
                lib.join("HTTPStorages").join(name),
                lib.join("Caches").join(name).join("WebKit"),
            ]
        })
        .collect()
}

/// キャッシュの置き場所。消してもログインは残る。
fn cache_dirs() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        mac_dirs()
            .into_iter()
            .filter(|dir| {
                dir.ends_with("WebKit")
                    && dir
                        .parent()
                        .is_some_and(|p| p.parent().is_some_and(|p| p.ends_with("Caches")))
            })
            .collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let profile = profile_dir();
        let chromium = profile.join("EBWebView");
        let default = chromium.join("Default");
        vec![
            // Windows（WebView2）
            default.join("Cache"),
            default.join("Code Cache"),
            default.join("GPUCache"),
            default.join("Service Worker").join("CacheStorage"),
            default.join("Service Worker").join("ScriptCache"),
            chromium.join("GrShaderCache"),
            chromium.join("ShaderCache"),
            chromium.join("GraphiteDawnCache"),
            // Linux（WebKitGTK）
            profile.join("WebKitCache"),
            profile.join("CacheStorage"),
        ]
    }
}

/// フォルダーの中のファイルの大きさの合計。
pub fn dir_size(dir: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => dir_size(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map_or(0, |m| m.len()),
            _ => 0,
        })
        .sum()
}

/// ブラウザのデータの大きさ（ログインの情報とキャッシュの合計）。
pub fn usage() -> u64 {
    #[cfg(target_os = "macos")]
    {
        mac_dirs().iter().map(|d| dir_size(d)).sum()
    }
    #[cfg(not(target_os = "macos"))]
    {
        dir_size(&profile_dir())
    }
}

/// キャッシュが上限を超えていれば消す。ページを開く前に 1 回だけ呼ぶ。
pub fn trim_caches(limit: u64) -> Result<(), String> {
    let dirs = cache_dirs();
    let total: u64 = dirs.iter().map(|d| dir_size(d)).sum();
    if total <= limit {
        return Ok(());
    }
    let mut failed = Vec::new();
    for dir in dirs.iter().filter(|d| d.exists()) {
        if let Err(e) = std::fs::remove_dir_all(dir) {
            failed.push(format!("{}: {e}", dir.display()));
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "ブラウザのキャッシュを消せませんでした: {}",
            failed.join("、")
        ))
    }
}

/// ブラウザのデータをすべて消す（ページを開いていないとき）。
fn remove_profile() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let dirs = mac_dirs();
    #[cfg(not(target_os = "macos"))]
    let dirs = [profile_dir()];
    let mut failed = Vec::new();
    for dir in dirs.iter().filter(|d| d.exists()) {
        if let Err(e) = std::fs::remove_dir_all(dir) {
            failed.push(format!("{}: {e}", dir.display()));
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("、"))
    }
}

thread_local! {
    /// キャッシュの整理を済ませたか。
    static TRIMMED: RefCell<bool> = const { RefCell::new(false) };
}

/// 最初のページを開く前に、キャッシュを整理する。
pub(crate) fn trim_once() -> Result<(), String> {
    if TRIMMED.with(|t| std::mem::replace(&mut *t.borrow_mut(), true)) {
        return Ok(());
    }
    trim_caches(CACHE_LIMIT)
}

/// ブラウザのデータの大きさを裏で数える。数え終わったら `Terms::usage` に入る。
pub fn count_usage(cx: &mut App) {
    let terms = term::shared(cx);
    cx.spawn(async move |cx| {
        let size = cx.background_executor().spawn(async { usage() }).await;
        terms.update(cx, |terms, cx| {
            terms.usage = Some(size);
            cx.notify();
        });
    })
    .detach();
}

/// 開いているウィンドウの、描いた回。
pub type Frames = HashMap<WindowId, u64>;

#[cfg(test)]
mod tests {
    use super::{host, is_web, normalize, page_text, SEARCH};

    #[test]
    fn what_is_typed_becomes_a_place() {
        assert_eq!(normalize("github.com"), "https://github.com");
        assert_eq!(normalize(" https://a.example/x "), "https://a.example/x");
        assert_eq!(normalize("localhost:3000"), "http://localhost:3000");
        assert_eq!(normalize(""), "about:blank");
        assert_eq!(normalize("rust gpui"), format!("{SEARCH}rust+gpui"));
        assert_eq!(normalize("日本"), format!("{SEARCH}%E6%97%A5%E6%9C%AC"));
    }

    #[test]
    fn the_host_is_shown_without_the_rest() {
        assert_eq!(host("https://github.com/a/b?c"), Some("github.com"));
        assert_eq!(
            host("https://user@example.com:8080/"),
            Some("example.com:8080")
        );
        assert_eq!(host("about:blank"), None);
        assert!(is_web("HTTPS://x.example"));
        assert!(!is_web("malakt-mahoz://task/1"));
    }

    #[test]
    fn the_zoom_moves_one_level_at_a_time() {
        use super::{zoom_label, zoom_step, Zoom};
        assert_eq!(zoom_step(1.0, Zoom::In), 1.1);
        assert_eq!(zoom_step(1.0, Zoom::Out), 0.9);
        assert_eq!(zoom_step(1.25, Zoom::Reset), 1.0);
        assert_eq!(zoom_step(5.0, Zoom::In), 5.0);
        assert_eq!(zoom_step(0.25, Zoom::Out), 0.25);
        // 段の間の値からは、近い次の段へ
        assert_eq!(zoom_step(1.2, Zoom::In), 1.25);
        assert_eq!(zoom_step(1.2, Zoom::Out), 1.1);
        assert_eq!(zoom_label(0.67), "67%");
        assert_eq!(zoom_label(1.1), "110%");
        assert_eq!(Zoom::parse("zoom:in"), Some(Zoom::In));
        assert_eq!(Zoom::parse("zoom:reset"), Some(Zoom::Reset));
        assert_eq!(Zoom::parse("focus"), None);
    }

    #[test]
    fn the_page_text_is_clipped_for_the_ai() {
        let raw = serde_json::to_string(
            &serde_json::json!({"url": "https://a", "title": "A", "selection": "", "text": "あいうえお"})
                .to_string(),
        )
        .unwrap();
        let out: serde_json::Value = serde_json::from_str(&page_text(3, &raw, false, 3)).unwrap();
        assert_eq!(out["text"], "あいう");
        assert_eq!(out["truncated"], 5);
        assert_eq!(out["tab"], 3);
        let out: serde_json::Value = serde_json::from_str(&page_text(3, "null", false, 3)).unwrap();
        assert!(out["error"].is_string());
    }
}
