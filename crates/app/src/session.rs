//! 前に閉じたときの画面。ウィンドウごとに、見ていた場所・絞り込み・開いていた詳細・位置と大きさを
//! 設定に残し、次に起動したときに戻す。

use gpui_kit::{px, App, Bounds, Context, EntityId, Global, Pixels, Window, WindowBounds};
use malakt_core::view::{AssigneeFilter, Priority, Sort, Tab};
use malakt_core::{Role, Status};
use serde::{Deserialize, Serialize};

use crate::db;
use crate::state::{MainView, Nav};

const SETTING: &str = "session.windows";

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
    pub sort: String,
    pub descending: bool,
    pub include_descendants: bool,
    pub due_mine: bool,
    pub panel: Option<String>,
    pub collapsed: Vec<String>,
    pub sidebar_hidden: bool,
    /// 位置と大きさ（x, y, 幅, 高さ）。
    pub bounds: Option<[f32; 4]>,
    pub maximized: bool,
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
    cx.default_global::<Sessions>().quitting = true;
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
    /// 今の画面を、保存する形にする。
    pub fn session_state(&self) -> WindowState {
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
            sort: name(q.sort),
            descending: q.descending,
            include_descendants: self.include_descendants,
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
        }
    }

    /// 保存した画面に戻す。なくなったワークスペースやタスクは、開かずに飛ばす。
    pub fn restore_session(
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
        q.sort = named(&Sort::ALL, &state.sort).unwrap_or_default();
        q.descending = state.descending;
        self.include_descendants = state.include_descendants;
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
        self.go(nav, cx);
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
        let id = cx.entity_id();
        cx.on_release(move |_, cx| {
            let sessions = cx.default_global::<Sessions>();
            // 最後の 1 枚と、終了で閉じるウィンドウは、次の起動で戻すので残す
            if sessions.quitting || sessions.windows.len() <= 1 {
                return;
            }
            sessions.windows.retain(|(window, _)| *window != id);
            save(cx);
        })
        .detach();
        self.remember_session(cx);
    }

    fn remember_session(&mut self, cx: &mut Context<Self>) {
        let state = self.session_state();
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
