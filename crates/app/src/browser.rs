//! アプリ内ブラウザ。ターミナルと同じ、シェルの場所のタブとして開く。
//!
//! ページは OS のブラウザの部品（[`crate::webview`]）で、ウィンドウの上に重なる。
//! 描いたときに場所を合わせ、描かなかったとき（ほかのタブ、閉じたシェル）と、
//! アプリのメニューやダイアログが上に出ているときは隠す。メニューやダイアログの間は、
//! 隠す前に撮ったページの画像を同じ場所に出すので、後ろのページは消えずに見えたままになる。
//!
//! ログインの情報（Cookie）は OS の部品がデータベースの隣の `browser` に保存する
//! （macOS は OS が決めた場所）。キャッシュは上限を超えたら、最初のページを開く前に消す。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, Placement, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::{json, Value};

use crate::feel;
use crate::render::app_icon;
use crate::term::{self, Terms};

/// 一度に開けるページの数。
pub const MAX_PAGES: usize = 12;
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
    /// ページが新しいウィンドウで開こうとした。アプリの新しいタブで開く。
    NewTab(String),
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
}

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
            zoom: 1.0,
            zoom_turn: 0,
            zoom_entered: 0,
            zoom_recent: false,
            zoom_leaving: false,
        }
    }

    /// ページが変わった（マウスが入った、読み込んだ、場所や大きさが変わった）。撮った画像を捨てる。
    fn forget_shot(&mut self) {
        self.shot = None;
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

    fn page_mut(&mut self, id: u64) -> Option<&mut Page> {
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
            if let Some(id) = self.pages.iter().find(|p| p.url == url).map(|p| p.id) {
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

    /// ページを閉じる（[`Terms::close`] から呼ぶ）。
    pub(crate) fn remove_page(&mut self, id: u64, cx: &mut Context<Self>) -> bool {
        let Some(at) = self.pages.iter().position(|page| page.id == id) else {
            return false;
        };
        // 部品を捨てると、ウィンドウからも消える
        self.pages.remove(at);
        self.save_pages(cx);
        true
    }

    /// 開いているページの場所を残す。
    fn save_pages(&mut self, cx: &mut Context<Self>) {
        let urls: Vec<&str> = self
            .pages
            .iter()
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
                self.open_page(&url, cx);
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
            Event::NewTab(_)
            | Event::External(_)
            | Event::Zoom(_)
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
        for page in &mut self.pages {
            if page.window != Some(here) {
                continue;
            }
            let Some(engine) = page.engine.clone() else {
                continue;
            };
            let wanted = page.painted == Some((here, now));
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
                Ok(()) => page.shown = visible,
                Err(e) => failed.push(format!("ページを切り替えられませんでした: {e}")),
            }
            // 隠している間に動かした場所は、OS によっては見せたときに戻る。見せたら置き直す
            if let (true, Some(bounds)) = (page.shown, page.bounds) {
                if let Err(e) = engine.place(bounds) {
                    failed.push(format!("ページを置けませんでした: {e}"));
                }
            }
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
struct OpenMenus(Vec<WeakEntity<PopupMenu>>);

impl Global for OpenMenus {}

/// メニューを開いた。`scroll::Menus` が作るときに呼ぶ。
pub fn note_menu(menu: WeakEntity<PopupMenu>, cx: &mut App) {
    let menus = cx.default_global::<OpenMenus>();
    menus.0.retain(|m| m.upgrade().is_some());
    menus.0.push(menu);
}

/// メニューやドロップダウンが開いているか。
pub fn menus_open(cx: &App) -> bool {
    cx.try_global::<OpenMenus>()
        .is_some_and(|menus| menus.0.iter().any(|m| m.upgrade().is_some()))
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
    let menu = PopupMenu::build(window, cx, move |menu, _, _| {
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
    note_menu(menu.downgrade(), cx);
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
    bar: Entity<InputState>,
    /// 場所の欄に出しているページと場所。
    shown: Option<(u64, String)>,
    _subscriptions: Vec<Subscription>,
}

impl BrowserPane {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let terms = term::shared(cx);
        let bar = cx.new(|cx| InputState::new(window, cx).placeholder("URL を入力、または検索"));
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
            bar,
            shown: None,
            _subscriptions: subscriptions,
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
        // 開いた場所を、欄に出し直す
        self.shown = None;
        window.blur(cx);
        cx.notify();
    }

    fn step(&self, id: u64, step: Step, cx: &mut App) {
        self.terms
            .update(cx, |terms, cx| terms.page_step(id, step, cx));
    }
}

/// 場所の欄の右に出す、ページの拡大率。押すと 100% に戻す。
struct ZoomBadge {
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
    fn render(
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
        let button = Button::new("browser-zoom")
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

/// ページを読み込んでいる間、上の端を流れる線。
fn loading_line(color: Hsla) -> AnyElement {
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
                    p.zoom_badge().then_some(ZoomBadge {
                        zoom: p.zoom,
                        turn: p.zoom_turn,
                        entering: p.zoom_turn == p.zoom_entered,
                        recent: p.zoom_recent,
                        leaving: p.zoom_leaving,
                    }),
                )
            });
            (page, terms.focus_bar)
        };
        let Some((id, url, title, loading, (can_back, can_forward), error, elsewhere, shot, zoom)) =
            page
        else {
            return div().into_any_element();
        };
        // 場所の欄は、書いている間は書き換えない
        let bar_focused = self.bar.read(cx).focus_handle(cx).is_focused(window);
        if self.shown.as_ref() != Some(&(id, url.clone())) && !bar_focused {
            let shown = if url == BLANK {
                String::new()
            } else {
                url.clone()
            };
            self.bar
                .update(cx, |bar, cx| bar.set_value(shown, window, cx));
            self.shown = Some((id, url.clone()));
        }
        if focus_bar {
            self.terms.update(cx, |terms, _| terms.focus_bar = false);
            self.bar.update(cx, |bar, cx| bar.focus(window, cx));
        }
        let pane = cx.entity();
        let tool = |id: &'static str, icon: &str, tip: &'static str| {
            Button::new(id)
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
                    .on_click(cx.listener(move |this, _, _, cx| this.step(id, Step::Back, cx))),
            ))
            .child(feel::gloss_when(
                can_forward,
                tool("browser-forward", "arrow-right", "進む")
                    .disabled(!can_forward)
                    .on_click(cx.listener(move |this, _, _, cx| this.step(id, Step::Forward, cx))),
            ))
            .child(feel::gloss(if loading {
                tool("browser-stop", "x", "読み込みを止める")
                    .on_click(cx.listener(move |this, _, _, cx| this.step(id, Step::Stop, cx)))
            } else {
                tool("browser-reload", "rotate-cw", "読み込み直す")
                    .on_click(cx.listener(move |this, _, _, cx| this.step(id, Step::Reload, cx)))
            }))
            .child(
                div()
                    .id("browser-url")
                    .flex_1()
                    .min_w_0()
                    .mx_1()
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
            ));
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
                    Button::new("browser-retry")
                        .icon(app_icon("rotate-cw"))
                        .label("もう一度開く")
                        .ghost()
                        .small()
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.step(id, Step::Reload, cx)),
                        ),
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
                    Button::new("browser-here")
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
                .id("browser-page")
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .bg(theme.background)
                // 撮った画像はページの下に置く。メニューやダイアログでページを隠している間だけ見える
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
                            let terms = pane.read(cx).terms.clone();
                            terms.update(cx, |terms, cx| terms.place_page(id, bounds, window, cx));
                        },
                        |_, _, _, _| {},
                    )
                    .size_full(),
                )
                .test_support()
                .into_any_element()
        };
        div()
            .id("browser")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .bg(theme.background)
            .child(toolbar)
            .child(
                div()
                    .flex_none()
                    .h(gpui_kit::px(2.))
                    .w_full()
                    .when(loading, |d| d.child(loading_line(theme.primary))),
            )
            .child(body)
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
