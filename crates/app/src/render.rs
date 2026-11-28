//! 画面の描画。状態は `state` にあり、ここは見た目だけを組み立てる。

use std::ops::Range;
use std::sync::Arc;

use crate::fonts::MonoFont;
use crate::press::{button, Unpressed};
use gpui_kit::base::{Disableable, TestSupportExt};
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::color_picker::ColorPicker;
use gpui_kit::component::input::{Input, Textarea, TextareaState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::slider::{Slider, SliderState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::bulk::{BulkOp, ListMode};
use malakt_core::view::{self, AssigneeFilter, DueState, ListRow, Priority as Prio, Sort, Tab};
use malakt_core::{Comment, DueRule, Role as Raci, Status, Task as Todo, WORKSPACE_COLORS};

use crate::ai::ShellPlace;
use crate::ai_view::{docked_width, TASKS_MIN};
use crate::feel::{self, Grip, Lit, Sunk};
use crate::scroll::Menus;
use crate::shadow::{Elevate, Elevation};
use crate::state::{
    Column, CommentMode, Compose, Detail, DropTarget, Editing, MainView, Modal, Nav, SubModal,
    When, RAIL_WIDTH,
};
use crate::ws_drag::{Spot as WsSpot, Zone as WsZone};
use crate::{Dismiss, LockNow, NewWindow, PanePrefix, ToggleAi};
use malakt_core::{Id, Person};

/// 参照する寸法（CSS の px）を、画面全体の拡大率に合わせる。
pub(crate) const fn px(value: f32) -> Pixels {
    gpui_kit::px(value * crate::UI_SCALE)
}

/// アイコンを選ぶ画面に一度に出す数。それより多いときは検索で絞る。
const ICONS_SHOWN: usize = 400;
/// 担当者の一覧の 1 行の高さ。名前を書く入力欄と同じ高さにする。
const PERSON_ROW_HEIGHT: Pixels = gpui_kit::px(34. * crate::UI_SCALE);
/// 上のバーの、画面の名前の左に出すアイコンの大きさ。
const TILE_ICON_SIZE: Pixels = px(18.);
/// ホームのカードの枠（56px）の中のアイコンの大きさ。
const CARD_ICON_SIZE: Pixels = px(32.);
/// アイコンを選ぶ画面のアイコンの大きさ。
const PICKER_ICON_SIZE: Pixels = px(22.);
/// 上のバーの高さ。1 行だけにする。浮かせても変えない（中身の場所を変えないため）。
const HEADER_HEIGHT: Pixels = px(46.);
/// 詳細を右に並べても、一覧に残す幅。
const LIST_MIN: f32 = 320.;
/// 詳細を狭めてでも一覧に残したい幅。
const LIST_ROOM: f32 = 400.;
const ROW_HEIGHT: Pixels = px(60.);
const CARD_HEIGHT: Pixels = px(96.);
/// 一覧で、サブワークスペースの中の行を 1 段下げる幅。
const LIST_INDENT: Pixels = px(24.);
/// 一覧の、サブワークスペースの見出しの行の高さ。
const FOLDER_HEIGHT: Pixels = px(44.);

/// タスクの 1 行の高さ。表の行か、2 段のカードか。
fn task_height(table: bool, density: crate::theme::Density) -> Pixels {
    if table {
        ROW_HEIGHT * density.rows()
    } else {
        CARD_HEIGHT * density.cards()
    }
}

/// 一覧の行ごとの高さと、その合計。
pub(crate) fn row_sizes(
    rows: &[ListRow],
    table: bool,
    density: crate::theme::Density,
) -> (std::rc::Rc<Vec<Size<Pixels>>>, Pixels) {
    let task = task_height(table, density);
    let mut total = px(0.);
    let sizes = rows
        .iter()
        .map(|row| {
            let height = match row {
                ListRow::Task { .. } => task,
                ListRow::Folder { .. } => FOLDER_HEIGHT * density.rows(),
            };
            total += height;
            size(px(0.), height)
        })
        .collect();
    (std::rc::Rc::new(sizes), total)
}

/// 枠つきのボタン。乗せると淡い金、押している間ははっきり濃くなる。
pub(crate) trait Framed {
    fn framed(self, cx: &App) -> Self;
}

impl Framed for Unpressed {
    fn framed(self, cx: &App) -> Self {
        self.map_button(|button| button.framed(cx))
    }
}

impl Framed for Button {
    fn framed(self, cx: &App) -> Self {
        let theme = cx.theme();
        let (hover, active) = feel::button(theme.accent, theme.primary.opacity(0.4), cx);
        self.custom(
            ButtonCustomVariant::new(cx)
                .color(theme.background)
                .foreground(theme.foreground)
                .hover(hover)
                .active(active),
        )
        // 部品は地を透かして塗るので、下の影が透けて暗く見える。地の色で塗りつぶす
        .bg(theme.background)
        .border_1()
        .border_color(theme.border)
        .elevate(Elevation::Low, cx)
    }
}

/// 最小化・最大化・閉じるのボタン。macOS は OS が描くので出さない。
/// バーの高さいっぱいに右の端まで並べる。浮かせるときは、カードの中にほかのボタンと同じ高さで並べる。
pub(crate) fn window_controls(
    height: Pixels,
    floating: bool,
    window: &Window,
    cx: &App,
) -> Option<Div> {
    if cfg!(target_os = "macos") {
        return None;
    }
    // OS がタイトルバーを描いているときは、そちらのボタンを使う
    #[cfg(target_os = "linux")]
    if !matches!(window.window_decorations(), Decorations::Client { .. }) {
        return None;
    }
    let theme = cx.theme().clone();
    let supported = window.window_controls();
    let max = if window.is_maximized() {
        IconName::WindowRestore
    } else {
        IconName::WindowMaximize
    };
    let buttons = [
        (
            "window-minimize",
            IconName::WindowMinimize,
            WindowControlArea::Min,
            supported.minimize,
        ),
        (
            "window-maximize",
            max,
            WindowControlArea::Max,
            supported.maximize,
        ),
        (
            "window-close",
            IconName::WindowClose,
            WindowControlArea::Close,
            true,
        ),
    ];
    let mut controls = row().flex_none().ml_1();
    for (id, icon, area, shown) in buttons {
        if !shown {
            continue;
        }
        let close = area == WindowControlArea::Close;
        // 閉じるは、テーマの赤を淡く敷く
        let (hover_bg, hover_fg, active_bg) = if close {
            (
                theme.danger.opacity(0.2),
                theme.danger,
                theme.danger.opacity(0.35),
            )
        } else {
            (
                theme.secondary_hover,
                theme.foreground,
                theme.secondary_active,
            )
        };
        controls = controls.child(
            div()
                .id(id)
                .w(rems(2.5))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .map(|d| {
                    if floating {
                        d.h_6().rounded(feel::md())
                    } else {
                        d.h(height)
                    }
                })
                .text_color(theme.foreground)
                .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
                .active(move |s| s.bg(active_bg).text_color(hover_fg))
                // Windows では、OS がこの場所をウィンドウのボタンとして扱う
                .when(cfg!(target_os = "windows"), |d| d.window_control_area(area))
                .when(!cfg!(target_os = "windows"), |d| {
                    d.on_mouse_down(MouseButton::Left, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        match area {
                            WindowControlArea::Min => window.minimize_window(),
                            WindowControlArea::Max => window.zoom_window(),
                            _ => {
                                // ページの部品は、ウィンドウより先に捨てる
                                crate::browser::window_closing(window, cx);
                                window.remove_window()
                            }
                        }
                    })
                })
                .child(Icon::new(icon).small())
                .test_support(),
        );
    }
    Some(controls)
}

/// ウィンドウのヘッダー（上のバー）を右クリックしたときのメニュー。どのウィンドウのヘッダーも、これを出す。
/// `main` は、設定を開くタスクの画面。メインのウィンドウなら自分、別ウィンドウなら開いたもとの画面
/// （ないときは、メインのウィンドウに開く）。
pub(crate) fn window_menu(
    menu: PopupMenu,
    main: Option<WeakEntity<MainView>>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let unlocked = crate::db::shared(cx).read(cx).unlocked;
    let supported = window.window_controls();
    let (max_icon, max_label) = if window.is_maximized() {
        (IconName::WindowRestore, "元のサイズに戻す")
    } else {
        (IconName::WindowMaximize, "最大化")
    };
    let (full_icon, full_label) = if window.is_fullscreen() {
        ("minimize-2", "全画面を終了")
    } else {
        ("maximize-2", "全画面")
    };
    let (top_icon, top_label) = if crate::on_top::is_on(window, cx) {
        ("pin-off", "最前面の表示を解除")
    } else {
        ("pin", "常に最前面に表示")
    };
    let menu = menu.item(
        // ロックが無効なときは、ロックの設定を開く（`lib.rs` の LockNow）
        PopupMenuItem::new("ロックする")
            .icon(app_icon("lock"))
            .action(Box::new(LockNow))
            .disabled(!unlocked),
    );
    // 新しいウィンドウは、タスクの画面のほかに、ポモドーロも開ける
    let menu = if unlocked {
        menu.submenu_with_icon(
            Some(app_icon("app-window")),
            "新しいウィンドウ",
            window,
            cx,
            |menu, _, _| {
                menu.item(
                    PopupMenuItem::new("タスク")
                        .icon(app_icon("list-todo"))
                        .action(Box::new(NewWindow)),
                )
                .item(
                    PopupMenuItem::new("ポモドーロ")
                        .icon(app_icon("timer"))
                        .action(Box::new(crate::pomodoro::OpenPomodoro)),
                )
                .item(
                    PopupMenuItem::new("タイマー")
                        .icon(app_icon("alarm-clock"))
                        .action(Box::new(crate::timer::OpenTimer)),
                )
            },
        )
    } else {
        menu
    };
    menu.when(unlocked, |menu| {
        menu.item(
            PopupMenuItem::new("設定")
                .icon(app_icon("settings"))
                .action(Box::new(crate::shortcuts::OpenSettings))
                .on_click(move |_, _, cx| crate::open_settings_in(main.as_ref(), None, cx)),
        )
    })
    .separator()
    .when(supported.minimize, |menu| {
        menu.item(
            PopupMenuItem::new("最小化")
                .icon(Icon::new(IconName::WindowMinimize))
                .on_click(|_, window, _| window.minimize_window()),
        )
    })
    .when(supported.maximize, |menu| {
        menu.item(
            PopupMenuItem::new(max_label)
                .icon(Icon::new(max_icon))
                .on_click(|_, window, _| window.zoom_window()),
        )
    })
    .item(
        PopupMenuItem::new(full_label)
            .icon(app_icon(full_icon))
            .action(Box::new(crate::shortcuts::ToggleFullscreen))
            .on_click(|_, window, _| window.toggle_fullscreen()),
    )
    .item(
        PopupMenuItem::new(top_label)
            .icon(app_icon(top_icon))
            .on_click(|_, window, cx| crate::on_top::toggle(window, cx)),
    )
    .separator()
    .item(
        PopupMenuItem::new("閉じる")
            .icon(Icon::new(IconName::WindowClose))
            .on_click(|_, window, cx| {
                // ページの部品は、ウィンドウより先に捨てる
                crate::browser::window_closing(window, cx);
                window.remove_window()
            }),
    )
}

/// タスクの画面の地。背景の画像の上では薄く塗って、文字を読めるようにする。
pub(crate) fn page_fill(theme: &gpui_kit::component::Theme, cx: &App) -> Hsla {
    match crate::theme::backdrop(cx) {
        Some(backdrop) => theme.background.opacity(backdrop.alpha * 0.45),
        None => theme.background,
    }
}

/// 背景の画像を出しているときは、面を透かして塗る。
fn glass(color: Hsla, cx: &App) -> Hsla {
    match crate::theme::backdrop(cx) {
        Some(backdrop) => color.opacity(backdrop.alpha),
        None => color,
    }
}

/// 「カスタマイズ」のダイアログの大きさと、テーマの見本の幅。見本は 1 行に 5 つ並ぶ。
const APPEARANCE_WIDTH: Pixels = px(1280.);
const APPEARANCE_HEIGHT: Pixels = px(700.);
/// 「設定」の右に出すプレビューの幅。
const PREVIEW_WIDTH: Pixels = px(300.);
/// 「カスタマイズ」の行の右に置く、一覧を開くボタンやスライダーの幅。
const CONTROL_WIDTH: Pixels = px(220.);
const PRESET_WIDTH: Pixels = px(124.);

/// 期限の通知で選べるものと、その名前。`timed` は時刻つきの期限（何分前）、ほかは日付だけの期限（何日前）。
fn alert_kind(timed: bool) -> (&'static [u32], fn(u32) -> String) {
    use malakt_core::alert;
    if timed {
        (&alert::TIMED_CHOICES, alert::timed_label)
    } else {
        (&alert::DATE_CHOICES, alert::date_label)
    }
}

/// 「カスタマイズ」の設定 1 つ。
struct Setting {
    section: crate::state::CustomizeSection,
    key: SettingKey,
    name: &'static str,
    note: Option<&'static str>,
    /// 名前と補足のほかに、検索で当たる言葉（小文字）。
    words: String,
}

#[derive(Clone, Copy, PartialEq)]
enum SettingKey {
    Themes,
    Layouts,
    Scene,
    Clear,
    Blur,
    Font,
    MonoFont,
    Color(crate::theme::Slot),
    Density,
    DueFormat,
    Feel,
    Cards,
    Float,
    Ai(crate::ai_settings::AiRow),
    LockOn,
    AutoLock,
    LockedNotices,
    Reminders,
    Autostart,
    AutoUpdate,
    Export,
    Import,
    BrowserData,
    AlertTimed,
    AlertDate,
    AlertHour,
    DoneOn,
    DoneWhen,
    DoneRoles,
    DonePeople,
    DoneWorkspaces,
    LockNow,
    McpKeyless,
    McpKeys,
    McpHttp,
    McpHttpPort,
    McpHttpUrl,
    People,
    CalDav,
    Keys,
    NewWindow,
    Pomodoro,
    Timer,
    Plugins,
    PluginFolders,
}

impl Setting {
    fn new(
        section: crate::state::CustomizeSection,
        key: SettingKey,
        name: &'static str,
        note: Option<&'static str>,
        words: String,
    ) -> Self {
        Setting {
            section,
            key,
            name,
            note,
            words: words.to_lowercase(),
        }
    }

    /// 検索の言葉（小文字）が、名前・補足・カテゴリ・言葉のどれかに入っている。
    fn matches(&self, query: &str) -> bool {
        self.name.to_lowercase().contains(query)
            || self.note.is_some_and(|n| n.to_lowercase().contains(query))
            || self.section.label().to_lowercase().contains(query)
            || self.words.contains(query)
    }
}

/// 浮かせたパネルのすき間。
pub(crate) const FLOAT_GAP: Pixels = px(8.);

/// 浮かせたパネルの地（角丸のカード）。パネルの最初の子にして、中身の後ろに敷く。
/// `edges` は上・右・下・左のすき間。隣のパネルと接する辺は半分にして、すき間をそろえる。
pub(crate) fn floating_card(fill: Hsla, edges: [Pixels; 4], cx: &App) -> Div {
    let [top, right, bottom, left] = edges;
    div()
        .absolute()
        .top(top)
        .right(right)
        .bottom(bottom)
        .left(left)
        .rounded(px(16.))
        .border_1()
        .border_color(feel::rim(cx))
        .bg(fill)
        .map(|d| match feel::panel_shadow(cx) {
            Some(shadow) => d.shadow(shadow),
            None => d.elevate(Elevation::Low, cx),
        })
}

thread_local! {
    /// 背景の画像を最後に描いた場所（ウィンドウごと）。すりガラスの面が、同じ場所に画像を合わせる。
    static BACKDROP_AT: std::cell::RefCell<Vec<(WindowId, Bounds<Pixels>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// 画面の後ろに敷く画像。描いた場所を覚えておき、すりガラスの面（`frosted_glass`）が使う。
fn backdrop_layer(cx: &App) -> Option<impl IntoElement> {
    let image = crate::theme::backdrop(cx)?.image.clone();
    Some(
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let id = window.window_handle().window_id();
                BACKDROP_AT.with_borrow_mut(|at| {
                    at.retain(|(other, _)| *other != id);
                    at.push((id, bounds));
                });
                let fit = ObjectFit::Cover.get_bounds(bounds, image.size(0));
                if let Err(error) =
                    window.paint_image(bounds, fit, Corners::default(), image.clone(), 0, false)
                {
                    // 描く途中なので画面には出せない。前の img と同じくログに残す
                    eprintln!("背景の画像を描けませんでした: {error}");
                }
            },
        )
        .absolute()
        .inset_0()
        .size_full(),
    )
}

/// 一覧の上に重ねるパネルの地。後ろの一覧は見せず、背景の画像を強くぼかしたものを
/// 画面の後ろの画像と同じ位置に敷いて、その上を `fills` の順に塗る（すりガラス）。
/// gpui には後ろをぼかす塗りがないので、ぼかした画像で代わりにする。
/// パネルの最初の子にする。`corners` はパネルの角の丸み。
fn frosted_glass(corners: Corners<Pixels>, fills: Vec<Hsla>, cx: &App) -> Option<impl IntoElement> {
    let image = crate::theme::backdrop(cx)?.frosted.clone();
    Some(
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let id = window.window_handle().window_id();
                let under = BACKDROP_AT
                    .with_borrow(|at| at.iter().find(|(other, _)| *other == id).map(|(_, b)| *b))
                    .unwrap_or_else(|| Bounds::new(Point::default(), window.viewport_size()));
                let fit = ObjectFit::Cover.get_bounds(under, image.size(0));
                if let Err(error) =
                    window.paint_image(bounds, fit, corners, image.clone(), 0, false)
                {
                    // 描く途中なので画面には出せない。ログに残す
                    eprintln!("パネルの後ろの画像を描けませんでした: {error}");
                }
                for color in fills {
                    window.paint_quad(fill(bounds, color).corner_radii(corners));
                }
            },
        )
        .absolute()
        .inset_0(),
    )
}

/// 同梱の画像の小さい絵。選ぶ画面に出す。
fn scene_thumbnail(scene: &crate::theme::Scene, dark: bool) -> Option<Arc<Image>> {
    use std::sync::OnceLock;
    static MADE: OnceLock<[Option<Arc<Image>>; 6]> = OnceLock::new();
    let made = MADE.get_or_init(|| {
        use crate::theme::Scene;
        [
            (Scene::Sky, false),
            (Scene::Sky, true),
            (Scene::Forest, false),
            (Scene::Forest, true),
            (Scene::Dusk, false),
            (Scene::Dusk, true),
        ]
        .map(|(scene, dark)| {
            scene
                .bundled(dark)
                .map(|bytes| Arc::new(Image::from_bytes(ImageFormat::Jpeg, bytes.to_vec())))
        })
    });
    let at = match scene {
        crate::theme::Scene::Sky => 0,
        crate::theme::Scene::Forest => 2,
        crate::theme::Scene::Dusk => 4,
        _ => return None,
    };
    made[at + usize::from(dark)].clone()
}

pub(crate) fn row() -> Div {
    div().flex().flex_row().items_center()
}

pub(crate) fn col() -> Div {
    div().flex().flex_col()
}

/// 「設定」の、設定の行をまとめるカード。
pub(crate) fn setting_card(cx: &App) -> Div {
    let theme = cx.theme();
    col()
        .px_4()
        .rounded(feel::lg())
        .bg(theme.popover)
        .border_1()
        .border_color(theme.border.opacity(0.6))
}

/// 「設定」の 1 行。左に名前と補足、右に変えるもの。`wide` のときは、変えるものを名前の下に横いっぱいに出す。
/// 狭いところでは、右に置くものが名前の下に回る。
pub(crate) fn setting_line(
    name: &'static str,
    note: Option<&'static str>,
    control: AnyElement,
    wide: bool,
    first: bool,
    theme: &gpui_kit::component::Theme,
) -> AnyElement {
    let label = col()
        .flex_1()
        .min_w(px(160.))
        .gap_0p5()
        .child(div().text_sm().child(name))
        .when_some(note, |d, note| {
            d.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(note),
            )
        });
    let line = if wide {
        col().py_3().gap_2().child(label).child(control)
    } else {
        row()
            .flex_wrap()
            .justify_end()
            .py_3()
            .gap_x_4()
            .gap_y_2()
            .child(label)
            .child(control)
    };
    line.when(!first, |d| {
        d.border_t_1().border_color(theme.border.opacity(0.6))
    })
    .into_any_element()
}

/// 押すと、選べる値のメニューが開くボタン。
fn menu_choice(
    id: &'static str,
    shown: SharedString,
    theme: &gpui_kit::component::Theme,
) -> Unpressed {
    button(id).outline().w(CONTROL_WIDTH).h_8().child(
        row()
            .min_w_0()
            .gap_2()
            .child(div().flex_1().min_w_0().truncate().child(shown))
            .child(
                app_icon("chevron-down")
                    .small()
                    .text_color(theme.muted_foreground),
            ),
    )
}

/// 押せる小さなラベル。選ばれているものは色を付ける。
/// 進捗の棒。
fn percent_bar(percent: u8, width: Pixels, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .w(width)
        .h(px(6.))
        .flex_none()
        .rounded_full()
        .bg(theme.muted)
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .w(relative(f32::from(percent.min(100)) / 100.))
                .rounded_full()
                .bg(theme.primary),
        )
}

/// 本文の Markdown の見た目。表のセルの左右の余白を px で決める。画面を 1.25 倍にしているので、
/// 標準の余白（`rem` で決まる）のままだと、表の幅の計算（16px の `rem` で測る）より余白が広くなり、
/// セルの最後の文字が次の行に落ちる。
pub(crate) fn md_style() -> TextViewStyle {
    TextViewStyle::default().table_cell(StyleRefinement::default().px(px(12. * crate::UI_SCALE)))
}

/// ドラッグで選んで Ctrl+C（⌘C）でコピーできる、Markdown にしない文字。選んだ所の色は
/// AI の返答（TextView）と同じ。チャットの発言、知らせ、エラーのように、読むだけの文に使う。
pub(crate) fn selectable_text(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    cx: &App,
) -> gpui_kit::base::SelectableText {
    gpui_kit::base::SelectableText::new(id, text).selection_color(cx.theme().selection)
}

pub(crate) fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded(feel::md())
        .border_1()
        .text_sm()
        .cursor_pointer()
        .flex_none()
        .when(active, |d| {
            d.bg(theme.primary)
                .text_color(theme.primary_foreground)
                .border_color(theme.primary)
        })
        .when(!active, |d| {
            d.border_color(theme.border).lit(theme.secondary_hover, cx)
        })
        .child(label.into())
}

/// 付いているものを外す ×（カテゴリ・URL・AI に渡すタスク）。ベースの `CategoryInput.vue` と同じ
/// 丸いボタン（size-6、アイコン 3.5）で、乗せると accent の面と文字色に、押している間は濃い金になる。
/// 何を外すかはホバーのラベルで出す。押したときの処理を渡さないと作れない。
pub(crate) fn remove_x(
    id: impl Into<ElementId>,
    tip: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    round_icon(id, "x", tip, on_click, cx)
}

/// 行の中の小さな丸いアイコンのボタン。乗せたときと押したときの動きは [`remove_x`] と同じ。
/// 同じ行に並べるボタンは、これでそろえる。
pub(crate) fn round_icon(
    id: impl Into<ElementId>,
    icon: &'static str,
    tip: impl Into<SharedString>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    let tip: SharedString = tip.into();
    let ink = theme.accent_foreground;
    div()
        .id(id)
        .flex_none()
        .size_6()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .lit(theme.accent, cx)
        .sunk(theme.primary.opacity(0.4), cx)
        .hover(move |s| s.text_color(ink))
        .child(app_icon(icon).size_3p5())
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
        })
        .on_click(move |event, window, cx| {
            // チップ（URL など）が押したときの処理を持っていても、外すだけにする
            cx.stop_propagation();
            on_click(event, window, cx);
        })
}

/// ファイルの種類のアイコン（ダウンロードの一覧、チャットに添えたファイル）。
pub(crate) fn file_icon(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "zip" | "7z" | "rar" | "gz" | "tgz" | "xz" | "bz2" | "tar" | "dmg" | "msi" | "exe" => {
            "file-archive"
        }
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "heic" | "ico" => "file-image",
        "mp3" | "wav" | "flac" | "m4a" | "aac" | "ogg" => "file-music",
        "mp4" | "mov" | "mkv" | "webm" | "avi" => "file-video-camera",
        "csv" | "xlsx" | "xls" | "ods" => "file-spreadsheet",
        "pdf" | "txt" | "md" | "doc" | "docx" | "rtf" => "file-text",
        "json" | "js" | "ts" | "rs" | "py" | "html" | "css" | "xml" | "yaml" | "yml" | "toml" => {
            "file-code"
        }
        _ => "file",
    }
}

fn status_color(status: Status, cx: &App) -> Hsla {
    let theme = cx.theme();
    match status {
        Status::NeedsAction => theme.foreground,
        Status::InProcess => theme.info,
        Status::Completed => theme.success,
        Status::Cancelled => theme.muted_foreground,
    }
}

pub(crate) fn status_icon(status: Status) -> &'static str {
    match status {
        Status::NeedsAction => "circle",
        Status::InProcess => "circle-dot",
        Status::Completed => "circle-check",
        Status::Cancelled => "circle-x",
    }
}

/// 状態の丸いラベル。
fn status_badge(status: Status, cx: &App) -> Div {
    let theme = cx.theme();
    row()
        .gap_1()
        .flex_none()
        .px_2()
        .py_0p5()
        .rounded_full()
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .child(
            app_icon(status_icon(status))
                .xsmall()
                .text_color(status_color(status, cx)),
        )
        .child(status.label())
}

/// 数の丸に出す数の上限。これを超えたら「99+」にする（ベースの COUNT_BUBBLE_LIMIT）。
const BUBBLE_LIMIT: u32 = 99;

/// 数の丸に出す文字。0 のときは丸を出さない。
pub(crate) fn bubble_text(count: u32) -> Option<String> {
    match count {
        0 => None,
        n if n > BUBBLE_LIMIT => Some(format!("{BUBBLE_LIMIT}+")),
        n => Some(n.to_string()),
    }
}

/// 数の丸の大きさ。ベースの 2 か所の値をそのまま持つ。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Bubble {
    /// ベルのアイコンの右上に重ねる（AdminNotificationBell: 高さ 1.125rem・文字 0.6875rem・px-1）
    Bell,
    /// 左のメニューの「通知」の右（AdminSidebar: 高さ 1.25rem・文字 text-xs・px-1.5）
    Nav,
}

impl Bubble {
    /// 高さ（rem）。幅の最小もこの値にして、1 桁は正円にする。
    pub(crate) const fn height(self) -> f32 {
        match self {
            Bubble::Bell => 1.125,
            Bubble::Nav => 1.25,
        }
    }

    /// 文字の大きさ（rem）
    pub(crate) const fn text(self) -> f32 {
        match self {
            Bubble::Bell => 0.6875,
            Bubble::Nav => 0.75,
        }
    }

    /// 左右の余白（rem）。2 桁以上はこの余白のまま横に伸びる。
    pub(crate) const fn pad(self) -> f32 {
        match self {
            Bubble::Bell => 0.25,
            Bubble::Nav => 0.375,
        }
    }
}

/// 未読などの数の丸。1 桁は正円、2 桁以上は横に伸びる丸。数は上下左右の真ん中に置く。
/// `corner` があれば、親の右上から（上, 右）だけずらして重ねる。数が 0 のときは何も出さない。
pub(crate) fn count_bubble(
    id: impl Into<ElementId>,
    count: u32,
    size: Bubble,
    corner: Option<(Pixels, Pixels)>,
    cx: &App,
) -> Option<impl IntoElement> {
    let text = bubble_text(count)?;
    let theme = cx.theme();
    let height = rems(size.height());
    Some(
        div()
            .id(id)
            .when_some(corner, |d, (top, right)| d.absolute().top(top).right(right))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(height)
            .min_w(height)
            .px(rems(size.pad()))
            .rounded_full()
            .bg(theme.danger)
            .text_color(theme.danger_foreground)
            .text_size(rems(size.text()))
            .line_height(height)
            .font_weight(FontWeight::BOLD)
            .whitespace_nowrap()
            .child(text)
            .test_support(),
    )
}

/// 表の値のボタンの中身。ボタンは中身を真ん中に寄せるので、幅いっぱいの行に入れて左から並べる。
/// 列に入らない文字は、末尾を「…」にする。
fn value_shown(shown: Div) -> impl IntoElement {
    row()
        .id("value")
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .child(shown.min_w_0().truncate())
        .test_support()
}

/// 知らせの中に出す「元に戻す」ボタン。
fn notice_undo(cx: &mut Context<MainView>) -> impl IntoElement {
    let theme = cx.theme();
    feel::gloss(
        button("notice-undo")
            .icon(app_icon("undo-2").text_color(theme.primary_foreground))
            .ghost()
            .small()
            .tooltip("元に戻す")
            .on_click(cx.listener(|this, _, _, cx| {
                this.undo(false, cx);
            })),
    )
}

/// 状態を選ぶメニューの中身。選ぶとすぐ変わり、コメントを付けるときだけダイアログを開く。
fn status_items(
    mut menu: PopupMenu,
    view: &WeakEntity<MainView>,
    task: &Id,
    current: Status,
) -> PopupMenu {
    for status in Status::ALL {
        let (view, task) = (view.clone(), task.clone());
        menu = menu.item(
            PopupMenuItem::new(status.label())
                .checked(status == current)
                .on_click(move |_, _, cx| {
                    // ウィンドウを閉じたあとなら、反映する先がない
                    let _ = view.update(cx, |this, cx| this.set_status(&task, status, cx));
                }),
        );
    }
    let (view, task) = (view.clone(), task.clone());
    menu.separator()
        .item(
            PopupMenuItem::new("コメントを付けて変更…").on_click(move |_, window, cx| {
                // ウィンドウを閉じたあとなら、反映する先がない
                let _ = view.update(cx, |this, cx| this.ask_status(&task, window, cx));
            }),
        )
}

/// 担当者を登録するメニューの中身。ほかのワークスペースの担当者を選ぶと、このワークスペースでも
/// 使えるようになって担当に入る。いちばん下から、新しい担当者をその場で足す。
fn person_items(
    mut menu: PopupMenu,
    view: &WeakEntity<MainView>,
    others: &[(Id, SharedString)],
) -> PopupMenu {
    for (id, name) in others {
        let (view, id) = (view.clone(), id.clone());
        menu = menu.item(PopupMenuItem::new(name.clone()).on_click(move |_, _, cx| {
            // ウィンドウを閉じたあとなら、反映する先がない
            let _ = view.update(cx, |this, cx| this.register_assignee(&id, cx));
        }));
    }
    if !others.is_empty() {
        menu = menu.separator();
    }
    let view = view.clone();
    menu.item(
        PopupMenuItem::new("新規担当として登録").on_click(move |_, window, cx| {
            // ウィンドウを閉じたあとなら、反映する先がない
            let _ = view.update(cx, |this, cx| this.add_assignee_person(window, cx));
        }),
    )
}

/// 担当者を使うワークスペースを選ぶメニューの中身。
fn person_workspace_items(
    menu: PopupMenu,
    view: &WeakEntity<MainView>,
    person: &Id,
    current: &[Id],
    roots: &[(Id, SharedString)],
) -> PopupMenu {
    let mut menu = menu.item({
        let (view, person) = (view.clone(), person.clone());
        PopupMenuItem::new("すべてのワークスペース")
            .checked(current.is_empty())
            .on_click(move |_, _, cx| {
                // ウィンドウを閉じたあとなら、反映する先がない
                let _ = view.update(cx, |this, cx| {
                    this.set_person_workspaces(&person, Vec::new(), cx)
                });
            })
    });
    if !roots.is_empty() {
        menu = menu.separator();
    }
    for (id, name) in roots {
        let (view, person, id) = (view.clone(), person.clone(), id.clone());
        menu = menu.item(
            PopupMenuItem::new(name.clone())
                .checked(current.contains(&id))
                .on_click(move |_, _, cx| {
                    // ウィンドウを閉じたあとなら、反映する先がない
                    let _ = view.update(cx, |this, cx| {
                        this.toggle_person_workspace(&person, &id, cx)
                    });
                }),
        );
    }
    menu
}

/// 優先度。棒の本数と色で見分ける。
fn priority_label(priority: Prio, cx: &App) -> Div {
    let theme = cx.theme();
    let (icon, color) = match priority {
        Prio::High => ("signal-high", rgb(0xb04e1c).into()),
        Prio::Mid => ("signal-medium", rgb(0x80660d).into()),
        Prio::Low => ("signal-low", theme.muted_foreground),
        Prio::None => {
            return row()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("—");
        }
    };
    row()
        .gap_1p5()
        .text_sm()
        .when(priority == Prio::Low, |d| d.text_color(color))
        .child(app_icon(icon).small().text_color(color))
        .child(priority.label())
}

/// 名前の頭文字の丸。
pub(crate) fn avatar(name: &str, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .size_6()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(theme.secondary)
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .child(name.chars().next().map(String::from).unwrap_or_default())
}

/// 欄の上に付ける小さな見出し。
/// 絞り込みを外す ×。場所がずれないよう、効いていないときも薄く置いておき、押せなくする。
/// 押せるようになったときと、押せなくなったときに、薄い所から濃さを戻して変わったことを見せる。
fn clear_filter(
    id: impl Into<SharedString>,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let id: SharedString = id.into();
    div()
        .flex_none()
        .child(feel::gloss_when(
            active,
            button(id.clone())
                .icon(app_icon("x"))
                .ghost()
                .small()
                .disabled(!active)
                .tooltip("解除")
                .on_click(on_click),
        ))
        .with_animation(
            SharedString::from(format!("{id}-{active}")),
            Animation::new(std::time::Duration::from_millis(180)).with_easing(ease_out_quint()),
            |el, delta| el.opacity(0.3 + 0.7 * delta),
        )
}

