//! キー操作の一覧（Ctrl+? / ⌘?、Ctrl+/、F1）。どのウィンドウでも、画面の上のダイアログとして出す。
//!
//! 一覧の中身は、それぞれの場所のキーの定義（`shortcuts.rs`、`shell_keys.rs`、`timer.rs` など）から
//! 作る（[`groups`]）。設定の検索も同じものを使う。開いたときにキーの入力先があった場所のキーを先頭に出し、
//! 上の欄で絞り込める。
//!
//! タスクの画面のウィンドウでは、ほかのダイアログと同じ重ね方にするため `MainView` が出す
//! （`PaneOverlay::Help`）。ほかのウィンドウ（AI bot、ターミナル、ブラウザの小窓、タイマー、
//! ポモドーロ）は [`toggle`] と [`overlay`] で出す。

use std::collections::HashMap;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::panes::Keys;

use crate::feel;
use crate::press::button;
use crate::render::{app_icon, col, row};
use crate::shadow::{Elevate, Elevation};

actions!(malakt, [CloseKeys]);

/// 一覧の中の場所の名前。Esc で一覧だけを閉じる。
pub const CONTEXT: &str = "KeysHelp";

/// ダイアログの幅と高さ。中身を絞り込んでも大きさを変えない。
const WIDTH: f32 = 720.;
const HEIGHT: f32 = 560.;
/// これより狭いウィンドウでは、1 列に並べる。
const TWO_COLUMNS: f32 = 640.;

pub fn bind(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", CloseKeys, Some(CONTEXT))]);
}

/// キーの入力先があった場所。その場所のキーを一覧の先頭に出す。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    /// タスクの画面
    Tasks,
    /// どのウィンドウでも使えるキー
    App,
    /// Ctrl+B のあとに押すペインのキー
    Panes,
    AiChat,
    /// AI のタブ（シェルの入力行）
    AiShell,
    Browser,
    Terminal,
    Calendar,
    Timer,
    Pomodoro,
}

/// 一覧の 1 つのまとまり。
pub struct Group {
    pub place: Place,
    pub title: &'static str,
    pub keys: Vec<(SharedString, SharedString)>,
}

/// 一覧に出すキーのすべて。`panes` はペインのキーの割り当て（タスクの画面のウィンドウだけ）。
pub fn groups(panes: Option<Keys>) -> Vec<Group> {
    fn own(keys: Vec<(&'static str, &'static str)>) -> Vec<(SharedString, SharedString)> {
        keys.into_iter()
            .map(|(key, about)| (key.into(), about.into()))
            .collect()
    }
    let group = |place, title, keys| Group { place, title, keys };
    let mut all = vec![
        group(
            Place::Tasks,
            "タスクの画面",
            own(crate::shortcuts::list_help()),
        ),
        group(
            Place::App,
            "どのウィンドウでも",
            own(crate::shortcuts::app_help()),
        ),
    ];
    if let Some(panes) = panes {
        all.push(group(
            Place::Panes,
            "ペイン（Ctrl+B のあとに押すキー）",
            panes
                .help()
                .into_iter()
                .map(|(key, about)| (key.into(), about.into()))
                .collect(),
        ));
    }
    all.extend([
        group(Place::AiChat, "AI bot", own(crate::ai_view::chat_help())),
        group(
            Place::AiShell,
            "AI のタブ",
            own(crate::ai_view::shell_help()),
        ),
        group(
            Place::Browser,
            "ブラウザのタブ",
            own(crate::shell_keys::browser_help()),
        ),
        group(
            Place::Terminal,
            "ターミナルのタブ",
            own(crate::shell_keys::terminal_help()),
        ),
        group(Place::Calendar, "カレンダー", own(crate::date_pick::help())),
        group(Place::Timer, "タイマー", own(crate::timer::help())),
        group(Place::Pomodoro, "ポモドーロ", own(crate::pomodoro::help())),
    ]);
    all
}

/// 設定の検索に使う、一覧のすべての文字。
pub fn search_words() -> String {
    let mut words = String::new();
    for group in groups(Some(Keys::default())) {
        words.push(' ');
        words.push_str(group.title);
        for (key, about) in group.keys {
            words.push(' ');
            words.push_str(&key);
            words.push(' ');
            words.push_str(&about);
        }
    }
    words.to_lowercase()
}

