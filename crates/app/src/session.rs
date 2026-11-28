//! 前に閉じたときの画面。ウィンドウごとに、見ていた場所・絞り込み・開いていた詳細・位置と大きさを
//! 設定に残し、次に起動したときに戻す。

use gpui_kit::{
    px, AnyWindowHandle, App, Bounds, Context, EntityId, Global, Pixels, Window, WindowBounds,
};
use malakt_core::view::{AssigneeFilter, Priority, Sort, Tab};
use malakt_core::{Role, Status};
use serde::{Deserialize, Serialize};

use crate::db;
use crate::state::{MainView, Nav};

pub(crate) const SETTING: &str = "session.windows";

/// ウィンドウ 1 枚の状態。保存する形なので、足した項目は `default` で読めるようにする。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    pub nav: String,
    pub closed_tab: bool,
    pub q: String,
    pub status: Option<Status>,
    pub assignee: String,
    pub my_role: Option<String>,
    pub priority: Option<String>,
    pub category: Option<String>,
    /// 予想工数と所要時間での絞り込み。
    pub time: Option<String>,
    pub sort: String,
    pub descending: bool,
    /// タスクの一覧で開いているサブワークスペース。
    pub list_open: Vec<String>,
    pub due_mine: bool,
    pub panel: Option<String>,
    pub collapsed: Vec<String>,
    pub sidebar_hidden: bool,
    /// 位置と大きさ（x, y, 幅, 高さ）。
    pub bounds: Option<[f32; 4]>,
    pub maximized: bool,
    /// 常に最前面に表示する（`crate::on_top`）。
    pub on_top: bool,
    /// ペインの配置（`malakt_core::panes::Layout::encode`）。分けていなければ `None`。
    pub layout: Option<String>,
    /// このウィンドウが中身を兼ねているペインの番号。
    pub primary: u32,
    /// 分けて増えたペインの番号と中身。
    pub panes: Vec<(u32, WindowState)>,
}

impl WindowState {
    /// このウィンドウを開く場所。保存した位置がどの画面にも掛からないときは `None`。
    pub fn window_bounds(&self, cx: &App) -> Option<WindowBounds> {
        let [x, y, w, h] = self.bounds?;
        let bounds = Bounds::new(gpui_kit::point(px(x), px(y)), gpui_kit::size(px(w), px(h)));
        let on_screen = |screen: Bounds<Pixels>| {
            let seen = screen.intersect(&bounds);
            seen.size.width >= px(120.) && seen.size.height >= px(80.)
        };
        let displays = cx.displays();
        if w < 200.
            || h < 200.
            || !(displays.is_empty() || displays.iter().any(|d| on_screen(d.bounds())))
        {
            return None;
        }
        Some(if self.maximized {
            WindowBounds::Maximized(bounds)
        } else {
            WindowBounds::Windowed(bounds)
        })
    }
}

/// 開いているメインのウィンドウの状態。開いた順に並ぶ。
#[derive(Default)]
struct Sessions {
    windows: Vec<(EntityId, WindowState)>,
    /// 終了の途中。閉じていくウィンドウを一覧から外さない。
    quitting: bool,
    /// メインのウィンドウ（最初に開いた 1 枚）。閉じるとアプリが終了する。
    main: Option<(EntityId, AnyWindowHandle)>,
    /// メインと一緒に最小化したウィンドウ。メインを元に戻すときに、一緒に戻す。
    tucked: Vec<AnyWindowHandle>,
}

impl Global for Sessions {}