fn field_label(text: &'static str, cx: &App) -> Div {
    div()
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

/// ワークスペースのアイコンの色。`None` は文字と同じ色。
fn workspace_color(color: Option<&str>) -> Option<Hsla> {
    let hex = match color? {
        "red" => 0xe5484d,
        "orange" => 0xf76b15,
        "yellow" => 0xd99a00,
        "green" => 0x30a46c,
        "teal" => 0x12a594,
        "blue" => 0x3e63dd,
        "purple" => 0x8e4ec6,
        "pink" => 0xd6409f,
        _ => return None,
    };
    Some(rgb(hex).into())
}

/// ワークスペースのアイコン。自分で足した画像はそのままの色で、ほかは選んだ色の線画で出す。
pub fn workspace_icon(
    icon: Option<&str>,
    color: Option<&str>,
    size: Pixels,
    cx: &App,
) -> AnyElement {
    let custom = icon
        .filter(|icon| malakt_core::icon::is_custom(icon))
        .and_then(|icon| crate::icons::custom_image(icon, cx));
    if let Some(image) = custom {
        return img(image)
            .size(size)
            .flex_none()
            .object_fit(ObjectFit::Contain)
            .into_any_element();
    }
    let icon = Icon::default()
        .path(crate::state::workspace_icon_path(icon))
        .size(size)
        .flex_none();
    match workspace_color(color) {
        Some(color) => icon.text_color(color).into_any_element(),
        None => icon.into_any_element(),
    }
}

/// 左のメニューの行に出すアイコンの大きさ。
const NAV_ICON_SIZE: Pixels = px(14.);
/// パンくずに並べる段の数の上限（「…」を含む）。
const MAX_CRUMBS: usize = 4;
/// パンくずの 1 段の、名前のほかの幅（アイコン・すき間・左右の余白、rem）。
const CRUMB_CHROME: f32 = 2.;

/// パンくずの名前を縮めるときの、いちばん狭い幅（rem）。名前の幅の見積もり（text_sm で、英数字は 0.7 字、
/// ほかは 1 字）と `cap` の小さいほう。
fn crumb_min(name: &str, cap: f32) -> f32 {
    let em: f32 = name
        .chars()
        .map(|c| if c.is_ascii() { 0.7 } else { 1. })
        .sum();
    (em * 0.875).min(cap)
}
/// 畳んだ左のメニューの、アイコンの大きさと押せる四角の大きさ（拡大率を掛ける前の値）。
const RAIL_ICON_SIZE: Pixels = px(18.);
const RAIL_BUTTON: f32 = 36.;

/// 同梱の線画アイコン。
pub(crate) fn app_icon(name: &str) -> Icon {
    Icon::default().path(crate::state::icon_path(name))
}

/// 出てくるときの動き。すっと出て、ゆっくり止まる。
fn appear(ms: u64) -> Animation {
    Animation::new(std::time::Duration::from_millis(ms)).with_easing(ease_out_quint())
}

/// ダイアログが閉じてから消えるまでの長さ（ミリ秒）。
const DIALOG_CLOSE_MS: u64 = 150;

/// タスクの詳細が出てくる動きと消える動きの長さ（ミリ秒）。ベースのサイドパネルと同じ。
const PANEL_MOTION_MS: u64 = 200;

/// 出入りの途中のタスクの詳細。`t` が 0 で右に 48px ずれて消えていて、1 で出きっている。
fn panel_shift(panel: Div, t: f32) -> Div {
    // 浮かせたパネルの中では、右のすき間の分だけ内側で止まる
    let rest = if crate::theme::floating_now() {
        FLOAT_GAP
    } else {
        px(0.)
    };
    panel.right(rest + px(-48.) * (1. - t)).opacity(t)
}

pub(crate) fn section_title(text: &'static str) -> Div {
    div().font_weight(FontWeight::BOLD).child(text)
}

/// 詳細の中の 1 まとまり。白い地に枠。
pub(crate) fn card(cx: &App) -> Div {
    let theme = cx.theme();
    col()
        .p_4()
        .gap_2()
        .rounded(feel::lg())
        .border_1()
        .border_color(theme.border)
        .bg(glass(theme.popover, cx))
        .elevate(Elevation::Low, cx)
}

impl MainView {
    /// 詳細の開始日か期限。値を押すと、その場にカレンダーが開く。
    fn when_property(&self, task: &Todo, which: When, cx: &mut Context<Self>) -> Div {
        let (value, name) = match which {
            When::Start => (task.start.as_deref(), "start"),
            When::Due => (task.due.as_deref(), "due"),
        };
        let shown = match value {
            Some(value) => div().text_sm().child(view::format_date(value, &self.today)),
            None => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("未設定"),
        };
        self.date_trigger(
            SharedString::from(format!("{name}-value")),
            crate::date_pick::DateTarget::Task {
                id: task.id.clone(),
                which,
                list: false,
            },
            shown,
            cx,
        )
    }

    /// 詳細の「通知」。押すと、期限をいつ知らせるかのメニューが開く。期限がなければ出さない。
    fn task_alert_menu(
        &self,
        task_id: &str,
        due: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        use malakt_core::alert;
        let due = due?;
        let timed = malakt_core::time_of(due).is_some();
        let standard = self.due_alert_standard(cx);
        let own = self.task_alerts(task_id, cx);
        let (choices, label) = alert_kind(timed);
        let (own, base) = if timed {
            (own.timed, standard.timed)
        } else {
            (own.date, standard.date)
        };
        let shown = match &own {
            Some(values) => alert::summary(values, label),
            None => format!("標準（{}）", alert::summary(&base, label)),
        };
        let muted = cx.theme().muted_foreground;
        let view = cx.entity().downgrade();
        let task_id = task_id.to_owned();
        let chosen = own.clone().unwrap_or_else(|| base.clone());
        let standard_label = format!("標準に合わせる（{}）", alert::summary(&base, label));
        let menu = button("task-alerts")
            .outline()
            .small()
            .child(
                row()
                    .min_w_0()
                    .gap_2()
                    .child(div().min_w_0().truncate().child(shown))
                    .child(app_icon("chevron-down").small().text_color(muted)),
            )
            .pop_menu(move |mut menu, _, _| {
                let reset_view = view.clone();
                let reset_task = task_id.clone();
                menu = menu
                    .item(
                        PopupMenuItem::new(standard_label.clone())
                            .checked(own.is_none())
                            .on_click(move |_, _, cx| {
                                // ウィンドウを閉じたあとなら、反映する先がない
                                let _ = reset_view.update(cx, |this, cx| {
                                    this.reset_task_alerts(&reset_task, timed, cx);
                                });
                            }),
                    )
                    .separator();
                for value in choices.iter().copied() {
                    let view = view.clone();
                    let task_id = task_id.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label(value))
                            .checked(chosen.contains(&value))
                            .on_click(move |_, _, cx| {
                                // ウィンドウを閉じたあとなら、反映する先がない
                                let _ = view.update(cx, |this, cx| {
                                    this.toggle_task_alert(&task_id, timed, value, cx);
                                });
                            }),
                    );
                }
                menu
            });
        Some(div().flex().child(menu).into_any_element())
    }

    fn due_label(&self, task: &Todo, cx: &App) -> Div {
        let theme = cx.theme();
        let format = crate::theme::due_format(cx);
        let Some(due) = &task.due else {
            // 期限がなく開始だけなら「11/24～」
            return match &task.start {
                Some(start) => {
                    let text = view::format_start_only(start, &self.today, format);
                    div()
                        .when(!task.status.is_open(), |d| {
                            d.text_color(theme.muted_foreground)
                        })
                        .child(text)
                }
                None => div().text_color(theme.muted_foreground).child("—"),
            };
        };
        let relative = format == view::DueFormat::Relative;
        let text = view::format_due(task.start.as_deref(), due, &self.today, format);
        if !task.status.is_open() {
            return div().text_color(theme.muted_foreground).child(text);
        }
        let past_day = malakt_core::date_of(due) < self.today.as_str();
        match view::due_state(due, &view::now()) {
            // 相対の表示は「2 日前」「今日」と書いてあるので、添えない
            DueState::Overdue if relative && past_day => div().text_color(theme.danger).child(text),
            DueState::Overdue => div()
                .text_color(theme.danger)
                .child(format!("{text}（期限切れ）")),
            DueState::Today if relative => div().text_color(theme.primary).child(text),
            DueState::Today => div()
                .text_color(theme.primary)
                .child(format!("{text}（今日）")),
            DueState::Soon => div().text_color(theme.primary).child(text),
            DueState::Later => div().child(text),
        }
    }

    // ---- 左のメニュー ----

    /// 左のメニューの 1 行。選んでいる行は色を付ける。
    fn nav_link(
        &self,
        id: impl Into<ElementId>,
        icon: impl IntoElement,
        label: impl Into<SharedString>,
        active: bool,
        cx: &App,
    ) -> Stateful<Div> {
        let theme = cx.theme();
        row()
            .id(id)
            .flex_1()
            .min_w_0()
            .gap_2()
            .px_3()
            .py_2()
            .rounded(feel::md())
            .text_sm()
            .cursor_pointer()
            .when(active, |d| {
                feel::selected(d, theme.accent, cx).text_color(theme.accent_foreground)
            })
            .when(!active, |d| d.lit(theme.accent.opacity(0.6), cx))
            .active(|s| s.bg(theme.accent).opacity(0.8))
            .child(icon)
            .child(div().flex_1().truncate().child(label.into()))
    }

    fn nav_item(
        &self,
        id: impl Into<ElementId>,
        icon: impl IntoElement,
        label: impl Into<SharedString>,
        nav: Nav,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.nav_link(id, icon, label, self.shown_nav(cx) == nav, cx)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.go(nav.clone(), cx)))
            .test_support()
            .into_any_element()
    }

    /// ドラッグ中のタスクを落とせる場所にする。マウスが上にある間は色を付ける。
    fn drop_zone(
        &self,
        el: Stateful<Div>,
        target: DropTarget,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = cx.theme();
        let over = self.drop_over() == Some(&target);
        el.relative()
            .when(self.dragged_task().is_some(), |d| {
                d.rounded(feel::md())
                    .when(over, |d| d.bg(theme.primary.opacity(0.25)))
            })
            .child(
                self.drops
                    .mark(cx.entity().downgrade(), Some(target), false),
            )
    }

    /// タスクをドラッグしている間、ウィンドウに届くマウスの動きをすべて受ける。
    /// 詳細の上でも、ウィンドウの外（ほかのウィンドウの上）でも追える。
    fn task_drag_listener(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        self.task_drag.as_ref()?;
        let view = cx.entity().downgrade();
        Some(
            canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    let moved = view.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                        if phase != DispatchPhase::Capture {
                            return;
                        }
                        // 閉じた画面のドラッグは、もう続かない
                        let _ = moved.update(cx, |this, cx| {
                            if event.pressed_button == Some(MouseButton::Left) {
                                this.drag_task_at(event.position, window, cx);
                            } else {
                                this.finish_task_drag(cx);
                            }
                        });
                    });
                    let released = view.clone();
                    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                        if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                            return;
                        }
                        // 閉じた画面のドラッグは、もう続かない
                        let _ = released.update(cx, |this, cx| this.finish_task_drag(cx));
                    });
                },
            )
            .absolute()
            .size_0(),
        )
    }

    /// ワークスペースをドラッグしている間、ウィンドウに届くマウスの動きをすべて受ける。
    fn ws_drag_listener(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        self.ws_drag.as_ref()?;
        let view = cx.entity().downgrade();
        Some(
            canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    let moved = view.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                        if phase != DispatchPhase::Capture {
                            return;
                        }
                        // 閉じた画面のドラッグは、もう続かない
                        let _ = moved.update(cx, |this, cx| {
                            if event.pressed_button == Some(MouseButton::Left) {
                                this.drag_ws_at(event.position, window, cx);
                            } else {
                                this.finish_ws_drag(cx);
                            }
                        });
                    });
                    let released = view.clone();
                    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                        if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                            return;
                        }
                        // 閉じた画面のドラッグは、もう続かない
                        let _ = released.update(cx, |this, cx| this.finish_ws_drag(cx));
                    });
                },
            )
            .absolute()
            .size_0(),
        )
    }

    /// マウスに付いて動く、ドラッグ中のワークスペース。
    fn ws_drag_ghost(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let drag = self.ws_drag.as_ref().filter(|d| d.moved)?;
        let ws = self.workspaces.iter().find(|w| w.id == drag.id)?;
        let theme = cx.theme();
        let ghost = row()
            .id("ws-drag-ghost")
            .absolute()
            .left(drag.at.x + px(10.))
            .top(drag.at.y + px(10.))
            .max_w(px(280.))
            .gap_2()
            .px_3()
            .py_2()
            .rounded(feel::md())
            .border_1()
            .border_color(theme.primary)
            .bg(theme.popover)
            .elevate(Elevation::High, cx)
            .text_sm()
            .child(workspace_icon(
                ws.icon.as_deref(),
                ws.color.as_deref(),
                NAV_ICON_SIZE,
                cx,
            ))
            .child(div().min_w_0().truncate().child(ws.name.clone()))
            .test_support();
        Some(if self.motion {
            ghost
                .with_animation("ws-ghost-in", appear(140), |d, t| d.opacity(t))
                .into_any_element()
        } else {
            ghost.into_any_element()
        })
    }

    /// マウスに付いて動く、ドラッグ中のタスクのタイトル。
    fn task_drag_ghost(&self, cx: &mut Context<Self>) -> Option<Div> {
        let own = self
            .task_drag
            .as_ref()
            .filter(|d| d.moved && !self.embedded)
            .map(|d| (d.title.clone(), d.at));
        let (title, at) = own.or_else(|| {
            let drop = self.task_drop.as_ref()?;
            Some((drop.title.clone(), drop.at?))
        })?;
        let theme = cx.theme();
        Some(
            div()
                .absolute()
                .left(at.x + px(10.))
                .top(at.y + px(10.))
                .max_w(px(320.))
                .px_3()
                .py_2()
                .rounded(feel::md())
                .border_1()
                .border_color(theme.primary)
                .bg(theme.popover)
                .elevate(Elevation::High, cx)
                .text_sm()
                .truncate()
                .child(title),
        )
    }

    /// 左のメニューを開く・閉じる。広いウィンドウではアイコンだけの列に畳み、
    /// 狭いウィンドウでは中身と入れ替えで出す。
    fn toggle_sidebar(&mut self, wide: bool, cx: &mut Context<Self>) {
        if wide {
            self.sidebar_hidden = !self.sidebar_hidden;
            self.sidebar_turns += 1;
        } else {
            self.sidebar_open = !self.sidebar_open;
        }
        cx.notify();
    }

    /// ワークスペースの「…」と右クリックで出すメニュー。
    fn workspace_menu(
        &self,
        id: Id,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu) -> PopupMenu + Clone + 'static {
        let view = cx.entity().downgrade();
        let plugin_commands = crate::plugins::get(cx).commands(malakt_plugins::Place::Workspace);
        move |menu: PopupMenu| {
            let item =
                |label: &'static str,
                 run: fn(&mut MainView, Id, &mut Window, &mut Context<MainView>)| {
                    let view = view.clone();
                    let id = id.clone();
                    PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        let _ = view.update(cx, |this, cx| run(this, id.clone(), window, cx));
                    })
                };
            menu.item(item("アイコンを変更", |this, id, window, cx| {
                // 開いたらすぐ打って探せるよう、検索欄を空にしてフォーカスする
                this.inputs.icon_search.update(cx, |state, cx| {
                    state.set_value("", window, cx);
                    state.focus(window, cx);
                });
                this.icon_category = None;
                this.modal = Modal::WorkspaceIcon(id);
                cx.notify();
            }))
            .item(item("名前を変更", |this, id, window, cx| {
                this.rename_workspace(id, window, cx)
            }))
            .item(item(
                "サブワークスペースを追加",
                |this, id, window, cx| this.add_workspace(Some(id), window, cx),
            ))
            .item(item("設定", |this, id, window, cx| {
                this.open_workspace_settings(&id, window, cx)
            }))
            .item(item("AI のルール", |this, id, window, cx| {
                this.open_workspace_rules(&id, window, cx)
            }))
            .item(item("定時タスク", |this, id, window, cx| {
                this.open_workspace_schedules(&id, window, cx)
            }))
            .separator()
            .item(item("AI bot に添える", |this, id, window, cx| {
                this.attach_workspace_to_chat(&id, window, cx)
            }))
            .item(item("コピー", |this, id, _, cx| {
                this.copy_workspace_link(&id, cx)
            }))
            .map(|menu| {
                crate::plugins::menu_items(
                    menu,
                    &plugin_commands,
                    &view,
                    &crate::plugins::Target::Workspace(id.clone()),
                )
            })
            .separator()
            .item(item("削除", |this, id, _, cx| {
                this.modal = Modal::DeleteWorkspace(id);
                cx.notify();
            }))
        }
    }

    fn render_sidebar(&self, wide: bool, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut tree = col()
            .ml_5()
            .pl_2()
            .border_l_1()
            .border_color(theme.sidebar_border)
            .gap_0p5();
        self.ws_rows.begin();
        let ws_over = self
            .ws_drag
            .as_ref()
            .filter(|d| d.moved)
            .and_then(|d| d.over.clone());
        let ws_dragged = self
            .ws_drag
            .as_ref()
            .filter(|d| d.moved)
            .map(|d| d.id.clone());
        for (ix, node) in self.tree.iter().enumerate() {
            let ws = &self.workspaces[node.index];
            let id = ws.id.clone();
            let collapsed = self.collapsed.contains(&ws.id);
            // ドラッグ中のワークスペースを落とすと入るところ
            let line = match &ws_over {
                Some(WsSpot::Row(over, zone)) if *over == ws.id => match zone {
                    WsZone::Before => Some((true, node.depth)),
                    WsZone::After => Some((
                        false,
                        node.depth
                            + usize::from(crate::ws_drag::expanded(
                                &self.workspaces,
                                &self.collapsed,
                                &ws.id,
                            )),
                    )),
                    WsZone::Into => None,
                },
                _ => None,
            };
            let nest = ws_over == Some(WsSpot::Row(ws.id.clone(), WsZone::Into));
            let lifted = ws_dragged.as_ref() == Some(&ws.id);
            let caret = if !node.has_children {
                div().w_4().flex_none().into_any_element()
            } else {
                let id = id.clone();
                // 矢印は小さいまま、押せる範囲は行の高さいっぱいで、左へも広げる
                div()
                    .id(("caret", ix))
                    .w(px(22.))
                    .ml(px(-8.))
                    .self_stretch()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(feel::md())
                    .cursor_pointer()
                    .text_color(theme.muted_foreground)
                    .lit(theme.muted, cx)
                    .sunk(theme.primary.opacity(0.4), cx)
                    .child(
                        app_icon(if collapsed {
                            "chevron-right"
                        } else {
                            "chevron-down"
                        })
                        .xsmall(),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.toggle_collapsed(&id, cx);
                    }))
                    .test_support()
                    .into_any_element()
            };
            let group = SharedString::from(format!("ws-row-{ix}"));
            let active = self.nav == Nav::Workspace(id.clone());
            let naming = self
                .workspace_naming
                .as_ref()
                .is_some_and(|naming| naming.id == id);
            let menu = self.workspace_menu(id.clone(), cx);
            // 「…」は、行に乗せたときと、開いているワークスペースでだけ見せる
            let more = button(("ws-more", ix))
                .icon(app_icon("ellipsis"))
                .ghost()
                .small()
                .when(!active, |b| {
                    b.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.))
                })
                .pop_menu({
                    let menu = menu.clone();
                    move |m, _, _| menu(m)
                });
            let landed = self
                .ws_landed
                .as_ref()
                .filter(|(landed, _)| self.motion && *landed == ws.id)
                .map(|(_, turn)| *turn);
            let drag_id = id.clone();
            let row_el = self
                .drop_zone(
                    row().id(("ws-row", ix)).group(group),
                    DropTarget::Workspace(Some(ws.id.clone())),
                    cx,
                )
                .pl(px(12.) * node.depth as f32)
                .child(self.ws_rows.mark(ws.id.clone()))
                .when(lifted, |d| d.opacity(0.4))
                .when(nest, |d| {
                    d.rounded(feel::md()).bg(theme.primary.opacity(0.25)).child(
                        div()
                            .id(("ws-nest", ix))
                            .absolute()
                            .inset_0()
                            .rounded(feel::md())
                            .border_1()
                            .border_color(theme.primary)
                            .test_support(),
                    )
                })
                .when_some(line, |d, (above, depth)| {
                    let bar = div()
                        .id(("ws-line", ix))
                        .absolute()
                        .left(px(12.) * depth as f32)
                        .right_0()
                        .h(px(2.))
                        .rounded_full()
                        .bg(theme.primary)
                        .when(above, |d| d.top(px(-2.)))
                        .when(!above, |d| d.bottom(px(-2.)))
                        .test_support();
                    d.child(if self.motion {
                        bar.with_animation(
                            ("ws-line-in", ix * 2 + usize::from(above)),
                            appear(140),
                            |d, t| d.opacity(t),
                        )
                        .into_any_element()
                    } else {
                        bar.into_any_element()
                    })
                })
                .child(caret)
                .child(if naming {
                    // その場で名前を書く。文字の大きさも位置も、読むときと同じ。
                    // Enter かほかの場所を押すと保存、Esc で取り消し
                    row()
                        .flex_1()
                        .min_w_0()
                        .gap_2()
                        .px_3()
                        // 入力欄の高さに足して、読むときの行と同じ高さにする
                        .py(px(3.2))
                        .rounded(feel::md())
                        .text_sm()
                        .bg(theme.accent)
                        .text_color(theme.accent_foreground)
                        .on_mouse_down_out(
                            cx.listener(|this, _, window, cx| this.save_workspace_name(window, cx)),
                        )
                        .child(workspace_icon(
                            ws.icon.as_deref(),
                            ws.color.as_deref(),
                            NAV_ICON_SIZE,
                            cx,
                        ))
                        .child(
                            div().flex_1().min_w_0().mx(px(-8.)).child(
                                Input::new(&self.inputs.workspace_name)
                                    .id("workspace-name-input")
                                    .aria_label("ワークスペース名")
                                    .text_sm()
                                    .bg(theme.background)
                                    .border_color(theme.ring)
                                    .px_2(),
                            ),
                        )
                        .into_any_element()
                } else {
                    let nav = Nav::Workspace(id);
                    // 押したまま動かすと、ワークスペースを並べ替えたり入れ子にしたりできる
                    self.nav_link(
                        ("ws", ix),
                        workspace_icon(ws.icon.as_deref(), ws.color.as_deref(), NAV_ICON_SIZE, cx),
                        ws.name.clone(),
                        self.shown_nav(cx) == nav,
                        cx,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.start_ws_drag(&drag_id, event.position, cx)
                        }),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        // ドラッグの終わりは、クリックにしない
                        if this.dragging_ws() {
                            return;
                        }
                        this.go(nav.clone(), cx)
                    }))
                    .test_support()
                    .into_any_element()
                })
                .child(more)
                // 右クリックでも同じメニューを出す
                .right_menu(move |m, _, _| menu(m));
            tree = tree.child(match landed {
                // 置いたばかりの行を、少しの間だけ光らせる
                Some(turn) => row_el
                    .with_animation(
                        ("ws-landed", turn),
                        Animation::new(std::time::Duration::from_millis(700))
                            .with_easing(ease_out_quint()),
                        {
                            let glow = theme.primary;
                            move |d, t| d.rounded(feel::md()).bg(glow.opacity(0.3 * (1. - t)))
                        },
                    )
                    .into_any_element(),
                None => row_el.into_any_element(),
            });
        }
        // 一覧の下の空いたところ。落とすと最上位の末尾に入る
        let end_line = ws_over == Some(WsSpot::End);
        let ws_end = div()
            .id("ws-end")
            .relative()
            .flex_1()
            .min_h(px(40.))
            .ml_5()
            .pl_2()
            .child(self.ws_rows.mark_end())
            .when(end_line, |d| {
                d.child(
                    div()
                        .id("ws-line-end")
                        .absolute()
                        .top(px(2.))
                        .left_2()
                        .right_0()
                        .h(px(2.))
                        .rounded_full()
                        .bg(theme.primary)
                        .test_support(),
                )
            });

        let unread = self.unread_notices;
        let view = cx.entity().downgrade();
        let floating = crate::theme::floating(cx);
        let half = FLOAT_GAP / 2.;
        let inner = col()
            .id("sidebar")
            .size_full()
            .relative()
            .text_color(theme.sidebar_foreground)
            // 浮かせるときも枠の幅は取っておき、中身の場所を変えない
            .border_r_1()
            .border_color(theme.transparent)
            .when(!floating, |d| {
                d.bg(glass(theme.sidebar, cx))
                    .border_color(theme.sidebar_border)
            })
            .when(floating, |d| {
                d.child(floating_card(
                    glass(theme.sidebar, cx),
                    // 上は、上のバーとの間のすき間の半分
                    [half, half, FLOAT_GAP, FLOAT_GAP],
                    cx,
                ))
            })
            .child(
                col()
                    .id("sidebar-nav")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .p_3()
                    .gap_1()
                    .child(row().child(self.nav_item(
                        "nav-home",
                        app_icon("house").size(NAV_ICON_SIZE).flex_none(),
                        "ホーム",
                        Nav::Home,
                        cx,
                    )))
                    .child(
                        row().child(
                            self.nav_link(
                                "nav-notices",
                                app_icon("bell").size(NAV_ICON_SIZE).flex_none(),
                                "通知",
                                self.modal == Modal::Notices,
                                cx,
                            )
                            .children(count_bubble(
                                "nav-notices-count",
                                unread,
                                Bubble::Nav,
                                None,
                                cx,
                            ))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.modal = Modal::Notices;
                                cx.notify();
                            }))
                            .test_support(),
                        ),
                    )
                    .child(row().child(self.nav_item(
                        "nav-all",
                        app_icon("list-todo").size(NAV_ICON_SIZE).flex_none(),
                        "すべてのタスク",
                        Nav::All,
                        cx,
                    )))
                    .child(
                        self.drop_zone(row().id("drop-unfiled"), DropTarget::Workspace(None), cx)
                            .child(self.nav_item(
                                "nav-unfiled",
                                app_icon("inbox").size(NAV_ICON_SIZE).flex_none(),
                                "未所属",
                                Nav::Unfiled,
                                cx,
                            )),
                    )
                    .child(
                        row()
                            .mt_3()
                            .px_3()
                            .gap_2()
                            .text_color(theme.muted_foreground)
                            .child(app_icon("layers").xsmall().flex_none())
                            .child(div().flex_1().text_xs().child("ワークスペース"))
                            .child(feel::gloss(
                                button("add-workspace")
                                    .icon(app_icon("plus"))
                                    .ghost()
                                    .small()
                                    .tooltip("ワークスペースを追加")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.add_workspace(None, window, cx)
                                    })),
                            )),
                    )
                    .when(!self.workspaces.is_empty(), |d| d.child(tree).child(ws_end)),
            )
            // 何もないところの右クリックで、ワークスペースを足せる
            .right_menu(move |menu, _, _| {
                let view = view.clone();
                menu.item(PopupMenuItem::new("ワークスペースを追加").on_click(
                    move |_, window, cx| {
                        let _ = view.update(cx, |this, cx| this.add_workspace(None, window, cx));
                    },
                ))
            });
        let outer = div()
            .relative()
            .w(px(self.sidebar_width))
            .flex_none()
            .h_full()
            .child(inner);
        if !wide {
            return outer;
        }
        // 右の境界をドラッグして幅を変える。ダブルクリックで最初の幅に戻す
        let resizing = self.sidebar_resizing;
        outer.child(
            div()
                .id("sidebar-resize")
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(-3.))
                .w(px(6.))
                .cursor_col_resize()
                .when(resizing, |d| d.bg(theme.ring))
                .lit(theme.ring, cx)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        if event.click_count >= 2 {
                            this.reset_sidebar_width(cx);
                        } else {
                            this.sidebar_resizing = true;
                            cx.notify();
                        }
                    }),
                )
                .test_support(),
        )
    }

    /// 畳んだ左のメニュー。アイコンだけを縦に並べ、名前は乗せたときに出す。
    fn render_rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let floating = crate::theme::floating(cx);
        let half = FLOAT_GAP / 2.;
        // アイコンは列の真ん中に置く。浮かせるときは、カードの真ん中
        let inset = if floating {
            (RAIL_WIDTH - RAIL_BUTTON - 4.) / 2. + 4.
        } else {
            (RAIL_WIDTH - RAIL_BUTTON) / 2.
        };
        let shown = self.shown_nav(cx);
        let mut nav = col()
            .id("rail-nav")
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .py_3()
            .pl(px(inset))
            .gap_1()
            .child(self.rail_item(
                "rail-home",
                app_icon("house").size(RAIL_ICON_SIZE).into_any_element(),
                "ホーム",
                Nav::Home,
                cx,
            ))
            .child({
                let unread = self.unread_notices;
                self.rail_link(
                    "rail-notices",
                    app_icon("bell").size(RAIL_ICON_SIZE).into_any_element(),
                    "通知",
                    self.modal == Modal::Notices,
                    cx,
                )
                .children(count_bubble(
                    "rail-notices-count",
                    unread,
                    Bubble::Bell,
                    Some((px(1.), px(-2.))),
                    cx,
                ))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.modal = Modal::Notices;
                    cx.notify();
                }))
                .test_support()
            })
            .child(
                self.rail_item(
                    "rail-all",
                    app_icon("list-todo")
                        .size(RAIL_ICON_SIZE)
                        .into_any_element(),
                    "すべてのタスク",
                    Nav::All,
                    cx,
                ),
            )
            .child(
                self.drop_zone(
                    div().id("rail-drop-unfiled"),
                    DropTarget::Workspace(None),
                    cx,
                )
                .child(self.rail_item(
                    "rail-unfiled",
                    app_icon("inbox").size(RAIL_ICON_SIZE).into_any_element(),
                    "未所属",
                    Nav::Unfiled,
                    cx,
                )),
            )
            .child(
                div()
                    .my_1()
                    .w(px(RAIL_BUTTON))
                    .h(gpui_kit::px(1.))
                    .bg(theme.sidebar_border),
            )
            .child(
                div()
                    .w(px(RAIL_BUTTON))
                    .flex()
                    .justify_center()
                    .child(feel::gloss(
                        button("rail-add-workspace")
                            .icon(app_icon("plus"))
                            .ghost()
                            .small()
                            .tooltip("ワークスペースを追加")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.add_workspace(None, window, cx)
                            })),
                    )),
            );
        for (ix, node) in self.tree.iter().enumerate() {
            let ws = &self.workspaces[node.index];
            let nav_to = Nav::Workspace(ws.id.clone());
            let active = shown == nav_to;
            // サブワークスペースは少し小さく出して、上のワークスペースと見分ける
            let size = if node.depth == 0 {
                RAIL_ICON_SIZE
            } else {
                NAV_ICON_SIZE
            };
            let menu = self.workspace_menu(ws.id.clone(), cx);
            nav = nav.child(
                self.drop_zone(
                    div().id(("rail-drop-ws", ix)),
                    DropTarget::Workspace(Some(ws.id.clone())),
                    cx,
                )
                .child(
                    self.rail_link(
                        ("rail-ws", ix),
                        workspace_icon(ws.icon.as_deref(), ws.color.as_deref(), size, cx),
                        ws.name.clone(),
                        active,
                        cx,
                    )
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, _, cx| this.go(nav_to.clone(), cx)),
                    )
                    .test_support(),
                )
                .right_menu(move |m, _, _| menu(m)),
            );
        }
        let view = cx.entity().downgrade();
        col()
            .id("rail")
            .size_full()
            .relative()
            .text_color(theme.sidebar_foreground)
            .border_r_1()
            .border_color(theme.transparent)
            .when(!floating, |d| {
                d.bg(glass(theme.sidebar, cx))
                    .border_color(theme.sidebar_border)
            })
            .when(floating, |d| {
                d.child(floating_card(
                    glass(theme.sidebar, cx),
                    [half, half, FLOAT_GAP, FLOAT_GAP],
                    cx,
                ))
            })
            .child(nav)
            .test_support()
            // 何もないところの右クリックで、ワークスペースを足せる（広げたときと同じ）
            .right_menu(move |menu, _, _| {
                let view = view.clone();
                menu.item(PopupMenuItem::new("ワークスペースを追加").on_click(
                    move |_, window, cx| {
                        let _ = view.update(cx, |this, cx| this.add_workspace(None, window, cx));
                    },
                ))
            })
            .into_any_element()
    }

    /// アイコンだけの列の 1 つ。選んでいるものは色を付け、乗せると名前が出る。
    fn rail_link(
        &self,
        id: impl Into<ElementId>,
        icon: AnyElement,
        label: impl Into<SharedString>,
        active: bool,
        cx: &App,
    ) -> Stateful<Div> {
        let theme = cx.theme();
        let label: SharedString = label.into();
        div()
            .id(id)
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(RAIL_BUTTON))
            .rounded(feel::md())
            .cursor_pointer()
            .when(active, |d| {
                feel::selected(d, theme.accent, cx).text_color(theme.accent_foreground)
            })
            .when(!active, |d| d.lit(theme.accent.opacity(0.6), cx))
            .active(|s| s.bg(theme.accent).opacity(0.8))
            .aria_label(label.clone())
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(label.clone()).build(window, cx)
            })
            .child(icon)
    }

    fn rail_item(
        &self,
        id: impl Into<ElementId>,
        icon: AnyElement,
        label: &'static str,
        nav: Nav,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.rail_link(id, icon, label, self.shown_nav(cx) == nav, cx)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.go(nav.clone(), cx)))
            .test_support()
            .into_any_element()
    }

    /// 広いウィンドウの左の列。開いているときはメニュー、畳んだときはアイコンだけの列。
    /// 開け閉めのときは、幅を動かして切り替える。
    fn render_side(&self, cx: &mut Context<Self>) -> AnyElement {
        let rail = self.sidebar_hidden;
        let (from, to) = if rail {
            (self.sidebar_width, RAIL_WIDTH)
        } else {
            (RAIL_WIDTH, self.sidebar_width)
        };
        let side = div().flex_none().h_full().w(px(to)).child(if rail {
            self.render_rail(cx)
        } else {
            self.render_sidebar(true, cx).into_any_element()
        });
        if !self.motion || self.sidebar_turns == 0 {
            return side.into_any_element();
        }
        side.with_animation(
            ("sidebar-turn", self.sidebar_turns),
            appear(220),
            move |d, t| {
                let d = d.w(px(from + (to - from) * t));
                // 動いている間だけ、はみ出した所を切る（止まったら境界のつまみを見せる）
                if t < 1. {
                    d.overflow_hidden()
                } else {
                    d
                }
            },
        )
        .into_any_element()
    }

    // ---- 上のバー ----

    /// 今の画面の名前と、名前の左に出すアイコン。
    fn header_title(&self, cx: &App) -> (SharedString, Option<AnyElement>) {
        match &self.nav {
            Nav::Home => ("ホーム".into(), None),
            Nav::All => ("すべてのタスク".into(), None),
            Nav::Unfiled => ("未所属のタスク".into(), None),
            Nav::Workspace(id) => {
                let ws = self.workspaces.iter().find(|w| &w.id == id);
                (
                    self.list_title(),
                    ws.map(|w| {
                        workspace_icon(w.icon.as_deref(), w.color.as_deref(), TILE_ICON_SIZE, cx)
                    }),
                )
            }
        }
    }

    fn theme_toggle(&self, cx: &mut Context<Self>) -> Div {
        use crate::theme::Choice;
        let theme = cx.theme();
        let current = crate::theme::choice(cx);
        // 枠は付けず、選んでいるものだけに面を敷く
        let mut toggle = row().flex_none().gap_0p5();
        for (ix, (choice, icon)) in [
            (Choice::Light, "sun"),
            (Choice::Dark, "moon"),
            (Choice::System, "monitor"),
        ]
        .into_iter()
        .enumerate()
        {
            let active = choice == current;
            toggle = toggle.child(
                div()
                    .rounded(feel::md())
                    .when(active, |d| d.bg(theme.secondary_hover))
                    .child(feel::gloss(
                        button(("theme", ix))
                            .icon(app_icon(icon))
                            .ghost()
                            .small()
                            .tooltip(choice.label())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Err(error) = crate::theme::set(choice, window, cx) {
                                    this.error = Some(
                                        format!("配色の選択を保存できませんでした。{error}").into(),
                                    );
                                }
                                // ライトとダークで敷く画像が変わる。読めなかったら知らせる
                                if let Some(error) = crate::theme::take_failure(cx) {
                                    this.error = Some(error.into());
                                }
                                cx.notify();
                            })),
                    )),
            );
        }
        toggle
    }

    /// 上のバー。右クリックで、ウィンドウのメニュー（ロック・最小化・閉じるなど）が開く。
    fn render_header(
        &self,
        unlocked: bool,
        wide: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity().downgrade();
        self.render_header_bar(unlocked, wide, window, cx)
            .debug_selector(|| "window-header".into())
            .right_menu(move |menu, window, cx| window_menu(menu, Some(view.clone()), window, cx))
    }

    fn render_header_bar(
        &self,
        unlocked: bool,
        wide: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme().clone();
        // バーの空いているところを掴むと、ウィンドウを動かせる。
        // Windows は OS に任せ、ほかは押したまま動かしたときに自分で動かし始める
        let floating = crate::theme::floating(cx);
        let half = FLOAT_GAP / 2.;
        // 中身はどれも、この行の中で縦の中央にそろう（flex）。
        // 浮かせるときは、カードの内側に収まるように、まわりのすき間の分だけ内側に寄せる
        let bar = row()
            .flex_none()
            .h(HEADER_HEIGHT)
            .relative()
            .border_b_1()
            .border_color(theme.transparent)
            // 空いているところに乗せるとうっすら、つまんでいる間は少し濃くなる
            .when(!floating, |d| {
                d.border_color(theme.border)
                    .bg(glass(theme.background, cx))
                    .grip(cx)
            })
            .when(floating, |d| {
                d.pt(FLOAT_GAP).pb(half).px(FLOAT_GAP).child(
                    floating_card(
                        glass(theme.popover, cx),
                        [FLOAT_GAP, FLOAT_GAP, half, FLOAT_GAP],
                        cx,
                    )
                    .grip(cx),
                )
            })
            .when(cfg!(target_os = "windows"), |d| {
                d.window_control_area(WindowControlArea::Drag)
            })
            .when(!cfg!(target_os = "windows"), |d| {
                d.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, window, cx| {
                        // ボタンが受けた押下では動かさない
                        if window.default_prevented() {
                            return;
                        }
                        if event.click_count == 2 {
                            if cfg!(target_os = "macos") {
                                window.titlebar_double_click();
                            } else {
                                window.zoom_window();
                            }
                            return;
                        }
                        this.header_pressed = true;
                        cx.notify();
                    }),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, _| this.header_pressed = false),
                )
                .on_mouse_move(cx.listener(
                    |this, event: &MouseMoveEvent, window, _| {
                        if !this.header_pressed {
                            return;
                        }
                        this.header_pressed = false;
                        if event.pressed_button == Some(MouseButton::Left) {
                            window.start_window_move();
                        }
                    },
                ))
            });
        if !unlocked {
            return bar
                .child(
                    div()
                        .flex_1()
                        .px_4()
                        // macOS は左上に信号機ボタンが乗るので、その分を空ける
                        .when(cfg!(target_os = "macos"), |d| d.pl(px(84.)))
                        .font_weight(FontWeight::BOLD)
                        .child(crate::APP_NAME.to_uppercase()),
                )
                // 下のバーの判定に取られないよう、ボタンのある場所はマウスを受け止める
                .child(
                    row()
                        .occlude()
                        .when(floating, |d| d.pr_2())
                        .children(crate::on_top::pin(
                            gpui_kit::component::Size::Small,
                            window,
                            cx,
                        ))
                        .children(window_controls(HEADER_HEIGHT, floating, window, cx)),
                );
        }
        // 分けているときは、今のペインの場所を出す
        let (title, icon) = match self.focused_child() {
            Some(child) => child.read(cx).header_title(cx),
            None => self.header_title(cx),
        };
        let unread = self.unread_notices;
        let update_ready = crate::update::ready_version(cx);
        let controls = window_controls(HEADER_HEIGHT, floating, window, cx);
        bar.child(
            row()
                .flex_1()
                .min_w_0()
                .gap_2()
                .pl_2()
                // macOS は左上に信号機ボタンが乗るので、その分を空ける
                .when(cfg!(target_os = "macos"), |d| d.pl(px(84.)))
                .child(
                    // ボタンのある場所はマウスを受け止める（右のボタンと同じ理由）
                    div().occlude().child(feel::gloss(
                        button("toggle-sidebar")
                            .icon(app_icon("menu"))
                            .ghost()
                            .small()
                            .tooltip("メニュー")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.toggle_sidebar(wide, cx)),
                            ),
                    )),
                )
                // 狭いウィンドウでは、アプリの名前を出さず、今の画面の名前に場所を譲る
                .when(wide, |d| {
                    d.child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::BOLD)
                            .child(crate::APP_NAME.to_uppercase()),
                    )
                    .child(div().flex_none().w(gpui_kit::px(1.)).h_4().bg(theme.border))
                })
                .when_some(icon, |d, icon| d.child(div().flex_none().child(icon)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_weight(FontWeight::BOLD)
                        .truncate()
                        .child(title),
                ),
        )
        .child(
            row()
                .flex_none()
                .gap_1()
                // ウィンドウのボタンは、浮かせないときは右の端まで寄せる
                .when(floating || controls.is_none(), |d| d.pr_2())
                // 下のバーの判定に取られないよう、ボタンのある場所はマウスを受け止める。
                // これがないと、Windows ではボタンを離したときのクリックがウィンドウの移動に取られる
                .occlude()
                .child(self.theme_toggle(cx))
                .child(feel::gloss(
                    button("pane-split")
                        .icon(app_icon("columns-2"))
                        .tooltip("右に分割（Ctrl+B → v）")
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.pane_action(
                                malakt_core::panes::Action::Split(malakt_core::panes::Dir::Right),
                                window,
                                cx,
                            )
                        })),
                ))
                .child(feel::gloss_when(
                    !(self.ai_session.read(cx).busy),
                    button("ai-toggle")
                        .icon(app_icon("sparkles"))
                        // AI が動いている間は、回る印に変わる
                        .loading(self.ai_session.read(cx).busy)
                        .ghost()
                        .small()
                        .tooltip("AI bot")
                        .on_click(cx.listener(|this, _, window, cx| this.toggle_ai(window, cx))),
                ))
                // 新しい版を取ってあるときだけ出る。閉じても同じ版に入れ替わる
                .children(update_ready.map(|version| {
                    feel::gloss(
                        button("update-now")
                            .icon(app_icon("refresh-cw"))
                            .ghost()
                            .small()
                            .tooltip(format!("v{version} に更新して再起動"))
                            .on_click(cx.listener(|this, _, _, cx| this.update_now(cx))),
                    )
                }))
                .child(
                    div()
                        .relative()
                        .child(feel::gloss(
                            button("open-notices")
                                .icon(app_icon("bell"))
                                .ghost()
                                .small()
                                .tooltip("通知")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.modal = Modal::Notices;
                                    cx.notify();
                                })),
                        ))
                        .children(count_bubble(
                            "open-notices-count",
                            unread,
                            Bubble::Bell,
                            Some((px(-4.), px(-4.))),
                            cx,
                        )),
                )
                .child(feel::gloss(
                    button("open-settings")
                        .icon(app_icon("settings"))
                        .ghost()
                        .small()
                        .tooltip("設定")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_appearance(window, cx)),
                        ),
                ))
                .children(crate::on_top::pin(
                    gpui_kit::component::Size::Small,
                    window,
                    cx,
                ))
                .children(controls),
        )
    }

    // ---- ホーム ----

    fn render_home(&self, roomy: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let due = self.due.clone();
        let count = due.len();
        let roots: Vec<_> = self
            .workspaces
            .iter()
            .filter(|w| w.parent_id.is_none())
            .cloned()
            .collect();
        let heading =
            |text: &'static str| div().text_lg().font_weight(FontWeight::BOLD).child(text);
        col()
            .id("home")
            .size_full()
            .overflow_y_scrollbar()
            .p_6()
            .gap_3()
            .child(
                row()
                    .gap_3()
                    .child(heading("期限が近いタスク"))
                    .child(div().flex_1())
                    .child(
                        Checkbox::new("due-mine")
                            // 見た目はそのままで、押せる範囲を上下に広げる
                            .py(px(4.))
                            .my(px(-4.))
                            .label("自分の担当だけ")
                            .checked(self.due_mine)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.due_mine = *checked;
                                this.reload(cx);
                            })),
                    ),
            )
            .child(if count == 0 {
                div()
                    .p_4()
                    .rounded(feel::lg())
                    .border_1()
                    .border_dashed()
                    .border_color(theme.border)
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(if self.due_mine {
                        "自分が担当の、期限が近いタスクはありません。"
                    } else {
                        "期限が近いタスクはありません。"
                    })
            } else {
                col()
                    .h(task_height(true, crate::theme::density(cx)) * count.min(8) as f32 + px(2.))
                    .flex_none()
                    .border_1()
                    .border_color(theme.border)
                    .rounded(feel::lg())
                    .bg(glass(theme.popover, cx))
                    .overflow_hidden()
                    .relative()
                    .child(
                        uniform_list(
                            "due-list",
                            count,
                            cx.processor(move |this, range: Range<usize>, _, cx| {
                                range
                                    .map(|ix| this.render_due_row(ix, &due[ix], roomy, cx))
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&self.due_scroll)
                        .size_full(),
                    )
                    .vertical_scrollbar(&self.due_scroll)
            })
            .child(div().mt_6().child(heading("ワークスペース")))
            .child(row().flex_wrap().items_start().gap_3().children(
                roots.into_iter().enumerate().map(|(ix, ws)| {
                    let id = ws.id.clone();
                    row()
                        .id(("home-ws", ix))
                        .elevate(Elevation::Low, cx)
                        .w(px(248.))
                        .p_2()
                        .pr_3()
                        .gap_2p5()
                        .items_center()
                        .border_1()
                        .border_color(theme.border)
                        .rounded(feel::lg())
                        .bg(glass(theme.popover, cx))
                        .cursor_pointer()
                        .hover(|s| s.border_color(theme.primary))
                        .sunk(theme.accent, cx)
                        .child(
                            div()
                                .size(px(44.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(feel::md())
                                .bg(theme.muted)
                                .child(workspace_icon(
                                    ws.icon.as_deref(),
                                    ws.color.as_deref(),
                                    CARD_ICON_SIZE,
                                    cx,
                                )),
                        )
                        .child(
                            col()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .font_weight(FontWeight::BOLD)
                                        .truncate()
                                        .child(ws.name.clone()),
                                )
                                .when_some(ws.date.clone(), |d, date| {
                                    d.child(
                                        div()
                                            .text_sm()
                                            .text_color(theme.muted_foreground)
                                            .child(view::format_date(&date, &self.today)),
                                    )
                                }),
                        )
                        .child(
                            app_icon("chevron-right")
                                .small()
                                .text_color(theme.muted_foreground),
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.go(Nav::Workspace(id.clone()), cx)
                        }))
                }),
            ))
    }

    fn render_due_row(
        &self,
        ix: usize,
        task: &Todo,
        roomy: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let id = task.id.clone();
        row()
            .id(("due", ix))
            .w_full()
            .h(task_height(true, crate::theme::density(cx)))
            .px_3()
            .gap_3()
            .border_b_1()
            .border_color(theme.border)
            .cursor_pointer()
            .lit(theme.list_hover, cx)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(task.title.clone()),
            )
            // 狭いウィンドウでは、タイトルが読めるように場所と担当を省く
            .when(roomy, |d| {
                d.child(
                    div()
                        .w(px(140.))
                        .truncate()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(self.workspace_name(task.workspace_id.as_deref())),
                )
            })
            .child({
                let (view, format) = (cx.entity().downgrade(), crate::theme::due_format(cx));
                div()
                    .id(("home-due", ix))
                    .flex_none()
                    .child(self.due_label(task, cx).w(px(170.)).truncate().text_sm())
                    .test_support()
                    // 右クリックで、期限の見せ方を選ぶ
                    .right_menu(move |menu, _, _| {
                        crate::state::due_format_items(menu, &view, format)
                    })
            })
            .child(status_badge(task.status, cx).w(px(76.)))
            .when(roomy, |d| {
                d.child(
                    div()
                        .w(px(120.))
                        .truncate()
                        .text_sm()
                        .child(self.assignee_label(&task.assignee_ids)),
                )
            })
            .on_click(
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_panel(&id, window, cx)
                }),
            )
            .test_support()
            .into_any_element()
    }

    // ---- 一覧 ----

    pub(crate) fn list_title(&self) -> SharedString {
        match &self.nav {
            Nav::Home => "ホーム".into(),
            Nav::All => "すべてのタスク".into(),
            Nav::Unfiled => "未所属のタスク".into(),
            // 上の段への道筋は、一覧の上のパンくずに出す
            Nav::Workspace(id) => self
                .workspaces
                .iter()
                .find(|w| &w.id == id)
                .map(|w| SharedString::from(w.name.clone()))
                .unwrap_or_default(),
        }
    }

    /// 選択肢から 1 つを選ぶ欄。押すとメニューが開く。
    fn select<T: Clone + 'static>(
        &self,
        id: &'static str,
        label: &'static str,
        options: Vec<(SharedString, bool, T)>,
        apply: fn(&mut MainView, T),
        cx: &mut Context<Self>,
    ) -> Div {
        self.select_with(id, label, options, apply, false, cx)
    }

    /// 絞り込みの選択。最初の選択肢（すべて）以外を選んでいるとき、右の × で外せる。
    fn filter_select<T: Clone + 'static>(
        &self,
        id: &'static str,
        label: &'static str,
        options: Vec<(SharedString, bool, T)>,
        apply: fn(&mut MainView, T),
        cx: &mut Context<Self>,
    ) -> Div {
        self.select_with(id, label, options, apply, true, cx)
    }

    fn select_with<T: Clone + 'static>(
        &self,
        id: &'static str,
        label: &'static str,
        options: Vec<(SharedString, bool, T)>,
        apply: fn(&mut MainView, T),
        clearable: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        // 最初の選択肢が「すべて」。それを選んでいないときが、絞り込みが効いているとき
        let reset = clearable
            .then(|| options.first().map(|o| (!o.1, o.2.clone())))
            .flatten();
        let current = options
            .iter()
            .find(|o| o.1)
            .map(|o| o.0.clone())
            .unwrap_or_default();
        let view = cx.entity().downgrade();
        let button = button(id)
            .framed(cx)
            .w_full()
            .child(
                row()
                    .w_full()
                    .gap_1()
                    .child(div().flex_1().min_w_0().truncate().child(current))
                    .child(app_icon("chevron-down").xsmall().flex_none()),
            )
            .pop_menu(move |mut menu, _, _| {
                for (label, checked, value) in options.iter().cloned() {
                    let view = view.clone();
                    menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                        move |_, _, cx| {
                            // ウィンドウを閉じたあとなら、反映する先がない
                            let _ = view.update(cx, |this, cx| {
                                apply(this, value.clone());
                                this.refilter();
                                cx.notify();
                            });
                        },
                    ));
                }
                menu
            });
        let control = row()
            .w_full()
            .gap_1()
            .child(div().flex_1().min_w_0().child(button));
        col()
            .flex_1()
            .min_w(px(110.))
            .gap_1()
            .child(field_label(label, cx))
            .child(match reset {
                Some((active, value)) => control.child(clear_filter(
                    SharedString::from(format!("{id}-clear")),
                    active,
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        apply(this, value.clone());
                        this.refilter();
                        cx.notify();
                    }),
                )),
                None => control,
            })
    }

    fn render_filters(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let q = &self.query;
        let mut status = vec![("すべて".into(), q.status.is_none(), None)];
        for s in Status::ALL {
            status.push((s.label().into(), q.status == Some(s), Some(s)));
        }
        let mut assignee = vec![
            (
                SharedString::from("すべて"),
                q.assignee == AssigneeFilter::All,
                AssigneeFilter::All,
            ),
            (
                "自分".into(),
                q.assignee == AssigneeFilter::Me,
                AssigneeFilter::Me,
            ),
            (
                "未割り当て".into(),
                q.assignee == AssigneeFilter::Unassigned,
                AssigneeFilter::Unassigned,
            ),
        ];
        for p in self.people_here() {
            let value = AssigneeFilter::Person(p.id.clone());
            assignee.push((p.name.clone().into(), q.assignee == value, value));
        }
        let mut my_role = vec![(SharedString::from("すべて"), q.my_role.is_none(), None)];
        for role in Raci::ALL {
            my_role.push((
                format!("{}（{}）", role.code(), role.label()).into(),
                q.my_role == Some(role),
                Some(role),
            ));
        }
        let mut priority = vec![(SharedString::from("すべて"), q.priority.is_none(), None)];
        for p in [Prio::High, Prio::Mid, Prio::Low, Prio::None] {
            priority.push((p.label().into(), q.priority == Some(p), Some(p)));
        }
        let mut category = vec![(SharedString::from("すべて"), q.category.is_none(), None)];
        for c in &self.categories {
            category.push((
                c.clone().into(),
                q.category.as_ref() == Some(c),
                Some(c.clone()),
            ));
        }
        let mut time = vec![(SharedString::from("すべて"), q.time.is_none(), None)];
        for f in view::TimeFilter::ALL {
            time.push((f.label().into(), q.time == Some(f), Some(f)));
        }
        let sort = Sort::ALL
            .into_iter()
            .map(|s| (SharedString::from(s.label()), q.sort == s, s))
            .collect();
        row()
            .flex_none()
            .flex_wrap()
            .items_end()
            .gap_3()
            .p_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                col()
                    .flex_1()
                    .min_w(px(180.))
                    .gap_1()
                    .child(field_label("検索", cx))
                    .child(
                        row()
                            .w_full()
                            .gap_1()
                            .child(
                                div().flex_1().min_w_0().child(
                                    Input::new(&self.inputs.search)
                                        .id("task-search")
                                        .aria_label("検索")
                                        .prefix(app_icon("search").small()),
                                ),
                            )
                            .child(clear_filter(
                                "task-search-clear",
                                !q.q.trim().is_empty(),
                                cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.query.q.clear();
                                    this.inputs
                                        .search
                                        .update(cx, |s, cx| s.set_value("", window, cx));
                                    this.refilter();
                                    cx.notify();
                                }),
                            )),
                    ),
            )
            .child(self.filter_select(
                "f-status",
                "状態",
                status,
                |this, v| this.query.status = v,
                cx,
            ))
            .child(self.filter_select(
                "f-assignee",
                "担当",
                assignee,
                |this, v| this.query.assignee = v,
                cx,
            ))
            .child(self.filter_select(
                "f-role",
                "自分の役",
                my_role,
                |this, v| this.query.my_role = v,
                cx,
            ))
            .child(self.filter_select(
                "f-priority",
                "優先度",
                priority,
                |this, v| this.query.priority = v,
                cx,
            ))
            .child(self.filter_select(
                "f-category",
                "カテゴリ",
                category,
                |this, v| this.query.category = v,
                cx,
            ))
            .child(self.filter_select(
                "f-time",
                "予想工数",
                time,
                |this, v| this.query.time = v,
                cx,
            ))
            .child(self.select("f-sort", "並び順", sort, |this, v| this.query.sort = v, cx))
    }

    fn render_bulk_bar(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let count = self.selected.len();
        let mut bar = row()
            .gap_2()
            .flex_wrap()
            .p_2()
            .rounded(feel::md())
            .bg(theme.secondary)
            .child(div().text_sm().child(format!("{count} 件を選択中")))
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("状態"),
            );
        for (i, s) in Status::ALL.into_iter().enumerate() {
            bar = bar.child(
                chip(("bulk-status", i), s.label(), false, cx).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        this.bulk_apply(&BulkOp::Status(s), cx);
                    },
                )),
            );
        }
        bar = bar.child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("優先度"),
        );
        for (i, p) in Prio::ALL.into_iter().enumerate() {
            bar = bar.child(
                chip(("bulk-prio", i), p.label(), false, cx).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        this.bulk_apply(&BulkOp::Priority(p.value()), cx);
                    },
                )),
            );
        }
        for (id, label, modal) in [
            ("bulk-due", "期限", Modal::BulkDue),
            ("bulk-assignees", "担当", Modal::BulkAssignees),
            ("bulk-categories", "カテゴリ", Modal::BulkCategories),
        ] {
            bar = bar.child(feel::gloss(button(id).label(label).small().on_click(
                cx.listener(move |this, _, window, cx| this.open_bulk(modal.clone(), window, cx)),
            )));
        }
        bar.child(div().flex_1())
            .child(feel::gloss(
                button("bulk-move")
                    .label("移動")
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.modal = Modal::MoveTasks(this.selected_ids());
                        cx.notify();
                    })),
            ))
            .child(feel::gloss(
                button("bulk-delete")
                    .label("削除")
                    .danger()
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.modal = Modal::DeleteTasks(this.selected_ids());
                        cx.notify();
                    })),
            ))
            .child(feel::gloss(
                button("bulk-clear")
                    .label("選択を解除")
                    .ghost()
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.selected.clear();
                        cx.notify();
                    })),
            ))
    }

    fn render_list(&self, table: bool, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let active = self.query.active_count();
        let tab = |id: &'static str,
                   icon: &'static str,
                   label: &'static str,
                   count: usize,
                   tab: Tab,
                   this: &Self,
                   cx: &mut Context<Self>| {
            let on = this.query.tab == tab;
            row()
                .id(id)
                .gap_1p5()
                .px_3()
                .py_1p5()
                .rounded(feel::md())
                .text_sm()
                .cursor_pointer()
                .when(on, |d| d.bg(theme.background).font_weight(FontWeight::BOLD))
                .when(!on, |d| {
                    d.text_color(theme.muted_foreground)
                        .hover(|s| s.text_color(theme.foreground))
                })
                .active(|s| s.bg(theme.background).opacity(0.7))
                .child(app_icon(icon).small())
                .child(label)
                .child(
                    div()
                        .font_weight(FontWeight::NORMAL)
                        .text_color(theme.muted_foreground)
                        .child(count.to_string()),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.query.tab = tab;
                    this.query.status = None;
                    this.refilter();
                    cx.notify();
                }))
                .test_support()
        };
        let visible = self.rows.len();
        let workspace_id = self.current_workspace().map(str::to_owned);
        let list_target = match &self.nav {
            Nav::Workspace(id) => Some(DropTarget::Workspace(Some(id.clone()))),
            Nav::Unfiled => Some(DropTarget::Workspace(None)),
            _ => None,
        };

        let mut header = row().flex_none().gap_2().flex_wrap();
        let date = workspace_id.as_ref().and_then(|id| {
            self.workspaces
                .iter()
                .find(|w| &w.id == id)
                .and_then(|w| w.date.clone())
        });
        // パンくずは、狭いときは名前を省いて縮む（右のボタンを押し出さない）
        let crumbs = self.render_crumbs(cx);
        let has_crumbs = crumbs.is_some();
        header = header
            .children(crumbs.map(|c| {
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .overflow_hidden()
                    .text_sm()
                    .child(c)
            }))
            .child(
                // 基準日が入らない幅では、右のボタンを次の行へ送る
                row()
                    .when(!has_crumbs, |d| d.flex_1())
                    .flex_shrink_0()
                    .whitespace_nowrap()
                    .gap_2()
                    .text_sm()
                    .when_some(date, |d, date| {
                        d.child(div().text_color(theme.muted_foreground).child("基準日"))
                            .child(view::format_date(&date, &self.today))
                    }),
            );
        if let Some(id) = workspace_id.clone() {
            let (c, d) = (id.clone(), id.clone());
            header = header
                .child(feel::gloss(
                    button("ws-settings")
                        .icon(app_icon("sliders-horizontal"))
                        .tooltip("タスク追加時の既定値")
                        .framed(cx)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_workspace_settings(&d, window, cx)
                        })),
                ))
                .child(feel::gloss(
                    button("ws-export")
                        .icon(app_icon("download"))
                        .tooltip("エクスポート")
                        .framed(cx)
                        .on_click(cx.listener(|this, _, window, cx| this.export(window, cx))),
                ))
                .child(feel::gloss(
                    button("ws-import")
                        .icon(app_icon("upload"))
                        .tooltip("インポート")
                        .framed(cx)
                        .on_click(cx.listener(|this, _, window, cx| this.import(window, cx))),
                ))
                .child(feel::gloss(
                    button("ws-delete")
                        .icon(app_icon("trash"))
                        .tooltip("削除")
                        .framed(cx)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.modal = Modal::DeleteWorkspace(c.clone());
                            cx.notify();
                        })),
                ));
        }
        header = header.child(self.stats_button(cx)).child(feel::gloss(
            button("new-task-open")
                .icon(app_icon("plus"))
                .label("新しいタスク")
                .primary()
                .elevate(Elevation::Low, cx)
                .on_click(cx.listener(|this, _, window, cx| this.add_task(window, cx))),
        ));

        let rows = if visible == 0 {
            col()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .py_8()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(if self.tasks.is_empty() {
                    "まだタスクがありません"
                } else if active > 0 {
                    "条件に合うタスクはありません。"
                } else if self.query.tab == Tab::Open {
                    "未完了のタスクはありません。"
                } else {
                    "完了したタスクはありません。"
                })
                .when(active > 0, |d| {
                    d.child(feel::gloss(
                        button("clear-filters")
                            .label("絞り込みを解除")
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.query.clear_filters();
                                this.inputs
                                    .search
                                    .update(cx, |s, cx| s.set_value("", window, cx));
                                this.refilter();
                                cx.notify();
                            })),
                    ))
                })
        } else {
            let (sizes, height) = self.row_sizes[usize::from(!table)].clone();
            // 件数が少ないときは、表を中身の高さで終える
            col()
                .relative()
                .h(height)
                // 低いペインでも、1 行は見えるようにする。入りきらない分は画面ごとスクロールする
                .min_h(height.min(task_height(table, self.row_density)))
                .child(
                    gpui_kit::base::v_virtual_list(
                        cx.entity(),
                        "task-list",
                        sizes,
                        move |this, range: Range<usize>, _, cx| {
                            range
                                .map(|at| match this.rows[at] {
                                    ListRow::Task { index, depth } => {
                                        this.render_task_row(index, depth, table, cx)
                                    }
                                    ListRow::Folder {
                                        workspace,
                                        depth,
                                        count,
                                        open,
                                    } => this
                                        .render_folder_row(at, workspace, depth, count, open, cx),
                                })
                                .collect::<Vec<_>>()
                        },
                    )
                    .track_scroll(&self.list_scroll)
                    .flex_1(),
                )
                .vertical_scrollbar(&self.list_scroll)
        };

        let card = col()
            .min_h_0()
            .border_1()
            .border_color(theme.border)
            .rounded(feel::lg())
            .bg(glass(theme.popover, cx))
            .elevate(Elevation::Low, cx)
            .overflow_hidden()
            .when(
                list_target.is_some()
                    && self.task_drop.as_ref().and_then(|d| d.over.as_ref())
                        == list_target.as_ref(),
                |d| d.border_color(theme.primary),
            )
            .child(
                row()
                    .flex_none()
                    .gap_1()
                    .p_2()
                    .bg(theme.muted.opacity(0.5))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(tab(
                        "tab-open",
                        "circle-dot",
                        "未完了",
                        self.counts.0,
                        Tab::Open,
                        self,
                        cx,
                    ))
                    .child(tab(
                        "tab-closed",
                        "circle-check",
                        "完了",
                        self.counts.1,
                        Tab::Closed,
                        self,
                        cx,
                    )),
            )
            .child(self.render_filters(cx))
            .when(!self.selected.is_empty(), |d| {
                d.child(div().p_2().child(self.render_bulk_bar(cx)))
            })
            .when(table, |d| d.child(self.render_table_head(cx)))
            .child(rows)
            .child(self.render_quick_add_row(table, cx));

        let view = cx.entity().downgrade();
        div()
            .size_full()
            .relative()
            // ほかのペインやウィンドウから来たタスクは、一覧のどこに落としても、ここのワークスペースへ移る
            .when_some(list_target, |d, target| {
                d.child(self.drops.mark(view.clone(), Some(target), true))
            })
            // 下は、右下の「＋」が表に重ならないだけ空ける
            .child(
                div()
                    .id("list-page")
                    .size_full()
                    .track_scroll(&self.page_scroll)
                    .overflow_y_scroll()
                    .child(
                        col()
                            .size_full()
                            // 絞り込みと 1 行と追加欄が入る高さ。ペインがこれより低いときは画面ごとスクロールする
                            .min_h(px(if table { 380. } else { 520. }))
                            .p_6()
                            .pb(px(72.))
                            .gap_3()
                            .child(header)
                            .child(card),
                    )
                    // 何もないところの右クリックで、コピーしたタスクを貼り付けられる
                    .right_menu(move |menu, _, cx| menu.item(paste_item(&view, cx))),
            )
            .vertical_scrollbar(&self.page_scroll)
            // 右下の「＋」。どこまでスクロールしていても、新しいタスクを始められる
            .child(
                div()
                    .absolute()
                    .right_6()
                    .bottom(px(16.))
                    .child(feel::gloss_round(
                        button("fab-new-task")
                            .icon(app_icon("plus"))
                            .primary()
                            .large()
                            .rounded(px(999.))
                            .elevate(Elevation::High, cx)
                            .tooltip("新しいタスク")
                            .on_click(cx.listener(|this, _, window, cx| this.add_task(window, cx))),
                    )),
            )
    }

    fn render_table_head(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let all = !self.visible.is_empty() && self.selected.len() == self.visible.len();
        row()
            .h_9()
            .px_3()
            .gap_3()
            .flex_none()
            .bg(theme.muted.opacity(0.4))
            .border_b_1()
            .border_color(theme.border)
            .text_xs()
            .font_weight(FontWeight::BOLD)
            .text_color(theme.muted_foreground)
            .child(
                div().w_6().flex_none().child(
                    Checkbox::new("select-all")
                        // 見た目はそのままで、押せる範囲を上下に広げる
                        .py(px(4.))
                        .my(px(-4.))
                        .accessibility_label("表示中のタスクをすべて選択")
                        .checked(all)
                        .indeterminate(!all && !self.selected.is_empty())
                        .on_click(cx.listener(|this, _: &bool, _, cx| this.toggle_select_all(cx))),
                ),
            )
            .child(div().flex_1().min_w_0().flex().child(self.column_menu(
                "col-title",
                "タイトル",
                None,
                cx,
            )))
            .children(
                self.shown_columns()
                    .cloned()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(|c| {
                        let column = c.column;
                        let over = self.column_drag.as_ref().is_some_and(|d| {
                            d.moved && d.over == Some(column) && d.column != column
                        });
                        let dragged = self
                            .column_drag
                            .as_ref()
                            .is_some_and(|d| d.moved && d.column == column);
                        let resizing = self.column_resize.is_some_and(|(r, _, _)| r == column);
                        div()
                            .id(("col-head", column as usize))
                            .relative()
                            .w(px(c.width))
                            .h_full()
                            .flex_none()
                            .flex()
                            .items_center()
                            .when(over, |d| d.bg(theme.accent))
                            .when(dragged, |d| d.opacity(0.5))
                            // 見出しを押したまま横へ動かすと、列を並べ替える
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    this.column_drag = Some(crate::state::ColumnDrag {
                                        column,
                                        start_x: f32::from(event.position.x),
                                        moved: false,
                                        over: None,
                                    });
                                    cx.notify();
                                }),
                            )
                            .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _, cx| {
                                if let Some(drag) = &mut this.column_drag {
                                    if drag.moved && drag.over != Some(column) {
                                        drag.over = Some(column);
                                        cx.notify();
                                    }
                                }
                            }))
                            .child(self.column_menu(
                                ("col-menu", column as usize),
                                column.label(),
                                Some(column),
                                cx,
                            ))
                            .child(
                                // 左の境界をドラッグして幅を変える。ダブルクリックで最初の幅に戻す
                                div()
                                    .id(("col-resize", column as usize))
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .left(px(-9.))
                                    .w(px(6.))
                                    .cursor_col_resize()
                                    .when(resizing, |d| d.bg(theme.ring))
                                    .lit(theme.ring, cx)
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                            cx.stop_propagation();
                                            if event.click_count >= 2 {
                                                this.reset_column_width(column, cx);
                                            } else {
                                                this.start_column_resize(
                                                    column,
                                                    f32::from(event.position.x),
                                                    cx,
                                                );
                                            }
                                        }),
                                    )
                                    .test_support(),
                            )
                            .test_support()
                    }),
            )
    }

    /// 表の見出し。押すとその列で並べ替え（押すたびに昇順と降順が入れ替わる）、右クリックで
    /// 並べ替えと、列を出す・隠すメニューが開く。`column` が `None` はタイトル。
    fn column_menu(
        &self,
        id: impl Into<ElementId>,
        label: &'static str,
        column: Option<Column>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let current = self.column_sorted(column);
        let open = self
            .column_menu
            .as_ref()
            .filter(|open| open.column == column);
        let motion = self.motion && self.sort_turns > 0;
        let turns = self.sort_turns;
        div()
            .relative()
            .child(
                row()
                    .id(id)
                    .ml(px(-6.))
                    .px(px(6.))
                    .py_0p5()
                    .gap_1()
                    .rounded(feel::md())
                    .cursor_pointer()
                    .when(open.is_some(), |d| d.bg(theme.accent))
                    .when(current.is_some(), |d| d.text_color(theme.foreground))
                    .lit(theme.accent, cx)
                    .sunk(theme.primary.opacity(0.4), cx)
                    .child(label)
                    .when_some(current, |d, descending| {
                        let arrow = div().relative().child(
                            app_icon(if descending { "arrow-down" } else { "arrow-up" }).xsmall(),
                        );
                        if !motion {
                            return d.child(arrow);
                        }
                        // 向きが変わるたびに、矢印の向きの方へすっと入る
                        let from = if descending { -5. } else { 5. };
                        d.child(arrow.with_animation(
                            ("sort-arrow", turns),
                            appear(200),
                            move |d, t| d.opacity(t).top(px(from * (1. - t))),
                        ))
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        // 見出しを動かして離したときは、並べ替えない
                        if this.column_drag.as_ref().is_some_and(|d| d.moved) {
                            return;
                        }
                        if event.standard_click() {
                            this.sort_by_column(column, cx);
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.toggle_column_menu(column, window, cx);
                        }),
                    )
                    .test_support(),
            )
            .when_some(open, |d, open| {
                d.child(
                    div().absolute().top_full().left(px(-6.)).child(
                        deferred(
                            anchored()
                                .snap_to_window_with_margin(px(8.))
                                .child(div().occlude().pt_1().child(open.menu.clone())),
                        )
                        .with_priority(1),
                    ),
                )
            })
    }

    /// 表の 1 行に、今出している列の順でセルを並べる。
    fn column_cells(&self, mut cells: [Option<AnyElement>; 7]) -> Vec<Div> {
        self.shown_columns()
            .map(|c| {
                div()
                    .w(px(c.width))
                    .flex_none()
                    .min_w_0()
                    .overflow_hidden()
                    .children(cells[c.column as usize].take())
            })
            .collect()
    }

    /// 一覧の 1 件の中で、担当の欄に使える高さ。表は行の高さ、カードは 1 段目（タイトル）の下の残り。
    fn assignee_room(&self, table: bool, cx: &App) -> Pixels {
        let rem = cx.theme().font_size;
        // 値のボタンの上下の余白（py_0p5）
        let pad = rem * 0.25;
        let height = task_height(table, self.row_density) - pad;
        if table {
            height
        } else {
            // タイトルの段（チェックボックスの高さ）と、段の間（gap_1）
            height - rem * 1.5 - rem * 0.25
        }
    }

    /// 一覧での担当の表示。全員を頭文字の丸と名前で並べ、幅が足りなければ人ごとに折り返す。
    /// 折り返すのは、1 件の高さに丸ごと入る行までにする（入らない行は丸ごと隠し、丸を途中で切らない）。
    fn assignee_cell(&self, ids: &[Id], table: bool, cx: &App) -> Div {
        let theme = cx.theme();
        if ids.is_empty() {
            return div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("未割り当て");
        }
        // 1 行は頭文字の丸（size_6）の高さ、行の間は gap_y_1
        let rem = theme.font_size;
        let line = rem * 1.5;
        let gap = rem * 0.25;
        let room = self.assignee_room(table, cx);
        let lines = ((room + gap) / (line + gap)).floor().max(1.);
        let mut cell = row()
            .min_w_0()
            .max_h(line * lines + gap * (lines - 1.))
            .overflow_hidden()
            .flex_wrap()
            .gap_x_2p5()
            .gap_y_1()
            .text_sm();
        for id in ids {
            let name = self.person_name(id);
            cell = cell.child(
                row()
                    .min_w_0()
                    .gap_1p5()
                    .child(avatar(name, cx))
                    .child(div().truncate().child(name.to_owned())),
            );
        }
        cell
    }

    /// 値を押して選ぶメニュー。表の中と追加行で使う。
    fn value_menu<T: Clone + 'static>(
        &self,
        id: impl Into<ElementId>,
        shown: Div,
        options: Vec<(SharedString, bool, T)>,
        apply: impl Fn(&mut MainView, T, &mut Context<MainView>) + Clone + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity().downgrade();
        button(id)
            .ghost()
            .small()
            // 中身（折り返した担当など）の高さに合わせて伸ばす。小さいボタンの高さのままだと、はみ出た上下が切れる
            .h_auto()
            .min_h_6()
            .max_w_full()
            .py_0p5()
            .justify_start()
            .child(value_shown(shown))
            .pop_menu(move |mut menu, _, _| {
                for (label, checked, value) in options.iter().cloned() {
                    let view = view.clone();
                    let apply = apply.clone();
                    menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                        move |_, _, cx| {
                            // ウィンドウを閉じたあとなら、反映する先がない
                            let _ = view.update(cx, |this, cx| apply(this, value.clone(), cx));
                        },
                    ));
                }
                menu
            })
    }

    /// 状態を押して選ぶメニュー。選ぶとすぐ変わる。
    fn status_menu(
        &self,
        id: impl Into<ElementId>,
        task: &Id,
        current: Status,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let task = task.clone();
        button(id)
            .ghost()
            .small()
            .justify_start()
            .child(value_shown(status_badge(current, cx)))
            .pop_menu(move |menu, _, _| status_items(menu, &view, &task, current))
    }

    fn priority_options(current: Prio) -> Vec<(SharedString, bool, u8)> {
        [Prio::High, Prio::Mid, Prio::Low, Prio::None]
            .into_iter()
            .map(|p| (p.label().into(), p == current, p.value()))
            .collect()
    }

    /// そのワークスペースで選べる担当者と、もう担当になっている人。
    fn assignee_options(
        &self,
        workspace: Option<&str>,
        ids: &[Id],
    ) -> Vec<(SharedString, bool, Id)> {
        self.people
            .iter()
            .filter(|p| ids.contains(&p.id) || self.person_is_in(p, workspace))
            .map(|p| (p.name.clone().into(), ids.contains(&p.id), p.id.clone()))
            .collect()
    }

    /// サブワークスペースの見出しの行。左の矢印で開く・閉じる。行を押すと、そのワークスペースへ移る。
    /// 一覧の上のパンくず（最上位 › … › 今いる所）。押すとそのワークスペースへ移り、タスクを落とすとそこへ移す。
    /// 最上位のワークスペースでは出さない（上のバーの名前と同じになる）。
    fn render_crumbs(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Nav::Workspace(id) = &self.nav else {
            return None;
        };
        let path = view::workspace_path(&self.workspaces, id);
        if path.len() < 2 {
            return None;
        }
        let theme = cx.theme().clone();
        let last = path.len() - 1;
        let mut bar = row().id("crumbs").min_w_0().gap_0p5();
        for (i, crumb) in view::crumbs(path.len(), MAX_CRUMBS).into_iter().enumerate() {
            if i > 0 {
                bar = bar.child(
                    div()
                        .flex_none()
                        .text_color(theme.muted_foreground)
                        .child(app_icon("chevron-right").small()),
                );
            }
            bar = bar.child(match crumb {
                view::Crumb::At(at) => self.render_crumb(path[at], at == last, cx),
                view::Crumb::More(hidden) => {
                    let view = cx.entity().downgrade();
                    let hidden: Vec<(Id, SharedString)> = hidden
                        .into_iter()
                        .map(|at| {
                            let w = &self.workspaces[path[at]];
                            (w.id.clone(), w.name.clone().into())
                        })
                        .collect();
                    button("crumb-more")
                        .icon(app_icon("ellipsis"))
                        .ghost()
                        .small()
                        .tooltip("上の階層")
                        .pop_menu(move |mut menu, _, _| {
                            for (id, name) in &hidden {
                                let (view, id) = (view.clone(), id.clone());
                                menu = menu.item(PopupMenuItem::new(name.clone()).on_click(
                                    move |_, _, cx| {
                                        // ウィンドウを閉じたあとなら、反映する先がない
                                        let _ = view.update(cx, |this, cx| {
                                            this.go_here(Nav::Workspace(id.clone()), cx)
                                        });
                                    },
                                ));
                            }
                            menu
                        })
                        .into_any_element()
                }
            });
        }
        Some(bar.test_support().into_any_element())
    }

    /// パンくずの 1 段。長い名前は省いて、乗せると全体を出す。今いる所は押せない。
    fn render_crumb(&self, workspace: usize, current: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let ws = &self.workspaces[workspace];
        let name: SharedString = ws.name.clone().into();
        let tip = name.clone();
        let go = ws.id.clone();
        let el = row()
            .id(("crumb", workspace))
            // 狭いときは長い名前から縮む。短い名前と今いる所は、なるべく省かない
            .min_w(rems(
                crumb_min(&ws.name, if current { 8. } else { 4. }) + CRUMB_CHROME,
            ))
            .gap_1p5()
            .px_1p5()
            .py_0p5()
            .rounded(feel::md())
            // 乗せたとき・押している間の面は、名前より奥に敷く（先に付ける）
            .when(!current, |d| {
                d.cursor_pointer()
                    .text_color(theme.muted_foreground)
                    .hover(|s| s.text_color(theme.foreground))
                    .lit(theme.accent, cx)
                    .sunk(theme.primary.opacity(0.4), cx)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        // ドラッグの終わりは、クリックにしない
                        if this.dragging_task() {
                            return;
                        }
                        this.go_here(Nav::Workspace(go.clone()), cx);
                    }))
            })
            .child(div().flex_none().child(workspace_icon(
                ws.icon.as_deref(),
                ws.color.as_deref(),
                NAV_ICON_SIZE,
                cx,
            )))
            .child(div().min_w_0().max_w(px(240.)).truncate().child(name))
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
            });
        if current {
            return el.test_support().into_any_element();
        }
        self.drop_zone(el, DropTarget::Workspace(Some(ws.id.clone())), cx)
            .test_support()
            .into_any_element()
    }

    fn render_folder_row(
        &self,
        at: usize,
        workspace: usize,
        depth: usize,
        count: usize,
        open: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let ws = &self.workspaces[workspace];
        let cursor = self.cursor.as_deref() == Some(ws.id.as_str());
        let (toggle_id, go_id, menu_id) = (ws.id.clone(), ws.id.clone(), ws.id.clone());
        let view = cx.entity().downgrade();
        let el = row()
            .id(("folder", at))
            .w_full()
            .h(FOLDER_HEIGHT * self.row_density.rows())
            .relative()
            .px_3()
            .gap_3()
            .border_b_1()
            .border_color(theme.border)
            .cursor_pointer()
            .when(cursor, |d| {
                d.bg(theme.accent.opacity(0.4)).child(
                    div()
                        .id(("folder-cursor", at))
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(3.))
                        .bg(theme.primary)
                        .test_support(),
                )
            })
            .when(!cursor, |d| d.lit(theme.muted.opacity(0.4), cx))
            .sunk(theme.accent.opacity(0.6), cx)
            // 矢印は、タスクの行のチェックボックスの列に置く
            .child(
                row()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .pl(LIST_INDENT * depth as f32)
                    .child(
                        div()
                            .id(("folder-toggle", at))
                            .flex_none()
                            .size_6()
                            .mr_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(feel::md())
                            .text_color(theme.muted_foreground)
                            .hover(|s| s.bg(theme.accent).text_color(theme.foreground))
                            .sunk(theme.primary.opacity(0.4), cx)
                            .child(
                                app_icon(if open {
                                    "chevron-down"
                                } else {
                                    "chevron-right"
                                })
                                .small(),
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.toggle_list_folder(&toggle_id, cx);
                            }))
                            .test_support(),
                    )
                    .child(div().flex_none().child(workspace_icon(
                        ws.icon.as_deref(),
                        ws.color.as_deref(),
                        NAV_ICON_SIZE,
                        cx,
                    )))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::BOLD)
                            .child(ws.name.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(count.to_string()),
                    ),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                // ドラッグの終わりは、クリックにしない
                if this.dragging_task() {
                    return;
                }
                this.go_here(Nav::Workspace(go_id.clone()), cx);
            }));
        // ドラッグ中のタスクを落とすと、このワークスペースへ移す
        self.drop_zone(el, DropTarget::Workspace(Some(ws.id.clone())), cx)
            .rounded_none()
            .test_support()
            .right_menu(move |menu, _, _| {
                let item =
                    |label: &'static str, run: fn(&mut MainView, Id, &mut Context<MainView>)| {
                        let view = view.clone();
                        let id = menu_id.clone();
                        PopupMenuItem::new(label).on_click(move |_, _, cx| {
                            // ウィンドウを閉じたあとなら、反映する先がない
                            let _ = view.update(cx, |this, cx| run(this, id.clone(), cx));
                        })
                    };
                menu.item(item("開く", |this, id, cx| {
                    this.go_here(Nav::Workspace(id), cx)
                }))
                .item(item(
                    if open { "折りたたむ" } else { "展開" },
                    |this, id, cx| this.toggle_list_folder(&id, cx),
                ))
            })
            .into_any_element()
    }

    fn render_task_row(
        &self,
        ix: usize,
        depth: usize,
        table: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let task = &self.tasks[self.visible[ix]];
        let id = task.id.clone();
        let active = self.panel.as_deref() == Some(id.as_str());
        let cursor = self.cursor.as_deref() == Some(id.as_str());
        let closed = !task.status.is_open();
        let now = malakt_core::now_ms();
        let arranging = {
            let session = self.ai_session.read(cx);
            session.arranging.contains(&task.id) || session.tracking.contains(&task.id)
        };

        let select = {
            let id = id.clone();
            div().w_6().flex_none().child(
                Checkbox::new(("select", ix))
                    // 見た目はそのままで、押せる範囲を上下に広げる
                    .py(px(4.))
                    .my(px(-4.))
                    .accessibility_label(format!("「{}」を選択", task.title))
                    .checked(self.selected.contains(&task.id))
                    .on_click(
                        // Shift を押しながらなら、前に押した行からの範囲をまとめて選ぶ
                        cx.listener(move |this, _: &bool, window, cx| {
                            this.click_select(&id, window.modifiers().shift, cx)
                        }),
                    ),
            )
        };
        let mut chips = row().gap_1().overflow_hidden();
        let mut has_chips = false;
        for category in &task.categories {
            has_chips = true;
            chips = chips.child(
                div()
                    .flex_none()
                    .px_2()
                    .rounded_full()
                    .border_1()
                    .border_color(theme.foreground.opacity(0.2))
                    .bg(theme.secondary)
                    .text_xs()
                    .child(category.clone()),
            );
        }
        if !closed && task.percent > 0 {
            has_chips = true;
            // 進んでいるタスクには、進捗の棒と数字を添える
            chips = chips.child(
                row()
                    .flex_none()
                    .gap_1()
                    .items_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(percent_bar(task.percent, px(48.), cx))
                    .child(format!("{}%", task.percent)),
            );
        }
        if !task.linked_workspace_ids.is_empty() {
            has_chips = true;
            // ほかのワークスペースにも出しているタスクには、か所の数を添える
            chips = chips.child(
                row()
                    .flex_none()
                    .gap_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(app_icon("link").xsmall())
                    .child(format!("同期 {}", task.linked_workspace_ids.len() + 1)),
            );
        }
        // 繰り返しのあるタスク（今の回）には、タイトルの横に印を添える。行の高さは変えない
        let repeating = self.repeating.contains(&task.id).then(|| {
            div()
                .id(("repeating", ix))
                .flex_none()
                .text_color(theme.muted_foreground)
                .child(app_icon("repeat").xsmall())
                .tooltip(|window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new("繰り返し").build(window, cx)
                })
        });
        let title = {
            let id = id.clone();
            col()
                .flex_1()
                .min_w_0()
                .gap_1()
                .pl(LIST_INDENT * depth as f32
                    + px(if task.parent_id.is_some() { 16. } else { 0. }))
                .child(
                    div()
                        .id(("title", ix))
                        .flex()
                        .items_center()
                        .gap_2()
                        // AI が整えている間は、タイトルの前で印が回る
                        .when(arranging, |d| {
                            d.child(
                                div()
                                    .id(("ai-working", ix))
                                    .flex_none()
                                    .child(Spinner::new().small())
                                    .test_support(),
                            )
                        })
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::BOLD)
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme.primary))
                        .active(|s| s.opacity(0.6))
                        .when(closed, |d| {
                            d.text_color(theme.muted_foreground).line_through()
                        })
                        .child(task.title.clone())
                        .children(repeating)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            // ドラッグの終わりは、クリックにしない
                            if this.dragging_task() {
                                return;
                            }
                            this.open_panel(&id, window, cx)
                        }))
                        .test_support(),
                )
                .when(has_chips, |d| d.child(chips))
        };
        let status = self.status_menu(("status", ix), &id, task.status, cx);
        let current = Prio::of(task.priority);
        let priority = {
            let id = id.clone();
            self.value_menu(
                ("prio", ix),
                priority_label(current, cx),
                Self::priority_options(current),
                move |this, value, cx| this.edit_task_by_id(&id, cx, |d| d.priority = value),
                cx,
            )
        };
        let assignees = {
            let id = id.clone();
            self.value_menu(
                ("assignee", ix),
                self.assignee_cell(&task.assignee_ids, table, cx),
                self.assignee_options(task.workspace_id.as_deref(), &task.assignee_ids),
                move |this, person: Id, cx| {
                    this.edit_task_by_id(&id, cx, |d| {
                        if let Some(i) = d.assignee_ids.iter().position(|a| a == &person) {
                            d.assignee_ids.remove(i);
                        } else {
                            d.assignee_ids.push(person);
                        }
                    })
                },
                cx,
            )
        };
        let due = {
            let format = crate::theme::due_format(cx);
            let view = cx.entity().downgrade();
            div()
                .id(("due-cell", ix))
                .max_w_full()
                .child(self.date_trigger(
                    ("due", ix),
                    crate::date_pick::DateTarget::Task {
                        id: id.clone(),
                        which: When::Due,
                        list: true,
                    },
                    // 列の幅に入らない範囲は、後ろを「…」にする
                    self.due_label(task, cx).min_w_0().truncate().text_sm(),
                    cx,
                ))
                .test_support()
                // 右クリックで、期限の見せ方を選ぶ（行のメニューは開かない）
                .right_menu(move |menu, _, _| crate::state::due_format_items(menu, &view, format))
        };
        let updated = div()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(view::format_relative(task.updated_at, now));
        let estimate = self.estimate_trigger(
            ("estimate", ix),
            crate::estimate_pick::EstimateTarget {
                id: id.clone(),
                list: true,
            },
            cx,
        );
        let elapsed = self.elapsed_cell(task, ix, cx);

        let view = cx.entity().downgrade();
        let menu_id = id.clone();
        let menu_status = task.status;
        let dragged = self
            .task_drag
            .as_ref()
            .is_some_and(|d| d.moved && d.id == id);
        // 落とすと入る側の縁に線を出す。ほかの画面から来たタスクは、行の上に入る
        let drop_edge = (self.drop_over() == Some(&DropTarget::Row(ix)))
            .then(|| {
                let from = self
                    .dragged_task()
                    .and_then(|d| self.visible.iter().position(|&i| self.tasks[i].id == *d));
                match from {
                    Some(from) => (from != ix).then_some(from > ix),
                    None => Some(true),
                }
            })
            .flatten();
        let drag_id = id.clone();
        let base = div()
            .id(("row", ix))
            .w_full()
            .relative()
            .border_b_1()
            .border_color(theme.border)
            .when(dragged, |d| d.opacity(0.4))
            .when_some(drop_edge, |d, above| {
                d.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .h(px(3.))
                        .when(above, |d| d.top_0())
                        .when(!above, |d| d.bottom_0())
                        .bg(theme.primary),
                )
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.start_task_drag(&drag_id, event.position, cx)
                }),
            )
            .child(
                self.drops
                    .mark(view.clone(), Some(DropTarget::Row(ix)), false),
            )
            // キーで選んでいる行は、左の縁に線を出す
            .when(cursor, |d| {
                d.child(
                    div()
                        .id(("row-cursor", ix))
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(3.))
                        .bg(theme.primary)
                        .test_support(),
                )
            })
            .when(active || cursor, |d| {
                feel::selected(d, theme.accent.opacity(0.4), cx)
            })
            .when(!active && !cursor, |d| d.lit(theme.muted.opacity(0.4), cx));
        let row_el = if table {
            base.h(task_height(true, self.row_density))
                .flex()
                .flex_row()
                .items_center()
                .px_3()
                .gap_3()
                .child(select)
                .child(title)
                .children(self.column_cells([
                    Some(status.into_any_element()),
                    Some(priority.into_any_element()),
                    Some(assignees.into_any_element()),
                    Some(due.into_any_element()),
                    Some(updated.into_any_element()),
                    Some(estimate.into_any_element()),
                    Some(elapsed),
                ]))
        } else {
            // 狭い画面では 1 件を 2 段のカードにする
            base.h(task_height(false, self.row_density))
                .flex()
                .flex_col()
                .justify_center()
                .px_3()
                .gap_1()
                .child(row().gap_3().child(select).child(title))
                .child(
                    row()
                        .gap_1()
                        .pl_8()
                        .child(status)
                        .child(priority)
                        .child(due)
                        .child(assignees),
                )
        };
        // 右クリックで、その行の操作を出す
        row_el
            .right_menu(move |menu, window, cx| {
                let item = |label: &'static str,
                            run: fn(&mut MainView, Id, &mut Window, &mut Context<MainView>)| {
                    let view = view.clone();
                    let id = menu_id.clone();
                    PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        // ウィンドウを閉じたあとなら、反映する先がない
                        let _ = view.update(cx, |this, cx| run(this, id.clone(), window, cx));
                    })
                };
                menu.item(item("開く", |this, id, window, cx| {
                    this.open_panel(&id, window, cx)
                }))
                .submenu("状態を変更", window, cx, {
                    let (view, id) = (view.clone(), menu_id.clone());
                    move |menu, _, _| status_items(menu, &view, &id, menu_status)
                })
                .separator()
                .item(item("コピー", |this, id, _, cx| this.copy_task(&id, cx)))
                .item(paste_item(&view, cx))
                .map(|menu| {
                    crate::plugins::menu_items(
                        menu,
                        &crate::plugins::get(cx).commands(malakt_plugins::Place::Task),
                        &view,
                        &crate::plugins::Target::Task(menu_id.clone()),
                    )
                })
                .separator()
                .item(item("削除", |this, id, _, cx| {
                    this.modal = Modal::DeleteTasks(vec![id]);
                    cx.notify();
                }))
            })
            .into_any_element()
    }

    /// 表の最後の追加行。タイトルと、状態・優先度・担当・期限を決めて足す。
    fn render_quick_add_row(&self, table: bool, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let quick = &self.quick;
        let status = self.value_menu(
            "quick-status",
            status_badge(quick.status, cx),
            Status::ALL
                .into_iter()
                .map(|s| (s.label().into(), s == quick.status, s))
                .collect(),
            |this, value, cx| {
                this.quick.status = value;
                this.touch_quick(cx);
            },
            cx,
        );
        let current = Prio::of(quick.priority);
        let priority = self.value_menu(
            "quick-priority",
            priority_label(current, cx),
            Self::priority_options(current),
            |this, value, cx| {
                this.quick.priority = value;
                this.touch_quick(cx);
            },
            cx,
        );
        let assignees = self.value_menu(
            "quick-assignee",
            self.assignee_cell(&quick.assignee_ids, table, cx),
            self.assignee_options(self.current_workspace(), &quick.assignee_ids),
            |this, person: Id, cx| {
                if let Some(i) = this.quick.assignee_ids.iter().position(|a| a == &person) {
                    this.quick.assignee_ids.remove(i);
                } else {
                    this.quick.assignee_ids.push(person);
                }
                this.touch_quick(cx);
            },
            cx,
        );
        let due = self.date_trigger(
            "quick-due",
            crate::date_pick::DateTarget::Quick,
            match &quick.due {
                Some(due) => div().text_sm().child(view::format_date(due, &self.today)),
                None => div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("—"),
            },
            cx,
        );
        let input = Input::new(&self.inputs.quick_add)
            .id("quick-add")
            .aria_label("タスクを追加")
            .prefix(app_icon("plus").small())
            .flex_1();
        let view = cx.entity().downgrade();
        let submit = move || {
            let view = view.clone();
            feel::gloss(button("quick-add-submit").label("追加").primary().on_click(
                move |_, window, cx| {
                    // ウィンドウを閉じたあとなら、反映する先がない
                    let _ = view.update(cx, |this, cx| this.submit_quick_add(window, cx));
                },
            ))
        };
        // 追加のボタンは「更新」の列に置く。隠しているときは右の端に出す
        let updated_shown = self.shown_columns().any(|c| c.column == Column::Updated);
        let base = div()
            .flex_none()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.muted.opacity(0.3))
            .px_3()
            .py_2();
        if table {
            base.flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(div().w_6().flex_none())
                .child(input)
                .children(self.column_cells([
                    Some(status.into_any_element()),
                    Some(priority.into_any_element()),
                    Some(assignees.into_any_element()),
                    Some(due.into_any_element()),
                    updated_shown.then(|| submit().into_any_element()),
                    None,
                    None,
                ]))
                .when(!updated_shown, |d| d.child(submit()))
        } else {
            base.flex()
                .flex_col()
                .gap_2()
                .child(row().gap_2().child(input).child(submit()))
                .child(
                    row()
                        .gap_1()
                        .flex_wrap()
                        .child(status)
                        .child(priority)
                        .child(assignees)
                        .child(due),
                )
        }
    }

    // ---- 詳細パネル ----

    /// Markdown の入力欄。上の「書く」「プレビュー」で、入力欄と描いた本文を切り替える。
    fn compose(
        &self,
        which: Compose,
        input: &Entity<TextareaState>,
        height: Pixels,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme().clone();
        let preview = self.previewing.contains(&which);
        let key = match which {
            Compose::Description => "desc",
            Compose::Comment => "comment",
            Compose::Status => "status",
        };
        let tab = |name: &str, label: &'static str, active: bool, cx: &mut Context<Self>| {
            let on = name == "preview";
            div()
                .id(SharedString::from(format!("{key}-{name}")))
                .relative()
                .mb(px(-1.))
                .px_3()
                .py_1()
                .rounded_t(feel::md())
                .border_1()
                .border_b_0()
                .text_sm()
                .font_weight(FontWeight::BOLD)
                .cursor_pointer()
                .when(active, |d| {
                    d.border_color(theme.border)
                        .bg(theme.background)
                        .text_color(theme.foreground)
                })
                .when(!active, |d| {
                    d.border_color(transparent_black())
                        .text_color(theme.muted_foreground)
                        .lit(theme.secondary_hover, cx)
                })
                .child(label)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.set_preview(which, on, window, cx)
                }))
                .test_support()
        };
        let tabs = row()
            .items_end()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .child(tab("write", "書く", !preview, cx))
            .child(tab("preview", "プレビュー", preview, cx));
        let body: AnyElement = if preview {
            let value = input.read(cx).value().to_string();
            let panel = div()
                .id(SharedString::from(format!("{key}-preview-body")))
                .min_h(height)
                .px_3()
                .py_2()
                .rounded(feel::md())
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .text_sm();
            if value.trim().is_empty() {
                panel
                    .text_color(theme.muted_foreground)
                    .child("プレビューするものがありません")
                    .test_support()
                    .into_any_element()
            } else {
                panel
                    .child(
                        TextView::markdown(
                            SharedString::from(format!("{key}-preview-text")),
                            self.md_text(&value),
                        )
                        .style(md_style())
                        .selectable(true)
                        .on_link_click(crate::browser::on_link),
                    )
                    .test_support()
                    .into_any_element()
            }
        } else {
            Textarea::new(input).h(height).into_any_element()
        };
        col().gap_2().child(tabs).child(body)
    }

    /// コメント 1 件。書いた人・時刻・操作と、Markdown の本文。
    fn render_attachments(&self, detail: &Detail, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mut list = col().gap_1();
        for (ix, a) in detail.attachments.iter().enumerate() {
            let (open_id, save_id, delete_id) = (a.id.clone(), a.id.clone(), a.id.clone());
            list = list.child(
                row()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(a.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(view::format_bytes(a.size)),
                    )
                    .child(feel::gloss(
                        button(("att-open", ix))
                            .label("開く")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_attachment(&open_id, cx);
                            })),
                    ))
                    .child(feel::gloss(
                        button(("att-save", ix))
                            .label("保存")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.save_attachment(&save_id, window, cx);
                            })),
                    ))
                    .child(feel::gloss(
                        button(("att-del", ix))
                            .label("削除")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.write(cx, |s| s.delete_attachment(&delete_id));
                            })),
                    )),
            );
        }
        col()
            .gap_2()
            .child(
                row()
                    .gap_2()
                    .items_center()
                    .child(section_title("添付"))
                    .when(!detail.attachments.is_empty(), |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("{} 件", detail.attachments.len())),
                        )
                    })
                    .child(div().flex_1())
                    .child(feel::gloss(
                        button("att-add")
                            .label("ファイルを追加")
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| this.attach(window, cx))),
                    )),
            )
            .child(list)
    }

    fn render_comment(&self, ix: usize, c: &Comment, now: i64, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let (pin_id, delete_id, edit_id, pinned) =
            (c.id.clone(), c.id.clone(), c.id.clone(), c.pinned);
        let author = c
            .author_id
            .as_deref()
            .map_or("", |a| self.person_name(a))
            .to_owned();
        // スレッドの中では枠を重ねない
        let boxed = !c.review && c.reply_to.is_none();
        let links = malakt_core::tasklink::find(&c.body);
        col()
            .gap_1()
            .when(boxed, |d| {
                d.p_2()
                    .border_1()
                    .border_color(theme.border)
                    .rounded(feel::md())
            })
            .child(
                row()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(author)
                    // AI や外のエージェントが書いたものは、それが分かる印を付ける
                    .children(
                        c.actor
                            .machine_name()
                            .or_else(|| c.actor.badge())
                            .map(|text| crate::history_view::badge(text, cx)),
                    )
                    .child(view::format_relative(c.created_at, now))
                    .when(c.is_edited(), |d| d.child("（編集済み）"))
                    .child(div().flex_1())
                    .child(feel::gloss(
                        button(("edit-comment", ix))
                            .label("編集")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.start_edit_comment(&edit_id, window, cx)
                            })),
                    ))
                    // 完了報告は最初から上に固定されているので、留めない
                    .when(!c.is_completion_report(), |d| {
                        d.child(feel::gloss(
                            button(("pin", ix))
                                .label(if pinned {
                                    "ピン留めを外す"
                                } else {
                                    "ピン留め"
                                })
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.write(cx, |s| s.set_comment_pinned(&pin_id, !pinned));
                                })),
                        ))
                    })
                    .child(feel::gloss(
                        button(("del-comment", ix))
                            .label("削除")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.write(cx, |s| s.delete_comment(&delete_id));
                            })),
                    )),
            )
            .child({
                let body = if links.is_empty() {
                    c.body.clone()
                } else {
                    malakt_core::tasklink::strip(&c.body)
                };
                div().text_sm().when(!body.is_empty(), |d| {
                    d.child(
                        TextView::markdown(("comment-body", ix), self.md_text(&body))
                            .style(md_style())
                            .selectable(true)
                            .on_link_click(crate::browser::on_link),
                    )
                })
            })
            .children(
                links
                    .into_iter()
                    .enumerate()
                    .map(|(n, id)| self.task_card(("task-card", ix * 16 + n), id, cx)),
            )
    }

    /// リンクが指すタスクのカード。状態とタイトルを出し、押すとそのタスクを開く。
    fn task_card(&self, id: impl Into<ElementId>, task_id: &str, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let task = self
            .detail
            .as_ref()
            .and_then(|d| d.linked.iter().find(|t| t.id == task_id));
        let card = row()
            .id(id)
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded(feel::md())
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .text_sm();
        let card = match task {
            Some(task) => {
                let open = task.id.clone();
                card.cursor_pointer()
                    .hover(|s| s.border_color(theme.primary))
                    .sunk(theme.accent, cx)
                    .child(status_badge(task.status, cx).flex_none())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::BOLD)
                            .child(task.title.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(self.workspace_name(task.workspace_id.as_deref())),
                    )
                    .child(
                        app_icon("arrow-right")
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_panel(&open, window, cx)
                    }))
            }
            None => card
                .text_color(theme.muted_foreground)
                .child(app_icon("link").small())
                .child("リンク先のタスクが見つかりません"),
        };
        div().child(card.test_support())
    }

    /// 詳細の「担当」。1 人ずつ、完了のチェック・頭文字の丸・名前・役（R・A など）を並べる。
    /// 押すと、担当者と役を選ぶところが下に開く。
    fn assignee_property(&self, task: &Todo, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut list = col().gap_1p5();
        if task.assignee_ids.is_empty() {
            list = list.child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("未割り当て"),
            );
        }
        for (i, person_id) in task.assignee_ids.iter().enumerate() {
            let roles = task.roles_of(person_id).unwrap_or_default();
            let done = task.is_done_by(person_id);
            let name = self.person_name(person_id);
            let check = div()
                .id(("p-done", i))
                .size_5()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .cursor_pointer()
                .when(done, |d| {
                    d.bg(theme.success)
                        .border_color(theme.success)
                        .text_color(theme.success_foreground)
                        .child(app_icon("check").size_3p5())
                })
                .when(!done, |d| {
                    d.bg(theme.background)
                        .border_color(theme.muted_foreground.opacity(0.6))
                        .hover(|s| s.border_color(theme.success))
                })
                .active(|s| s.opacity(0.7))
                .tooltip({
                    let text = if done {
                        format!("{name} さんの分を未完了に戻す")
                    } else {
                        format!("{name} さんの分を完了にする")
                    };
                    move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx)
                    }
                })
                .on_click(cx.listener({
                    let id = person_id.clone();
                    move |this, _: &ClickEvent, _, cx| {
                        // 行を押したことにしない（担当を選ぶところを開かない）
                        cx.stop_propagation();
                        this.toggle_assignee_done(&id, cx)
                    }
                }))
                .test_support();
            let badge = div()
                .id(("p-roles", i))
                .flex_none()
                .px_1p5()
                .py_0p5()
                .rounded_sm()
                .border_1()
                .border_color(theme.border)
                .bg(theme.muted)
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(theme.muted_foreground)
                .lit(theme.accent, cx)
                .tooltip({
                    let text = roles.labels();
                    move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx)
                    }
                })
                .child(roles.iter().map(Raci::code).collect::<Vec<_>>().join("・"));
            list = list.child(
                row()
                    .min_w_0()
                    .gap_2()
                    .child(check)
                    .child(avatar(name, cx))
                    .child(div().min_w_0().text_sm().child(name.to_owned()))
                    .child(badge),
            );
        }
        let value = div()
            .id("p-assignees")
            .mx(px(-8.))
            .px_2()
            .py_1()
            .rounded(feel::md())
            .cursor_pointer()
            .lit(theme.accent.opacity(0.4), cx)
            .when(self.assignee_picker, |d| d.bg(theme.accent.opacity(0.4)))
            .child(list)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.assignee_picker = !this.assignee_picker;
                cx.notify();
            }))
            .test_support();
        let mut people = col().flex_1().min_w_0().gap_1().child(value);
        if self.assignee_picker {
            people = people.child(self.assignee_picker(task, cx));
        }
        people
    }

    /// 担当者を選ぶところ。選んだ人の下に、役（R・A・C・I）を選ぶチップが出る。
    fn assignee_picker(&self, task: &Todo, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut list = col()
            .rounded(feel::md())
            .border_1()
            .border_color(theme.border)
            .bg(theme.background);
        let workspace = task.workspace_id.as_deref();
        // このワークスペースで選べる人と、もう担当になっている人
        let (here, others): (Vec<&Person>, Vec<&Person>) = self
            .people
            .iter()
            .partition(|p| task.assignee_ids.contains(&p.id) || self.person_is_in(p, workspace));
        let naming = self
            .person_naming
            .as_ref()
            .filter(|_| self.modal == Modal::None);
        for (i, p) in here.into_iter().enumerate() {
            let row_ix = task.assignee_ids.iter().position(|a| a == &p.id);
            let id = p.id.clone();
            let mut item = col()
                .when(i > 0, |d| d.border_t_1().border_color(theme.border))
                .child(
                    row()
                        .id(("p-as", i))
                        .gap_2()
                        .px_3()
                        .py_2()
                        .cursor_pointer()
                        .lit(theme.accent.opacity(0.4), cx)
                        .sunk(theme.accent, cx)
                        .child(
                            div()
                                .size_5()
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_sm()
                                .border_1()
                                .when(row_ix.is_some(), |d| {
                                    d.bg(theme.primary)
                                        .border_color(theme.primary)
                                        .text_color(theme.primary_foreground)
                                        .child(app_icon("check").size_3p5())
                                })
                                .when(row_ix.is_none(), |d| d.border_color(theme.input)),
                        )
                        .child(avatar(&p.name, cx))
                        .map(|d| {
                            if naming == Some(&p.id) {
                                // その場で名前を書く。Enter かほかの場所を押すと保存、Esc で取り消し
                                d.child(
                                    div()
                                        .id("p-as-name")
                                        .flex_1()
                                        .min_w_0()
                                        .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                                            this.save_person_name(window, cx)
                                        }))
                                        // 名前の欄を押しても、担当から外さない
                                        .on_click(|_, _, cx| cx.stop_propagation())
                                        .child(
                                            Input::new(&self.inputs.name)
                                                .id("person-input")
                                                .aria_label("名前")
                                                .text_sm()
                                                .bg(theme.background)
                                                .border_color(theme.ring)
                                                .px_2(),
                                        ),
                                )
                            } else {
                                d.child(div().min_w_0().truncate().text_sm().child(p.name.clone()))
                            }
                        })
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.toggle_assignee(&id, cx)
                        }))
                        .test_support(),
                );
            if let Some(row_ix) = row_ix {
                let roles = task.roles_of(&p.id).unwrap_or_default();
                let mut chips = row().flex_wrap().gap_1p5().pt_0p5().pr_3().pb_2().pl_10();
                for role in Raci::ALL {
                    let id = p.id.clone();
                    let on = roles.contains(role);
                    chips = chips.child(
                        row()
                            .id(("p-role", row_ix * 4 + role as usize))
                            .gap_1()
                            .px_2()
                            .py_1()
                            .rounded(feel::md())
                            .border_1()
                            .text_xs()
                            .cursor_pointer()
                            .when(on, |d| {
                                d.bg(theme.primary)
                                    .border_color(theme.primary)
                                    .text_color(theme.primary_foreground)
                            })
                            .when(!on, |d| {
                                d.bg(theme.background)
                                    .border_color(theme.border)
                                    .lit(theme.accent, cx)
                            })
                            .active(|s| s.opacity(0.7))
                            .child(div().font_weight(FontWeight::BOLD).child(role.code()))
                            .child(role.label())
                            .tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(role.hint())
                                    .build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.toggle_role(&id, role, cx)
                            }))
                            .test_support(),
                    );
                }
                item = item.child(chips);
            }
            list = list.child(item);
        }
        let view = cx.entity().downgrade();
        let others: Vec<(Id, SharedString)> = others
            .into_iter()
            .map(|p| (p.id.clone(), p.name.clone().into()))
            .collect();
        col().gap_1().child(list).child(
            row().child(
                button("p-add-person")
                    .icon(app_icon("user-plus"))
                    .label("担当者を登録")
                    .ghost()
                    .small()
                    .pop_menu(move |menu, _, _| person_items(menu, &view, &others)),
            ),
        )
    }

    /// タイトルの下に並べる URL。押すとアプリ内ブラウザで開き、右クリックでリンクのメニューを出す。
    /// 足すときは、その場に入力欄を出す。
    fn render_urls(&self, task: &Todo, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut urls = row().gap_1p5().flex_wrap().items_center();
        for (ix, url) in task.urls.iter().enumerate() {
            let shown = crate::task_urls::shown(url);
            let (link, aux, tip, remove): (SharedString, SharedString, SharedString, String) = (
                url.clone().into(),
                url.clone().into(),
                url.clone().into(),
                url.clone(),
            );
            urls = urls.child(
                row()
                    .id(("task-url", ix))
                    .flex_none()
                    .max_w(px(320.))
                    .gap_1()
                    .items_center()
                    .pl_2()
                    .pr_0p5()
                    .py_0p5()
                    .rounded(feel::md())
                    .border_1()
                    .border_color(theme.border)
                    .text_sm()
                    .cursor_pointer()
                    .lit(theme.secondary_hover, cx)
                    .active(|s| s.opacity(0.7))
                    .child(app_icon(shown.icon).small().flex_none())
                    .child(div().min_w_0().truncate().child(shown.label))
                    .child(
                        remove_x(
                            ("task-url-remove", ix),
                            "URL を外す",
                            cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.remove_url(&remove, cx);
                            }),
                            cx,
                        )
                        .text_color(theme.muted_foreground)
                        .test_support(),
                    )
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                    })
                    // 左とホイールはアプリ内ブラウザ、右はリンクのメニュー
                    .on_click(move |event, window, cx| {
                        crate::browser::on_link(&link, event, window, cx)
                    })
                    .on_aux_click(move |event, window, cx| {
                        crate::browser::on_link(&aux, event, window, cx)
                    })
                    .test_support(),
            );
        }
        if self.editing == Some(Editing::Url) {
            // Enter かほかの場所を押すと足し、Esc で取り消し。空白で区切って、いくつも貼れる
            urls.child(
                div()
                    .w(px(320.))
                    .max_w_full()
                    .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                        this.save_url(window, cx);
                    }))
                    .child(
                        Input::new(&self.inputs.url)
                            .id("url-input")
                            .aria_label("URL")
                            .small()
                            .bg(theme.background)
                            .border_color(theme.ring),
                    ),
            )
        } else {
            urls.child(feel::gloss(
                button("url-add")
                    .icon(app_icon("link"))
                    .ghost()
                    .xsmall()
                    .tooltip("URL を追加")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.start_editing(Editing::Url, window, cx)
                    })),
            ))
        }
    }

    fn property(&self, label: &'static str, cx: &App) -> Div {
        row().gap_2().items_start().child(
            div()
                .w(px(96.))
                .flex_none()
                .pt_1()
                .text_sm()
                .font_weight(FontWeight::BOLD)
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
    }

    /// タスクの詳細。`width` があれば一覧の右に並ぶカラム、なければ全面のシート。
    /// タスクの詳細に、出入りの動きを付けて描く。開くと右から入り、閉じると右へ抜けながら
    /// 薄くなって消える（どちらも 200ms）。消えるまでの間は閉じた詳細の上を覆い、
    /// そのボタンは押せない。
    fn render_panel_motion(
        &mut self,
        width: Option<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if let Some(panel) = self.render_panel(width, cx) {
            if !self.motion {
                return Some(panel.into_any_element());
            }
            return Some(
                panel
                    .with_animation("panel-in", appear(PANEL_MOTION_MS), panel_shift)
                    .into_any_element(),
            );
        }
        let (at, last) = self.panel_closing.take()?;
        let progress = at.elapsed().as_millis() as f32 / PANEL_MOTION_MS as f32;
        if !self.motion || progress >= 1. {
            return None;
        }
        window.request_animation_frame();
        // 閉じる前の中身で描く。描いたら元の閉じた状態に戻す
        let editing = self.editing.take();
        self.detail = Some(last);
        let panel = self.render_panel(width, cx);
        if let Some(last) = self.detail.take() {
            self.panel_closing = Some((at, last));
        }
        self.editing = editing;
        Some(
            panel_shift(panel?, 1. - ease_out_quint()(progress))
                .id("panel-closing")
                .child(div().absolute().inset_0().occlude())
                .test_support()
                .into_any_element(),
        )
    }

    fn render_panel(&self, width: Option<Pixels>, cx: &mut Context<Self>) -> Option<Div> {
        let sheet = width.is_none();
        let detail = self.detail.as_ref()?;
        let theme = cx.theme().clone();
        let task = &detail.task;
        let now = malakt_core::now_ms();
        let open = task.status.is_open();

        let title = if self.editing == Some(Editing::Title) {
            // その場で書き直す。文字の大きさも太さも位置も、読むときと同じ。
            // Enter かほかの場所を押すと保存、Esc で取り消し
            div()
                .mx(px(-8.))
                .text_xl()
                .font_weight(FontWeight::BOLD)
                .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                    if this.editing == Some(Editing::Title) {
                        this.save_title(window, cx);
                        cx.notify();
                    }
                }))
                .child(
                    Input::new(&self.inputs.title)
                        .id("title-input")
                        .aria_label("タイトル")
                        .text_xl()
                        .bg(theme.background)
                        .border_color(theme.ring)
                        .px_2(),
                )
        } else {
            div().child(
                div()
                    .id("detail-title")
                    .mx(px(-8.))
                    .px_2()
                    .py_1()
                    .rounded(feel::md())
                    .border_1()
                    .border_color(gpui_kit::transparent_black())
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .when(!open, |d| d.text_color(theme.muted_foreground))
                    .cursor_pointer()
                    .lit(theme.muted, cx)
                    .child(task.title.clone())
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.start_editing(Editing::Title, window, cx)
                    }))
                    .test_support(),
            )
        };

        // AI が整えている間の印と、整えられなかった理由
        let ai_state = {
            let session = self.ai_session.read(cx);
            if session.tracking.contains(&task.id) {
                // コメントからの更新中の印は「進捗」の行に出す（書いている途中の画面を動かさない）
                None
            } else if let Some(note) = session.track_notes.get(&task.id) {
                let detail = note.clone();
                let id = task.id.clone();
                Some(
                    row()
                        .gap_1()
                        .items_center()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(
                            // 短い言葉だけを出し、くわしい理由はホバーで出す
                            row()
                                .id("ai-track-note")
                                .gap_2()
                                .items_center()
                                .child(app_icon("sparkles").small().flex_none())
                                .child(crate::ai::arrange_note_label(note))
                                .tooltip(move |window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(detail.clone())
                                        .build(window, cx)
                                })
                                .test_support(),
                        )
                        .child(feel::gloss(
                            button("ai-track-retry")
                                .icon(app_icon("refresh-cw"))
                                .ghost()
                                .small()
                                .tooltip("もう一度更新する")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.ai_session.update(cx, |s, cx| s.retry_track(&id, cx));
                                })),
                        ))
                        .into_any_element(),
                )
            } else if session.arranging.contains(&task.id) {
                Some(
                    row()
                        .id("ai-working")
                        .gap_2()
                        .items_center()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(Spinner::new().small())
                        .child("AI が整えています")
                        .test_support()
                        .into_any_element(),
                )
            } else {
                session.arrange_notes.get(&task.id).map(|note| {
                    let detail = note.clone();
                    let id = task.id.clone();
                    row()
                        .gap_1()
                        .items_center()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(
                            // 短い言葉だけを出し、くわしい理由はホバーで出す
                            row()
                                .id("ai-note")
                                .gap_2()
                                .items_center()
                                .child(app_icon("sparkles").small().flex_none())
                                .child(crate::ai::arrange_note_label(note))
                                .tooltip(move |window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(detail.clone())
                                        .build(window, cx)
                                })
                                .test_support(),
                        )
                        .child(feel::gloss(
                            button("ai-note-retry")
                                .icon(app_icon("refresh-cw"))
                                .ghost()
                                .small()
                                .tooltip("もう一度整える")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.ai_session.update(cx, |s, cx| s.retry_arrange(&id, cx));
                                })),
                        ))
                        .into_any_element()
                })
            }
        };

        // このタスクについての AI の記録と、AI と話すボタン
        let ai_log = {
            let session = self.ai_session.read(cx);
            let runs = session.runs_for(&task.id);
            let now = malakt_core::now_ms();
            let id = task.id.clone();
            let open = self.ai_log_open;
            let mut log = col().gap_1().child(
                row()
                    .gap_1()
                    .items_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(feel::gloss(
                        button("ai-task-chat")
                            .icon(app_icon("sparkles"))
                            .ghost()
                            .xsmall()
                            .tooltip("このタスクについて AI と話す")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.chat_about_task(&id, window, cx);
                            })),
                    ))
                    .child(feel::gloss(
                        button("task-terminal")
                            .icon(app_icon("terminal"))
                            .ghost()
                            .xsmall()
                            .tooltip("このタスクでターミナルを開く")
                            .on_click(cx.listener({
                                let id = task.id.clone();
                                move |this, _, window, cx| {
                                    this.open_task_terminal(&id, window, cx);
                                }
                            })),
                    ))
                    .when(!runs.is_empty(), |d| {
                        d.child(
                            row()
                                .id("ai-log")
                                .gap_1()
                                .items_center()
                                .px_1()
                                .rounded(feel::md())
                                .cursor_pointer()
                                .lit(theme.muted, cx)
                                .child(format!("AI の記録 {}", runs.len()))
                                .child(
                                    app_icon(if open {
                                        "chevron-down"
                                    } else {
                                        "chevron-right"
                                    })
                                    .small(),
                                )
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.ai_log_open = !this.ai_log_open;
                                    cx.notify();
                                }))
                                .test_support(),
                        )
                    }),
            );
            if open {
                for (ix, run) in runs.into_iter().enumerate() {
                    log = log.child(
                        col()
                            .id(("ai-log-run", ix))
                            .gap_1()
                            .child(
                                row()
                                    .gap_2()
                                    .px_3()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(run.kind.label())
                                    .child(malakt_core::view::format_relative(run.at, now)),
                            )
                            .child(crate::ai_view::run_details(run, &theme))
                            .test_support(),
                    );
                }
            }
            log
        };

        let status_actions = {
            let id = task.id.clone();
            let mut actions = row().gap_2().flex_wrap();
            let set =
                |label: &'static str, key: &'static str, status: Status, primary: bool| {
                    let id = id.clone();
                    let button = button(key).icon(app_icon(status_icon(status))).label(label);
                    let button = if primary {
                        button.primary()
                    } else {
                        button.framed(cx)
                    };
                    feel::gloss(button.on_click(
                        cx.listener(move |this, _, _, cx| this.set_status(&id, status, cx)),
                    ))
                };
            if open {
                actions = actions
                    .child(set("完了にする", "to-completed", Status::Completed, true))
                    .child(set("中止にする", "to-cancelled", Status::Cancelled, false));
            } else {
                actions = actions.child(set("再開する", "to-reopen", Status::NeedsAction, true));
            }
            let id = task.id.clone();
            actions.child(feel::gloss(
                button("status-with-comment")
                    .icon(app_icon("message-square-plus"))
                    .framed(cx)
                    .tooltip("コメントを付けて状態を変更")
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.ask_status(&id, window, cx)),
                    ),
            ))
        };

        let pinned: Vec<_> = detail.comments.iter().filter(|c| c.pinned).collect();
        let description = if self.editing == Some(Editing::Description) {
            col()
                .gap_2()
                .child(self.compose(Compose::Description, &self.inputs.description, px(160.), cx))
                .child(
                    row()
                        .gap_2()
                        .child(feel::gloss(
                            button("desc-save")
                                .label("保存")
                                .primary()
                                .small()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.save_description(window, cx)
                                })),
                        ))
                        .child(feel::gloss(
                            button("desc-cancel").label("キャンセル").small().on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.editing = None;
                                    cx.notify();
                                }),
                            ),
                        ))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("Ctrl / ⌘ + Enter で保存、Esc でキャンセル"),
                        ),
                )
        } else if task.description.is_empty() {
            div().child(
                div()
                    .id("detail-description")
                    .cursor_pointer()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("説明を追加…")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.start_editing(Editing::Description, window, cx)
                    })),
            )
        } else {
            // Markdown で見せる。押すと編集になる（リンクを押したとき・文字を選んだときはならない）
            col()
                .gap_1()
                .child(
                    div()
                        .id("detail-description")
                        .relative()
                        .mx(px(-8.))
                        .px_2()
                        .py_1()
                        .rounded(feel::md())
                        .cursor_text()
                        .text_sm()
                        .lit(theme.secondary_hover, cx)
                        .child(
                            TextView::new(&self.description_view)
                                .style(md_style())
                                .on_link_click(crate::browser::on_link),
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            if this.description_selected(cx) {
                                return;
                            }
                            this.start_editing(Editing::Description, window, cx)
                        }))
                        .test_support(),
                )
                .child(
                    row()
                        .gap_1()
                        .child(feel::gloss(
                            button("desc-edit").label("編集").ghost().xsmall().on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.start_editing(Editing::Description, window, cx)
                                }),
                            ),
                        ))
                        .child(feel::gloss(
                            button("desc-quote")
                                .label("選択範囲にコメント")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.quote_selection(window, cx)
                                })),
                        )),
                )
        };

        let done = detail
            .subtasks
            .iter()
            .filter(|t| !t.status.is_open())
            .count();
        let mut subtasks = col().gap_1();
        for (ix, sub) in detail.subtasks.iter().enumerate() {
            let id = sub.id.clone();
            subtasks = subtasks.child(
                row()
                    .id(("subtask", ix))
                    .gap_2()
                    .cursor_pointer()
                    .text_sm()
                    .child(status_badge(sub.status, cx))
                    .child(div().flex_1().truncate().child(sub.title.clone()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_panel(&id, window, cx)
                    })),
            );
        }

        // プロパティ
        let status_chips = self
            .property("状態", cx)
            .child(
                div()
                    .flex()
                    .child(self.status_menu("p-status", &task.id, task.status, cx)),
            );
        let current_priority = Prio::of(task.priority);
        let prio_chips = self.value_menu(
            "p-prio",
            priority_label(current_priority, cx),
            Self::priority_options(current_priority),
            |this, value, cx| this.edit_task(cx, |d| d.priority = value),
            cx,
        );

        let people = self.assignee_property(task, cx);

        let start = self.when_property(task, When::Start, cx);
        let due = self.when_property(task, When::Due, cx);

        let mut percent = row().gap_1p5().flex_wrap().items_center().child(
            row()
                .flex_none()
                .gap_2()
                .mr_1()
                .items_center()
                .text_sm()
                .child(percent_bar(task.percent, px(64.), cx))
                .child(format!("{}%", task.percent)),
        );
        for (i, value) in [0u8, 25, 50, 75, 100].into_iter().enumerate() {
            percent = percent.child(
                chip(
                    ("p-percent", i),
                    format!("{value}%"),
                    task.percent == value,
                    cx,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.edit_task(cx, |d| d.percent = value)
                })),
            );
        }

        // AI がコメントから更新している間の印と、変えたあとの元に戻すボタン。同じ場所に出す
        if self.ai_session.read(cx).tracking.contains(&task.id) {
            percent = percent.child(
                div()
                    .id("ai-tracking")
                    .flex_none()
                    .px_1()
                    .child(Spinner::new().small())
                    .tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new("AI がコメントから更新中")
                            .build(window, cx)
                    })
                    .test_support(),
            );
        } else if let Some(tracked) = self.ai_session.read(cx).tracked_for(task) {
            let id = task.id.clone();
            percent = percent.child(feel::gloss(
                button("ai-tracked-undo")
                    .icon(app_icon("undo-2"))
                    .ghost()
                    .small()
                    .tooltip(format!("AI の更新（{tracked}）を元に戻す"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.ai_session.update(cx, |s, cx| s.undo_track(&id, cx));
                    })),
            ));
        }

        let mut categories = row().gap_1p5().flex_wrap();
        // ベースの CategoryInput.vue と同じ形: 名前と、外す × のボタン。外すのは × だけ
        for (i, c) in task.categories.iter().enumerate() {
            let value = c.clone();
            categories = categories.child(
                row()
                    .id(("p-cat", i))
                    .flex_none()
                    .max_w_full()
                    .gap_0p5()
                    .rounded_full()
                    .bg(theme.secondary)
                    .py_0p5()
                    .pr_0p5()
                    .pl_2p5()
                    .text_sm()
                    .text_color(theme.secondary_foreground)
                    .child(div().min_w_0().truncate().child(c.clone()))
                    .child(
                        remove_x(
                            ("p-cat-remove", i),
                            format!("カテゴリ「{c}」を外す"),
                            cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.remove_category(&value, cx)
                            }),
                            cx,
                        )
                        .test_support(),
                    )
                    .test_support(),
            );
        }
        categories = categories.child(
            Input::new(&self.inputs.category)
                .id("category-input")
                .aria_label("カテゴリ")
                .small()
                .w(px(168.)),
        );

        let parent = match &detail.parent {
            Some(parent) => {
                let id = parent.id.clone();
                row()
                    .gap_2()
                    .child(div().text_sm().truncate().child(parent.title.clone()))
                    .child(feel::gloss(
                        button("parent-open").label("開く").xsmall().on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.open_panel(&id, window, cx)
                            }),
                        ),
                    ))
                    .child(feel::gloss(
                        button("parent-clear")
                            .label("外す")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.edit_task(cx, |d| d.parent_id = None)
                            })),
                    ))
            }
            None => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("なし"),
        };

        let workspace = {
            let id = task.id.clone();
            let link_id = task.id.clone();
            let shared = !task.linked_workspace_ids.is_empty();
            let mut places = col().gap_1().child(
                row()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .child(self.workspace_name(task.workspace_id.as_deref())),
                    )
                    .child(feel::gloss(
                        button("move-task")
                            .label("移動")
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.modal = Modal::MoveTasks(vec![id.clone()]);
                                cx.notify();
                            })),
                    ))
                    .when_some(task.workspace_id.clone().filter(|_| shared), |d, home| {
                        let task_id = task.id.clone();
                        d.child(feel::gloss(
                            button("unlink-home")
                                .label("外す")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.unlink_task_from(&task_id, &home, cx)
                                })),
                        ))
                    }),
            );
            for (ix, linked) in task.linked_workspace_ids.iter().enumerate() {
                let (task_id, linked_id) = (task.id.clone(), linked.clone());
                places = places.child(
                    row()
                        .gap_2()
                        .items_center()
                        .child(div().text_sm().child(self.workspace_name(Some(linked))))
                        .child(feel::gloss(
                            button(("unlink", ix))
                                .label("外す")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.unlink_task_from(&task_id, &linked_id, cx)
                                })),
                        )),
                );
            }
            // どこにも入っていないタスクは「移動」で入れる
            places.when(task.workspace_id.is_some(), |d| {
                d.child(
                    row().child(feel::gloss(
                        button("link-add")
                            .label("ほかのワークスペースと同期")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.modal = Modal::LinkTask(link_id.clone());
                                cx.notify();
                            })),
                    )),
                )
            })
        };

        // 返信はルートの下にまとめる。完了報告は上に固定して出すので、ここには出さない
        let unresolved = detail
            .comments
            .iter()
            .filter(|c| c.is_thread_root() && c.resolved_at.is_none())
            .count();
        let mut comments = col().gap_2();
        // 変更の出来事を、コメントと時刻の順に挟む（同じ時刻なら出来事が先）
        let events = self.timeline_events(detail, cx);
        let mut events = events.iter().enumerate().peekable();
        for (ix, c) in detail.comments.iter().enumerate() {
            if c.reply_to.is_some() || c.is_completion_report() {
                continue;
            }
            while let Some((n, event)) = events.next_if(|(_, e)| e.at <= c.created_at) {
                comments = comments.child(self.render_event(n, event, now, cx));
            }
            if !c.is_thread_root() {
                comments = comments.child(self.render_comment(ix, c, now, cx));
                continue;
            }
            let resolved = c.resolved_at.is_some();
            let open = !resolved || self.expanded_threads.contains(&c.id);
            let (toggle_id, resolve_id, reply_id) = (c.id.clone(), c.id.clone(), c.id.clone());
            let mut thread =
                col()
                    .gap_1()
                    .p_2()
                    .border_1()
                    .border_color(theme.border)
                    .rounded(feel::md())
                    .child(
                        row()
                            .gap_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(if resolved {
                                "レビュー（解決済み）"
                            } else {
                                "レビュー"
                            })
                            .when(!open, |d| {
                                d.child(
                                    div().flex_1().min_w_0().truncate().child(
                                        c.body.lines().next().unwrap_or_default().to_owned(),
                                    ),
                                )
                            })
                            .when(open, |d| d.child(div().flex_1()))
                            .when(resolved, |d| {
                                d.child(feel::gloss(
                                    button(("thread-toggle", ix))
                                        .label(if open { "折りたたむ" } else { "開く" })
                                        .ghost()
                                        .xsmall()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.toggle_thread(&toggle_id, cx)
                                        })),
                                ))
                            })
                            .child(feel::gloss(
                                button(("thread-resolve", ix))
                                    .label(if resolved {
                                        "再開する"
                                    } else {
                                        "解決済みにする"
                                    })
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_thread_resolved(&resolve_id, !resolved, cx)
                                    })),
                            )),
                    );
            if open {
                if let Some(anchor) = &c.anchor {
                    // 引用。説明が書き換わって見つからなくなったら、そう伝える
                    let found = task.description.contains(anchor.exact.as_str());
                    thread = thread.child(
                        col()
                            .pl_2()
                            .border_l_2()
                            .border_color(theme.border)
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(anchor.exact.clone())
                            .when(!found, |d| {
                                d.child(
                                    div()
                                        .text_xs()
                                        .child("（説明のこの部分は書き換えられました）"),
                                )
                            }),
                    );
                }
                thread = thread.child(self.render_comment(ix, c, now, cx));
                let mut replies = col().gap_1().pl_4();
                for (reply_ix, reply) in detail.comments.iter().enumerate() {
                    if reply.reply_to.as_deref() == Some(c.id.as_str()) {
                        replies = replies.child(self.render_comment(reply_ix, reply, now, cx));
                    }
                }
                thread = thread.child(replies).child(
                    row().child(feel::gloss(
                        button(("thread-reply", ix))
                            .label("返信")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.start_reply(&reply_id, window, cx)
                            })),
                    )),
                );
            }
            comments = comments.child(thread);
        }
        for (n, event) in events {
            comments = comments.child(self.render_event(n, event, now, cx));
        }
        let report_items: Vec<Div> = detail
            .comments
            .iter()
            .enumerate()
            .filter(|(_, c)| c.is_completion_report())
            .map(|(ix, c)| self.render_comment(ix, c, now, cx))
            .collect();

        // コメント欄の上に、いま何を書いているかを出す
        let composing: Option<SharedString> = match (&self.comment_mode, &self.pending_anchor) {
            (CommentMode::Edit(_), _) => Some("コメントを書き直しています".into()),
            (CommentMode::Reply(_), _) => Some("スレッドに返信します".into()),
            (CommentMode::New, Some(anchor)) => {
                Some(format!("「{}」にレビューコメントを付けます", anchor.exact).into())
            }
            (CommentMode::New, None) => None,
        };
        let is_new = self.comment_mode == CommentMode::New;
        let mut mention_row = row().gap_1().flex_wrap();
        for (i, person) in self.mention_candidates(cx).into_iter().enumerate() {
            mention_row = mention_row.child(
                chip(("mention", i), format!("@{}", person.name), false, cx)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.complete_mention(&person, window, cx)
                    }))
                    .test_support(),
            );
        }

        let delete_id = task.id.clone();
        let bar = row()
            .flex_none()
            .h_12()
            .px_4()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            // 浮かせたパネルの中では、パネルの地と角の丸みをそのまま見せる
            .when(!crate::theme::floating_now(), |d| d.bg(theme.popover))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::BOLD)
                    .child("タスクの詳細"),
            )
            .child({
                let link_task = task.id.clone();
                feel::gloss(
                    button("copy-link")
                        .icon(app_icon("link"))
                        .tooltip("リンクをコピー")
                        .framed(cx)
                        .small()
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.copy_task_link(&link_task, cx)),
                        ),
                )
            })
            .child({
                let ticket_task = task.id.clone();
                feel::gloss(
                    button("ticket-open")
                        .icon(app_icon("bookmark"))
                        .tooltip("完了チケット")
                        .framed(cx)
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_ticket_dialog(&ticket_task, cx)
                        })),
                )
            })
            .child(feel::gloss(
                button("close-panel")
                    .icon(app_icon("x"))
                    .tooltip("閉じる")
                    .framed(cx)
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.close_panel(cx))),
            ));
        let content =
            col()
                .p_4()
                .gap_4()
                // URL はタイトルのすぐ下に並べる
                .child(
                    col()
                        .gap_1p5()
                        .child(title)
                        .child(self.render_urls(task, cx)),
                )
                .children(ai_state)
                .child(ai_log)
                .child(
                    row()
                        .gap_3()
                        .flex_wrap()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(status_badge(task.status, cx))
                        .child(feel::gloss(
                            button("history")
                                .icon(app_icon("rotate-ccw-clock"))
                                .label("変更履歴")
                                .framed(cx)
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.modal = Modal::History;
                                    cx.notify();
                                })),
                        ))
                        .child(self.workspace_name(task.workspace_id.as_deref()))
                        // 作った人・最後に変えた人（AI や外のエージェントなら、その名前と印）
                        .child(self.render_authorship(task, now, cx)),
                )
                .child(
                    card(cx)
                        // 行ごとにボタンが並ぶので、行の間を空ける
                        .gap_3()
                        .child(status_chips)
                        .child(self.property("優先度", cx).child(prio_chips))
                        .child(self.property("担当", cx).child(people))
                        .child(self.property("開始日", cx).child(start))
                        .child(self.property("期限", cx).child(due))
                        .when_some(
                            self.task_alert_menu(&task.id, task.due.as_deref(), cx),
                            |d, menu| d.child(self.property("通知", cx).child(menu)),
                        )
                        .child(
                            self.property("繰り返し", cx)
                                .child(self.repeat_property(detail, cx)),
                        )
                        .when_some(self.repeat_ai_property(detail, cx), |d, ai| {
                            d.child(self.property("AI への指示", cx).child(ai))
                        })
                        .child(self.property("進捗", cx).child(percent))
                        // ポモドーロで測ったことがあるタスクだけに出す
                        .when(detail.work.0 > 0, |d| {
                            d.child(self.property("作業時間", cx).child({
                                let text = format!(
                                    "{}（{} 回）",
                                    malakt_core::pomodoro::duration_label(detail.work.1),
                                    detail.work.0
                                );
                                div()
                                    .id("work-total")
                                    .text_sm()
                                    .aria_label(text.clone())
                                    .child(text)
                                    .test_support()
                            }))
                        })
                        .child(self.property("予想工数", cx).child(self.estimate_trigger(
                            "estimate-value",
                            crate::estimate_pick::EstimateTarget {
                                id: task.id.clone(),
                                list: false,
                            },
                            cx,
                        )))
                        .child(
                            self.property("所要時間", cx)
                                .child(self.elapsed_property(task, cx)),
                        )
                        .child(self.property("カテゴリ", cx).child(categories))
                        .child(self.property("親タスク", cx).child(parent))
                        .child(self.property("ワークスペース", cx).child(workspace))
                        .when_some(task.completed_at, |d, at| {
                            d.child(
                                self.property("完了日時", cx)
                                    .child(div().text_sm().child(view::format_datetime(at))),
                            )
                        }),
                )
                .child(status_actions)
                .when(!pinned.is_empty(), |d| {
                    d.child(
                        col()
                            .gap_1()
                            .p_2()
                            .rounded(feel::md())
                            .bg(theme.secondary)
                            .child(section_title("ピン留め"))
                            .children(pinned.iter().enumerate().map(|(i, c)| {
                                div().text_sm().child(
                                    TextView::markdown(("pinned-body", i), self.md_text(&c.body))
                                        .style(md_style())
                                        .selectable(true)
                                        .on_link_click(crate::browser::on_link),
                                )
                            })),
                    )
                })
                .when(!report_items.is_empty(), |d| {
                    d.child(
                        col()
                            .gap_1()
                            .p_2()
                            .rounded(feel::md())
                            .border_1()
                            .border_color(theme.border)
                            .child(section_title("完了報告"))
                            .children(report_items),
                    )
                })
                .child(card(cx).child(section_title("説明")).child(description))
                .child(
                    card(cx)
                        .child(row().gap_2().child(section_title("サブタスク")).when(
                            !detail.subtasks.is_empty(),
                            |d| {
                                d.child(div().text_xs().text_color(theme.muted_foreground).child(
                                    format!("（{done}/{} 件が完了）", detail.subtasks.len()),
                                ))
                            },
                        ))
                        .child(subtasks)
                        .child(
                            Input::new(&self.inputs.subtask)
                                .id("subtask-input")
                                .aria_label("サブタスクを追加")
                                .small(),
                        ),
                )
                .child(card(cx).child(self.render_attachments(detail, cx)))
                .child(
                    card(cx)
                        .child(row().gap_2().child(section_title("コメント")).when(
                            unresolved > 0,
                            |d| {
                                d.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("（未解決のレビュー {unresolved} 件）")),
                                )
                            },
                        ))
                        .child(comments)
                        .when_some(composing, |d, text| {
                            d.child(
                                row()
                                    .gap_2()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(div().flex_1().min_w_0().truncate().child(text))
                                    .child(feel::gloss(
                                        button("comment-cancel")
                                            .label("キャンセル")
                                            .ghost()
                                            .xsmall()
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.cancel_comment_mode(window, cx)
                                            })),
                                    )),
                            )
                        })
                        .child(self.compose(Compose::Comment, &self.inputs.comment, px(88.), cx))
                        .child(mention_row)
                        .child(
                            row()
                                .gap_2()
                                .child(feel::gloss(
                                    button("comment-submit")
                                        .label(match self.comment_mode {
                                            CommentMode::Edit(_) => "保存",
                                            CommentMode::Reply(_) => "返信する",
                                            CommentMode::New => "コメントする",
                                        })
                                        .primary()
                                        .small()
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.submit_comment(window, cx)
                                        })),
                                ))
                                .when(is_new && self.pending_anchor.is_none(), |d| {
                                    d.child(feel::gloss(
                                        button("comment-review")
                                            .label("レビューとしてコメント")
                                            .small()
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.submit_comment_as(true, window, cx)
                                            })),
                                    ))
                                })
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child("Ctrl / ⌘ + Enter で送信"),
                                ),
                        ),
                )
                .child(
                    card(cx).child(section_title("削除")).child(
                        row().child(feel::gloss(
                            button("delete-task")
                                .label("このタスクを削除")
                                .danger()
                                .small()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.modal = Modal::DeleteTasks(vec![delete_id.clone()]);
                                    cx.notify();
                                })),
                        )),
                    ),
                );

        // 背景の画像の上に浮かせたパネルは、地を透かして塗る
        let glass_fill = crate::theme::backdrop(cx)
            .filter(|_| crate::theme::floating_now())
            .map(|backdrop| theme.background.opacity((backdrop.alpha + 1.) / 2.));
        // 一覧の上に重ねる（全面のシートか、重ねる詳細）ときの角の丸み。一覧の上では
        // 地をすりガラスにする
        let corner = px(15.);
        let frost = (sheet || crate::theme::overlay(cx)).then(|| {
            if !crate::theme::floating_now() {
                Corners::default()
            } else if sheet {
                Corners::all(corner)
            } else {
                Corners {
                    top_right: corner,
                    bottom_right: corner,
                    ..Default::default()
                }
            }
        });
        Some(
            div()
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .when(sheet, |d| d.left_0())
                .when_some(width, |d, width| {
                    d.w(width).border_l_1().border_color(theme.border)
                })
                // 浮かせたパネルの中に収めて、角をパネルに合わせる
                .when(crate::theme::floating_now(), |d| {
                    let half = FLOAT_GAP / 2.;
                    d.top(half)
                        .right(FLOAT_GAP)
                        .bottom(FLOAT_GAP)
                        .rounded_tr(corner)
                        .rounded_br(corner)
                        .when(sheet, |d| {
                            d.left(half).rounded_tl(corner).rounded_bl(corner)
                        })
                        .overflow_hidden()
                })
                // 一覧の上に重ねるときは、影で浮かせる
                .when(!sheet && crate::theme::overlay(cx), |d| {
                    d.elevate(Elevation::High, cx)
                })
                // 背景の画像の上では、後ろが少し透ける。一覧の上に重ねるときは、一覧は透かさず、
                // ぼかした背景の画像だけを透かす（すりガラス）
                .map(|d| match (frost, glass_fill) {
                    (Some(corners), Some(fill)) => d.children(frosted_glass(
                        corners,
                        vec![page_fill(&theme, cx), fill],
                        cx,
                    )),
                    (_, fill) => d.bg(fill.unwrap_or(theme.background)),
                })
                .occlude()
                // 詳細の下にある行は、ドラッグの落とし先にしない
                .child(self.drops.mark(cx.entity().downgrade(), None, false))
                .flex()
                .flex_col()
                .child(bar)
                .child(
                    div()
                        .id("detail-panel")
                        .flex_1()
                        .min_h_0()
                        .child(
                            div()
                                .id("detail-scroll")
                                .size_full()
                                .overflow_y_scrollbar()
                                .child(content),
                        )
                        .test_support(),
                )
                // 左の境界をドラッグして幅を変える。ダブルクリックで最初の幅に戻す
                .when(!sheet, |d| {
                    d.child(
                        div()
                            .id("panel-resize")
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(px(-3.))
                            .w(px(6.))
                            .cursor_col_resize()
                            .when(self.panel_resize.is_some(), |d| d.bg(theme.ring))
                            .lit(theme.ring, cx)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    if event.click_count >= 2 {
                                        this.reset_panel_width(cx);
                                    } else {
                                        this.start_panel_resize(event.position.x.into(), cx);
                                    }
                                }),
                            )
                            .test_support(),
                    )
                }),
        )
    }

    // ---- ダイアログ ----

    pub(crate) fn dialog(
        &self,
        title: impl Into<SharedString>,
        body: Div,
        cx: &mut Context<Self>,
    ) -> Div {
        self.dialog_as("dialog", px(460.), title, body, cx)
    }

    /// 下に敷いたダイアログは `dialog-under`、いちばん上のものは `dialog` と名付ける。
    pub(crate) fn dialog_as(
        &self,
        id: &'static str,
        width: Pixels,
        title: impl Into<SharedString>,
        body: Div,
        cx: &mut Context<Self>,
    ) -> Div {
        self.dialog_sized(id, width, None, title, body, cx)
    }

    /// 高さを決めたダイアログ。中身を切り替えても、大きさが変わらない。
    pub(crate) fn dialog_sized(
        &self,
        id: &'static str,
        width: Pixels,
        height: Option<Pixels>,
        title: impl Into<SharedString>,
        body: Div,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme().clone();
        // 外側の暗い所。押して離すまでがここなら、いちばん上のダイアログを閉じる。
        // ダイアログの中で押し始めて外で離したときは閉じない
        let backdrop = if id == "dialog" {
            "dialog-backdrop"
        } else {
            "dialog-under-backdrop"
        };
        // 出てくるときと消えるときは、全体を薄くして、ダイアログを少し下から浮かび上がらせる
        let t = self.dialog_t.get();
        let dialog = div()
            .id(backdrop)
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.overlay)
            .when(t < 1., |d| d.opacity(t))
            .occlude()
            .child(self.drops.mark(cx.entity().downgrade(), None, false))
            // 名前を書いている途中で外を押したら、その 1 回は名前を確定するだけにする
            .capture_any_mouse_down(cx.listener(|this, _, _, _| {
                this.outside_busy = this.person_naming.is_some()
                    || this.preset_naming.is_some()
                    || this.key_naming.is_some()
                    || this.key_limit_editing.is_some();
            }))
            .on_click(cx.listener(|this, _, window, cx| this.dismiss_outside(window, cx)))
            .child(
                col()
                    .id(id)
                    .occlude()
                    .when(t < 1., |d| d.relative().top(px(12.) * (1. - t)))
                    .w(width)
                    .when_some(height, |d, height| d.h(height))
                    .max_w(relative(0.94))
                    .max_h(relative(0.9))
                    .p_4()
                    .gap_3()
                    // 浮かせたパネルと同じ丸みにする
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
                            .child(div().flex_1().text_lg().child(title.into()))
                            .child(feel::gloss(
                                button("dialog-close")
                                    .icon(app_icon("x"))
                                    .tooltip("閉じる")
                                    .ghost()
                                    .small()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.close_top_dialog(window, cx)
                                    })),
                            )),
                    )
                    .child(body)
                    .test_support(),
            );
        div().absolute().inset_0().child(dialog)
    }

    /// 担当者の一覧。名前はその場で書き換え、使うワークスペースを選び、足し、消す。
    fn people_editor(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut list = col().gap_1();
        let view = cx.entity().downgrade();
        let roots: Vec<(Id, SharedString)> = self
            .workspaces
            .iter()
            .filter(|w| w.parent_id.is_none())
            .map(|w| (w.id.clone(), w.name.clone().into()))
            .collect();
        for (ix, p) in self.people.iter().enumerate() {
            let id = p.id.clone();
            // 使うワークスペース。「自分」は、すべてのワークスペースで使う
            let scope = (!p.is_self).then(|| {
                let (view, person, roots) = (view.clone(), p.id.clone(), roots.clone());
                // いちばん上の親に直して、メニューの印と合わせる
                let current: Vec<Id> = p
                    .workspace_ids
                    .iter()
                    .map(|w| self.root_of(w).to_owned())
                    .collect();
                let label: SharedString = match current.as_slice() {
                    [] => "すべてのワークスペース".into(),
                    [one] => self.workspace_name(Some(one)),
                    many => format!(
                        "{} ほか {}",
                        self.workspace_name(Some(&many[0])),
                        many.len() - 1
                    )
                    .into(),
                };
                button(("person-scope", ix))
                    .icon(app_icon(if p.is_global() { "globe" } else { "folder" }))
                    .ghost()
                    .xsmall()
                    .tooltip(label)
                    .pop_menu(move |menu, _, _| {
                        person_workspace_items(menu, &view, &person, &current, &roots)
                    })
            });
            let name = if self.person_naming.as_ref() == Some(&p.id) {
                // その場で名前を書く。Enter かほかの場所を押すと保存、Esc で取り消し
                div()
                    .flex_1()
                    .min_w_0()
                    .on_mouse_down_out(
                        cx.listener(|this, _, window, cx| this.save_person_name(window, cx)),
                    )
                    .child(
                        Input::new(&self.inputs.name)
                            .id("person-input")
                            .aria_label("名前")
                            .text_base()
                            .bg(theme.background)
                            .border_color(theme.ring)
                            .px_2(),
                    )
                    .into_any_element()
            } else {
                let id = id.clone();
                div()
                    .id(("person", ix))
                    .flex_1()
                    .min_w_0()
                    .h(PERSON_ROW_HEIGHT)
                    .flex()
                    .items_center()
                    .px_2()
                    .rounded(feel::md())
                    .border_1()
                    .border_color(gpui_kit::transparent_black())
                    .cursor_pointer()
                    .lit(theme.muted, cx)
                    .child(div().truncate().child(p.name.clone()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.rename_person(id.clone(), window, cx)
                    }))
                    .test_support()
                    .into_any_element()
            };
            list = list.child(
                row()
                    .gap_2()
                    .child(name)
                    .when(p.is_self, |d| {
                        d.child(div().text_xs().text_color(theme.primary).child("自分"))
                    })
                    .children(scope)
                    .child(feel::gloss(
                        button(("del-person", ix))
                            .label("削除")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.write(cx, |s| s.delete_person(&id));
                            })),
                    )),
            );
        }
        let body = col().gap_3().child(list).child(
            row().child(feel::gloss(
                button("person-add")
                    .icon(app_icon("plus"))
                    .framed(cx)
                    .tooltip("担当者を追加")
                    .on_click(cx.listener(|this, _, window, cx| this.add_person(window, cx))),
            )),
        );
        body
    }

    /// 「設定」。左に検索と目次、まん中にすべての設定を見出しごとに 1 本の縦スクロールで並べ、
    /// 右に見た目のプレビューを出す。左を押すとその見出しへスクロールし、
    /// スクロールすると、いちばん上に見えている見出しを左で光らせる。
    /// 書体と背景の画像の一覧は、この上にもう 1 枚重ねて出す。
    fn render_appearance(&self, cx: &mut Context<Self>) -> Div {
        use crate::state::CustomizeSection as Section;
        let theme = cx.theme().clone();
        let query = self
            .inputs
            .customize_search
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let searching = !query.is_empty();
        let items: Vec<Setting> = self
            .customize_settings(cx)
            .into_iter()
            .filter(|item| !searching || item.matches(&query))
            .collect();
        let shown: Vec<Section> = Section::ALL
            .into_iter()
            .filter(|section| items.iter().any(|item| item.section == *section))
            .collect();
        // 左を押した直後はその見出し、スクロールしたら、いちばん上に見えている見出し
        let active = if self.appearance_pinned {
            self.appearance_section
        } else {
            shown
                .get(self.appearance_scroll.top_item())
                .copied()
                .unwrap_or(Section::Theme)
        };

        // 左: 検索と、見出しでまとめた目次
        let mut nav = col().gap_0p5().child(
            div().pb_2().child(
                Input::new(&self.inputs.customize_search)
                    .id("customize-search")
                    .aria_label("設定を検索")
                    .prefix(app_icon("search").small()),
            ),
        );
        let mut group = "";
        for (ix, section) in Section::ALL.into_iter().enumerate() {
            // 今出せる設定がないカテゴリ（この OS にない連携など）は出さない
            if !shown.contains(&section) {
                continue;
            }
            if section.group() != group {
                group = section.group();
                nav = nav.child(
                    div()
                        .px_3()
                        .pt_2()
                        .pb_0p5()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(group),
                );
            }
            nav = nav.child(
                row().child(
                    self.nav_link(
                        ("customize-nav", ix),
                        app_icon(section.icon()).small(),
                        section.label(),
                        active == section,
                        cx,
                    )
                    // 見出しが多いので、左のメニューより詰めて並べる
                    .py_1()
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.show_appearance_section(section, window, cx)
                    }))
                    .test_support(),
                ),
            );
        }
        // 低いウィンドウで入りきらないときは、目次の列だけスクロールする。版はいちばん下
        let nav = col()
            .w(px(208.))
            .flex_none()
            .min_h_0()
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .id("customize-nav-list")
                    .flex_1()
                    .min_h_0()
                    // 右の端はスクロールバーの場所
                    .pr_3()
                    .child(nav)
                    .overflow_y_scrollbar(),
            )
            .child(
                div()
                    .px_3()
                    .pt_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(concat!("v", env!("CARGO_PKG_VERSION"))),
            );

        // まん中: 見出しとカード。1 つの見出しが、スクロールする所の子 1 つ
        let mut list = div()
            .id("appearance-scroll")
            .size_full()
            .pl_5()
            // 右の端はスクロールバーの場所
            .pr_4()
            .pb_2()
            .track_scroll(&self.appearance_scroll)
            .overflow_y_scroll()
            .on_scroll(cx.listener(|this, _, _, cx| {
                if this.appearance_pinned {
                    this.appearance_pinned = false;
                    cx.notify();
                }
            }));
        let mut group = "";
        for section in &shown {
            let mut block = col().gap_2().pb_6();
            if section.group() != group {
                group = section.group();
                block = block.child(div().px_1().text_xl().child(group));
            }
            let mut card = setting_card(cx);
            let mut first = true;
            for item in items.iter().filter(|item| item.section == *section) {
                card = card.child(self.setting_row(item, first, cx));
                first = false;
            }
            list = list.child(
                block
                    .child(
                        div()
                            .px_1()
                            .text_color(theme.muted_foreground)
                            .child(section.label()),
                    )
                    .child(card),
            );
        }
        if searching && shown.is_empty() {
            list = list.child(
                div()
                    .px_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("見つかりません。"),
            );
        }
        let content = col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .gap_2()
            .child(
                col()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(list.test_support())
                    .vertical_scrollbar(&self.appearance_scroll),
            )
            // 失敗の理由は、どこまでスクロールしていても見える所に出す
            .when_some(self.error.clone(), |d, error| {
                d.child(div().pl_5().text_sm().text_color(theme.danger).child(error))
            });
        let body = row()
            .flex_1()
            .min_h_0()
            .items_stretch()
            .child(nav)
            .child(content)
            .child(self.appearance_preview(cx));
        let sized = |id: &'static str, body: Div, cx: &mut Context<Self>| {
            self.dialog_sized(
                id,
                APPEARANCE_WIDTH,
                Some(APPEARANCE_HEIGHT),
                "設定",
                body,
                cx,
            )
        };
        let (title, width, over) = match self.sub_shown().map(|(sub, _)| sub) {
            None | Some(SubModal::Rules | SubModal::Schedules) => return sized("dialog", body, cx),
            Some(SubModal::Font) => ("フォント", px(440.), self.font_picker(false, cx)),
            Some(SubModal::MonoFont) => ("等幅フォント", px(440.), self.font_picker(true, cx)),
            Some(SubModal::Scene) => ("背景の画像", px(680.), self.scene_picker(cx)),
            Some(SubModal::AddAccount) => {
                let (title, over) = self
                    .caldav_settings
                    .update(cx, |s, cx| (s.add_title(), s.add_dialog(cx)));
                (title, px(440.), over)
            }
            Some(SubModal::DonePeople) => ("知らせる担当者", px(400.), self.done_picker(true, cx)),
            Some(SubModal::DoneWorkspaces) => (
                "知らせるワークスペース",
                px(400.),
                self.done_picker(false, cx),
            ),
            Some(SubModal::EnablePlugin) => {
                let (title, approval) = self.plugin_approval(cx);
                let over = self.dialog_as("dialog", px(440.), title, approval, cx);
                let over = self.sub_motion(over);
                return div()
                    .absolute()
                    .inset_0()
                    .child(sized("dialog-under", body, cx))
                    .child(over);
            }
            Some(SubModal::ClearBrowser) => {
                let body = col()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("アプリ内ブラウザのログインの情報とキャッシュを消します。サイトからはログアウトします。"),
                    )
                    .child(
                        row()
                            .gap_2()
                            .justify_end()
                            .child(feel::gloss(
                                button("browser-clear-cancel")
                                    .label("キャンセル")
                                    .ghost()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sub_modal = None;
                                        cx.notify();
                                    })),
                            ))
                            .child(feel::gloss(
                                button("browser-clear-confirm")
                                    .label("消す")
                                    .danger()
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.clear_browser_data(cx)),
                                    ),
                            )),
                    );
                ("ブラウザのデータを消す", px(440.), body)
            }
        };
        let over = self.dialog_as("dialog", width, title, over, cx);
        let over = self.sub_motion(over);
        div()
            .absolute()
            .inset_0()
            .child(sized("dialog-under", body, cx))
            .child(over)
    }

    /// 「設定」の右のプレビュー。今の見た目（選択肢にマウスを乗せている間は、その選択肢）で、
    /// 左のメニュー・一覧・詳細を小さく描く。一覧の 2 行目は、マウスを乗せたときの見え方。
    fn appearance_preview(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::state::PreviewPick as Pick;
        let theme = cx.theme().clone();
        let mut density = crate::theme::density(cx);
        let mut due = crate::theme::due_format(cx);
        let mut feel_now = crate::theme::feel(cx);
        let mut cards = crate::theme::cards(cx);
        let mut floating = crate::theme::floating(cx);
        let mut overlay = crate::theme::overlay(cx);
        let mut chat = self.ai_docked(cx);
        match self.preview_pick {
            Some(Pick::Density(pick)) => density = pick,
            Some(Pick::Due(pick)) => due = pick,
            Some(Pick::Feel(pick)) => feel_now = pick,
            Some(Pick::Cards(pick)) => cards = pick,
            Some(Pick::Float(pick)) => floating = pick,
            Some(Pick::Layout(ix)) => {
                let layout = crate::theme::Layout::PATTERNS[ix];
                (overlay, cards, floating) = (layout.overlay, layout.cards, layout.floating);
                chat = layout.chat.unwrap_or(chat);
            }
            None => {}
        }
        let image = scene_thumbnail(&crate::theme::scene(cx), theme.is_dark());
        let panel_bg = glass(theme.background, cx);
        let ink = theme.muted_foreground.opacity(0.45);
        let line = |width: f32, color: Hsla| {
            div()
                .w(relative(width))
                .h(px(4.))
                .flex_none()
                .rounded_full()
                .bg(color)
        };
        let radius = if floating { px(8.) } else { px(0.) };
        let panel = || {
            col()
                .min_h_0()
                .gap(px(6.))
                .p(px(8.))
                .bg(panel_bg)
                .rounded(radius)
                .when(floating, |d| d.border_1().border_color(theme.border))
        };
        // 左のメニュー
        let side = panel()
            .w(px(46.))
            .flex_none()
            .when(!floating, |d| d.border_r_1().border_color(theme.border))
            .child(line(0.8, theme.primary))
            .child(line(1., ink))
            .child(line(0.7, ink))
            .child(line(0.85, ink));
        // 一覧。期限は今日から数えた見本
        let today = view::today();
        let due_label =
            |days: i64| view::format_due(None, &view::add_days(&today, days), &today, due);
        let row_height = px(26. * density.rows());
        let mut rows = col().gap(px(if cards { 5. } else { 0. }));
        for (ix, (title, days)) in [
            ("Lorem ipsum", 0),
            ("Dolor sit amet", 1),
            ("Consectetur", 3),
            ("Adipiscing elit", -2),
        ]
        .into_iter()
        .enumerate()
        {
            let hovered = ix == 1;
            rows = rows.child(
                row()
                    .relative()
                    .h(row_height)
                    .flex_none()
                    .px(px(6.))
                    .gap(px(6.))
                    .text_xs()
                    .when(cards, |d| {
                        d.rounded(px(6.))
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.background)
                    })
                    .when(!cards && ix > 0, |d| {
                        d.border_t_1().border_color(theme.border.opacity(0.6))
                    })
                    .when(hovered, |d| {
                        d.child(feel::hover_sample(
                            feel_now,
                            theme.secondary_hover,
                            if cards { px(6.) } else { px(0.) },
                        ))
                    })
                    .child(
                        div()
                            .size(px(9.))
                            .flex_none()
                            .rounded_full()
                            .border_1()
                            .border_color(theme.muted_foreground),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .child(
                        div()
                            .flex_none()
                            .text_color(if days < 0 {
                                theme.danger
                            } else {
                                theme.muted_foreground
                            })
                            .child(due_label(days)),
                    )
                    .when(hovered, |d| {
                        d.child(
                            div().absolute().right(px(18.)).bottom(px(-6.)).child(
                                app_icon("mouse-pointer-2")
                                    .xsmall()
                                    .text_color(theme.foreground),
                            ),
                        )
                    }),
            );
        }
        let list = panel()
            .flex_1()
            .min_w_0()
            .when(cards, |d| {
                d.bg(theme.transparent).border_color(theme.transparent)
            })
            .child(div().text_xs().child("すべてのタスク"))
            .child(rows);
        // 詳細
        let detail = panel()
            .w(px(66.))
            .flex_none()
            .when(!floating && !overlay, |d| {
                d.border_l_1().border_color(theme.border)
            })
            .child(line(0.6, theme.primary))
            .child(line(1., ink))
            .child(line(0.8, ink))
            .child(line(0.9, ink));
        let detail = if overlay {
            detail
                .absolute()
                .top(px(14.))
                .bottom(px(14.))
                .right(px(10.))
                .rounded(px(8.))
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .elevate(Elevation::High, cx)
        } else {
            detail
        };
        // AI bot
        let chat_panel = chat.then(|| {
            panel()
                .w(px(46.))
                .flex_none()
                .when(!floating, |d| d.border_l_1().border_color(theme.border))
                .child(line(0.9, ink))
                .child(line(0.6, ink))
        });
        let frame = row()
            .relative()
            .h(px(236.))
            .items_stretch()
            .rounded(feel::lg())
            .overflow_hidden()
            .border_1()
            .border_color(theme.border)
            .bg(if image.is_some() || floating {
                theme.muted
            } else {
                theme.background
            })
            .children(image.map(|image| {
                img(image)
                    .absolute()
                    .inset_0()
                    .size_full()
                    .object_fit(ObjectFit::Cover)
            }))
            .when(floating, |d| d.p(px(6.)).gap(px(6.)))
            .child(side)
            .child(list)
            .child(detail)
            .children(chat_panel);
        col()
            .w(PREVIEW_WIDTH)
            .flex_none()
            .pl_4()
            .gap_2()
            .border_l_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("プレビュー"),
            )
            .child(frame)
    }

    /// 「設定」に出す見出し。今出せる設定があるものだけ、並ぶ順に。
    pub(crate) fn customize_sections(&self, cx: &App) -> Vec<crate::state::CustomizeSection> {
        let items = self.customize_settings(cx);
        crate::state::CustomizeSection::ALL
            .into_iter()
            .filter(|section| items.iter().any(|item| item.section == *section))
            .collect()
    }

    /// 「設定」に並ぶ設定。名前・補足・カテゴリの名前と、検索で使う言葉を持つ。
    fn customize_settings(&self, cx: &App) -> Vec<Setting> {
        use crate::state::CustomizeSection as Section;
        let mut themes = String::from("プリセット 見本 保存");
        for preset in crate::theme::Preset::recommended_with(&self.fonts)
            .iter()
            .chain(&self.own_presets)
        {
            themes.push(' ');
            themes.push_str(&preset.name);
        }
        let mut layouts = String::from("並べ方 一覧 詳細 チャット");
        for layout in crate::theme::Layout::PATTERNS {
            layouts.push(' ');
            layouts.push_str(layout.name);
        }
        let list = |items: &[&str]| items.join(" ");
        let mut all = vec![
            Setting::new(Section::Theme, SettingKey::Themes, "テーマ", None, themes),
            Setting::new(
                Section::Layout,
                SettingKey::Layouts,
                "レイアウト",
                None,
                layouts,
            ),
            Setting::new(
                Section::Backdrop,
                SettingKey::Scene,
                "背景の画像",
                None,
                list(&["壁紙", "ガラス", "青い空", "森", "夕暮れ"]),
            ),
        ];
        // 透明度とぼかしは、背景の画像を出しているときだけ
        if crate::theme::scene(cx) != crate::theme::Scene::None {
            all.push(Setting::new(
                Section::Backdrop,
                SettingKey::Clear,
                "透明度",
                Some("画像の上の面の透け方"),
                list(&["ガラス", "透明"]),
            ));
            all.push(Setting::new(
                Section::Backdrop,
                SettingKey::Blur,
                "ぼかし",
                Some("背景の画像のぼかし"),
                list(&["ガラス"]),
            ));
        }
        all.push(Setting::new(
            Section::Look,
            SettingKey::Font,
            "フォント",
            None,
            list(&["書体", "文字", "優先順", "フォールバック"]),
        ));
        all.push(Setting::new(
            Section::Look,
            SettingKey::MonoFont,
            "等幅フォント",
            Some("ターミナルとコード"),
            list(&[
                "書体",
                "ターミナル",
                "コード",
                "Nerd Font",
                "優先順",
                "フォールバック",
            ]),
        ));
        for slot in crate::theme::Slot::ALL {
            all.push(Setting::new(
                Section::Look,
                SettingKey::Color(slot),
                slot.label(),
                None,
                list(&["色", "カラー"]),
            ));
        }
        all.extend([
            Setting::new(
                Section::Display,
                SettingKey::Density,
                "密度",
                Some("一覧の行の間隔"),
                crate::theme::Density::ALL.map(|d| d.label()).join(" "),
            ),
            Setting::new(
                Section::Display,
                SettingKey::DueFormat,
                "期限の表示",
                Some("一覧の期限の出し方"),
                list(&["期限", "開始", "日付", "相対", "あと", "残り"]),
            ),
            Setting::new(
                Section::Display,
                SettingKey::Feel,
                "ホバー",
                Some("乗せたとき・押したときの見え方"),
                crate::theme::Feel::ALL.map(|f| f.label()).join(" "),
            ),
            Setting::new(
                Section::Display,
                SettingKey::Cards,
                "カードで表示",
                Some("タスクの一覧"),
                list(&["カード"]),
            ),
            Setting::new(
                Section::Display,
                SettingKey::Float,
                "パネルを浮かせる",
                Some("パネルを角丸のカードにする"),
                list(&["カード", "パネル"]),
            ),
        ]);
        for row_kind in crate::ai_settings::AiRow::ALL {
            if !row_kind.shown(cx) {
                continue;
            }
            let section = match row_kind.group() {
                crate::ai_settings::AiGroup::Model => Section::AiModel,
                crate::ai_settings::AiGroup::Behavior => Section::AiBehavior,
                crate::ai_settings::AiGroup::Rules => Section::AiRules,
                crate::ai_settings::AiGroup::Schedules => Section::AiSchedules,
                crate::ai_settings::AiGroup::Github => Section::AiGithub,
            };
            all.push(Setting::new(
                section,
                SettingKey::Ai(row_kind),
                row_kind.name(),
                row_kind.note(),
                row_kind.words().to_owned(),
            ));
        }
        all.push(Setting::new(
            Section::Lock,
            SettingKey::LockOn,
            "ロック",
            Some("パスキーで開く"),
            list(&["パスキー", "Windows Hello", "Touch ID"]),
        ));
        let locked = self.lock_enabled(cx);
        // 自動でロックする時間と通知は、ロックが有効なときだけ
        if locked {
            all.push(Setting::new(
                Section::Lock,
                SettingKey::AutoLock,
                "自動でロック",
                Some("操作がないままこの時間がたつとロックする"),
                list(&["時間"]),
            ));
            all.push(Setting::new(
                Section::Lock,
                SettingKey::LockNow,
                "今すぐロック",
                Some(crate::shortcuts::LOCK_KEY),
                list(&["ロック"]),
            ));
        }
        all.extend([Setting::new(
            Section::AccessKeys,
            SettingKey::McpKeys,
            "アクセスキー",
            None,
            list(&[
                "MCP",
                "キー",
                "トークン",
                "エージェント",
                "Claude Code",
                "Cursor",
                "Codex",
                "期限",
                "時間帯",
                "セキュリティ",
            ]),
        )]);
        all.extend([
            Setting::new(
                Section::McpConnection,
                SettingKey::McpKeyless,
                "キーなしの接続",
                None,
                list(&[
                    "MCP",
                    "アクセスキー",
                    "エージェント",
                    "Claude Code",
                    "ロック",
                    "時間帯",
                    "セキュリティ",
                ]),
            ),
            Setting::new(
                Section::McpConnection,
                SettingKey::McpHttp,
                "HTTP で開く",
                Some("この PC からだけ"),
                list(&[
                    "MCP",
                    "HTTP",
                    "ポート",
                    "URL",
                    "Streamable HTTP",
                    "Claude Code",
                ]),
            ),
        ]);
        // ポートと URL は、HTTP で開いているときだけ
        if self.mcp_http(cx).0 {
            all.extend([
                Setting::new(
                    Section::McpConnection,
                    SettingKey::McpHttpPort,
                    "ポート",
                    None,
                    list(&["MCP", "HTTP", "ポート"]),
                ),
                Setting::new(
                    Section::McpConnection,
                    SettingKey::McpHttpUrl,
                    "URL",
                    None,
                    list(&["MCP", "HTTP", "URL", "アドレス"]),
                ),
            ]);
        }
        all.extend([
            Setting::new(
                Section::Notices,
                SettingKey::AlertTimed,
                "時刻つきの期限",
                None,
                list(&["期限", "通知", "リマインダー", "分前", "時間前"]),
            ),
            Setting::new(
                Section::Notices,
                SettingKey::AlertDate,
                "日付だけの期限",
                None,
                list(&["期限", "通知", "リマインダー", "当日", "前日"]),
            ),
            Setting::new(
                Section::Notices,
                SettingKey::AlertHour,
                "日付だけの期限の時刻",
                None,
                list(&["期限", "通知", "時刻"]),
            ),
            Setting::new(
                Section::Notices,
                SettingKey::DoneOn,
                "ほかの人の完了",
                None,
                list(&["完了", "通知", "担当者", "完了チケット"]),
            ),
        ]);
        if self.done_notices(cx).enabled {
            all.extend([
                Setting::new(
                    Section::Notices,
                    SettingKey::DoneWhen,
                    "知らせるとき",
                    None,
                    list(&["完了", "通知", "1 人ずつ"]),
                ),
                Setting::new(
                    Section::Notices,
                    SettingKey::DoneRoles,
                    "完了にした人の役",
                    None,
                    list(&["完了", "通知", "役", "RACI", "R", "A", "C", "I"]),
                ),
                Setting::new(
                    Section::Notices,
                    SettingKey::DonePeople,
                    "知らせる担当者",
                    None,
                    list(&["完了", "通知", "担当者", "人"]),
                ),
                Setting::new(
                    Section::Notices,
                    SettingKey::DoneWorkspaces,
                    "知らせるワークスペース",
                    None,
                    list(&["完了", "通知", "ワークスペース"]),
                ),
            ]);
        }
        // macOS のリマインダーは「同期」のアカウントに並べる。ここは Windows の通知の予約
        if let Some(name) =
            crate::reminders::setting_label(cx).filter(|_| !crate::reminders::is_app(cx))
        {
            all.push(Setting::new(
                Section::Notices,
                SettingKey::Reminders,
                name,
                None,
                list(&["リマインダー", "期限", "通知"]),
            ));
        }
        if locked {
            all.push(Setting::new(
                Section::Notices,
                SettingKey::LockedNotices,
                "ロック中の通知",
                None,
                list(&["ロック"]),
            ));
        }
        if crate::autostart::available(cx) {
            all.push(Setting::new(
                Section::Startup,
                SettingKey::Autostart,
                "ログインしたときに起動",
                None,
                list(&["スタートアップ", "自動起動", "起動", "ログイン"]),
            ));
        }
        if crate::update::menu_label(cx).is_some() {
            all.push(Setting::new(
                Section::Update,
                SettingKey::AutoUpdate,
                "自動アップデート",
                Some("ウィンドウを閉じたときに新しい版にする"),
                list(&["更新", "バージョン"]),
            ));
        }
        let mut people = String::from("人 名前 追加 削除 自分 ワークスペース");
        for person in &self.people {
            people.push(' ');
            people.push_str(&person.name);
        }
        let mut keys = String::from("キー ショートカット ペイン ブラウザ ターミナル");
        for (key, about) in crate::shortcuts::help()
            .into_iter()
            .chain(crate::shell_keys::browser_help())
            .chain(crate::shell_keys::terminal_help())
        {
            keys.push(' ');
            keys.push_str(key);
            keys.push(' ');
            keys.push_str(about);
        }
        all.extend([
            Setting::new(Section::People, SettingKey::People, "担当者", None, people),
            Setting::new(
                Section::Sync,
                SettingKey::CalDav,
                if crate::reminders::is_app(cx) {
                    "CalDAV・リマインダー同期"
                } else {
                    "CalDAV 同期"
                },
                None,
                list(&[
                    "同期",
                    "CalDAV",
                    "サーバー",
                    "カレンダー",
                    "Fastmail",
                    "Nextcloud",
                    "リマインダー",
                    "アカウント",
                ]),
            ),
            Setting::new(Section::Keys, SettingKey::Keys, "キー操作", None, keys),
            Setting::new(
                Section::Window,
                SettingKey::NewWindow,
                "新しいウィンドウ",
                Some(crate::shortcuts::NEW_WINDOW_KEY),
                list(&["ウィンドウ", "開く"]),
            ),
            Setting::new(
                Section::Window,
                SettingKey::Pomodoro,
                "ポモドーロ",
                None,
                list(&["ウィンドウ", "タイマー", "作業時間", "休憩", "音楽"]),
            ),
            Setting::new(
                Section::Window,
                SettingKey::Timer,
                "タイマー",
                None,
                list(&[
                    "ウィンドウ",
                    "カウントダウン",
                    "ストップウォッチ",
                    "アラーム",
                ]),
            ),
            Setting::new(
                Section::Plugins,
                SettingKey::Plugins,
                "プラグイン",
                None,
                list(&["拡張", "拡張機能", "プラグイン", "WASM", "有効", "無効"]),
            ),
            Setting::new(
                Section::Plugins,
                SettingKey::PluginFolders,
                "フォルダー",
                None,
                list(&["プラグイン", "フォルダー", "場所", malakt_plugins::ENV_DIRS]),
            ),
            Setting::new(
                Section::Data,
                SettingKey::Export,
                "エクスポート",
                Some("すべてのワークスペースをファイルに書き出す"),
                list(&["書き出し", "バックアップ", "保存"]),
            ),
            Setting::new(
                Section::Data,
                SettingKey::Import,
                "インポート",
                Some("書き出したファイルを読み込む"),
                list(&["読み込み", "復元"]),
            ),
            Setting::new(
                Section::Data,
                SettingKey::BrowserData,
                "ブラウザのデータ",
                Some("アプリ内ブラウザのログインの情報とキャッシュ"),
                list(&[
                    "ブラウザ",
                    "Cookie",
                    "キャッシュ",
                    "ログイン",
                    "使用量",
                    "容量",
                ]),
            ),
        ]);
        all
    }

    /// 設定 1 つ分の行。左に名前と補足、右に変えるもの。テーマとレイアウトは見本を並べる。
    fn setting_row(&self, item: &Setting, first: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let edge = |d: Div| {
            d.when(!first, |d| {
                d.border_t_1().border_color(theme.border.opacity(0.6))
            })
        };
        match item.key {
            SettingKey::Themes => {
                return edge(div().py_3())
                    .child(self.preset_tiles(cx))
                    .into_any_element();
            }
            SettingKey::Layouts => {
                return edge(div().py_3())
                    .child(self.layout_tiles(cx))
                    .into_any_element();
            }
            SettingKey::Ai(row_kind) => {
                let control = self
                    .ai_settings
                    .update(cx, |settings, cx| settings.control(row_kind, cx));
                return setting_line(
                    item.name,
                    item.note,
                    control,
                    row_kind.wide(),
                    first,
                    &theme,
                );
            }
            SettingKey::AlertTimed | SettingKey::AlertDate => {
                return edge(div())
                    .child(self.due_alert_row(item, item.key == SettingKey::AlertTimed, cx))
                    .into_any_element();
            }
            SettingKey::People => {
                return edge(div().py_3())
                    .child(self.people_editor(cx))
                    .into_any_element();
            }
            SettingKey::CalDav => {
                return setting_line(
                    item.name,
                    item.note,
                    self.caldav_settings.clone().into_any_element(),
                    true,
                    first,
                    &theme,
                );
            }
            SettingKey::Keys => {
                return edge(div().py_3())
                    .child(self.keys_help(cx))
                    .into_any_element();
            }
            SettingKey::McpKeys => {
                return edge(div().py_3())
                    .child(self.access_keys_editor(cx))
                    .into_any_element();
            }
            SettingKey::McpKeyless => {
                return setting_line(
                    item.name,
                    item.note,
                    self.keyless_editor(cx).into_any_element(),
                    true,
                    first,
                    &theme,
                );
            }
            SettingKey::Plugins => {
                return edge(div().py_3())
                    .child(self.plugins_editor(cx))
                    .into_any_element();
            }
            SettingKey::PluginFolders => {
                return setting_line(
                    item.name,
                    item.note,
                    self.plugin_folders_editor(cx).into_any_element(),
                    true,
                    first,
                    &theme,
                );
            }
            _ => {}
        }
        use crate::state::PreviewPick as Pick;
        let look = crate::theme::look(cx);
        let reset = |id: &'static str, shown: bool| {
            button(id)
                .icon(app_icon("rotate-ccw"))
                .ghost()
                .small()
                .tooltip("標準に戻す")
                // 場所は取っておき、標準のままのときは見せない
                .when(!shown, |b| b.invisible().disabled(true))
        };
        // 押すと一覧が重なって開くもの（書体、背景の画像）
        let opener = |id: &'static str, text: String, cx: &mut Context<Self>| {
            row()
                .id(id)
                .w(CONTROL_WIDTH)
                .h_8()
                .px_3()
                .gap_2()
                .rounded(feel::md())
                .border_1()
                .border_color(theme.input)
                .cursor_pointer()
                .text_sm()
                .lit(theme.accent, cx)
                .sunk(theme.primary.opacity(0.4), cx)
                .child(div().flex_1().truncate().child(text))
                .child(
                    app_icon("chevron-right")
                        .small()
                        .text_color(theme.muted_foreground),
                )
        };
        let slider = |id: &'static str, state: &Entity<SliderState>, name: &'static str| {
            div()
                .id(id)
                .w(CONTROL_WIDTH)
                .child(Slider::new(state).accessibility_label(name))
                .test_support()
        };
        let control: AnyElement =
            match item.key {
                SettingKey::Themes
                | SettingKey::Layouts
                | SettingKey::Ai(_)
                | SettingKey::AlertTimed
                | SettingKey::AlertDate
                | SettingKey::People
                | SettingKey::CalDav
                | SettingKey::McpKeys
                | SettingKey::McpKeyless
                | SettingKey::Plugins
                | SettingKey::PluginFolders
                | SettingKey::Keys => unreachable!(),
                SettingKey::LockNow => feel::gloss(
                    button("lock-now")
                        .icon(app_icon("lock"))
                        .label("ロック")
                        .outline()
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.close_top_dialog(window, cx);
                            window.dispatch_action(Box::new(LockNow), cx)
                        })),
                )
                .into_any_element(),
                SettingKey::NewWindow => feel::gloss(
                    button("new-window")
                        .icon(app_icon("app-window"))
                        .label("開く")
                        .outline()
                        .small()
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(NewWindow), cx)),
                )
                .into_any_element(),
                SettingKey::Pomodoro => feel::gloss(
                    button("open-pomodoro")
                        .icon(app_icon("timer"))
                        .label("開く")
                        .outline()
                        .small()
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(crate::pomodoro::OpenPomodoro), cx)
                        }),
                )
                .into_any_element(),
                SettingKey::Timer => feel::gloss(
                    button("open-timer")
                        .icon(app_icon("alarm-clock"))
                        .label("開く")
                        .outline()
                        .small()
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(crate::timer::OpenTimer), cx)
                        }),
                )
                .into_any_element(),
                SettingKey::AlertHour => {
                    let hour = self.due_alert_standard(cx).hour;
                    row()
                        .gap_1()
                        .flex_wrap()
                        .justify_end()
                        .children(
                            malakt_core::alert::HOUR_CHOICES
                                .into_iter()
                                .enumerate()
                                .map(|(ix, h)| {
                                    chip(
                                        ("alert-hour", ix),
                                        malakt_core::alert::hour_label(h),
                                        hour == h,
                                        cx,
                                    )
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.set_due_alert_hour(h, cx);
                                    }))
                                    .test_support()
                                }),
                        )
                        .child(
                            reset(
                                "alert-hour-reset",
                                hour != malakt_core::alert::STANDARD_HOUR,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_due_alert_hour(malakt_core::alert::STANDARD_HOUR, cx);
                            })),
                        )
                        .into_any_element()
                }
                SettingKey::Scene => {
                    let scene = crate::theme::scene(cx);
                    let has_scene = scene != crate::theme::Scene::None;
                    row()
                        .gap_1()
                        .child(
                            opener("scene-open", scene.label(), cx)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.open_scene_picker(cx)
                                }))
                                .test_support(),
                        )
                        .child(reset("scene-reset", has_scene).on_click(cx.listener(
                            |this, _, window, cx| {
                                this.set_scene(crate::theme::Scene::None, window, cx)
                            },
                        )))
                        .into_any_element()
                }
                SettingKey::Clear => row()
                    .gap_1()
                    .child(slider("clear-slider", &self.clear_slider, "透明度"))
                    .child(reset("clear-reset", false).placeholder())
                    .into_any_element(),
                SettingKey::Blur => row()
                    .gap_1()
                    .child(slider("blur-slider", &self.blur_slider, "ぼかし"))
                    .child(reset("blur-reset", false).placeholder())
                    .into_any_element(),
                SettingKey::Font | SettingKey::MonoFont => {
                    let mono = item.key == SettingKey::MonoFont;
                    let (open, clear) = if mono {
                        ("mono-font-open", "mono-font-reset")
                    } else {
                        ("font-open", "font-reset")
                    };
                    let chosen = if mono {
                        crate::theme::mono_fonts(cx).is_some()
                    } else {
                        !look.fonts.is_empty()
                    };
                    row()
                        .gap_1()
                        .child(
                            opener(open, self.font_list(mono, cx).join(", "), cx)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.open_font_picker(mono, window, cx)
                                }))
                                .test_support(),
                        )
                        .child(reset(clear, chosen).on_click(
                            cx.listener(move |this, _, _, cx| this.reset_fonts(mono, cx)),
                        ))
                        .into_any_element()
                }
                SettingKey::Color(slot) => {
                    let ix = crate::theme::Slot::ALL
                        .iter()
                        .position(|s| *s == slot)
                        .unwrap_or(0);
                    let id = ["accent-reset", "light-reset", "dark-reset"][ix];
                    row()
                        .gap_1()
                        .child(
                            div()
                                .id(("color-open", ix))
                                .child(
                                    ColorPicker::new(&self.color_pickers[ix])
                                        .accessibility_label(slot.label())
                                        .small(),
                                )
                                .test_support(),
                        )
                        .child(reset(id, look.color(slot).is_some()).on_click(cx.listener(
                            move |this, _, window, cx| this.set_color(slot, None, window, cx),
                        )))
                        .into_any_element()
                }
                SettingKey::Density => row()
                    .gap_1()
                    .children(crate::theme::Density::ALL.into_iter().enumerate().map(
                        |(ix, density)| {
                            chip(
                                ("density", ix),
                                density.label(),
                                crate::theme::density(cx) == density,
                                cx,
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.set_density(density, cx)
                            }))
                            .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                                this.preview_hover(Pick::Density(density), *on, cx)
                            }))
                            .test_support()
                        },
                    ))
                    .child(reset("density-reset", false).placeholder())
                    .into_any_element(),
                SettingKey::DueFormat => {
                    row()
                        .gap_1()
                        .children(view::DueFormat::ALL.into_iter().enumerate().map(
                            |(ix, format)| {
                                chip(
                                    ("due-format", ix),
                                    format.label(),
                                    crate::theme::due_format(cx) == format,
                                    cx,
                                )
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.set_due_format(format, cx)
                                }))
                                .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                                    this.preview_hover(Pick::Due(format), *on, cx)
                                }))
                                .test_support()
                            },
                        ))
                        .child(reset("due-format-reset", false).placeholder())
                        .into_any_element()
                }
                SettingKey::Feel => {
                    row()
                        .gap_1()
                        .children(crate::theme::Feel::ALL.into_iter().enumerate().map(
                            |(ix, feel)| {
                                chip(
                                    ("feel", ix),
                                    feel.label(),
                                    crate::theme::feel(cx) == feel,
                                    cx,
                                )
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.set_feel(feel, cx)
                                }))
                                .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                                    this.preview_hover(Pick::Feel(feel), *on, cx)
                                }))
                                .test_support()
                            },
                        ))
                        .child(reset("feel-reset", false).placeholder())
                        .into_any_element()
                }
                SettingKey::Cards => row()
                    .gap_1()
                    .child({
                        let on = crate::theme::cards(cx);
                        div()
                            .id("cards-preview")
                            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                                this.preview_hover(Pick::Cards(!on), *hovered, cx)
                            }))
                            .child(Switch::new("cards-switch").checked(on).on_click(
                                cx.listener(|this, on: &bool, _, cx| this.set_cards(*on, cx)),
                            ))
                    })
                    .child(reset("cards-reset", false).placeholder())
                    .into_any_element(),
                SettingKey::Float => row()
                    .gap_1()
                    .child({
                        let on = crate::theme::floating(cx);
                        div()
                            .id("float-preview")
                            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                                this.preview_hover(Pick::Float(!on), *hovered, cx)
                            }))
                            .child(Switch::new("float-switch").checked(on).on_click(
                                cx.listener(|this, on: &bool, _, cx| this.set_floating(*on, cx)),
                            ))
                    })
                    .child(reset("float-reset", false).placeholder())
                    .into_any_element(),
                SettingKey::DoneOn => Switch::new("done-switch")
                    .checked(self.done_notices(cx).enabled)
                    .on_click(cx.listener(|this, on: &bool, _, cx| {
                        let mut notices = this.done_notices(cx);
                        notices.enabled = *on;
                        this.set_done_notices(&notices, cx);
                    }))
                    .into_any_element(),
                SettingKey::DoneWhen => {
                    use malakt_core::done_notice::When;
                    let view = cx.entity().downgrade();
                    let when = self.done_notices(cx).when;
                    menu_choice("done-when", when.label().into(), &theme)
                        .pop_menu(move |mut menu, _, _| {
                            for option in When::ALL {
                                let view = view.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(option.label())
                                        .checked(when == option)
                                        .on_click(move |_, _, cx| {
                                            // ウィンドウを閉じたあとなら、反映する先がない
                                            let _ = view.update(cx, |this, cx| {
                                                let mut notices = this.done_notices(cx);
                                                notices.when = option;
                                                this.set_done_notices(&notices, cx);
                                            });
                                        }),
                                );
                            }
                            menu
                        })
                        .into_any_element()
                }
                SettingKey::DoneRoles => {
                    let roles = self.done_notices(cx).roles;
                    row()
                        .gap_1()
                        .justify_end()
                        .children(malakt_core::Role::ALL.into_iter().enumerate().map(
                            |(ix, role)| {
                                chip(("done-role", ix), role.code(), roles.contains(role), cx)
                                    .tooltip(move |window, cx| {
                                        gpui_kit::component::tooltip::Tooltip::new(role.label())
                                            .build(window, cx)
                                    })
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.toggle_done_role(role, cx);
                                    }))
                                    .test_support()
                            },
                        ))
                        .child(
                            reset(
                                "done-roles-reset",
                                roles != malakt_core::done_notice::all_roles(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                let mut notices = this.done_notices(cx);
                                notices.roles = malakt_core::done_notice::all_roles();
                                this.set_done_notices(&notices, cx);
                            })),
                        )
                        .into_any_element()
                }
                SettingKey::DonePeople | SettingKey::DoneWorkspaces => {
                    let people = item.key == SettingKey::DonePeople;
                    let notices = self.done_notices(cx);
                    let muted = if people {
                        notices.muted_people.len()
                    } else {
                        notices.muted_workspaces.len()
                    };
                    let text = match (people, muted) {
                        (true, 0) => "全員".to_owned(),
                        (false, 0) => "すべて".to_owned(),
                        (true, n) => format!("{n} 人を除く"),
                        (false, n) => format!("{n} 個を除く"),
                    };
                    let (id, reset_id) = if people {
                        ("done-people-open", "done-people-reset")
                    } else {
                        ("done-workspaces-open", "done-workspaces-reset")
                    };
                    row()
                        .gap_1()
                        .child(
                            opener(id, text, cx)
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.open_done_picker(people, cx)
                                }))
                                .test_support(),
                        )
                        .child(reset(reset_id, muted > 0).on_click(cx.listener(
                            move |this, _, _, cx| {
                                let mut notices = this.done_notices(cx);
                                if people {
                                    notices.muted_people.clear();
                                } else {
                                    notices.muted_workspaces.clear();
                                }
                                this.set_done_notices(&notices, cx);
                            },
                        )))
                        .into_any_element()
                }
                SettingKey::LockOn => Switch::new("lock-switch")
                    .checked(self.lock_enabled(cx))
                    .disabled(self.verifying(cx))
                    .on_click(cx.listener(|this, on: &bool, window, cx| {
                        if *on {
                            this.enable_lock(window, cx)
                        } else {
                            this.disable_lock(window, cx)
                        }
                    }))
                    .into_any_element(),
                SettingKey::AutoLock => {
                    let view = cx.entity().downgrade();
                    let auto = self.auto_lock(cx);
                    menu_choice("auto-lock", auto_lock_label(auto).into(), &theme)
                        .pop_menu(move |mut menu, _, _| {
                            let options = std::iter::once(None)
                                .chain(malakt_core::lock::AUTO_CHOICES.into_iter().map(Some));
                            for minutes in options {
                                let view = view.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(auto_lock_label(minutes))
                                        .checked(auto == minutes)
                                        .on_click(move |_, _, cx| {
                                            // ウィンドウを閉じたあとなら、反映する先がない
                                            let _ = view.update(cx, |this, cx| {
                                                this.set_auto_lock(minutes, cx);
                                            });
                                        }),
                                );
                            }
                            menu
                        })
                        .into_any_element()
                }
                SettingKey::McpHttp => Switch::new("mcp-http-switch")
                    .checked(self.mcp_http(cx).0)
                    .on_click(cx.listener(|this, on: &bool, _, cx| this.set_mcp_http(*on, cx)))
                    .into_any_element(),
                SettingKey::McpHttpPort => self.mcp_port_control(cx),
                SettingKey::McpHttpUrl => self.mcp_url_control(cx),
                SettingKey::LockedNotices => {
                    let view = cx.entity().downgrade();
                    let notices = self.locked_notices(cx);
                    menu_choice("locked-notices", notices.label().into(), &theme)
                        .pop_menu(move |mut menu, _, _| {
                            for option in malakt_core::lock::LockedNotices::ALL {
                                let view = view.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(option.label())
                                        .checked(notices == option)
                                        .on_click(move |_, _, cx| {
                                            // ウィンドウを閉じたあとなら、反映する先がない
                                            let _ = view.update(cx, |this, cx| {
                                                this.set_locked_notices(option, cx);
                                            });
                                        }),
                                );
                            }
                            menu
                        })
                        .into_any_element()
                }
                SettingKey::Reminders => Switch::new("reminders-switch")
                    .checked(crate::reminders::enabled(cx))
                    .on_click(cx.listener(|this, _: &bool, _, cx| this.toggle_reminders(cx)))
                    .into_any_element(),
                SettingKey::Autostart => Switch::new("autostart-switch")
                    .checked(crate::autostart::enabled(cx))
                    .on_click(cx.listener(|this, _: &bool, _, cx| this.toggle_autostart(cx)))
                    .into_any_element(),
                SettingKey::AutoUpdate => Switch::new("auto-update-switch")
                    .checked(crate::update::enabled(cx))
                    .on_click(cx.listener(|this, _: &bool, _, cx| this.toggle_auto_update(cx)))
                    .into_any_element(),
                SettingKey::Export => feel::gloss(
                    button("export-all")
                        .label("エクスポート")
                        .outline()
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| this.export(window, cx))),
                )
                .into_any_element(),
                SettingKey::Import => feel::gloss(
                    button("import-all")
                        .label("インポート")
                        .outline()
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| this.import(window, cx))),
                )
                .into_any_element(),
                SettingKey::BrowserData => {
                    let size = crate::term::shared(cx).read(cx).usage;
                    row()
                        .gap_3()
                        .child(
                            div()
                                .id("browser-usage")
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(match size {
                                    Some(size) => view::format_bytes(size),
                                    None => "…".to_owned(),
                                })
                                .test_support(),
                        )
                        .child(feel::gloss(
                            button("browser-clear")
                                .label("消す")
                                .outline()
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.sub_modal = Some(SubModal::ClearBrowser);
                                    cx.notify();
                                })),
                        ))
                        .into_any_element()
                }
            };
        setting_line(item.name, item.note, control, false, first, &theme)
    }

    /// 期限をいつ知らせるかの行。上に名前、下に選べるものを並べ、押すたびに入れる・外す。
    fn due_alert_row(&self, item: &Setting, timed: bool, cx: &mut Context<Self>) -> Div {
        use malakt_core::alert;
        let standard = self.due_alert_standard(cx);
        let (choices, label) = alert_kind(timed);
        let (chosen, base, prefix, reset_id) = if timed {
            (
                standard.timed,
                alert::STANDARD_TIMED,
                "alert-timed",
                "alert-timed-reset",
            )
        } else {
            (
                standard.date,
                alert::STANDARD_DATE,
                "alert-date",
                "alert-date-reset",
            )
        };
        let changed = chosen != base;
        let chips = row()
            .gap_1()
            .flex_wrap()
            .children(choices.iter().copied().enumerate().map(|(ix, value)| {
                chip((prefix, ix), label(value), chosen.contains(&value), cx)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if timed {
                            this.toggle_due_alert_timed(value, cx);
                        } else {
                            this.toggle_due_alert_date(value, cx);
                        }
                    }))
                    .test_support()
            }));
        col()
            .py_3()
            .gap_2()
            .child(
                row()
                    .gap_4()
                    .child(div().flex_1().min_w_0().text_sm().child(item.name))
                    .child(
                        button(reset_id)
                            .icon(app_icon("rotate-ccw"))
                            .ghost()
                            .small()
                            .tooltip("標準に戻す")
                            .when(!changed, |b| b.invisible().disabled(true))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if timed {
                                    this.set_due_alert_timed(base, cx);
                                } else {
                                    this.set_due_alert_date(base, cx);
                                }
                            })),
                    ),
            )
            .child(chips)
    }

    /// テーマの見本。おすすめ、自分で保存したもの、今のテーマを保存するボタンの順に並べる。
    fn preset_tiles(&self, cx: &mut Context<Self>) -> Div {
        use crate::theme::{Preset, Scene};
        let theme = cx.theme().clone();
        let now = crate::theme::current("", cx);
        let frame = |id: ElementId, selected: bool| {
            col()
                .id(id)
                .w(PRESET_WIDTH)
                .gap_1()
                .p_1()
                .rounded(feel::lg())
                .cursor_pointer()
                .border_2()
                .border_color(if selected {
                    theme.primary
                } else {
                    theme.transparent
                })
                .lit(theme.accent, cx)
                .sunk(theme.primary.opacity(0.4), cx)
        };
        // 見本は、そのテーマの画像と色で、浮かせたパネルの形を小さく描く
        let picture = |preset: &Preset| {
            let dark = preset.dark(theme.is_dark());
            let (accent, page) = preset.swatch(dark);
            let (accent, page): (Hsla, Hsla) = (accent.into(), page.into());
            let ink = if dark {
                gpui_kit::white()
            } else {
                gpui_kit::black()
            };
            let image = scene_thumbnail(&preset.scene, dark);
            let seen = image.is_some() || matches!(preset.scene, Scene::File(_));
            let panel = if seen {
                page.opacity(1. - f32::from(preset.clear) / 100.)
            } else {
                page.blend(ink.opacity(0.06))
            };
            let inset = if preset.floating { px(6.) } else { px(0.) };
            let radius = if preset.floating { px(4.) } else { px(0.) };
            let part = |fill: Hsla| div().rounded(radius).bg(fill);
            div()
                .relative()
                .h(px(56.))
                .rounded(feel::md())
                .overflow_hidden()
                .border_1()
                .border_color(theme.border)
                .bg(page)
                .children(image.map(|image| {
                    img(image)
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(ObjectFit::Cover)
                }))
                .when(matches!(preset.scene, Scene::File(_)), |d| {
                    d.child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(app_icon("image").text_color(ink.opacity(0.5))),
                    )
                })
                .child(
                    row()
                        .absolute()
                        .inset(inset)
                        .items_stretch()
                        .gap(inset.min(px(3.)).max(px(1.)))
                        .child(part(panel).w(px(22.)))
                        .child(
                            part(panel).flex_1().p(px(5.)).child(
                                col()
                                    .gap(px(4.))
                                    .child(div().w(px(26.)).h(px(5.)).rounded_full().bg(accent))
                                    .child(
                                        div()
                                            .w_full()
                                            .h(px(3.))
                                            .rounded_full()
                                            .bg(ink.opacity(0.45)),
                                    )
                                    .child(
                                        div()
                                            .w(relative(0.6))
                                            .h(px(3.))
                                            .rounded_full()
                                            .bg(ink.opacity(0.3)),
                                    ),
                            ),
                        ),
                )
        };
        let name = |text: SharedString| div().h_6().px_1().text_sm().truncate().child(text);
        let mut tiles = row().flex_wrap().items_start().gap_1();
        for (ix, preset) in Preset::recommended_with(&self.fonts)
            .into_iter()
            .enumerate()
        {
            let selected = preset.same_look(&now);
            tiles = tiles.child(
                frame(("preset", ix).into(), selected)
                    .child(picture(&preset))
                    .child(name(preset.name.clone().into()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.apply_preset(&preset, window, cx)
                    }))
                    .test_support(),
            );
        }
        for (ix, preset) in self.own_presets.iter().enumerate() {
            let group = SharedString::from(format!("own-preset-{ix}"));
            let label = if self.preset_naming == Some(ix) {
                // その場で名前を書く。Enter かほかの場所を押すと保存、Esc で取り消し
                div()
                    .h_6()
                    .on_mouse_down_out(
                        cx.listener(|this, _, window, cx| this.save_preset_name(window, cx)),
                    )
                    // 名前を書いている間は、見本を押したことにしない
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Input::new(&self.inputs.preset_name)
                            .id("preset-input")
                            .aria_label("テーマの名前")
                            .text_sm()
                            .h_6()
                            .bg(theme.background)
                            .border_color(theme.ring)
                            .px_1(),
                    )
                    .into_any_element()
            } else {
                name(preset.name.clone().into())
                    .id(("own-preset-name", ix))
                    .rounded_sm()
                    .lit(theme.muted, cx)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        this.rename_preset(ix, window, cx)
                    }))
                    .test_support()
                    .into_any_element()
            };
            let chosen = preset.clone();
            tiles = tiles.child(
                frame(("own-preset", ix).into(), preset.same_look(&now))
                    .group(group.clone())
                    .relative()
                    .child(picture(preset))
                    .child(label)
                    .child(
                        div()
                            .absolute()
                            .top(px(6.))
                            .right(px(6.))
                            .rounded(feel::md())
                            .bg(theme.background)
                            .opacity(0.)
                            .group_hover(group, |s| s.opacity(1.))
                            .child(feel::gloss(
                                button(("own-preset-delete", ix))
                                    .icon(app_icon("trash"))
                                    .ghost()
                                    .small()
                                    .tooltip("削除")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.delete_preset(ix, cx)
                                    })),
                            )),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.apply_preset(&chosen, window, cx)
                    }))
                    .test_support(),
            );
        }
        tiles.child(
            frame("preset-save".into(), false)
                .child(
                    div()
                        .h(px(56.))
                        .rounded(feel::md())
                        .border_1()
                        .border_dashed()
                        .border_color(theme.input)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(app_icon("plus").text_color(theme.muted_foreground)),
                )
                .child(name("今のテーマを保存".into()))
                .on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.save_preset(window, cx)),
                )
                .test_support(),
        )
    }

    /// レイアウトの見本。一覧・詳細・AI bot の並び方を、小さい図で出す。
    fn layout_tiles(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let chat = self.ai_docked(cx);
        let line = |width: f32| {
            div()
                .w(relative(width))
                .h(px(3.))
                .rounded_full()
                .bg(theme.muted_foreground.opacity(0.5))
        };
        let part = |round: bool| {
            col()
                .p(px(4.))
                .gap(px(3.))
                .bg(theme.background)
                .border_1()
                .border_color(theme.border)
                .when(round, |d| d.rounded(px(4.)))
        };
        let picture = |layout: &crate::theme::Layout| {
            let float = layout.floating;
            let list = if layout.cards {
                // 一覧の 1 行ずつがカード
                col()
                    .flex_1()
                    .p(px(3.))
                    .gap(px(3.))
                    .children((0..3).map(|_| part(true).child(line(0.7))))
            } else {
                part(float)
                    .flex_1()
                    .child(line(0.8))
                    .child(line(0.6))
                    .child(line(0.7))
                    .child(line(0.5))
            };
            let side = |accent: bool| {
                part(float)
                    .w(px(30.))
                    .child(
                        div()
                            .w(relative(0.7))
                            .h(px(4.))
                            .rounded_full()
                            .bg(if accent {
                                theme.primary
                            } else {
                                theme.muted_foreground.opacity(0.5)
                            }),
                    )
                    .child(line(1.))
                    .child(line(0.8))
            };
            row()
                .relative()
                .h(px(56.))
                .items_stretch()
                .rounded(feel::md())
                .overflow_hidden()
                .border_1()
                .border_color(theme.border)
                .bg(theme.muted)
                .when(float, |d| d.p(px(5.)).gap(px(4.)))
                .child(list)
                .when(!layout.overlay, |d| d.child(side(true)))
                .when(layout.chat == Some(true), |d| d.child(side(false)))
                // 重ねる詳細は、一覧の上に影つきで乗る
                .when(layout.overlay, |d| {
                    d.child(
                        side(true)
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .right_0()
                            .w(px(44.))
                            .elevate(Elevation::High, cx),
                    )
                })
        };
        let mut tiles = row().flex_wrap().items_start().gap_1();
        for (ix, layout) in crate::theme::Layout::PATTERNS.into_iter().enumerate() {
            tiles = tiles.child(
                col()
                    .id(("layout", ix))
                    .w(PRESET_WIDTH)
                    .gap_1()
                    .p_1()
                    .rounded(feel::lg())
                    .cursor_pointer()
                    .border_2()
                    .border_color(if layout.shown(chat, cx) {
                        theme.primary
                    } else {
                        theme.transparent
                    })
                    .lit(theme.accent, cx)
                    .sunk(theme.primary.opacity(0.4), cx)
                    .child(picture(&layout))
                    .child(div().h_6().px_1().text_sm().truncate().child(layout.name))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.apply_layout(&layout, window, cx)
                    }))
                    .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                        this.preview_hover(crate::state::PreviewPick::Layout(ix), *on, cx)
                    }))
                    .test_support(),
            );
        }
        tiles
    }

    /// 書体の優先順と、この PC に入っている書体の一覧。上の行はドラッグで並べ替え、× で外す。
    /// 下の一覧を押すと末尾に足し、入っているものを押すと外す。
    fn font_picker(&self, mono: bool, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let chosen = self.font_list(mono, cx);
        let only = chosen.len() == 1;
        let drag = self.font_drag.clone().filter(|d| d.mono == mono);
        let mut chain = col().id("font-chain").gap_1();
        for (ix, name) in chosen.iter().enumerate() {
            let missing = !crate::fonts::is_installed(name, cx);
            let dragged = drag.as_ref().is_some_and(|d| d.moved && d.from == ix);
            // 落とす場所の印。上から下へ動かすときは行の下、下から上へは行の上
            let mark = drag
                .as_ref()
                .filter(|d| d.moved && d.over == Some(ix) && d.from != ix)
                .map(|d| d.from < ix);
            let row_name = name.clone();
            let line = |below: bool| {
                div()
                    .absolute()
                    .left_2()
                    .right_2()
                    .h(px(2.))
                    .rounded_full()
                    .bg(theme.ring)
                    .when(below, |d| d.bottom(px(-3.)))
                    .when(!below, |d| d.top(px(-3.)))
            };
            let item = row()
                .id(("font-chain-row", ix))
                .relative()
                .w_full()
                .h(px(40.))
                .pl_2()
                .pr_1()
                .gap_2()
                .rounded(feel::md())
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .cursor_grab()
                .when(dragged, |d| d.opacity(0.4))
                .lit(theme.accent.opacity(0.6), cx)
                .child(
                    app_icon("grip-vertical")
                        .small()
                        .text_color(theme.muted_foreground),
                )
                .child(
                    div()
                        .w(px(16.))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child((ix + 1).to_string()),
                )
                // 名前は、その書体で出す
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(name.clone())
                        .when(missing, |d| {
                            d.text_color(theme.muted_foreground).line_through()
                        })
                        .child(name.clone()),
                )
                .when(missing, |d| {
                    d.child(
                        div()
                            .id(("font-missing", ix))
                            .child(
                                app_icon("triangle-exclamation-point")
                                    .small()
                                    .text_color(theme.warning),
                            )
                            .tooltip(|window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new("この PC にありません")
                                    .build(window, cx)
                            }),
                    )
                })
                .when(!only, |d| {
                    d.child(
                        remove_x(
                            ("font-remove", ix),
                            "外す",
                            cx.listener(move |this, _, _, cx| this.remove_font(mono, ix, cx)),
                            cx,
                        )
                        .test_support(),
                    )
                })
                .when_some(mark, |d, below| d.child(line(below)))
                // 押したまま上下へ動かすと、並べ替える
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.font_drag = Some(crate::state::FontDrag {
                            mono,
                            from: ix,
                            start_y: f32::from(event.position.y),
                            moved: false,
                            over: None,
                        });
                        cx.notify();
                    }),
                )
                .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _, cx| {
                    if let Some(drag) = &mut this.font_drag {
                        if drag.moved && drag.over != Some(ix) {
                            drag.over = Some(ix);
                            cx.notify();
                        }
                    }
                }))
                .test_support();
            // 足した行は、すっと出てくる
            chain = chain.child(item.with_animation(
                SharedString::from(format!("font-in-{row_name}")),
                appear(200),
                |d, t| d.opacity(t).top(px(-6. * (1. - t))),
            ));
        }
        let chain = chain.when(drag.is_some(), |d| {
            d.on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if event.pressed_button != Some(MouseButton::Left) {
                    this.finish_font_drag(cx);
                } else if let Some(drag) = &mut this.font_drag {
                    if !drag.moved && (f32::from(event.position.y) - drag.start_y).abs() > 6. {
                        drag.moved = true;
                        cx.notify();
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_font_drag(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_font_drag(cx)),
            )
        });
        let found = self.found_fonts(cx);
        let none_found = found.is_empty();
        let count = found.len();
        let list = uniform_list(
            "font-list",
            count,
            cx.processor(move |_, range: Range<usize>, _, cx| {
                let theme = cx.theme().clone();
                range
                    .map(|ix| {
                        let name = found[ix].clone();
                        let selected = chosen.contains(&name);
                        row()
                            .id(("font", ix))
                            .group("font-row")
                            .w_full()
                            .h(px(40.))
                            .px_3()
                            .gap_2()
                            .rounded(feel::md())
                            .cursor_pointer()
                            .lit(theme.accent.opacity(0.6), cx)
                            .sunk(theme.primary.opacity(0.4), cx)
                            // 名前は、その書体で出す
                            .child(
                                div()
                                    .flex_1()
                                    .truncate()
                                    .font_family(name.clone())
                                    .child(name.clone()),
                            )
                            .child(if selected {
                                app_icon("check").small().text_color(theme.primary)
                            } else {
                                app_icon("plus").small().text_color(theme.muted_foreground)
                            })
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.toggle_font(mono, &name, cx)
                            }))
                            .test_support()
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.font_scroll)
        // 右の端はスクロールバーの場所
        .pr_4()
        .size_full();
        col()
            .gap_3()
            .child(chain)
            .child(div().h(px(1.)).bg(theme.border))
            .child(
                Input::new(&self.inputs.font_search)
                    .id("font-search")
                    .aria_label("フォントを検索")
                    .prefix(app_icon("search").small())
                    .cleanable(true),
            )
            .child(
                div()
                    .relative()
                    .h(px(300.))
                    .child(list)
                    .vertical_scrollbar(&self.font_scroll)
                    .when(none_found, |d| {
                        d.child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("見つかりません。"),
                        )
                    }),
            )
    }

    /// ほかの人の完了を知らせる担当者・ワークスペースを選ぶ。押すたびに入れる・外す。
    /// ワークスペースは左のメニューと同じ順に、サブワークスペースを字下げして並べる。
    fn done_picker(&self, people: bool, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let notices = self.done_notices(cx);
        // (ID, 名前, 字下げ, 知らせるか)
        let items: Vec<(Id, String, usize, bool)> = if people {
            self.people
                .iter()
                .filter(|p| !p.is_self)
                .map(|p| {
                    let on = !notices.muted_people.contains(&p.id);
                    (p.id.clone(), p.name.clone(), 0, on)
                })
                .collect()
        } else {
            crate::state::build_tree(&self.workspaces, &std::collections::HashSet::new())
                .into_iter()
                .map(|r| {
                    let w = &self.workspaces[r.index];
                    // 親を外していれば、サブワークスペースも知らせない
                    let mut on = true;
                    let mut at = Some(w);
                    while let Some(here) = at {
                        if notices.muted_workspaces.contains(&here.id) {
                            on = false;
                            break;
                        }
                        at = here
                            .parent_id
                            .as_deref()
                            .and_then(|p| self.workspaces.iter().find(|w| w.id == p));
                    }
                    (w.id.clone(), w.name.clone(), r.depth, on)
                })
                .collect()
        };
        let empty = items.is_empty();
        let list = col()
            .id("done-pick-list")
            .gap_0p5()
            .children(
                items
                    .into_iter()
                    .enumerate()
                    .map(|(ix, (id, name, depth, on))| {
                        row()
                            .id(("done-pick", ix))
                            .w_full()
                            .h(px(36.))
                            .pl(px(12. + 16. * depth as f32))
                            .pr_3()
                            .gap_2()
                            .rounded(feel::md())
                            .cursor_pointer()
                            .lit(theme.accent.opacity(0.6), cx)
                            .sunk(theme.primary.opacity(0.4), cx)
                            .child(div().flex_1().min_w_0().truncate().child(name))
                            .child(
                                app_icon("check")
                                    .small()
                                    .text_color(theme.primary)
                                    .when(!on, |i| i.invisible()),
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if people {
                                    this.toggle_done_person(&id, cx);
                                } else {
                                    this.toggle_done_workspace(&id, cx);
                                }
                            }))
                            .test_support()
                    }),
            )
            .max_h(px(400.))
            .overflow_y_scrollbar();
        col().gap_3().child(list).when(empty, |d| {
            d.child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(if people {
                        "ほかの担当者はいません。"
                    } else {
                        "ワークスペースはありません。"
                    }),
            )
        })
    }

    /// 背景の画像を選ぶ。同梱の画像は、今のライト/ダークの絵を小さく出す。
    fn scene_picker(&self, cx: &mut Context<Self>) -> Div {
        use crate::theme::Scene;
        let theme = cx.theme().clone();
        let dark = theme.is_dark();
        let current = crate::theme::scene(cx);
        let tile = |id: ElementId, selected: bool, picture: AnyElement, label: String| {
            col()
                .id(id)
                .w(px(152.))
                .gap_1()
                .p_1()
                .rounded(feel::lg())
                .cursor_pointer()
                .border_2()
                .border_color(if selected {
                    theme.primary
                } else {
                    theme.transparent
                })
                .lit(theme.accent, cx)
                .sunk(theme.primary.opacity(0.4), cx)
                .child(
                    div()
                        .h(px(80.))
                        .rounded(feel::md())
                        .overflow_hidden()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.muted)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(picture),
                )
                .child(div().text_sm().truncate().child(label))
        };
        let mut tiles = row().flex_wrap().items_start().gap_2();
        for (ix, scene) in Scene::BUNDLED.into_iter().enumerate() {
            let picture = match scene_thumbnail(&scene, dark) {
                Some(image) => img(image)
                    .size_full()
                    .object_fit(ObjectFit::Cover)
                    .into_any_element(),
                None => app_icon("x")
                    .text_color(theme.muted_foreground)
                    .into_any_element(),
            };
            let chosen = scene.clone();
            tiles = tiles.child(
                tile(
                    ("scene", ix).into(),
                    current == scene,
                    picture,
                    scene.label(),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.set_scene(chosen.clone(), window, cx)
                }))
                .test_support(),
            );
        }
        let own = matches!(current, Scene::File(_));
        let label = if own {
            current.label()
        } else {
            "画像を選ぶ".to_owned()
        };
        let picture = match (own, crate::theme::backdrop(cx)) {
            (true, Some(backdrop)) => img(backdrop.image.clone())
                .size_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            _ => app_icon("upload")
                .text_color(theme.muted_foreground)
                .into_any_element(),
        };
        tiles =
            tiles.child(
                tile("scene-file".into(), own, picture, label)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.pick_scene_file(window, cx)
                    }))
                    .test_support(),
            );
        col()
            .gap_3()
            .child(tiles)
            .when_some(self.error.clone(), |d, error| {
                d.child(div().text_sm().text_color(theme.danger).child(error))
            })
    }

    fn workspace_options(&self, cx: &mut Context<Self>) -> Div {
        let mut list = col().gap_1().child(
            chip("move-unfiled", "未所属", false, cx)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.move_tasks_to(None, cx))),
        );
        let all = crate::state::build_tree(&self.workspaces, &Default::default());
        for (ix, node) in all.iter().enumerate() {
            let ws = &self.workspaces[node.index];
            let id = ws.id.clone();
            list = list.child(div().pl(px(14.) * node.depth as f32).child(
                chip(("move-to", ix), ws.name.clone(), false, cx).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| this.move_tasks_to(Some(id.clone()), cx),
                )),
            ));
        }
        div().child(
            div()
                .id("move-options")
                .max_h(px(320.))
                .overflow_y_scrollbar()
                .child(list),
        )
    }

    fn render_modal(&self, cx: &mut Context<Self>) -> Option<Div> {
        let modal = self.modal.clone();
        self.render_modal_of(&modal, cx)
    }

    /// ダイアログが出てくる動きと消える動きを、今の時刻まで進める。
    /// 出てきたら 200ms でふわっと浮かび上がり、閉じたら 150ms で薄くなって消える。
    fn tick_dialog_motion(&mut self, window: &mut Window) {
        use std::time::{Duration, Instant};
        const OPEN: Duration = Duration::from_millis(200);
        const CLOSE: Duration = Duration::from_millis(DIALOG_CLOSE_MS);
        let shown = (self.modal != Modal::None || self.pane_overlay.is_some())
            .then(|| (self.modal.clone(), self.pane_overlay));
        let now = Instant::now();
        match (self.modal_seen.take(), &shown) {
            (None, Some(_)) => {
                self.modal_opened = Some(now);
                self.modal_closing = None;
            }
            (Some((modal, overlay)), None) => {
                self.modal_opened = None;
                self.modal_closing = Some((now, modal, overlay));
            }
            _ => {}
        }
        self.modal_seen = shown;
        // 重ねたダイアログだけを閉じたときは、下のダイアログの上で薄くなって消える
        match (self.sub_seen.take(), self.sub_modal) {
            (Some(last), None) if self.modal != Modal::None => {
                self.sub_closing = Some((now, last));
            }
            (_, Some(_)) => self.sub_closing = None,
            _ => {}
        }
        self.sub_seen = self.sub_modal;
        if self.modal == Modal::None {
            self.sub_closing = None;
        }
        self.dialog_t.set(1.);
        if !self.motion {
            self.modal_closing = None;
            self.sub_closing = None;
            return;
        }
        if let Some((at, _)) = self.sub_closing {
            if now.duration_since(at) >= CLOSE {
                self.sub_closing = None;
            } else {
                window.request_animation_frame();
            }
        }
        if let Some(at) = self.modal_opened {
            let progress = now.duration_since(at).as_secs_f32() / OPEN.as_secs_f32();
            if progress < 1. {
                self.dialog_t.set(ease_out_quint()(progress));
                window.request_animation_frame();
            }
        }
        if let Some((at, ..)) = &self.modal_closing {
            if now.duration_since(*at) >= CLOSE {
                self.modal_closing = None;
            } else {
                window.request_animation_frame();
            }
        }
    }

    /// 重ねて出しているダイアログと、その濃さ（出ているときは 1）。閉じたばかりなら、
    /// 消える動きの間だけそれを返し、濃さは 1 から 0 へ下がる。
    fn sub_shown(&self) -> Option<(SubModal, f32)> {
        if let Some(sub) = self.sub_modal {
            return Some((sub, 1.));
        }
        let (at, sub) = self.sub_closing?;
        let progress = at.elapsed().as_millis() as f32 / DIALOG_CLOSE_MS as f32;
        (progress < 1.).then(|| (sub, 1. - ease_out_quint()(progress)))
    }

    /// 重ねたダイアログに出入りの動きを付ける。出るときは薄い所から浮かび、閉じたあとは
    /// 薄くなって消える。消えるまでの間は上を覆い、閉じたダイアログのボタンは押せない。
    fn sub_motion(&self, over: Div) -> AnyElement {
        if !self.motion {
            return over.into_any_element();
        }
        match self.sub_shown() {
            Some((_, fade)) if self.sub_modal.is_none() => over
                .opacity(fade)
                .child(
                    div()
                        .id("sub-modal-closing")
                        .absolute()
                        .inset_0()
                        .occlude()
                        .test_support(),
                )
                .into_any_element(),
            _ => over
                .with_animation("sub-modal-in", appear(140), |d, t| d.opacity(t))
                .into_any_element(),
        }
    }

    /// 閉じたばかりのダイアログ。薄くしながら描き、上を覆って押せないようにする。
    fn render_closing_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (at, modal, overlay) = self.modal_closing.clone()?;
        let progress = (at.elapsed().as_millis() as f32 / DIALOG_CLOSE_MS as f32).min(1.);
        let open = self.dialog_t.replace(1. - ease_out_quint()(progress));
        let dialog = match overlay {
            Some(overlay) => Some(self.pane_overlay_dialog(overlay, cx)),
            None => self
                .render_modal_of(&modal, cx)
                .map(IntoElement::into_any_element),
        };
        self.dialog_t.set(open);
        Some(
            div()
                .id("dialog-closing")
                .absolute()
                .inset_0()
                .child(dialog?)
                .child(div().absolute().inset_0().occlude())
                .test_support()
                .into_any_element(),
        )
    }

    fn render_modal_of(&self, modal: &Modal, cx: &mut Context<Self>) -> Option<Div> {
        let theme = cx.theme().clone();
        let muted = |text: String| {
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let cancel = |cx: &mut Context<Self>| {
            feel::gloss(
                button("modal-cancel")
                    .label("キャンセル")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.modal = Modal::None;
                        this.issued_ticket = None;
                        cx.notify();
                    })),
            )
        };
        Some(match modal {
            Modal::None => return None,
            Modal::WorkspaceIcon(id) => {
                let theme = cx.theme().clone();
                let current = self.workspaces.iter().find(|w| &w.id == id);
                let icon = current.and_then(|w| w.icon.clone());
                let color = current.and_then(|w| w.color.clone());
                let chosen = icon
                    .clone()
                    .unwrap_or_else(|| crate::state::DEFAULT_WORKSPACE_ICON.to_owned());
                let mut colors = row().gap_1().flex_wrap();
                let all = std::iter::once(None).chain(WORKSPACE_COLORS.iter().copied().map(Some));
                for (ix, key) in all.enumerate() {
                    let (ws, icon) = (id.clone(), icon.clone());
                    let selected = color.as_deref() == key;
                    colors = colors.child(
                        div()
                            .id(("ws-color", ix))
                            .size_8()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(feel::md())
                            .cursor_pointer()
                            .border_1()
                            .border_color(if selected {
                                theme.foreground
                            } else {
                                theme.transparent
                            })
                            .lit(theme.secondary_hover, cx)
                            .child(
                                div()
                                    .size_4()
                                    .rounded_full()
                                    .bg(workspace_color(key).unwrap_or(theme.foreground)),
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.set_workspace_icon(&ws, icon.as_deref(), key, cx)
                            }))
                            .test_support(),
                    );
                }
                let query = self.inputs.icon_search.read(cx).value().to_string();
                let found = crate::icons::search(
                    &query,
                    self.icon_category,
                    &crate::state::WORKSPACE_ICONS,
                    ICONS_SHOWN,
                );
                // カテゴリはもう一度押すと外れる
                let mut categories = row().gap_1();
                for (ix, category) in crate::icons::Category::ALL.into_iter().enumerate() {
                    let active = self.icon_category == Some(category);
                    categories = categories.child(
                        div()
                            .rounded(feel::md())
                            .when(active, |d| d.bg(theme.secondary_hover))
                            .child(feel::gloss(
                                button(("icon-category", ix))
                                    .icon(app_icon(category.icon()))
                                    .ghost()
                                    .large()
                                    .tooltip(category.label())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.icon_category = (!active).then_some(category);
                                        cx.notify();
                                    })),
                            )),
                    );
                }
                categories = categories.child(div().flex_1()).child({
                    let ws = id.clone();
                    feel::gloss(
                        button("icon-add-image")
                            .icon(app_icon("image-plus"))
                            .ghost()
                            .large()
                            .tooltip("画像を追加")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.add_icon_image(ws.clone(), window, cx)
                            })),
                    )
                });
                // 自分で足した画像は、絞り込んでいないときに先頭に出す
                let (custom, custom_error) =
                    if query.trim().is_empty() && self.icon_category.is_none() {
                        match self.db.read(cx).store.custom_icons() {
                            Ok(names) => (names, None),
                            Err(error) => (
                                Vec::new(),
                                Some(format!("追加した画像を読めませんでした。{error}")),
                            ),
                        }
                    } else {
                        (Vec::new(), None)
                    };
                let none_found = found.is_empty() && custom.is_empty();
                let mut icons = row().gap_1().flex_wrap();
                for (ix, name) in custom.into_iter().enumerate() {
                    let (ws, color) = (id.clone(), color.clone());
                    let selected = chosen == name;
                    icons = icons.child(
                        div()
                            .id(("ws-custom-icon", ix))
                            .size(px(40.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(feel::md())
                            .cursor_pointer()
                            .border_1()
                            .border_color(if selected {
                                theme.foreground
                            } else {
                                theme.transparent
                            })
                            .lit(theme.secondary_hover, cx)
                            .child(workspace_icon(Some(&name), None, PICKER_ICON_SIZE, cx))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.set_workspace_icon(&ws, Some(&name), color.as_deref(), cx)
                            }))
                            .test_support(),
                    );
                }
                for (ix, name) in found.into_iter().enumerate() {
                    let (ws, color) = (id.clone(), color.clone());
                    let selected = chosen == name;
                    icons = icons.child(
                        div()
                            .id(("ws-icon", ix))
                            .size(px(40.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(feel::md())
                            .cursor_pointer()
                            .border_1()
                            .border_color(if selected {
                                theme.foreground
                            } else {
                                theme.transparent
                            })
                            .lit(theme.secondary_hover, cx)
                            .child(workspace_icon(
                                Some(name),
                                color.as_deref(),
                                PICKER_ICON_SIZE,
                                cx,
                            ))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.set_workspace_icon(&ws, Some(name), color.as_deref(), cx)
                            }))
                            .test_support(),
                    );
                }
                let body = col()
                    .gap_3()
                    .child(colors)
                    .child(
                        Input::new(&self.inputs.icon_search)
                            .id("icon-search")
                            .aria_label("アイコンを検索")
                            .prefix(app_icon("search").small())
                            .cleanable(true),
                    )
                    .child(categories)
                    .child(
                        div()
                            // カテゴリごとに別のものにして、切り替えたら上から見せる
                            .id((
                                "icon-list",
                                self.icon_category
                                    .map_or(0, |category| category as usize + 1),
                            ))
                            .h(px(320.))
                            .overflow_y_scrollbar()
                            .child(icons)
                            .when(none_found, |d| {
                                d.child(muted("見つかりません。".to_owned()))
                            }),
                    )
                    .when_some(custom_error, |d, error| {
                        d.child(div().text_sm().text_color(theme.danger).child(error))
                    })
                    .when_some(self.error.clone(), |d, error| {
                        d.child(div().text_sm().text_color(theme.danger).child(error))
                    });
                let title = format!("「{}」のアイコン", self.workspace_name(Some(id)));
                self.dialog(title, body, cx)
            }
            Modal::Appearance => self.render_appearance(cx),
            Modal::Stats(scope) => self.stats_dialog(scope.as_deref(), cx),
            Modal::AiPromo => {
                let body = col()
                    .gap_4()
                    .child(
                        row()
                            .gap_3()
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(theme.primary)
                                    .child(app_icon("sparkles").size_8())
                                    .with_animation(
                                        "ai-promo-icon",
                                        Animation::new(std::time::Duration::from_millis(1600))
                                            .repeat()
                                            .with_easing(pulsating_between(0.55, 1.)),
                                        |d, t| d.opacity(t),
                                    ),
                            )
                            .child(muted(
                                "頼むだけで、AI がタスクの追加・整理・更新をします。".to_owned(),
                            )),
                    )
                    .child(
                        Checkbox::new("ai-promo-never")
                            .py(px(4.))
                            .my(px(-4.))
                            .label("今後は表示しない")
                            .checked(self.ai_promo_never)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.set_ai_promo_never(*checked, cx)
                            })),
                    )
                    // ボタンが 3 つあるので、チェックとは別の行で右に寄せる
                    .child(
                        row()
                            .gap_2()
                            .justify_end()
                            .child(cancel(cx))
                            // 手元で動かす AI なら、API キーも通信もいらない
                            .child(feel::gloss(
                                button("ai-promo-local")
                                    .icon(app_icon("server"))
                                    .label("ローカル AI を使う")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.use_local_ai_from_promo(window, cx)
                                    })),
                            ))
                            .child(feel::gloss(
                                button("ai-promo-open")
                                    .icon(app_icon("sparkles"))
                                    .label("AI bot を開く")
                                    .primary()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_ai_from_promo(window, cx)
                                    })),
                            )),
                    );
                self.dialog("AI で楽々タスク管理", body, cx)
            }
            Modal::DeleteWorkspace(id) => {
                let name = self.workspace_name(Some(id));
                let body = col()
                    .gap_3()
                    .child(muted(format!(
                        "「{name}」と、そのサブワークスペースとタスクをすべて削除します。元に戻せません。"
                    )))
                    .child(
                        row().gap_2().justify_end().child(cancel(cx)).child(
                            feel::gloss(button("confirm-delete-workspace")
                                .label("削除する")
                                .danger()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_delete_workspace(cx)
                                }))),
                        ),
                    );
                self.dialog("ワークスペースを削除", body, cx)
            }
            Modal::DeleteTasks(ids) => {
                // ほかのワークスペースにも出しているタスクは、ここから外すだけにもできる
                let here = self.current_workspace();
                let shared = here.is_some()
                    && self
                        .tasks
                        .iter()
                        .chain(self.detail.as_ref().map(|d| &d.task))
                        .any(|t| ids.contains(&t.id) && !t.linked_workspace_ids.is_empty());
                let body = col()
                    .gap_3()
                    .child(muted(format!(
                        "{} 件のタスクを削除します。元に戻せません。",
                        ids.len()
                    )))
                    .when(shared, |d| {
                        d.child(muted(
                            "ほかのワークスペースと同期しているタスクがあります。削除すると、すべてのワークスペースから消えます。"
                                .to_owned(),
                        ))
                    })
                    .child(
                        row().gap_2().justify_end().child(cancel(cx)).when(shared, |d| {
                            d.child(
                                feel::gloss(button("unlink-here")
                                    .label("このワークスペースから外す")
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.unlink_selected_here(cx)),
                                    )),
                            )
                        }).child(
                            feel::gloss(button("confirm-delete-tasks")
                                .label("削除する")
                                .danger()
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.confirm_delete_tasks(cx)),
                                )),
                        ),
                    );
                self.dialog("タスクを削除", body, cx)
            }
            Modal::MoveTasks(_) => {
                let body = col().gap_3().child(self.workspace_options(cx));
                self.dialog("タスクを移動", body, cx)
            }
            Modal::History => {
                let body = self.render_history(cx);
                self.dialog_as("dialog", px(600.), "変更履歴", body, cx)
            }
            Modal::Status(id, next) => {
                let (id, next) = (id.clone(), *next);
                let report = next == Status::Completed;
                let mut options = row().gap_1().flex_wrap();
                for (i, s) in Status::ALL.into_iter().enumerate() {
                    let id = id.clone();
                    options =
                        options.child(chip(("st-opt", i), s.label(), s == next, cx).on_click(
                            cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.modal = Modal::Status(id.clone(), s);
                                cx.notify();
                            }),
                        ));
                }
                let body = col()
                    .gap_3()
                    .child(section_title("新しい状態"))
                    .child(options)
                    .child(section_title(if report {
                        "完了報告（任意）"
                    } else {
                        "コメント（任意）"
                    }))
                    .child(self.compose(Compose::Status, &self.inputs.status_comment, px(96.), cx))
                    .child(
                        row()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Ctrl / ⌘ + Enter で送信"),
                            )
                            .child(cancel(cx))
                            .child(feel::gloss(
                                button("status-submit")
                                    .label(format!("{}にする", next.label()))
                                    .primary()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.submit_status(window, cx)
                                    })),
                            )),
                    );
                self.dialog("状態を変更", body, cx)
            }
            Modal::Ticket(id) => {
                let theme = cx.theme().clone();
                let task = self
                    .detail
                    .as_ref()
                    .map(|d| &d.task)
                    .filter(|t| &t.id == id);
                // 自分（タスク全体を完了にする）と、ほかの担当者（その人の分だけ）
                let mut who = row().gap_1().flex_wrap();
                let mut options: Vec<(Option<malakt_core::Id>, String)> = Vec::new();
                match self.people.iter().find(|p| p.is_self) {
                    Some(me) => options.push((Some(me.id.clone()), format!("{}（自分）", me.name))),
                    None => options.push((None, "自分".to_owned())),
                }
                for p in &self.people {
                    if !p.is_self && task.is_some_and(|t| t.roles_of(&p.id).is_some()) {
                        options.push((Some(p.id.clone()), p.name.clone()));
                    }
                }
                for (ix, (person, name)) in options.into_iter().enumerate() {
                    let active = self.ticket_person == person;
                    who = who.child(
                        chip(("ticket-who", ix), name, active, cx)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.ticket_person = person.clone();
                                this.issued_ticket = None;
                                cx.notify();
                            }))
                            .test_support(),
                    );
                }
                let active = self.active_tickets(id, cx);
                let body = col()
                    .gap_3()
                    .child(
                        col()
                            .gap_1()
                            .child(div().text_sm().child("完了にする担当者"))
                            .child(who),
                    )
                    .when_some(self.issued_ticket.as_ref(), |d, ticket| {
                        d.child(
                            col()
                                .gap_1()
                                .p_2()
                                .rounded(feel::md())
                                .bg(theme.secondary)
                                .child(
                                    div()
                                        .text_sm()
                                        .child("発行しました。チケットは今だけ表示されます。"),
                                )
                                .child(
                                    div()
                                        .id("ticket-secret")
                                        .text_sm()
                                        .mono_font(cx)
                                        .child(ticket.secret.clone()),
                                )
                                .child(muted(format!(
                                    "使い方: malakt-mahoz redeem <チケット> --message \"完了報告\"　（{} まで有効）",
                                    view::format_datetime(ticket.expires_at)
                                )))
                                .child(
                                    row().child(
                                        feel::gloss(button("ticket-copy")
                                            .label("コピー")
                                            .small()
                                            .on_click(cx.listener(|this, _, _, cx| this.copy_ticket(cx)))),
                                    ),
                                ),
                        )
                    })
                    .when_some(self.error.clone(), |d, error| {
                        d.child(div().text_sm().text_color(theme.danger).child(error))
                    })
                    .child(
                        row()
                            .gap_2()
                            .items_center()
                            .child(muted(format!("未使用のチケット: {active} 枚")))
                            .child(div().flex_1())
                            .when(active > 0, |d| {
                                d.child(
                                    feel::gloss(button("ticket-revoke")
                                        .label("すべて取り消す")
                                        .small()
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.revoke_tickets(cx)),
                                        )),
                                )
                            })
                            .child(
                                feel::gloss(button("ticket-issue")
                                    .label("発行")
                                    .primary()
                                    .on_click(cx.listener(|this, _, _, cx| this.issue_ticket(cx)))),
                            ),
                    );
                self.dialog("完了チケット", body, cx)
            }
            Modal::Notices => {
                let theme = cx.theme().clone();
                let now = malakt_core::now_ms();
                let mut list = col().gap_1();
                for (ix, n) in self.notices.iter().enumerate() {
                    let id = n.id.clone();
                    list = list.child(
                        col()
                            .id(("notice", ix))
                            .gap_0p5()
                            .p_2()
                            .rounded(feel::md())
                            .cursor_pointer()
                            .lit(theme.secondary_hover, cx)
                            .when(n.is_unread(), |d| d.bg(theme.secondary))
                            .child(
                                div()
                                    .text_sm()
                                    .when(n.is_unread(), |d| d)
                                    .child(n.headline()),
                            )
                            .when(!n.excerpt.is_empty(), |d| {
                                d.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .truncate()
                                        .child(n.excerpt.clone()),
                                )
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(view::format_relative(n.created_at, now)),
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.open_notice(&id, window, cx)
                            }))
                            .test_support(),
                    );
                }
                let body =
                    col()
                        .gap_3()
                        .min_h_0()
                        .when(self.notices.is_empty(), |d| {
                            d.child(muted("通知はまだありません。".to_owned()))
                        })
                        .child(
                            div()
                                .id("notices-scroll")
                                .flex_1()
                                .min_h_0()
                                .max_h(px(420.))
                                .overflow_y_scrollbar()
                                .child(list),
                        )
                        .when(self.unread_notices > 0, |d| {
                            d.child(
                                row().justify_end().child(feel::gloss(
                                    button("notices-read-all")
                                        .label("すべて既読にする")
                                        .small()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.mark_all_notices_read(cx)
                                        })),
                                )),
                            )
                        });
                self.dialog("通知", body, cx)
            }
            Modal::BulkDue => {
                let body = col()
                    .gap_3()
                    .w(px(340.))
                    .child(self.date_inline(cx))
                    .when_some(self.error.clone(), |d, error| {
                        d.child(div().text_sm().text_color(cx.theme().danger).child(error))
                    })
                    .child(
                        row()
                            .gap_2()
                            .justify_end()
                            .child(cancel(cx))
                            .child(feel::gloss(
                                button("bulk-due-clear").label("期限なしにする").on_click(
                                    cx.listener(|this, _, _, cx| this.submit_bulk_due(true, cx)),
                                ),
                            ))
                            .child(feel::gloss(
                                button("bulk-due-submit")
                                    .label("変更する")
                                    .primary()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.submit_bulk_due(false, cx)
                                    })),
                            )),
                    );
                self.dialog("期限をまとめて変更", body, cx)
            }
            Modal::BulkAssignees => {
                let mut modes = row().gap_1();
                for (ix, mode) in ListMode::ALL.into_iter().enumerate() {
                    modes = modes.child(
                        chip(("bulk-mode", ix), mode.label(), self.bulk_mode == mode, cx)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.bulk_mode = mode;
                                cx.notify();
                            }))
                            .test_support(),
                    );
                }
                let mut people = row().gap_1().flex_wrap();
                for (ix, p) in self.people_here().into_iter().enumerate() {
                    let pid = p.id.clone();
                    people = people.child(
                        chip(
                            ("bulk-person", ix),
                            p.name.clone(),
                            self.bulk_people.contains(&p.id),
                            cx,
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.toggle_bulk_person(&pid, cx)
                        }))
                        .test_support(),
                    );
                }
                let body =
                    col()
                        .gap_3()
                        .child(modes)
                        .child(people)
                        .when_some(self.error.clone(), |d, error| {
                            d.child(div().text_sm().text_color(cx.theme().danger).child(error))
                        })
                        .child(
                            row()
                                .gap_2()
                                .justify_end()
                                .child(cancel(cx))
                                .child(feel::gloss(
                                    button("bulk-assignees-submit")
                                        .label("変更する")
                                        .primary()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.submit_bulk_assignees(cx)
                                        })),
                                )),
                        );
                self.dialog("担当者をまとめて変更", body, cx)
            }
            Modal::BulkCategories => {
                let mut modes = row().gap_1();
                for (ix, (text, add)) in [("追加", true), ("外す", false)].into_iter().enumerate()
                {
                    modes = modes.child(
                        chip(
                            ("bulk-cat-mode", ix),
                            text,
                            self.bulk_category_add == add,
                            cx,
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.bulk_category_add = add;
                            cx.notify();
                        }))
                        .test_support(),
                    );
                }
                let body = col()
                    .gap_3()
                    .child(modes)
                    .child(
                        Input::new(&self.inputs.bulk_categories)
                            .id("bulk-categories-input")
                            .aria_label("カテゴリ"),
                    )
                    .when_some(self.error.clone(), |d, error| {
                        d.child(div().text_sm().text_color(cx.theme().danger).child(error))
                    })
                    .child(
                        row()
                            .gap_2()
                            .justify_end()
                            .child(cancel(cx))
                            .child(feel::gloss(
                                button("bulk-categories-submit")
                                    .label("変更する")
                                    .primary()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.submit_bulk_categories(cx)
                                    })),
                            )),
                    );
                self.dialog("カテゴリをまとめて変更", body, cx)
            }
            Modal::LinkTask(id) => {
                let task = self
                    .detail
                    .as_ref()
                    .map(|d| &d.task)
                    .filter(|t| &t.id == id);
                // 基準日が同じワークスペースを先に出す（同じ日の催しに出すことが多い）
                let date_of = |ws: Option<&str>| {
                    self.workspaces
                        .iter()
                        .find(|w| Some(w.id.as_str()) == ws)
                        .and_then(|w| w.date.clone())
                };
                let home_date = date_of(task.and_then(|t| t.workspace_id.as_deref()));
                let mut same = col().gap_1();
                let mut others = col().gap_1();
                let (mut any_same, mut any_other) = (false, false);
                for (ix, ws) in self.workspaces.iter().enumerate() {
                    if task.is_none_or(|t| t.is_in(&ws.id)) {
                        continue;
                    }
                    let target = ws.id.clone();
                    let item = chip(("link-to", ix), ws.name.clone(), false, cx)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.link_task_to(&target, cx)
                        }))
                        .test_support();
                    if home_date.is_some() && ws.date == home_date {
                        any_same = true;
                        same = same.child(item);
                    } else {
                        any_other = true;
                        others = others.child(item);
                    }
                }
                let body = col()
                    .gap_3()
                    .when(!any_same && !any_other, |d| {
                        d.child(muted("同期できるワークスペースがありません。".to_owned()))
                    })
                    .child(
                        div()
                            .id("link-options")
                            .max_h(px(320.))
                            .overflow_y_scrollbar()
                            .child(
                                col()
                                    .gap_2()
                                    .when(any_same, |d| {
                                        d.child(muted("基準日が同じ".to_owned())).child(same)
                                    })
                                    .when(any_same && any_other, |d| {
                                        d.child(muted("その他".to_owned()))
                                    })
                                    .child(others),
                            ),
                    );
                self.dialog("ほかのワークスペースと同期", body, cx)
            }
            Modal::WorkspaceSettings(id) => {
                let theme = cx.theme().clone();
                let draft = &self.defaults_draft;
                let label = |text: &'static str| div().text_sm().child(text);
                let mut assignees = row().gap_1().flex_wrap();
                for (ix, p) in self.people_in(Some(id.as_str())).into_iter().enumerate() {
                    let pid = p.id.clone();
                    assignees = assignees.child(
                        chip(
                            ("def-as", ix),
                            p.name.clone(),
                            draft.assignee_ids.contains(&p.id),
                            cx,
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.toggle_default_assignee(&pid, cx)
                        }))
                        .test_support(),
                    );
                }
                if self.people.is_empty() {
                    assignees = assignees.child(muted("担当者はまだいません。".to_owned()));
                }
                let due_kinds = [
                    ("def-due-none", "なし", DueRule::None),
                    (
                        "def-due-relative",
                        "基準日からの日数",
                        DueRule::Relative { days: 0 },
                    ),
                    (
                        "def-due-fixed",
                        "日付を指定",
                        DueRule::Fixed {
                            date: String::new(),
                        },
                    ),
                ];
                let mut due = row().gap_1().flex_wrap();
                for (chip_id, text, rule) in due_kinds {
                    let active =
                        std::mem::discriminant(&draft.due) == std::mem::discriminant(&rule);
                    due = due.child(
                        chip(chip_id, text, active, cx)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.set_default_due(rule.clone(), cx)
                            }))
                            .test_support(),
                    );
                }
                let due_detail = match &draft.due {
                    DueRule::None => col(),
                    DueRule::Relative { .. } => col()
                        .gap_1()
                        .child(muted("基準日からの日数と時刻".to_owned()))
                        .child(
                            Input::new(&self.inputs.def_days)
                                .id("def-days")
                                .aria_label("日数"),
                        )
                        .child(
                            Input::new(&self.inputs.def_time)
                                .id("def-time")
                                .aria_label("時刻"),
                        ),
                    DueRule::Fixed { .. } => col().child(self.date_trigger(
                        "def-date",
                        crate::date_pick::DateTarget::DefaultDue,
                        self.date_shown(&crate::date_pick::DateTarget::DefaultDue, cx),
                        cx,
                    )),
                };
                let mut priority = row().gap_1().flex_wrap();
                for (ix, (text, value)) in [
                    ("指定しない", None),
                    ("高", Some(1u8)),
                    ("中", Some(5)),
                    ("低", Some(9)),
                ]
                .into_iter()
                .enumerate()
                {
                    priority = priority.child(
                        chip(("def-prio", ix), text, draft.priority == value, cx)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.set_default_priority(value, cx)
                            }))
                            .test_support(),
                    );
                }
                let mut links = row().gap_1().flex_wrap();
                let mut any_link = false;
                for (ix, w) in self.workspaces.iter().enumerate() {
                    if &w.id == id {
                        continue;
                    }
                    any_link = true;
                    let wid = w.id.clone();
                    links = links.child(
                        chip(
                            ("def-link", ix),
                            w.name.clone(),
                            draft.link_workspace_ids.contains(&w.id),
                            cx,
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.toggle_default_link(&wid, cx)
                        }))
                        .test_support(),
                    );
                }
                let form = col()
                    .gap_3()
                    .child(
                        col()
                            .gap_1()
                            .child(label("基準日"))
                            .child(self.date_trigger(
                                "ws-date",
                                crate::date_pick::DateTarget::BaseDate,
                                self.date_shown(&crate::date_pick::DateTarget::BaseDate, cx),
                                cx,
                            )),
                    )
                    .child(div().h(px(1.)).bg(theme.border))
                    .child(col().gap_1().child(label("担当者")).child(assignees))
                    .child(
                        col()
                            .gap_1()
                            .child(label("期限"))
                            .child(due)
                            .child(due_detail),
                    )
                    .child(col().gap_1().child(label("優先度")).child(priority))
                    .child(
                        col().gap_1().child(label("カテゴリ")).child(
                            Input::new(&self.inputs.def_categories)
                                .id("def-categories")
                                .aria_label("カテゴリ"),
                        ),
                    )
                    .child(
                        col()
                            .gap_1()
                            .child(label("説明の雛形"))
                            .child(Textarea::new(&self.inputs.def_description).h(px(96.))),
                    )
                    .child(
                        col()
                            .gap_1()
                            .child(label("AI のルール"))
                            .child(self.rules_opener(id, cx)),
                    )
                    .child(
                        col()
                            .gap_1()
                            .child(label("定時タスク"))
                            .child(self.schedules_opener(id, cx)),
                    )
                    .child(self.workspace_basis_setting(id, cx))
                    .child(
                        col().gap_1().child(label("ターミナルのフォルダー")).child(
                            Input::new(&self.inputs.ws_term_dir)
                                .id("ws-term-dir")
                                .aria_label("ターミナルのフォルダー"),
                        ),
                    )
                    .when(any_link, |d| {
                        d.child(
                            col()
                                .gap_1()
                                .child(label("同期するワークスペース"))
                                .child(links),
                        )
                    });
                let body = col()
                    .gap_3()
                    .min_h_0()
                    .child(
                        div()
                            .id("ws-settings-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scrollbar()
                            .child(form),
                    )
                    .when_some(self.error.clone(), |d, error| {
                        d.child(div().text_sm().text_color(theme.danger).child(error))
                    })
                    .child(
                        row()
                            .gap_2()
                            .justify_end()
                            .child(cancel(cx))
                            .child(feel::gloss(
                                button("ws-settings-save").label("保存").primary().on_click(
                                    cx.listener(|this, _, _, cx| this.save_workspace_settings(cx)),
                                ),
                            )),
                    );
                let title = format!("「{}」の設定", self.workspace_name(Some(id)));
                let name = self.workspace_name(Some(id));
                // AI のルールと定時タスクは、設定の上にもう 1 枚重ねる
                let (over_title, over_body) = match self.sub_shown().map(|(sub, _)| sub) {
                    Some(SubModal::Rules) => (
                        format!("「{name}」の AI のルール"),
                        self.ai_settings
                            .update(cx, |s, cx| s.rules_editor(false, cx)),
                    ),
                    Some(SubModal::Schedules) => (
                        format!("「{name}」の定時タスク"),
                        self.ai_settings
                            .update(cx, |s, cx| s.schedules_editor(false, cx)),
                    ),
                    _ => return Some(self.dialog(title, body, cx)),
                };
                let over =
                    self.dialog_as("dialog", px(560.), over_title, col().child(over_body), cx);
                let over = self.sub_motion(over);
                div()
                    .absolute()
                    .inset_0()
                    .child(self.dialog_as("dialog-under", px(460.), title, body, cx))
                    .child(over)
            }
        })
    }

    /// ワークスペースの設定の「AI のルール」。ルールの数を出し、押すとルールの一覧を重ねて開く。
    fn rules_opener(&self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let rules = self.db.read(cx).store.ai_rules(Some(id));
        let shown: SharedString = match &rules {
            Ok(rules) if rules.is_empty() => "なし".into(),
            Ok(rules) => {
                let on = rules.iter().filter(|r| r.enabled).count();
                if on == rules.len() {
                    format!("{} 件", rules.len()).into()
                } else {
                    format!("{} 件（使うのは {on} 件）", rules.len()).into()
                }
            }
            Err(error) => format!("読めませんでした: {error}").into(),
        };
        row()
            .id("ws-rules")
            .h_8()
            .px_3()
            .gap_2()
            .rounded(feel::md())
            .border_1()
            .border_color(theme.input)
            .cursor_pointer()
            .text_sm()
            .lit(theme.muted, cx)
            .child(
                app_icon("scroll-text")
                    .small()
                    .text_color(theme.muted_foreground),
            )
            .child(div().flex_1().min_w_0().truncate().child(shown))
            .child(
                app_icon("chevron-right")
                    .small()
                    .text_color(theme.muted_foreground),
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_rules_over_settings(cx)))
            .test_support()
            .into_any_element()
    }

    /// ワークスペースの設定の「定時タスク」。数を出し、押すと定時タスクの一覧を重ねて開く。
    fn schedules_opener(&self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let list = self.db.read(cx).store.schedules(Some(id));
        let shown: SharedString = match &list {
            Ok(list) if list.is_empty() => "なし".into(),
            Ok(list) => {
                let on = list.iter().filter(|s| s.enabled).count();
                if on == list.len() {
                    format!("{} 件", list.len()).into()
                } else {
                    format!("{} 件（動かすのは {on} 件）", list.len()).into()
                }
            }
            Err(error) => format!("読めませんでした: {error}").into(),
        };
        row()
            .id("ws-schedules")
            .h_8()
            .px_3()
            .gap_2()
            .rounded(feel::md())
            .border_1()
            .border_color(theme.input)
            .cursor_pointer()
            .text_sm()
            .lit(theme.muted, cx)
            .child(
                app_icon("alarm-clock")
                    .small()
                    .text_color(theme.muted_foreground),
            )
            .child(div().flex_1().min_w_0().truncate().child(shown))
            .child(
                app_icon("chevron-right")
                    .small()
                    .text_color(theme.muted_foreground),
            )
            .on_click(
                cx.listener(|this, _: &ClickEvent, _, cx| this.open_schedules_over_settings(cx)),
            )
            .test_support()
            .into_any_element()
    }

    fn render_lock_screen(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        col()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(div().text_xl().child("ロックされています"))
            .child(feel::gloss_when(
                !(self.verifying(cx)),
                button("unlock")
                    .icon(app_icon("fingerprint-pattern"))
                    .label("パスキーで解除")
                    .primary()
                    .loading(self.verifying(cx))
                    .on_click(cx.listener(|this, _, window, cx| this.request_unlock(window, cx))),
            ))
            .when_some(self.error.clone(), |d, error| {
                d.child(div().text_sm().text_color(theme.danger).child(error))
            })
    }
}