/// キーの入力先の場所。内側の場所から見る。
pub fn place_of(window: &Window) -> Place {
    for context in window.context_stack().iter().rev() {
        let is = |name: &str| context.contains(name);
        if is("DateGrid") {
            return Place::Calendar;
        }
        if is(crate::shell_keys::BROWSER)
            || is(crate::shell_keys::FIND)
            || is(crate::shell_keys::PRINT)
            || is(crate::shell_keys::DOWNLOADS)
        {
            return Place::Browser;
        }
        if is(crate::shell_keys::TERMINAL) {
            return Place::Terminal;
        }
        if is(crate::shell_keys::SHELL) {
            return Place::AiShell;
        }
        if is("AiChat") {
            return Place::AiChat;
        }
        if is("TimerWindow") || is("TimerFace") {
            return Place::Timer;
        }
        if is("Pomodoro") || is("PomodoroTimer") {
            return Place::Pomodoro;
        }
        if is("MainView") {
            return Place::Tasks;
        }
    }
    Place::App
}

/// `place` のまとまりを先頭に、どのウィンドウでも使えるキーをその次に並べる。
pub fn ordered(mut all: Vec<Group>, place: Place) -> Vec<Group> {
    let rank = |group: &Group| {
        if group.place == place {
            0
        } else if group.place == Place::App {
            1
        } else {
            2
        }
    };
    // 同じ順位の中は元の順のまま
    all.sort_by_key(rank);
    all
}

/// 書いた言葉（空白で区切った全部）を含む行だけにする。まとまりの名前に合えば、まとまりごと残す。
/// macOS の記号は、名前でも探せる（⌘ は cmd など）。
pub fn filter(all: Vec<Group>, query: &str) -> Vec<Group> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if terms.is_empty() {
        return all;
    }
    let has_all = |text: &str| terms.iter().all(|term| text.contains(term.as_str()));
    all.into_iter()
        .filter_map(|mut group| {
            let title = group.title.to_lowercase();
            if has_all(&title) {
                return Some(group);
            }
            group.keys.retain(|(key, about)| {
                has_all(&format!(
                    "{} {} {title} {}",
                    key.to_lowercase(),
                    about.to_lowercase(),
                    symbol_names(key)
                ))
            });
            (!group.keys.is_empty()).then_some(group)
        })
        .collect()
}

/// macOS の修飾キーの記号の名前。
fn symbol_names(key: &str) -> String {
    let mut names = String::new();
    for (symbol, name) in [
        ('⌘', "cmd command"),
        ('⌃', "ctrl control"),
        ('⇧', "shift"),
        ('⌥', "alt option"),
        ('⌫', "backspace delete"),
    ] {
        if key.contains(symbol) {
            names.push(' ');
            names.push_str(name);
        }
    }
    names
}

/// キー操作の一覧の中身（検索の欄と、まとまりごとのキー）。ダイアログの枠は出す側が付ける。
pub struct KeysHelp {
    search: Entity<InputState>,
    place: Place,
    panes: Option<Keys>,
    /// 開く前にキーの入力先だった所。閉じたら戻す
    back: Option<FocusHandle>,
    /// 閉じたあと、まだ入力先を戻していない
    returning: bool,
    _subscription: Subscription,
}

impl EventEmitter<DismissEvent> for KeysHelp {}

impl KeysHelp {
    /// 今のキーの入力先の場所を覚えて作り、検索の欄に入力先を移す。
    pub fn new(panes: Option<Keys>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let place = place_of(window);
        let back = window.focused(cx);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("キーを検索"));
        search.update(cx, |search, cx| search.focus(window, cx));
        let subscription = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            search,
            place,
            panes,
            back,
            returning: true,
            _subscription: subscription,
        }
    }

    /// 開いたときの場所。
    pub fn place(&self) -> Place {
        self.place
    }

    /// 閉じたあとに戻す入力先。1 回だけ返す（`Some(None)` は戻す先がない）。
    pub fn take_back(&mut self) -> Option<Option<FocusHandle>> {
        std::mem::take(&mut self.returning).then(|| self.back.clone())
    }

    /// 今出しているまとまり。
    pub fn shown(&self, cx: &App) -> Vec<Group> {
        let query = self.search.read(cx).value().to_string();
        filter(ordered(groups(self.panes), self.place), &query)
    }
}

