//! malakt-mahoz の画面。gpui で描く。

pub mod ai;
pub mod ai_view;
pub mod cli;
pub mod db;
mod render;
pub mod state;
pub mod theme;

use std::path::PathBuf;

use gpui_kit::*;

pub use ai::AiConfig;
pub use ai_view::{AiPane, PaneMode};
pub use db::{Db, Shared};
pub use state::{anchor_at, Assets, CommentMode, MainView, Modal, Nav};

actions!(
    malakt,
    [NewWindow, Quit, LockNow, Dismiss, ToggleAi, StopAi, ClearShell]
);

/// 画面に出すアプリの名前。
pub const APP_NAME: &str = "Malakt Mahoz";
/// OS がウィンドウや通知をこのアプリのものと見分けるための名前。
pub const APP_ID: &str = "io.github.richinosan.malakt-mahoz";

/// データベースの場所。`MALAKT_MAHOZ_DB` で変えられる。
pub fn database_path() -> PathBuf {
    if let Some(path) = std::env::var_os("MALAKT_MAHOZ_DB") {
        return PathBuf::from(path);
    }
    let dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("malakt-mahoz");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("data.sqlite3")
}

/// アプリ全体の準備。ウィンドウを開く前に 1 回呼ぶ。
pub fn init(store: malakt_core::Store, cx: &mut App) {
    init_with(store, AiConfig::default(), cx);
}

/// AI の外とのやり取り（API、キーチェーン、gh）を差し替えて準備する。テスト用。
pub fn init_with(store: malakt_core::Store, ai: AiConfig, cx: &mut App) {
    // OS の通知に出るアプリの名前。通知を出す前に決めておく
    cx.set_app_identity(APP_ID, APP_NAME);
    gpui_kit::init(cx);
    load_fonts(cx);
    db::init(store, cx);
    ai::init(ai, cx);
    theme::init(cx);
    cx.bind_keys([
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-j", ToggleAi, Some("MainView")),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-j", ToggleAi, Some("MainView")),
        // 入力欄の中では、Ctrl+C はコピーのまま
        KeyBinding::new("ctrl-c", StopAi, Some("AiChat")),
        KeyBinding::new("ctrl-c", StopAi, Some("AiShell")),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-n", NewWindow, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-n", NewWindow, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-q", Quit, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-q", Quit, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-l", LockNow, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-l", LockNow, None),
        // シェルの中では、Ctrl+L は画面を消す。場所を決めていない割り当て（ロック）は
        // いちばん深い場所と同じ強さなので、入力行の中の分も、ロックより後に置く
        KeyBinding::new("ctrl-l", ClearShell, Some("AiShell")),
        KeyBinding::new("ctrl-l", ClearShell, Some("AiShell > Input")),
        KeyBinding::new("escape", Dismiss, Some("MainView")),
    ]);
    cx.on_action(|_: &NewWindow, cx| {
        let _ = open_main_window(cx);
    });
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &LockNow, cx| db::lock_now(cx));
    cx.set_menus([Menu {
        name: APP_NAME.into(),
        items: vec![
            MenuItem::action("新しいウィンドウ", NewWindow),
            MenuItem::action("AI チャット", ToggleAi),
            MenuItem::action("今すぐロック", LockNow),
            MenuItem::separator(),
            MenuItem::action("終了", Quit),
        ],
        disabled: false,
    }]);
}

/// メインのウィンドウを 1 枚開く。何枚でも開ける。
pub fn open_main_window(cx: &mut App) -> Result<(AnyWindowHandle, Entity<MainView>)> {
    let screen = cx.primary_display().map(|display| display.bounds().size);
    let mut bounds = Bounds::centered(None, initial_window_size(screen), cx);
    // 2 枚目からは少しずらして重ならないようにする
    let offset = px(28.) * (cx.windows().len() % 8) as f32;
    bounds.origin += point(offset, offset);
    // OS のタイトルバーは出さず、アプリの中の細いバーで代える
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(360.), px(420.))),
        titlebar: Some(TitlebarOptions {
            title: Some(APP_NAME.into()),
            ..gpui_kit::component::TitleBar::title_bar_options()
        }),
        #[cfg(target_os = "linux")]
        window_decorations: Some(WindowDecorations::Client),
        app_id: Some(APP_ID.into()),
        icon: window_icon(),
        ..gpui_kit::component::TitleBar::window_options()
    };
    gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| MainView::new(window, cx))
    })
}