impl MainView {
    /// 一覧と詳細の分け方。`own_width` は一覧と詳細を置く場所の幅。
    ///
    /// 詳細は一覧の右に並ぶカラムで、一覧はその残りの幅に収まる。幅が足りないときは、
    /// 詳細を `PANEL_MIN` まで狭めて一覧に `LIST_ROOM` を残す（1920 幅の画面で AI bot を
    /// 右に並べても、一覧・詳細・チャットの 3 つが並ぶ）。詳細を `PANEL_MIN` にしても一覧に
    /// `LIST_MIN` を残せない狭さでは、詳細が全面を使う（`None`）。返すのは詳細の幅。
    fn panel_split(&self, own_width: Pixels) -> Option<Pixels> {
        let least = px(crate::state::PANEL_MIN);
        if own_width < least + px(LIST_MIN) {
            return None;
        }
        Some(
            px(self.panel_width)
                .min((own_width - px(LIST_ROOM)).max(least))
                .min(own_width - px(LIST_MIN)),
        )
    }

    /// 詳細の境界をドラッグしている間だけ、全面に敷く。詳細の上でも一覧の上でもマウスを追い、
    /// 下にあるものには触らせない。
    fn panel_drag_layer(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        self.panel_resize?;
        Some(
            div()
                .id("panel-drag")
                .absolute()
                .inset_0()
                .occlude()
                .cursor_col_resize()
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.drag_panel(f32::from(event.position.x), cx);
                    } else {
                        this.finish_panel_resize(cx);
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.finish_panel_resize(cx)),
                ),
        )
    }

    /// ドラッグの途中は、どこにマウスがあっても追えるように、いちばん外の要素で受ける。
    fn with_drags(&self, d: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        d.when(self.border_drag.is_some(), |d| {
            d.on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if event.pressed_button == Some(MouseButton::Left) {
                    this.drag_pane_border(event.position, cx);
                } else {
                    this.border_drag = None;
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.border_drag = None;
                    cx.notify();
                }),
            )
        })
        // 左のメニューの境界をドラッグしている間は、どこにマウスがあっても幅を追う
        .when(self.sidebar_resizing, |d| {
            d.on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if event.pressed_button == Some(MouseButton::Left) {
                    this.drag_sidebar(f32::from(event.position.x) / crate::UI_SCALE, cx);
                } else {
                    this.finish_sidebar_drag(cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_sidebar_drag(cx)),
            )
        })
        // 表の列の幅を変えている間と、見出しを動かしている間も、どこにマウスがあっても追う
        .when(self.column_resize.is_some(), |d| {
            d.on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if event.pressed_button == Some(MouseButton::Left) {
                    this.drag_column(f32::from(event.position.x), cx);
                } else {
                    this.finish_column_resize(cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_column_resize(cx)),
            )
        })
        .when(self.column_drag.is_some(), |d| {
            d.on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if event.pressed_button != Some(MouseButton::Left) {
                    this.finish_column_drag(cx);
                } else if let Some(drag) = &mut this.column_drag {
                    if !drag.moved && (f32::from(event.position.x) - drag.start_x).abs() > 6. {
                        drag.moved = true;
                        cx.notify();
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_column_drag(cx)),
            )
        })
    }

    /// 時間がたって消えていく知らせ。少し下へずれながら薄くなる。
    fn notice_fade(&self, notice: impl IntoElement) -> AnyElement {
        let clock = &self.notice_clock;
        let notice = div().relative().child(notice);
        if !clock.closing || self.error.is_some() {
            return notice.into_any_element();
        }
        notice
            .with_animation(
                ("notice-out", clock.closes as usize),
                Animation::new(crate::state::NOTICE_FADE).with_easing(ease_in_out),
                |d, t| d.opacity(1. - t).top(px(8. * t)),
            )
            .into_any_element()
    }

    /// ほかのウィンドウの中のペインとして出す。左のメニューと上のバーは出さない。
    fn render_embedded(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.arm_notice(cx);
        self.drops.begin(crate::drops::PANE);
        let theme = cx.theme().clone();
        let bounds = self.pane_bounds.unwrap_or_default();
        let width = bounds.size.width;
        let panel_width = self.panel_split(width);
        // 詳細を開いている間、一覧はその左の残りの幅に収まる
        let beside = panel_width.filter(|_| self.detail.is_some() && !crate::theme::overlay(cx));
        let list_width = width - beside.unwrap_or(px(0.));
        let table = list_width >= px(880.) && !crate::theme::cards(cx);
        self.list_width = f32::from(list_width) / crate::UI_SCALE;
        // 行の詰め方が変わっていたら、行の高さを作り直す
        if self.row_density != crate::theme::density(cx) {
            self.size_rows(cx);
        }
        let main = match self.nav {
            Nav::Home => self.render_home(table, cx).into_any_element(),
            _ => self.render_list(table, cx).into_any_element(),
        };
        div()
            .id("pane-view")
            .track_focus(&self.focus)
            .map(|d| self.with_drags(d, cx))
            .size_full()
            .relative()
            // ペインの後ろには、ウィンドウが敷いた画像か、浮かせたカードが見えている
            .when(
                crate::theme::backdrop(cx).is_none() && !crate::theme::floating(cx),
                |d| d.bg(theme.background),
            )
            .text_color(theme.foreground)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(list_width)
                    .child(main),
            )
            .children(self.render_panel_motion(panel_width, window, cx))
            .children(self.panel_drag_layer(cx))
            .children(self.task_drag_listener(cx))
            .children(self.render_modal(cx))
            .children(self.render_closing_dialog(cx))
            .when_some(
                self.error
                    .clone()
                    .map(|e| (e, true))
                    .or(self.notice.clone().map(|n| (n, false))),
                |d, (text, is_error)| {
                    d.child(
                        div()
                            .absolute()
                            .bottom_4()
                            .left_0()
                            .right_0()
                            .flex()
                            .justify_center()
                            .child(
                                self.notice_fade(
                                    div()
                                        .id("pane-notice")
                                        .px_3()
                                        .py_2()
                                        .rounded(feel::md())
                                        .elevate(Elevation::High, cx)
                                        .text_sm()
                                        .cursor_pointer()
                                        .when(is_error, |d| {
                                            d.bg(theme.danger).text_color(theme.danger_foreground)
                                        })
                                        .when(!is_error, |d| {
                                            d.bg(theme.primary).text_color(theme.primary_foreground)
                                        })
                                        .child(text)
                                        .when(
                                            !is_error
                                                && self.notice.is_some()
                                                && self.notice == self.notice_undo,
                                            |d| d.child(notice_undo(cx)),
                                        )
                                        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                                            this.hover_notice(*hovered, cx)
                                        }))
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.error = None;
                                            this.notice = None;
                                            cx.notify();
                                        })),
                                ),
                            ),
                    )
                },
            )
            .into_any_element()
    }
}

