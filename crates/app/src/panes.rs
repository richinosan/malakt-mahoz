//! ペイン。タスクの画面を縦横に何度でも分け、Ctrl+B のキーとマウスで動かす。
//!
//! ウィンドウの `MainView` が 1 つ目のペインの中身を兼ね、分けて増えたペインは、
//! それぞれ別の `MainView`（`embedded`）が中身を持つ。どのペインも別々の場所・絞り込み・詳細を持つ。

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::panes::{resize_step, Action, Dir, Keys, Layout, PaneId, Rect, PRESETS};

use crate::db;
use crate::feel::{self, Lit};
use crate::render::app_icon;
use crate::session::WindowState;
use crate::shadow::{Elevate, Elevation};
use crate::state::{MainView, Nav};

const KEYS_SETTING: &str = "panes.keys";
const PRESETS_SETTING: &str = "panes.presets";

/// ペインの上に重ねて出すもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneOverlay {
    /// キーの一覧。
    Help,
    /// 配置のプリセット。
    Presets,
}

/// ペインの境界のドラッグ。境界の道順、左右に分ける境界か、分けている範囲。
#[derive(Debug, Clone, PartialEq)]
pub struct BorderDrag {
    pub path: Vec<bool>,
    pub horizontal: bool,
    pub within: Rect,
}

/// ペインの上のバーの高さと、境界をつかめる幅。
const BAR: f32 = 30.;
const GRIP: f32 = 8.;

impl MainView {
    /// 今のペインが、分けて増えたペインなら、その中身。
    pub fn focused_child(&self) -> Option<Entity<MainView>> {
        self.children.get(&self.layout.focus()).cloned()
    }

    /// 今のペインが見ている場所。左のメニューと上のバーは、これを出す。
    pub fn shown_nav(&self, cx: &App) -> Nav {
        match self.focused_child() {
            Some(child) => child.read(cx).nav.clone(),
            None => self.nav.clone(),
        }
    }

    pub fn load_pane_keys(&mut self, cx: &App) {
        if let Ok(Some(code)) = db::shared(cx).read(cx).store.setting(KEYS_SETTING) {
            self.pane_keys = Keys::from_code(&code);
        }
    }

    pub fn load_pane_presets(&mut self, cx: &App) {
        if let Ok(Some(json)) = db::shared(cx).read(cx).store.setting(PRESETS_SETTING) {
            self.pane_presets = serde_json::from_str(&json).unwrap_or_default();
        }
    }

    fn store_pane_presets(&mut self, presets: Vec<(String, String)>, cx: &mut Context<Self>) {
        let Ok(json) = serde_json::to_string(&presets) else {
            return;
        };
        if self
            .write(cx, |s| s.set_setting(PRESETS_SETTING, Some(&json)))
            .is_some()
        {
            self.pane_presets = presets;
        }
        cx.notify();
    }

    /// 今の配置を「配置 N」の名前で保存する。同じ配置がもうあれば増やさない。
    pub fn save_pane_preset(&mut self, cx: &mut Context<Self>) {
        let text = self.layout.encode();
        if self.pane_presets.iter().any(|(_, saved)| *saved == text) {
            return;
        }
        let name = (1..)
            .map(|n| format!("配置 {n}"))
            .find(|name| self.pane_presets.iter().all(|(used, _)| used != name))
            .unwrap_or_default();
        let mut presets = self.pane_presets.clone();
        presets.push((name, text));
        self.store_pane_presets(presets, cx);
    }

    pub fn delete_pane_preset(&mut self, at: usize, cx: &mut Context<Self>) {
        if at < self.pane_presets.len() {
            let mut presets = self.pane_presets.clone();
            presets.remove(at);
            self.store_pane_presets(presets, cx);
        }
    }

    pub fn set_pane_keys(&mut self, keys: Keys, cx: &mut Context<Self>) {
        if self
            .write(cx, |s| s.set_setting(KEYS_SETTING, Some(keys.code())))
            .is_some()
        {
            self.pane_keys = keys;
        }
        cx.notify();
    }