/// AI チャット、またはシェルだけのウィンドウを開く。
pub fn open_ai_window(mode: PaneMode, cx: &mut App) -> Result<AnyWindowHandle> {
    let (title, window_size) = match mode {
        PaneMode::ShellWindow => ("AI のシェル - Malakt Mahoz", size(px(640.), px(480.))),
        _ => ("AI チャット - Malakt Mahoz", size(px(860.), px(720.))),
    };
    let offset = px(28.) * (cx.windows().len() % 8) as f32;
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(160.) + offset, px(100.) + offset),
            size: window_size,
        })),
        window_min_size: Some(size(px(320.), px(280.))),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| AiPane::new(mode, None, window, cx))
    })
    .map(|(handle, _)| handle)
}

/// 同梱のフォントを読み込んで、画面全体の書体にする。
fn load_fonts(cx: &mut App) {
    let fonts: Vec<std::borrow::Cow<'static, [u8]>> = vec![
        include_bytes!("../assets/fonts/LINESeedJP-Regular.ttf")
            .as_slice()
            .into(),
        include_bytes!("../assets/fonts/LINESeedJP-Bold.ttf")
            .as_slice()
            .into(),
    ];
    // 書体の名前は配色の設定（theme.rs）に書いてある
    let _ = cx.text_system().add_fonts(fonts);
}

/// 最初のウィンドウの大きさ。画面の半分を目安に、1280×720 から 1920×1080 の間で決める。
/// 4K の画面なら 1920×1080 になる。画面からはみ出す大きさにはしない。
pub fn initial_window_size(screen: Option<Size<Pixels>>) -> Size<Pixels> {
    let Some(screen) = screen else {
        return size(px(1280.), px(720.));
    };
    let screen_w = f32::from(screen.width);
    let screen_h = f32::from(screen.height);
    let width = (screen_w / 2.).clamp(1280., 1920.).min(screen_w * 0.92);
    let height = (width * 9. / 16.).min(screen_h * 0.88);
    size(px(width.round()), px(height.round()))
}

/// ウィンドウのアイコン（X11 で使う。ほかの OS は実行ファイルやバンドルのアイコンを使う）。
fn window_icon() -> Option<std::sync::Arc<image::RgbaImage>> {
    static ICON: std::sync::OnceLock<Option<std::sync::Arc<image::RgbaImage>>> =
        std::sync::OnceLock::new();
    ICON.get_or_init(|| {
        image::load_from_memory_with_format(
            include_bytes!("../assets/icon/icon.png"),
            image::ImageFormat::Png,
        )
        .ok()
        .map(|icon| std::sync::Arc::new(icon.into_rgba8()))
    })
    .clone()
}

#[cfg(test)]
mod tests {
    // `super::*` は gpui の `test` マクロまで持ち込むので、使うものだけを書く
    use super::initial_window_size;
    use gpui_kit::{px, size};

    #[test]
    fn window_size_follows_the_screen() {
        let of = |w: f32, h: f32| {
            let s = initial_window_size(Some(size(px(w), px(h))));
            (f32::from(s.width), f32::from(s.height))
        };
        // 4K
        assert_eq!(of(3840., 2160.), (1920., 1080.));
        // WQHD
        assert_eq!(of(2560., 1440.), (1280., 720.));
        // フル HD
        assert_eq!(of(1920., 1080.), (1280., 720.));
        // 5K 以上でも 1920 まで
        assert_eq!(of(5120., 2880.), (1920., 1080.));
        // 小さい画面でははみ出さない
        let (w, h) = of(1366., 768.);
        assert!(w < 1366. && h < 768.);
        assert_eq!(initial_window_size(None), size(px(1280.), px(720.)));
    }
}