impl Render for MainView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 「アカウントを追加」を Esc や外側で閉じたら、調べている途中の結果を捨てる。
        // 前に描いたときに出していたかで見る（開いた直後は、まだ `sub_modal` に入っていない）
        if self.sub_seen == Some(SubModal::AddAccount)
            && self.sub_modal != Some(SubModal::AddAccount)
        {
            self.caldav_settings.update(cx, |s, cx| s.end_adding(cx));
        }
        self.tick_dialog_motion(window);
        if self.embedded {
            return self.render_embedded(window, cx);
        }
        self.arm_notice(cx);
        // 起動してすぐ出した AI のおすすめを、Esc で閉じられるようにする
        if std::mem::take(&mut self.ai_promo_focus) {
            self.focus.focus(window, cx);
        }
        // 落とせる場所を覚え直す。描かれなかったペインと AI bot の場所は残さない
        self.drops.begin(crate::drops::ROOT);
        for child in self.children.values() {
            child.read(cx).drops.clear();
        }
        if let Some(pane) = &self.ai_pane {
            pane.read(cx).drops.clear();
        }
        let theme = cx.theme().clone();
        let window_width = window.viewport_size().width;
        // 右に AI bot を置いた分、タスクの画面は狭くなる
        let ai = self.ai_docked(cx).then(|| self.ai_pane.clone()).flatten();
        let ai_width = ai.as_ref().and_then(|_| {
            let s = self.ai_session.read(cx);
            let shell = s.shell_place == ShellPlace::Docked;
            docked_width(window_width, shell, (s.chat_width, s.shell_width))
        });
        let width = window_width - ai_width.unwrap_or(px(0.));
        let unlocked = self.unlocked(cx);
        // 狭いウィンドウでは左のメニューをたたみ、詳細は全面のシートにする
        let wide = width >= TASKS_MIN;
        let sidebar = if wide {
            !self.sidebar_hidden
        } else {
            self.sidebar_open && self.panel.is_none()
        };
        if !unlocked {
            // ロックの画面に、閉じかけの詳細は出さない
            self.panel_closing = None;
            return col()
                .size_full()
                .bg(theme.background)
                .text_color(theme.foreground)
                .child(self.render_header(false, wide, window, cx))
                .child(div().flex_1().min_h_0().child(self.render_lock_screen(cx)))
                // ロックしている間は、ブラウザのページも隠す
                .child(crate::browser::frame_end(true, window, cx))
                .into_any_element();
        }
        let main_width = if wide && sidebar {
            width - px(self.sidebar_width)
        } else if wide {
            width - px(RAIL_WIDTH)
        } else {
            width
        };
        // ペインを並べる場所。上のバーの下で、左のメニューの右
        let area = Bounds::new(
            point(width - main_width, HEADER_HEIGHT),
            size(
                main_width,
                (window.viewport_size().height - HEADER_HEIGHT).max(px(0.)),
            ),
        );
        // 分けているときは、このウィンドウが兼ねているペインの幅で決める
        let own_width = self
            .layout
            .visible()
            .into_iter()
            .find(|(id, _)| *id == self.primary)
            .map_or(main_width, |(_, rect)| main_width * rect.w);
        let panel_width = self.panel_split(own_width);
        // 詳細を開いている間、一覧はその左の残りの幅に収まる
        let beside = panel_width.filter(|_| self.detail.is_some() && !crate::theme::overlay(cx));
        let list_width = own_width - beside.unwrap_or(px(0.));
        // 表は、タイトルの列に読める幅が残るときだけ
        let table = list_width >= px(880.) && !crate::theme::cards(cx);
        self.list_width = f32::from(list_width) / crate::UI_SCALE;
        // 行の詰め方が変わっていたら、行の高さを作り直す
        if self.row_density != crate::theme::density(cx) {
            self.size_rows(cx);
        }

        let main = if !wide && sidebar {
            // 狭い画面でメニューを開いているあいだは、メニューだけを見せる
            self.render_sidebar(wide, cx)
                .w_full()
                .flex_1()
                .min_h_0()
                .into_any_element()
        } else {
            match self.nav {
                Nav::Home => self.render_home(table, cx).into_any_element(),
                _ => self.render_list(table, cx).into_any_element(),
            }
        };

        // 詳細は右から入って右へ抜け、ダイアログはふわっと出る
        let panel = self.render_panel_motion(panel_width, window, cx);
        // 幅が足りないときは、AI bot を上のバーの下で、タスクの画面の右に重ねる
        let (ai_beside, ai_over) = if ai_width.is_some() {
            (ai, None)
        } else {
            (None, ai)
        };
        let page_fill = page_fill(&theme, cx);
        let body = div()
            .id("main-view")
            .key_context("MainView")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss_in(window, cx)))
            // 色を選ぶところにフォーカスがあると、Esc はそちらの取り消しとして届く
            .on_action(
                cx.listener(|this, _: &gpui_kit::base::actions::Cancel, window, cx| {
                    this.dismiss_in(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ToggleAi, window, cx| this.toggle_ai(window, cx)))
            .on_action(cx.listener(|this, _: &PanePrefix, _, cx| {
                this.pane_prefix = true;
                cx.notify();
            }))
            // Ctrl+B のあとのキーとリサイズモードのキーは、入力欄より先に受ける
            .when(self.pane_prefix || self.pane_resize, |d| {
                d.capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if this.pane_key(&event.keystroke, window, cx) {
                        crate::caret::cancel(cx);
                        cx.stop_propagation();
                    }
                }))
            })
            .map(|d| self.with_shortcuts(d, cx))
            .map(|d| self.with_drags(d, cx))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            // パネルを浮かせるときは、すき間に少し暗い地が見える
            .bg(if crate::theme::floating(cx) {
                theme.muted
            } else {
                theme.background
            })
            .text_color(theme.foreground)
            .children(backdrop_layer(cx))
            // 操作がないまま決めた時間が過ぎたらロックするので、マウスの操作を見張る
            .child(crate::db::input_watcher())
            // 上のバーは、ウィンドウの端から端まで 1 本
            .child(self.render_header(true, wide, window, cx))
            .child(
                row()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .when(wide, |d| d.child(self.render_side(cx)))
                    .child(
                        // 上のバーの下に、タスクの画面と AI bot を横に並べる
                        row()
                            .flex_1()
                            .min_h_0()
                            .relative()
                            // 背景の画像の上では、中身の後ろも薄く塗って文字を読めるようにする
                            .when(
                                !crate::theme::floating(cx) && crate::theme::backdrop(cx).is_some(),
                                |d| d.bg(page_fill),
                            )
                            .items_stretch()
                            .child(
                                // 画面を切り替えたときは、少し下から浮かび上がる
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .relative()
                                    // 浮かせたカードはタスクの画面だけを包み、右に並べた AI bot の
                                    // 手前で角丸の端として閉じる。分けているときは、ペインごとにカードを置く
                                    .when(
                                        crate::theme::floating(cx)
                                            && self.layout.panes().len() == 1,
                                        |d| {
                                            let half = FLOAT_GAP / 2.;
                                            d.child(floating_card(
                                                page_fill,
                                                [half, FLOAT_GAP, FLOAT_GAP, half],
                                                cx,
                                            ))
                                        },
                                    )
                                    .child({
                                        let own = div()
                                            .size_full()
                                            .relative()
                                            .child(
                                                div()
                                                    .absolute()
                                                    .top_0()
                                                    .bottom_0()
                                                    .left_0()
                                                    .right(beside.unwrap_or(px(0.)))
                                                    .child(main)
                                                    .with_animation(
                                                        ElementId::Name(
                                                            format!("page-{:?}", self.nav).into(),
                                                        ),
                                                        appear(160),
                                                        |page, t| {
                                                            page.top(px(8.) * (1. - t)).opacity(t)
                                                        },
                                                    ),
                                            )
                                            .children(panel)
                                            .into_any_element();
                                        self.render_panes(own, area, cx)
                                    }),
                            )
                            .children(ai_beside)
                            .children(ai_over),
                    ),
            )
            .children(self.task_drag_listener(cx))
            // ドラッグ中のタスクは、マウスに付いて動く
            .children(self.task_drag_ghost(cx))
            .children(self.ws_drag_listener(cx))
            .children(self.ws_drag_ghost(cx))
            .children(self.panel_drag_layer(cx))
            .children(self.render_pane_overlay(cx))
            .children(self.render_modal(cx))
            .children(self.render_closing_dialog(cx))
            .when_some(
                self.error
                    .clone()
                    .map(|e| (e, true))
                    .or(self.notice.clone().map(|n| (n, false))),
                |d, (text, is_error)| {
                    d.child(
                        div()
                            .absolute()
                            .bottom_4()
                            .left_0()
                            .right_0()
                            .flex()
                            .justify_center()
                            .child(
                                self.notice_fade(
                                    row()
                                        .id("notice")
                                        // 下に重なったボタンまで押さない
                                        .occlude()
                                        .gap_3()
                                        .px_3()
                                        .py_2()
                                        .rounded(feel::md())
                                        .elevate(Elevation::High, cx)
                                        .text_sm()
                                        .cursor_pointer()
                                        .when(is_error, |d| {
                                            d.bg(theme.danger).text_color(theme.danger_foreground)
                                        })
                                        .when(!is_error, |d| {
                                            d.bg(theme.primary).text_color(theme.primary_foreground)
                                        })
                                        .child(text)
                                        .when(
                                            !is_error
                                                && self.notice.is_some()
                                                && self.notice == self.notice_undo,
                                            |d| d.child(notice_undo(cx)),
                                        )
                                        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                                            this.hover_notice(*hovered, cx)
                                        }))
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.error = None;
                                            this.notice = None;
                                            cx.notify();
                                        }))
                                        .test_support(),
                                ),
                            ),
                    )
                },
            )
            // ブラウザのページは画面の上に重なるので、ダイアログやメニューが出ている間は隠す
            .child(crate::browser::frame_end(
                !matches!(self.modal, Modal::None)
                    || self.sub_modal.is_some()
                    || self.pane_overlay.is_some()
                    || self.column_menu.is_some(),
                window,
                cx,
            ))
            .into_any_element();
        body
    }
}