    fn new_child(
        &mut self,
        id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<MainView> {
        let motion = self.motion;
        let child = cx.new(|cx| {
            let mut view = MainView::new(window, cx);
            view.embedded = true;
            view.motion = motion;
            view
        });
        // 中身が変わったら、上のバーの名前と保存する状態も変わる
        cx.observe(&child, |_, _, cx| cx.notify()).detach();
        self.children.insert(id, child.clone());
        child
    }

    pub(crate) fn new_child_for_session(
        &mut self,
        id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<MainView> {
        self.new_child(id, window, cx)
    }

    /// ペインの状態。保存や複製に使う。
    fn pane_state(&self, id: PaneId, cx: &App) -> WindowState {
        match self.children.get(&id) {
            Some(child) => child.read(cx).own_state(),
            None => self.own_state(),
        }
    }

    /// 今のペインを分け、新しいペインをその向きに置く。新しいペインはホームから始まる。
    pub fn split_pane(&mut self, dir: Dir, window: &mut Window, cx: &mut Context<Self>) -> PaneId {
        let id = self.layout.split(dir);
        let child = self.new_child(id, window, cx);
        child.update(cx, |c, cx| c.reload(cx));
        cx.notify();
        id
    }

    /// 今のペインと同じ中身のペインを、右に足す。
    pub fn duplicate_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> PaneId {
        let state = self.pane_state(self.layout.focus(), cx);
        let id = self.layout.split(Dir::Right);
        let child = self.new_child(id, window, cx);
        child.update(cx, |c, cx| c.restore_own(&state, window, cx));
        cx.notify();
        id
    }

    /// ペインを閉じる。最後の 1 つは閉じない。
    pub fn close_pane(&mut self, id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if self.layout.panes().len() <= 1 || !self.layout.panes().contains(&id) {
            return;
        }
        if id == self.primary {
            // ウィンドウが中身を兼ねているペインを閉じるときは、ほかのペインの中身を引き取る
            let Some(heir) = self.layout.panes().into_iter().find(|p| *p != id) else {
                return;
            };
            let state = self.pane_state(heir, cx);
            self.children.remove(&heir);
            self.layout.close(id);
            self.primary = heir;
            self.restore_own(&state, window, cx);
        } else {
            self.layout.close(id);
            self.children.remove(&id);
        }
        cx.notify();
    }

    /// 今のペインを別のウィンドウに出す。ペインが 2 つ以上あれば、元のペインは閉じる。
    pub fn pop_out_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.layout.focus();
        let mut state = self.pane_state(id, cx);
        state.bounds = None;
        state.layout = None;
        state.panes.clear();
        if self.layout.panes().len() > 1 {
            self.close_pane(id, window, cx);
        }
        cx.defer(move |cx| {
            if let Err(error) = crate::open_window_with(Some(state), cx) {
                eprintln!("ウィンドウを開けませんでした: {error}");
            }
        });
    }

    /// プリセットの配置にする。足りないペインはホームで足し、余ったペインは閉じる。
    pub fn apply_pane_layout(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = Layout::decode(text) else {
            self.error = Some("この配置は読めませんでした。".into());
            cx.notify();
            return;
        };
        // 今あるペインを、左上から順に新しい配置の場所へ入れる
        let old = self.layout.panes();
        let slots = target.panes();
        let states: Vec<WindowState> = old.iter().map(|id| self.pane_state(*id, cx)).collect();
        let focus_at = old
            .iter()
            .position(|id| *id == self.layout.focus())
            .unwrap_or(0);
        self.children.clear();
        self.layout = target;
        self.primary = slots[0];
        for (at, slot) in slots.iter().enumerate() {
            match (at, states.get(at)) {
                (0, Some(state)) => self.restore_own(state, window, cx),
                (_, Some(state)) => {
                    let child = self.new_child(*slot, window, cx);
                    child.update(cx, |c, cx| c.restore_own(state, window, cx));
                }
                (_, None) => {
                    let child = self.new_child(*slot, window, cx);
                    child.update(cx, |c, cx| c.reload(cx));
                }
            }
        }
        self.layout.set_focus(slots[focus_at.min(slots.len() - 1)]);
        self.pane_overlay = None;
        cx.notify();
    }

    pub fn focus_pane(&mut self, id: PaneId, cx: &mut Context<Self>) {
        if self.layout.focus() != id {
            self.layout.set_focus(id);
            cx.notify();
        }
    }

    /// ペインの操作を 1 つ行う。
    pub fn pane_action(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::Split(dir) => {
                self.split_pane(dir, window, cx);
            }
            Action::Focus(dir) => self.layout.focus_dir(dir),
            Action::FocusNext => self.layout.focus_step(1),
            Action::FocusPrev => self.layout.focus_step(-1),
            Action::Swap(dir) => self.layout.swap_dir(dir),
            Action::Zoom => self.layout.toggle_zoom(),
            Action::Close => self.close_pane(self.layout.focus(), window, cx),
            Action::Duplicate => {
                self.duplicate_pane(window, cx);
            }
            Action::ResizeMode => self.pane_resize = self.layout.panes().len() > 1,
            Action::Presets => {
                self.pane_overlay = (self.pane_overlay != Some(PaneOverlay::Presets))
                    .then_some(PaneOverlay::Presets)
            }
            Action::PopOut => self.pop_out_pane(window, cx),
            Action::Help => {
                self.pane_overlay =
                    (self.pane_overlay != Some(PaneOverlay::Help)).then_some(PaneOverlay::Help)
            }
        }
        cx.notify();
    }