impl Render for KeysHelp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let shown = self.shown(cx);
        let scale = crate::UI_SCALE;
        let two = window.viewport_size().width >= px(TWO_COLUMNS) * scale;
        let line = |key: SharedString, about: SharedString| {
            row()
                .items_start()
                .gap_3()
                .text_sm()
                .child(
                    div()
                        // 1 列の狭いウィンドウでは、説明の幅を残す
                        .map(|d| {
                            if two {
                                d.w(px(150.) * scale)
                            } else {
                                d.w(relative(0.42))
                            }
                        })
                        .flex_none()
                        .text_color(theme.primary)
                        .child(key),
                )
                .child(div().flex_1().min_w_0().child(about))
        };
        let block = |n: usize, group: Group| {
            let mut block = col().id(("keys-group", n)).gap_1().pb_4().child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(group.title),
            );
            for (key, about) in group.keys {
                block = block.child(line(key, about));
            }
            block.test_support()
        };
        let list = if shown.is_empty() {
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("見つかりません。")
        } else {
            // 2 列のときは、短いほうの列に順に積む（先頭のまとまりは左の上）
            let mut columns = [col().flex_1().min_w_0(), col().flex_1().min_w_0()];
            let mut heights = [0usize; 2];
            for (n, group) in shown.into_iter().enumerate() {
                let at = usize::from(two && heights[1] < heights[0]);
                heights[at] += group.keys.len() + 1;
                let column = std::mem::replace(&mut columns[at], div());
                columns[at] = column.child(block(n, group));
            }
            let [left, right] = columns;
            row()
                .items_start()
                .gap_6()
                .child(left)
                .when(two, |d| d.child(right))
        };
        col()
            .id("keys-help-body")
            .key_context(CONTEXT)
            .on_action(cx.listener(|_, _: &CloseKeys, _, cx| cx.emit(DismissEvent)))
            .flex_1()
            .min_h_0()
            .gap_3()
            .child(
                Input::new(&self.search)
                    .id("keys-search")
                    .aria_label("キーを検索")
                    .prefix(app_icon("search").small()),
            )
            .child(
                div()
                    .id("keys-help")
                    .flex_1()
                    .min_h_0()
                    .child(list)
                    .overflow_y_scrollbar(),
            )
    }
}

/// ダイアログの幅と高さ。
pub fn size() -> (Pixels, Pixels) {
    (px(WIDTH) * crate::UI_SCALE, px(HEIGHT) * crate::UI_SCALE)
}

/// タスクの画面ではないウィンドウで開いている一覧。
#[derive(Default)]
struct Opened(HashMap<WindowId, Entity<KeysHelp>>);

impl Global for Opened {}

/// 一覧を開く。開いていれば閉じる（タスクの画面ではないウィンドウ）。
pub fn toggle(window: &mut Window, cx: &mut App) {
    if is_open(window, cx) {
        close(window, cx);
    } else {
        open(window, cx);
    }
}

/// 一覧を開く（タスクの画面ではないウィンドウ）。開いていれば何もしない。
pub fn open(window: &mut Window, cx: &mut App) {
    if is_open(window, cx) {
        return;
    }
    let id = window.window_handle().window_id();
    let help = cx.new(|cx| KeysHelp::new(None, window, cx));
    window
        .subscribe(&help, cx, |_, _: &DismissEvent, window, cx| {
            close(window, cx)
        })
        .detach();
    cx.default_global::<Opened>().0.insert(id, help);
    window.refresh();
}

/// 一覧を閉じて、開く前の入力先に戻す。
pub fn close(window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let Some(help) = cx.default_global::<Opened>().0.remove(&id) else {
        return;
    };
    if let Some(Some(back)) = help.update(cx, |help, _| help.take_back()) {
        window.focus(&back, cx);
    }
    window.refresh();
}

/// このウィンドウで開いている一覧（タスクの画面ではないウィンドウ）。
pub fn opened(window: &Window, cx: &App) -> Option<Entity<KeysHelp>> {
    let id = window.window_handle().window_id();
    cx.try_global::<Opened>()?.0.get(&id).cloned()
}

/// このウィンドウで一覧を開いているか（タスクの画面ではないウィンドウ）。
pub fn is_open(window: &Window, cx: &App) -> bool {
    opened(window, cx).is_some()
}

/// タスクの画面のウィンドウなら、その画面。
fn main_view(window: &Window, cx: &App) -> Option<Entity<crate::state::MainView>> {
    window
        .root::<gpui_kit::base::Root>()
        .flatten()
        .and_then(|root| root.read(cx).view().clone().downcast().ok())
}

/// ウィンドウのメニューから開く。メニューを閉じて入力先が戻ってから開く。
pub fn open_from_menu(window: &Window, cx: &mut App) {
    let handle = window.window_handle();
    cx.defer(move |cx| {
        // 閉じたばかりのウィンドウなら、開く先がない
        let _ = handle.update(cx, |_, window, cx| match main_view(window, cx) {
            Some(main) => main.update(cx, |main, cx| main.open_keys(window, cx)),
            None => open(window, cx),
        });
    });
}