/// 右クリックのメニューの「貼り付け」。クリップボードに文字がないときは押せない。
fn paste_item(view: &WeakEntity<MainView>, cx: &App) -> PopupMenuItem {
    let filled = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .is_some_and(|text| !text.trim().is_empty());
    let view = view.clone();
    PopupMenuItem::new("貼り付け")
        .disabled(!filled)
        .on_click(move |_, _, cx| {
            // ウィンドウを閉じたあとなら、反映する先がない
            let _ = view.update(cx, |this, cx| this.paste_here(cx));
        })
}

/// 自動でロックするまでの時間の表示。`None` は自動でロックしない。
fn auto_lock_label(minutes: Option<u32>) -> String {
    match minutes {
        None => "しない".to_owned(),
        Some(m) if m % 60 == 0 => format!("{} 時間", m / 60),
        Some(m) => format!("{m} 分"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn bubble_text_caps_at_99() {
        assert_eq!(bubble_text(0), None);
        assert_eq!(bubble_text(1).as_deref(), Some("1"));
        assert_eq!(bubble_text(99).as_deref(), Some("99"));
        assert_eq!(bubble_text(100).as_deref(), Some("99+"));
    }

    /// 1 桁が正円になるのは、幅の最小が高さと同じで、余白を足しても 1 文字が高さに収まるとき。
    #[::core::prelude::v1::test]
    fn one_digit_fits_in_a_circle() {
        for size in [Bubble::Bell, Bubble::Nav] {
            // 数字 1 文字の幅は文字の大きさの 0.6 倍以下
            assert!(
                size.pad() * 2. + size.text() * 0.6 <= size.height(),
                "{size:?}"
            );
            assert!(size.text() < size.height(), "{size:?}");
        }
    }
}