    /// Ctrl+B のあと、またはリサイズモードの間のキー。受けたら `true`。
    pub fn pane_key(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let shift = keystroke.modifiers.shift;
        let key = match keystroke.key.as_str() {
            "left" => "←".to_owned(),
            "right" => "→".to_owned(),
            "up" => "↑".to_owned(),
            "down" => "↓".to_owned(),
            "tab" => "Tab".to_owned(),
            // 打った文字（Shift を押した記号や大文字）があれば、それで見る
            other => match keystroke.key_char.as_deref() {
                Some(typed) if !typed.trim().is_empty() => typed.to_owned(),
                // 打った文字が来ないときは、US 配列の Shift の記号で読む
                _ if shift => match other {
                    "/" => "?".to_owned(),
                    "5" => "%".to_owned(),
                    "'" => "\"".to_owned(),
                    "1" => "!".to_owned(),
                    "[" => "{".to_owned(),
                    "]" => "}".to_owned(),
                    _ => other.to_uppercase(),
                },
                _ => other.to_owned(),
            },
        };
        if self.pane_prefix {
            self.pane_prefix = false;
            if let Some(action) = self.pane_keys.action(&key, shift) {
                self.pane_action(action, window, cx);
            }
            cx.notify();
            return true;
        }
        if self.pane_resize {
            if let Some((horizontal, px)) = resize_step(&key, shift) {
                let size = self.pane_area.size;
                let whole = f32::from(if horizontal { size.width } else { size.height });
                if whole > 0. {
                    self.layout.grow(horizontal, px * crate::UI_SCALE / whole);
                }
            } else if matches!(keystroke.key.as_str(), "escape" | "enter") {
                self.pane_resize = false;
            }
            cx.notify();
            return true;
        }
        false
    }

    /// ペインの境界のドラッグ。
    pub fn drag_pane_border(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = &self.border_drag else {
            return;
        };
        let area = self.pane_area;
        if area.size.width <= px(0.) || area.size.height <= px(0.) {
            return;
        }
        let ratio = if drag.horizontal {
            (f32::from(at.x - area.origin.x) / f32::from(area.size.width) - drag.within.x)
                / drag.within.w
        } else {
            (f32::from(at.y - area.origin.y) / f32::from(area.size.height) - drag.within.y)
                / drag.within.h
        };
        let path = drag.path.clone();
        self.layout.set_ratio(&path, ratio);
        cx.notify();
    }

    fn pane_title(&self, id: PaneId, cx: &App) -> SharedString {
        let title = |view: &MainView| -> SharedString {
            match &view.nav {
                Nav::Home => "ホーム".into(),
                Nav::All => "すべてのタスク".into(),
                Nav::Unfiled => "未所属のタスク".into(),
                Nav::Workspace(_) => view.list_title(),
            }
        };
        match self.children.get(&id) {
            Some(child) => title(child.read(cx)),
            None => title(self),
        }
    }