/// 前に閉じたときのウィンドウ。なければ空。
pub fn saved(cx: &App) -> Vec<WindowState> {
    let saved = db::shared(cx).read(cx).store.setting(SETTING);
    match saved {
        Ok(Some(json)) => serde_json::from_str(&json).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// 終了を始める。このあと閉じるウィンドウは、次の起動で戻す。
pub fn quitting(cx: &mut App) {
    crate::db::close_open_mark(cx);
    // 動いているシェルと、その中のプログラムを終わらせる
    if let Some(terms) = cx.try_global::<crate::term::SharedTerms>() {
        terms.0.clone().update(cx, |terms, cx| terms.close_all(cx));
    }
    // Claude Code や Cursor が起こした MCP サーバーを終わらせる
    let _held = crate::mcp::quitting();
    cx.default_global::<Sessions>().quitting = true;
}

/// 最初に開いたウィンドウをメインにする。もう決まっていれば変えずに `false` を返す。
pub(crate) fn set_main(view: EntityId, window: AnyWindowHandle, cx: &mut App) -> bool {
    let sessions = cx.default_global::<Sessions>();
    if sessions.main.is_some() {
        return false;
    }
    sessions.main = Some((view, window));
    true
}

/// メインのウィンドウ。まだ開いていなければ `None`。
pub fn main_window(cx: &App) -> Option<AnyWindowHandle> {
    cx.try_global::<Sessions>()
        .and_then(|sessions| sessions.main)
        .map(|(_, window)| window)
}

/// メインと一緒に最小化するウィンドウを覚える。もう覚えているときは、変えずに空を返す。
pub fn tuck(windows: Vec<AnyWindowHandle>, cx: &mut App) -> Vec<AnyWindowHandle> {
    let sessions = cx.default_global::<Sessions>();
    if !sessions.tucked.is_empty() {
        return Vec::new();
    }
    sessions.tucked = windows;
    sessions.tucked.clone()
}

/// メインと一緒に最小化したウィンドウのうち、まだ開いているものを返して、覚えを消す。
pub fn untuck(cx: &mut App) -> Vec<AnyWindowHandle> {
    let open = cx.windows();
    let tucked = std::mem::take(&mut cx.default_global::<Sessions>().tucked);
    tucked.into_iter().filter(|w| open.contains(w)).collect()
}

/// メインのウィンドウを閉じたか。メインを開いていないうちは `false`。
pub fn main_closed(cx: &mut App) -> bool {
    let main = cx.default_global::<Sessions>().main;
    main.is_some_and(|(_, window)| !cx.windows().contains(&window))
}

/// 終了を始めたあとか。
pub fn is_quitting(cx: &mut App) -> bool {
    cx.default_global::<Sessions>().quitting
}

fn save(cx: &mut App) {
    let states: Vec<&WindowState> = cx
        .default_global::<Sessions>()
        .windows
        .iter()
        .map(|(_, state)| state)
        .collect();
    let json = serde_json::to_string(&states).unwrap_or_default();
    let result = db::shared(cx).update(cx, |db, _| db.store.set_setting(SETTING, Some(&json)));
    if let Err(error) = result {
        eprintln!("画面の状態を保存できませんでした: {error}");
    }
}

fn name<T: std::fmt::Debug>(value: T) -> String {
    format!("{value:?}")
}

fn named<T: std::fmt::Debug + Copy>(all: &[T], saved: &str) -> Option<T> {
    all.iter().copied().find(|v| name(v) == saved)
}

const PRIORITIES: [Priority; 4] = [Priority::None, Priority::High, Priority::Mid, Priority::Low];

impl MainView {
    /// 今の画面を、保存する形にする。分けて増えたペインの中身も入る。
    pub fn session_state(&self, cx: &App) -> WindowState {
        let mut state = self.own_state();
        if self.layout.panes().len() > 1 {
            state.layout = Some(self.layout.encode());
            state.primary = self.primary;
            let mut panes: Vec<_> = self
                .children
                .iter()
                .map(|(id, child)| (*id, child.read(cx).own_state()))
                .collect();
            panes.sort_by_key(|(id, _)| *id);
            state.panes = panes;
        }
        state
    }

    /// 保存した画面に戻す。ペインの配置と、それぞれのペインの中身も戻す。
    pub fn restore_session(
        &mut self,
        state: &WindowState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.restore_own(state, window, cx);
        let layout = state
            .layout
            .as_deref()
            .and_then(malakt_core::panes::Layout::decode);
        let Some(layout) = layout else {
            return;
        };
        // 配置にあるペインが、保存した中身とちょうど合うときだけ戻す
        let mut wanted = layout.panes();
        wanted.sort_unstable();
        let mut saved: Vec<u32> = state.panes.iter().map(|(id, _)| *id).collect();
        saved.push(state.primary);
        saved.sort_unstable();
        if wanted != saved {
            return;
        }
        self.children.clear();
        self.layout = layout;
        self.primary = state.primary;
        for (id, pane) in &state.panes {
            let child = self.new_child_for_session(*id, window, cx);
            child.update(cx, |c, cx| c.restore_own(pane, window, cx));
        }
    }

    /// このペインだけの状態を、保存する形にする。
    pub fn own_state(&self) -> WindowState {
        let q = &self.query;
        WindowState {
            nav: match &self.nav {
                Nav::Home => "home".into(),
                Nav::All => "all".into(),
                Nav::Unfiled => "unfiled".into(),
                Nav::Workspace(id) => format!("ws:{id}"),
            },
            closed_tab: q.tab == Tab::Closed,
            q: q.q.clone(),
            status: q.status,
            assignee: match &q.assignee {
                AssigneeFilter::All => "all".into(),
                AssigneeFilter::Me => "me".into(),
                AssigneeFilter::Unassigned => "none".into(),
                AssigneeFilter::Person(id) => format!("p:{id}"),
            },
            my_role: q.my_role.map(name),
            priority: q.priority.map(name),
            category: q.category.clone(),
            time: q.time.map(name),
            sort: name(q.sort),
            descending: q.descending,
            list_open: {
                let mut ids: Vec<String> = self.list_open.iter().cloned().collect();
                ids.sort();
                ids
            },
            due_mine: self.due_mine,
            panel: self.panel.clone(),
            collapsed: {
                let mut ids: Vec<String> = self.collapsed.iter().cloned().collect();
                ids.sort();
                ids
            },
            sidebar_hidden: self.sidebar_hidden,
            bounds: self.window_place.map(|(bounds, _)| bounds),
            maximized: self.window_place.is_some_and(|(_, maximized)| maximized),
            on_top: self.on_top,
            ..WindowState::default()
        }
    }

    /// このペインを保存した画面に戻す。なくなったワークスペースやタスクは、開かずに飛ばす。
    pub fn restore_own(
        &mut self,
        state: &WindowState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let q = &mut self.query;
        q.tab = if state.closed_tab {
            Tab::Closed
        } else {
            Tab::Open
        };
        q.q = state.q.clone();
        q.status = state.status;
        q.assignee = match state.assignee.as_str() {
            "me" => AssigneeFilter::Me,
            "none" => AssigneeFilter::Unassigned,
            other => match other.strip_prefix("p:") {
                Some(id) => AssigneeFilter::Person(id.to_owned()),
                None => AssigneeFilter::All,
            },
        };
        q.my_role = state.my_role.as_deref().and_then(|s| named(&Role::ALL, s));
        q.priority = state
            .priority
            .as_deref()
            .and_then(|s| named(&PRIORITIES, s));
        q.category = state.category.clone();
        q.time = state
            .time
            .as_deref()
            .and_then(|s| named(&malakt_core::view::TimeFilter::ALL, s));
        q.sort = named(&Sort::ALL, &state.sort).unwrap_or_default();
        q.descending = state.descending;
        self.list_open = state.list_open.iter().cloned().collect();
        self.due_mine = state.due_mine;
        self.sidebar_hidden = state.sidebar_hidden;
        self.collapsed = state.collapsed.iter().cloned().collect();
        self.inputs
            .search
            .update(cx, |s, cx| s.set_value(state.q.clone(), window, cx));
        let nav = match state.nav.as_str() {
            "all" => Nav::All,
            "unfiled" => Nav::Unfiled,
            other => match other.strip_prefix("ws:") {
                Some(id) if self.workspaces.iter().any(|w| w.id == id) => {
                    Nav::Workspace(id.to_owned())
                }
                _ => Nav::Home,
            },
        };
        self.go_here(nav, cx);
        if let Some(id) = &state.panel {
            let exists = self.db.read(cx).store.task(id).is_ok();
            if exists {
                self.open_panel(id, window, cx);
            }
        }
    }

    /// 画面の状態が変わるたびに保存し、位置と大きさも追う。ウィンドウを作るときに 1 度呼ぶ。
    pub fn track_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let place = |window: &Window| {
            let b = window.bounds();
            (
                [
                    f32::from(b.origin.x),
                    f32::from(b.origin.y),
                    f32::from(b.size.width),
                    f32::from(b.size.height),
                ],
                window.is_maximized(),
            )
        };
        self.window_place = Some(place(window));
        // AI の画面の操作は、いちばん最近手前に出したウィンドウに届く
        crate::ai_tools::register(window.window_handle(), cx.weak_entity(), cx);
        cx.observe_window_activation(window, |_, window, cx| {
            if window.is_window_active() {
                crate::ai_tools::register(window.window_handle(), cx.weak_entity(), cx);
            }
        })
        .detach();
        // 開いてすぐ、キーで操作できるようにする
        self.focus.focus(window, cx);
        cx.observe_window_bounds(window, move |this, window, cx| {
            let now = Some(place(window));
            if this.window_place != now {
                this.window_place = now;
                this.remember_session(cx);
            }
        })
        .detach();
        cx.observe_self(|this, cx| this.remember_session(cx))
            .detach();
        // 最前面の表示を切り替えたら、覚えてヘッダーのピンを出し直す
        cx.observe_global_in::<crate::on_top::Pinned>(window, |this, window, cx| {
            let on = crate::on_top::is_on(window, cx);
            if this.on_top != on {
                this.on_top = on;
                this.remember_session(cx);
                cx.notify();
            }
        })
        .detach();
        let id = cx.entity_id();
        cx.on_release(move |_, cx| {
            let sessions = cx.default_global::<Sessions>();
            // 最後の 1 枚と、終了で閉じるウィンドウは、次の起動で戻すので残す。
            // メインを閉じるとアプリが終了するので、メインも残す
            let main = sessions.main.is_some_and(|(view, _)| view == id);
            if sessions.quitting || main || sessions.windows.len() <= 1 {
                return;
            }
            sessions.windows.retain(|(window, _)| *window != id);
            save(cx);
        })
        .detach();
        self.remember_session(cx);
    }

    fn remember_session(&mut self, cx: &mut Context<Self>) {
        let state = self.session_state(cx);
        let id = cx.entity_id();
        let sessions = cx.default_global::<Sessions>();
        match sessions
            .windows
            .iter_mut()
            .find(|(window, _)| *window == id)
        {
            Some((_, saved)) if *saved == state => return,
            Some((_, saved)) => *saved = state,
            None => sessions.windows.push((id, state)),
        }
        save(cx);
    }
}