/// 一覧のダイアログ（タスクの画面ではないウィンドウ）。開いていなければ `None`。
pub fn overlay(window: &Window, cx: &mut App) -> Option<AnyElement> {
    let help = opened(window, cx)?;
    let theme = cx.theme().clone();
    let (width, height) = size();
    let motion = !cx.reduce_motion();
    let panel = col()
        .id("dialog")
        .occlude()
        .w(width)
        .h(height)
        .max_w(relative(0.94))
        .max_h(relative(0.9))
        .p_4()
        .gap_3()
        .map(|d| {
            if crate::theme::floating_now() {
                d.rounded(px(16.))
            } else {
                d.rounded(feel::lg())
            }
        })
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .elevate(Elevation::High, cx)
        .child(
            row()
                .child(div().flex_1().text_lg().child("キー操作"))
                .child(feel::gloss(
                    button("dialog-close")
                        .icon(app_icon("x"))
                        .tooltip("閉じる")
                        .ghost()
                        .small()
                        .on_click(|_, window, cx| close(window, cx)),
                )),
        )
        .child(help)
        .test_support();
    let backdrop = div()
        .id("dialog-backdrop")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme.overlay)
        .occlude()
        .on_click(|_, window, cx| close(window, cx))
        .child(panel);
    Some(if motion {
        backdrop
            .with_animation(
                "keys-help-in",
                Animation::new(std::time::Duration::from_millis(200)).with_easing(ease_out_quint()),
                |d, t| d.opacity(t),
            )
            .into_any_element()
    } else {
        backdrop.into_any_element()
    })
}

#[cfg(test)]
mod tests {
    // `super::*` は gpui の `test` マクロまで持ち込むので、使うものだけを書く
    use super::{filter, groups, ordered, search_words, symbol_names, Group, Keys, Place};

    fn titles(all: &[Group]) -> Vec<&'static str> {
        all.iter().map(|g| g.title).collect()
    }

    /// 今の場所のまとまりが先頭、どのウィンドウでも使えるキーがその次。残りは元の順。
    #[test]
    fn place_comes_first() {
        let all = ordered(groups(None), Place::Browser);
        assert_eq!(
            &titles(&all)[..4],
            &[
                "ブラウザのタブ",
                "どのウィンドウでも",
                "タスクの画面",
                "AI bot"
            ]
        );
        let all = ordered(groups(Some(Keys::default())), Place::Tasks);
        assert_eq!(
            &titles(&all)[..3],
            &[
                "タスクの画面",
                "どのウィンドウでも",
                "ペイン（Ctrl+B のあとに押すキー）"
            ]
        );
        let all = ordered(groups(None), Place::Timer);
        assert_eq!(&titles(&all)[..2], &["タイマー", "どのウィンドウでも"]);
        assert_eq!(all.len(), groups(None).len());
    }

    /// どの場所のまとまりも空ではなく、ペインのキーはタスクの画面のウィンドウにだけ出す。
    #[test]
    fn every_group_has_keys() {
        for group in groups(Some(Keys::Tmux)) {
            assert!(!group.keys.is_empty(), "{}", group.title);
        }
        assert!(groups(None).iter().all(|g| g.place != Place::Panes));
    }

    /// 言葉はすべて含む行だけに絞る。まとまりの名前に合えば、そのまとまりは全部残す。
    #[test]
    fn filter_by_words() {
        let all = groups(None);
        let tabs = filter(groups(None), "新しいタブ");
        assert_eq!(titles(&tabs), ["ブラウザのタブ"]);
        assert_eq!(tabs[0].keys.len(), 1);
        let browser = filter(groups(None), "ブラウザのタブ");
        assert_eq!(titles(&browser), ["ブラウザのタブ"]);
        assert_eq!(
            browser[0].keys.len(),
            all.iter()
                .find(|g| g.place == Place::Browser)
                .unwrap()
                .keys
                .len()
        );
        // 大文字と小文字は区別しない。空白で区切ると、どれも含む行だけ
        let pdf = filter(groups(None), "pdf  印刷");
        assert_eq!(titles(&pdf), ["ブラウザのタブ"]);
        assert!(filter(groups(None), "存在しないキー").is_empty());
        assert_eq!(filter(groups(None), "  ").len(), all.len());
    }

    #[test]
    fn mac_symbols_have_names() {
        assert!(symbol_names("⇧⌘T").contains("cmd"));
        assert!(symbol_names("⇧⌘T").contains("shift"));
        assert!(symbol_names("Ctrl+T").is_empty());
    }

    /// 設定の検索にも、一覧のすべてのキーが入る。
    #[test]
    fn settings_search_has_every_key() {
        let words = search_words();
        for group in groups(Some(Keys::default())) {
            for (key, about) in group.keys {
                assert!(words.contains(&key.to_lowercase()), "{key}");
                assert!(words.contains(&about.to_lowercase()), "{about}");
            }
        }
    }
}