    /// タスクの画面を、ペインの配置どおりに並べる。`own` はこのウィンドウが兼ねているペインの中身。
    pub(crate) fn render_panes(
        &mut self,
        own: AnyElement,
        area: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.pane_area = area;
        let theme = cx.theme().clone();
        let visible = self.layout.visible();
        if self.layout.panes().len() == 1 {
            return div().absolute().inset_0().child(own).into_any_element();
        }
        let focus = self.layout.focus();
        let zoomed = self.layout.zoomed();
        let mut own = Some(own);
        let mut root = div().id("panes").absolute().inset_0().bg(theme.border);
        for (id, rect) in visible {
            let bounds = Bounds::new(
                point(
                    area.origin.x + area.size.width * rect.x,
                    area.origin.y + area.size.height * rect.y + px(BAR) * crate::UI_SCALE,
                ),
                size(
                    area.size.width * rect.w,
                    (area.size.height * rect.h - px(BAR) * crate::UI_SCALE).max(px(0.)),
                ),
            );
            let content = match self.children.get(&id) {
                Some(child) => {
                    child.update(cx, |c, _| c.pane_bounds = Some(bounds));
                    child.clone().into_any_element()
                }
                None => own.take().unwrap_or_else(|| div().into_any_element()),
            };
            let on = id == focus;
            let button = |name: &'static str, icon: &'static str, label: &'static str| {
                Button::new((name, id as usize))
                    .icon(app_icon(icon))
                    .ghost()
                    .xsmall()
                    .tooltip(label)
            };
            let bar = div()
                .id(("pane-bar", id as usize))
                .flex()
                .flex_row()
                .items_center()
                .flex_none()
                .h(px(BAR) * crate::UI_SCALE)
                .px_2()
                .gap_1()
                .text_xs()
                .border_b_1()
                .border_color(theme.border)
                .when(on, |d| d.bg(theme.primary.opacity(0.18)))
                .when(!on, |d| {
                    d.bg(theme.muted.opacity(0.5))
                        .text_color(theme.muted_foreground)
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(self.pane_title(id, cx)),
                )
                .child(feel::gloss(
                    button("pane-split-right", "columns-2", "右に分割").on_click(cx.listener(
                        move |this, _, window, cx| {
                            this.focus_pane(id, cx);
                            this.pane_action(Action::Split(Dir::Right), window, cx)
                        },
                    )),
                ))
                .child(feel::gloss(
                    button("pane-split-down", "rows-2", "下に分割").on_click(cx.listener(
                        move |this, _, window, cx| {
                            this.focus_pane(id, cx);
                            this.pane_action(Action::Split(Dir::Down), window, cx)
                        },
                    )),
                ))
                .child(feel::gloss(
                    button(
                        "pane-zoom",
                        if zoomed { "minimize-2" } else { "maximize-2" },
                        if zoomed {
                            "元の大きさに戻す"
                        } else {
                            "拡大表示"
                        },
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.focus_pane(id, cx);
                        this.pane_action(Action::Zoom, window, cx)
                    })),
                ))
                .child(feel::gloss(
                    button("pane-close", "x", "ペインを閉じる").on_click(
                        cx.listener(move |this, _, window, cx| this.close_pane(id, window, cx)),
                    ),
                ))
                .test_support();
            root = root.child(
                div()
                    .id(("pane", id as usize))
                    .absolute()
                    .left(relative(rect.x))
                    .top(relative(rect.y))
                    .w(relative(rect.w))
                    .h(relative(rect.h))
                    .p(px(1.))
                    // どこを押しても、そのペインが今のペインになる
                    .capture_any_mouse_down(cx.listener(
                        move |this, event: &MouseDownEvent, _, cx| {
                            // 境界をつかんだときは、今のペインを変えない
                            let grip = px(GRIP) / 2.;
                            let inner = Bounds::new(
                                point(
                                    area.origin.x + area.size.width * rect.x + grip,
                                    area.origin.y + area.size.height * rect.y + grip,
                                ),
                                size(
                                    area.size.width * rect.w - grip * 2.,
                                    area.size.height * rect.h - grip * 2.,
                                ),
                            );
                            if inner.contains(&event.position) {
                                this.focus_pane(id, cx);
                            }
                        },
                    ))
                    .child(
                        div()
                            .size_full()
                            .flex()
                            .flex_col()
                            .bg(theme.background)
                            .overflow_hidden()
                            .border_1()
                            .border_color(if on { theme.primary } else { theme.border })
                            .child(bar)
                            .child(div().flex_1().min_h_0().relative().child(content)),
                    )
                    .test_support(),
            );
        }
        // 境界はドラッグで動かせる
        for (n, (path, horizontal, within, ratio)) in self.layout.borders().into_iter().enumerate()
        {
            let grip = px(GRIP);
            let handle = div()
                .id(("pane-border", n))
                .absolute()
                .when(horizontal, |d| {
                    d.left(relative(within.x + within.w * ratio))
                        .ml(-grip / 2.)
                        .w(grip)
                        .top(relative(within.y))
                        .h(relative(within.h))
                        .cursor_col_resize()
                })
                .when(!horizontal, |d| {
                    d.top(relative(within.y + within.h * ratio))
                        .mt(-grip / 2.)
                        .h(grip)
                        .left(relative(within.x))
                        .w(relative(within.w))
                        .cursor_row_resize()
                })
                .lit(theme.primary.opacity(0.5), cx)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.border_drag = Some(BorderDrag {
                            path: path.clone(),
                            horizontal,
                            within,
                        });
                        cx.notify();
                    }),
                )
                .test_support();
            root = root.child(handle);
        }
        root.into_any_element()
    }

    /// Ctrl+B を押したあとの印、リサイズモードの印、キーの一覧、プリセット。
    pub(crate) fn render_pane_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        if let Some(overlay) = self.pane_overlay {
            let mut body = div().flex().flex_col().gap_1();
            match overlay {
                PaneOverlay::Help => {
                    let line = |key: SharedString, about: SharedString| {
                        div()
                            .flex()
                            .flex_row()
                            .gap_3()
                            .text_sm()
                            .child(
                                div()
                                    .w(px(112.) * crate::UI_SCALE)
                                    .flex_none()
                                    .text_color(theme.primary)
                                    .child(key),
                            )
                            .child(about)
                    };
                    let column = || {
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(280.) * crate::UI_SCALE)
                            .gap_1()
                    };
                    let mut app = column().id("app-keys");
                    for (key, about) in crate::shortcuts::help() {
                        app = app.child(line(key.into(), about.into()));
                    }
                    let mut keys = div().flex().flex_row().gap_1();
                    for scheme in Keys::ALL {
                        let on = self.pane_keys == scheme;
                        keys = keys.child(feel::gloss(
                            Button::new(("pane-keys", scheme as usize))
                                .label(scheme.label())
                                .xsmall()
                                .when(on, |b| b.primary())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_pane_keys(scheme, cx)
                                })),
                        ));
                    }
                    let mut panes = column().child(keys).child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("ペイン: Ctrl+B のあとに押すキー"),
                    );
                    for (key, about) in self.pane_keys.help() {
                        panes = panes.child(line(key.into(), about.into()));
                    }
                    // 狭いウィンドウでは縦に並び、入りきらない分はスクロールする
                    body = body.min_h_0().child(
                        div()
                            .id("keys-help")
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap_x_6()
                            .gap_y_3()
                            .min_h_0()
                            .overflow_y_scroll()
                            .child(app.test_support())
                            .child(panes),
                    );
                }
                PaneOverlay::Presets => {
                    for (n, (name, text)) in PRESETS.into_iter().enumerate() {
                        body = body.child(feel::gloss(
                            Button::new(("pane-preset", n))
                                .label(name)
                                .ghost()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.apply_pane_layout(text, window, cx)
                                })),
                        ));
                    }
                    for (n, (name, text)) in self.pane_presets.iter().cloned().enumerate() {
                        body = body.child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1()
                                .child(
                                    div().flex_1().flex().flex_col().child(feel::gloss(
                                        Button::new(("pane-preset-own", n))
                                            .label(name)
                                            .ghost()
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.apply_pane_layout(&text, window, cx)
                                            })),
                                    )),
                                )
                                .child(feel::gloss(
                                    Button::new(("pane-preset-delete", n))
                                        .icon(app_icon("trash"))
                                        .ghost()
                                        .xsmall()
                                        .tooltip("この配置を削除")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.delete_pane_preset(n, cx)
                                        })),
                                )),
                        );
                    }
                    body = body.child(feel::gloss(
                        Button::new("pane-preset-save")
                            .icon(app_icon("plus"))
                            .label("今の配置を保存")
                            .on_click(cx.listener(|this, _, _, cx| this.save_pane_preset(cx))),
                    ));
                }
            }
            let (title, width) = match overlay {
                PaneOverlay::Help => ("キー操作", 720.),
                PaneOverlay::Presets => ("配置のプリセット", 460.),
            };
            return Some(
                self.dialog_as("dialog", px(width) * crate::UI_SCALE, title, body, cx)
                    .into_any_element(),
            );
        }
        let label = if self.pane_prefix {
            "Ctrl+B"
        } else if self.pane_resize {
            "リサイズ中"
        } else {
            return None;
        };
        Some(
            div()
                .id("pane-mode")
                .absolute()
                .top_2()
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(theme.primary)
                        .text_color(theme.primary_foreground)
                        .text_sm()
                        .elevate(Elevation::High, cx)
                        .child(label),
                )
                .test_support()
                .into_any_element(),
        )
    }
}
