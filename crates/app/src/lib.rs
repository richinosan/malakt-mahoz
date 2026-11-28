//! malakt-mahoz の画面。gpui で描く。

pub mod ai;
pub mod ai_settings;
pub mod ai_tools;
pub mod ai_view;
pub mod browser;
pub mod browser_popup;
mod caret;
pub mod cli;
pub mod date_pick;
pub mod db;
pub mod downloads;
pub mod drops;
mod feel;
mod history_view;
pub mod icons;
pub mod mcp;
mod mcp_keys;
pub mod mcp_register;
pub mod panes;
pub mod passkey;
mod press;
pub mod reminders;
mod render;
mod repeat_view;
pub mod scroll;
pub mod session;
pub mod shadow;
pub mod shell_keys;
pub mod shortcuts;
pub mod state;
mod task_urls;
pub mod term;
pub mod theme;
pub mod update;
mod webview;
pub mod ws_drag;

use std::path::PathBuf;

use gpui_kit::*;

pub use ai::AiConfig;
pub use ai_view::{AiPane, PaneMode};
pub use db::{Db, Shared};
pub use state::{anchor_at, Assets, CommentMode, MainView, Modal, Nav, NOTICE_FADE, NOTICE_LIFE};

actions!(
    malakt,
    [
        NewWindow,
        Quit,
        LockNow,
        Dismiss,
        ToggleAi,
        StopAi,
        ClearShell,
        PanePrefix,
        CancelKeyEdit
    ]
);

/// 画面に出すアプリの名前。
pub const APP_NAME: &str = "Malakt Mahoz";
/// 寸法（px で決めた幅・高さ）の拡大率。
pub const UI_SCALE: f32 = 1.25;
/// 文字（rem）の拡大率。寸法より控えめにする。
pub const TEXT_SCALE: f32 = 1.1;
/// OS がウィンドウや通知をこのアプリのものと見分けるための名前。
pub const APP_ID: &str = "io.github.richinosan.malakt-mahoz";

/// データベースの場所。`MALAKT_MAHOZ_DB` で変えられる。
///
/// この PC だけのデータを置く場所に置く（Windows は `%LOCALAPPDATA%`、macOS は
/// `~/Library/Application Support`、Linux は `~/.local/share`）。タスクも添付も設定も、
/// この 1 つのファイルに入る。
pub fn database_path() -> PathBuf {
    if let Some(path) = std::env::var_os("MALAKT_MAHOZ_DB") {
        return PathBuf::from(path);
    }
    let dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(DATA_DIR);
    // 失敗したら、このあとデータベースを開くところでエラーになる
    let _ = std::fs::create_dir_all(&dir);
    // 前の版は、Windows ではほかの PC と同期される場所（%APPDATA%）に置いていた
    if let Some(old) = dirs::data_dir().map(|old| old.join(DATA_DIR)) {
        if let Err(error) = move_database(&old, &dir) {
            eprintln!(
                "データベースを {} から {} へ移せませんでした: {error}",
                old.display(),
                dir.display()
            );
            return old.join(DATA_FILE);
        }
    }
    dir.join(DATA_FILE)
}

const DATA_DIR: &str = "malakt-mahoz";
const DATA_FILE: &str = "data.sqlite3";

/// 前の場所にあるデータベースを、新しい場所へ移す。新しい場所にもうあれば何もしない。
/// 途中で失敗したら、移した分を戻して、前の場所のまま使えるようにする。
pub fn move_database(old: &std::path::Path, new: &std::path::Path) -> std::io::Result<()> {
    if old == new || !old.join(DATA_FILE).exists() || new.join(DATA_FILE).exists() {
        return Ok(());
    }
    // 本体のほかに、書きかけの分（-wal）と、その目次（-shm）がある
    let names = [
        format!("{DATA_FILE}-wal"),
        format!("{DATA_FILE}-shm"),
        DATA_FILE.to_owned(),
    ];
    let mut moved: Vec<&String> = Vec::new();
    for name in &names {
        let from = old.join(name);
        if !from.exists() {
            continue;
        }
        // 別のドライブへは名前の付け替えができないので、写してから消す
        let result = std::fs::rename(&from, new.join(name)).or_else(|_| {
            std::fs::copy(&from, new.join(name)).and_then(|_| std::fs::remove_file(&from))
        });
        if let Err(error) = result {
            for name in moved {
                // 戻せなかった分は新しい場所に残り、次に開くときにもう一度試す
                let _ = std::fs::rename(new.join(name), old.join(name));
            }
            let _ = std::fs::remove_file(new.join(name));
            return Err(error);
        }
        moved.push(name);
    }
    Ok(())
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
    // 部品の中の文言（色を選ぶところの「パレット」など）も日本語にする
    gpui_kit::component::set_locale("ja");
    load_fonts(cx);
    db::init(store, cx);
    db::watch_keys(cx);
    passkey::init(cx);
    ai::init(ai, cx);
    term::init(cx);
    downloads::init(cx);
    reminders::init(cx);
    update::init(cx);
    theme::init(cx);
    cx.bind_keys([
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-j", ToggleAi, Some("MainView")),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-j", ToggleAi, Some("MainView")),
        // 入力欄の中では、Ctrl+C はコピーのまま
        KeyBinding::new("ctrl-c", StopAi, Some("AiChat")),
        KeyBinding::new("ctrl-c", StopAi, Some("AiShell")),
        // Ctrl+N は新しいタスク（shortcuts.rs）
        KeyBinding::new(secondary!("shift-n"), NewWindow, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-q", Quit, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-q", Quit, None),
        // ロックは Shift も一緒に押す。Ctrl+L はシェルとターミナルの「画面を消す」なので、
        // どの画面（ターミナル・ブラウザのページを含む）でも同じキーでロックできるようにする
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-l", LockNow, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-l", LockNow, None),
        // シェルの中では、Ctrl+L は画面を消す
        KeyBinding::new("ctrl-l", ClearShell, Some("AiShell")),
        KeyBinding::new("ctrl-l", ClearShell, Some("AiShell > Input")),
        KeyBinding::new("escape", Dismiss, Some("MainView")),
        // API キーを変えている間の Esc は、書きかけのキーを捨てるだけ（設定は閉じない）
        KeyBinding::new("escape", CancelKeyEdit, Some("AiKeyEditor")),
        KeyBinding::new("ctrl-b", PanePrefix, Some("MainView")),
    ]);
    shortcuts::bind(cx);
    // カレンダーの中の矢印などは、一覧のキーより後に割り当てる
    date_pick::bind(cx);
    // 場所を決めていない割り当て（ロックなど）より後に割り当てる
    shell_keys::bind(cx);
    caret::init(cx);
    cx.on_action(|_: &NewWindow, cx| {
        let _ = open_main_window(cx);
    });
    cx.on_action(|_: &Quit, cx| {
        session::quitting(cx);
        update::install_on_quit(cx);
        cx.quit()
    });
    cx.on_action(|_: &LockNow, cx| lock_now_or_settings(cx));
    // メインのウィンドウの外（小さなウィンドウ、シェルのウィンドウ）では、メインのウィンドウに出す
    cx.on_action(|_: &shell_keys::ShowDownloads, cx| show_downloads(cx));
    cx.set_menus([Menu {
        name: APP_NAME.into(),
        items: vec![
            MenuItem::action("新しいタスク", shortcuts::NewTask),
            MenuItem::action("新しいウィンドウ", NewWindow),
            MenuItem::action("元に戻す", shortcuts::Undo),
            MenuItem::action("やり直す", shortcuts::Redo),
            MenuItem::action("検索", shortcuts::Find),
            MenuItem::action("AI チャット", ToggleAi),
            MenuItem::action("全画面", shortcuts::ToggleFullscreen),
            MenuItem::action("キー操作", shortcuts::ShowKeys),
            MenuItem::action("今すぐロック", LockNow),
            MenuItem::separator(),
            MenuItem::action("終了", Quit),
        ],
        disabled: false,
    }]);
}

/// 前に閉じたときのウィンドウを、見ていた画面ごと開き直す。なければ 1 枚だけ開く。
pub fn open_saved_windows(cx: &mut App) -> Result<(AnyWindowHandle, Entity<MainView>)> {
    let mut first = None;
    for state in session::saved(cx) {
        let opened = open_window_with(Some(state), cx)?;
        first.get_or_insert(opened);
    }
    match first {
        Some(first) => Ok(first),
        None => open_main_window(cx),
    }
}

/// メインのウィンドウを 1 枚開く。何枚でも開ける。
pub fn open_main_window(cx: &mut App) -> Result<(AnyWindowHandle, Entity<MainView>)> {
    open_window_with(None, cx)
}

pub(crate) fn open_window_with(
    saved: Option<session::WindowState>,
    cx: &mut App,
) -> Result<(AnyWindowHandle, Entity<MainView>)> {
    let screen = cx.primary_display().map(|display| display.bounds().size);
    let mut bounds = Bounds::centered(None, initial_window_size(screen), cx);
    // 2 枚目からは少しずらして重ならないようにする
    let offset = px(28.) * (cx.windows().len() % 8) as f32;
    bounds.origin += point(offset, offset);
    // 前に置いていた位置と大きさがあれば、そこに開く
    let place = saved
        .as_ref()
        .and_then(|state| state.window_bounds(cx))
        .unwrap_or(WindowBounds::Windowed(bounds));
    // OS のタイトルバーは出さず、アプリの中の細いバーで代える
    let options = WindowOptions {
        window_bounds: Some(place),
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
    let opened = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| {
            let mut view = MainView::new(window, cx);
            if let Some(state) = &saved {
                view.restore_session(state, window, cx);
            }
            view.track_session(window, cx);
            view
        })
    })?;
    if session::set_main(opened.1.entity_id(), opened.0, cx) {
        // メインを最小化したら、ほかのウィンドウも最小化する。元に戻したら、一緒に戻す
        opened.0.update(cx, |_, window, _| {
            window
                .observe_window_visibility(|_, window, cx| follow_main(window, cx))
                .detach();
        })?;
    }
    Ok(opened)
}

/// ウィンドウを最小化しているか。
fn is_minimized(window: &Window) -> bool {
    // macOS では、ほかのウィンドウに隠れたときも「見えていない」になるので、ウィンドウに直接聞く
    #[cfg(target_os = "macos")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return false;
        };
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return false;
        };
        let view = handle.ns_view.as_ptr().cast::<objc2::runtime::AnyObject>();
        // SAFETY: `view` は、このウィンドウが開いている間ずっと生きている NSView。メインスレッドで呼ぶ
        unsafe {
            let native: *mut objc2::runtime::AnyObject = objc2::msg_send![view, window];
            !native.is_null() && objc2::msg_send![native, isMiniaturized]
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        !window.is_visible()
    }
}

/// メインのウィンドウの最小化と復帰に、ほかのウィンドウを合わせる。
fn follow_main(main: &mut Window, cx: &mut App) {
    let main_handle = main.window_handle();
    if is_minimized(main) {
        let mut shown = Vec::new();
        for window in cx.windows() {
            // 自分で最小化してあるウィンドウは、メインを戻すときに戻さない
            let open = window.update(cx, |_, window, _| !is_minimized(window));
            if window != main_handle && open.unwrap_or(false) {
                shown.push(window);
            }
        }
        for window in session::tuck(shown, cx) {
            // 調べたあとに閉じたウィンドウは、そのままでよい
            let _ = window.update(cx, |_, window, _| window.minimize_window());
        }
    } else {
        let tucked = session::untuck(cx);
        if tucked.is_empty() {
            return;
        }
        for window in tucked {
            // 覚えたあとに閉じたウィンドウは、そのままでよい
            let _ = window.update(cx, |_, window, _| window.activate_window());
        }
        // 最後にメインを手前に戻す
        main.activate_window();
    }
}

/// メインのウィンドウ（最初に開いた 1 枚）を閉じたら、ほかのウィンドウ（追加のウィンドウ、
/// 別ウィンドウに出したペイン、AI チャット、シェル）もすべて閉じて終了する。
/// 閉じたウィンドウは次の起動で戻す。新しい版を取ってあれば、ここで入れ替える。
pub fn quit_when_main_closes(cx: &mut App) {
    cx.on_window_closed(|cx, _| {
        if session::is_quitting(cx) || !(session::main_closed(cx) || cx.windows().is_empty()) {
            return;
        }
        session::quitting(cx);
        for window in cx.windows() {
            // もう閉じたウィンドウは、そのままでよい
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
        update::install_on_quit(cx);
        cx.quit();
    })
    .detach();
}

/// 「ロックする」。ロックが無効なときは、ロックを有効にする設定を開く。
pub(crate) fn lock_now_or_settings(cx: &mut App) {
    if db::lock_enabled(cx) {
        db::lock_now(cx);
    } else {
        open_settings_in(None, Some(state::CustomizeSection::Lock), cx);
    }
}

/// ウィンドウの中のタスクの画面。タスクの画面のウィンドウでなければ `None`。
fn main_view_in(handle: AnyWindowHandle, cx: &mut App) -> Option<Entity<MainView>> {
    handle
        .update(cx, |root, _, cx| {
            let root = root.downcast::<gpui_kit::base::Root>().ok()?;
            let view = root.read(cx).view().clone();
            view.downcast::<MainView>().ok()
        })
        .ok()
        .flatten()
}

/// 設定を開いて、そのウィンドウを手前に出す。`section` があれば、その見出しまでスクロールする。
/// `main` がない（閉じた）ときは、今のウィンドウのタスクの画面、なければメインのウィンドウに開く。
pub(crate) fn open_settings_in(
    main: Option<&WeakEntity<MainView>>,
    section: Option<state::CustomizeSection>,
    cx: &mut App,
) {
    // キーやメニューから呼ぶときは、そのウィンドウを更新している最中なので、終わってから開く
    let main = main.cloned();
    cx.defer(move |cx| open_settings_now(main.as_ref(), section, cx));
}

fn open_settings_now(
    main: Option<&WeakEntity<MainView>>,
    section: Option<state::CustomizeSection>,
    cx: &mut App,
) {
    let mut windows = cx.windows();
    // 今のウィンドウ、メインのウィンドウの順に探す
    for first in [session::main_window(cx), cx.active_window()]
        .into_iter()
        .flatten()
    {
        if let Some(ix) = windows.iter().position(|w| *w == first) {
            let window = windows.remove(ix);
            windows.insert(0, window);
        }
    }
    let wanted = main.and_then(WeakEntity::upgrade);
    let found = windows.into_iter().find_map(|handle| {
        let view = main_view_in(handle, cx)?;
        match &wanted {
            Some(wanted) if *wanted != view => None,
            _ => Some((handle, view)),
        }
    });
    let Some((handle, view)) = found else {
        return;
    };
    // 探したあとに閉じたウィンドウなら、開く先がない
    let _ = handle.update(cx, |_, window, cx| {
        window.activate_window();
        view.update(cx, |this, cx| {
            this.open_appearance(window, cx);
            if let Some(section) = section {
                this.show_appearance_section(section, window, cx);
            }
        });
    });
}

/// ダウンロードの一覧を、今のウィンドウかメインのウィンドウに出して、手前に出す。
pub(crate) fn show_downloads(cx: &mut App) {
    // キーから呼ぶときは、そのウィンドウを更新している最中なので、終わってから開く
    cx.defer(|cx| {
        let mut windows = cx.windows();
        for first in [session::main_window(cx), cx.active_window()]
            .into_iter()
            .flatten()
        {
            if let Some(ix) = windows.iter().position(|w| *w == first) {
                let window = windows.remove(ix);
                windows.insert(0, window);
            }
        }
        let Some((handle, view)) = windows
            .into_iter()
            .find_map(|handle| Some((handle, main_view_in(handle, cx)?)))
        else {
            return;
        };
        // 探したあとに閉じたウィンドウなら、開く先がない
        let _ = handle.update(cx, |_, window, cx| {
            window.activate_window();
            view.update(cx, |this, cx| this.open_downloads(window, cx));
        });
    });
}

/// 別ウィンドウの名前。中身の名前のあとに、アプリの名前を付ける。
pub fn window_title(name: &str) -> String {
    format!("{name} - {APP_NAME}")
}

/// AI チャット、またはシェルだけのウィンドウを開く。
pub fn open_ai_window(mode: PaneMode, cx: &mut App) -> Result<AnyWindowHandle> {
    let px = |value: f32| px(value * UI_SCALE);
    let (title, window_size) = match mode {
        PaneMode::ShellWindow => (
            window_title(&term::shared(cx).read(cx).active_label()),
            size(px(640.), px(480.)),
        ),
        _ => (window_title("AI チャット"), size(px(860.), px(720.))),
    };
    let offset = px(28.) * (cx.windows().len() % 8) as f32;
    // 前に置いていた位置と大きさがあれば、そこに開く
    let slot = usize::from(mode == PaneMode::ShellWindow);
    let bounds = match ai::shared(cx).read(cx).window_bounds[slot] {
        Some([x, y, w, h]) => Bounds {
            origin: point(gpui_kit::px(x), gpui_kit::px(y)),
            size: size(gpui_kit::px(w), gpui_kit::px(h)),
        },
        None => Bounds {
            origin: point(px(160.) + offset, px(100.) + offset),
            size: window_size,
        },
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(320.), px(280.))),
        // メインのウィンドウと同じく、OS のタイトルバーは出さず、中の細いバーで代える
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..gpui_kit::component::TitleBar::title_bar_options()
        }),
        #[cfg(target_os = "linux")]
        window_decorations: Some(WindowDecorations::Client),
        app_id: Some(APP_ID.into()),
        icon: window_icon(),
        ..gpui_kit::component::TitleBar::window_options()
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
        // ターミナルのプロンプト（oh-my-posh など）が使う Nerd Font のアイコン
        include_bytes!("../assets/fonts/JetBrainsMonoNerdFontMono-Regular.ttf")
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
    fn database_moves_to_the_new_place_once() {
        let root = tempfile::tempdir().unwrap();
        let (old, new) = (root.path().join("old"), root.path().join("new"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        // 前の場所に何もなければ、何もしない
        super::move_database(&old, &new).unwrap();
        assert!(!new.join("data.sqlite3").exists());

        std::fs::write(old.join("data.sqlite3"), b"db").unwrap();
        std::fs::write(old.join("data.sqlite3-wal"), b"wal").unwrap();
        super::move_database(&old, &new).unwrap();
        assert_eq!(std::fs::read(new.join("data.sqlite3")).unwrap(), b"db");
        assert_eq!(std::fs::read(new.join("data.sqlite3-wal")).unwrap(), b"wal");
        assert!(!old.join("data.sqlite3").exists());
        assert!(!old.join("data.sqlite3-wal").exists());

        // 新しい場所にもうあるときは、前の場所のものを上書きしない
        std::fs::write(old.join("data.sqlite3"), b"stale").unwrap();
        super::move_database(&old, &new).unwrap();
        assert_eq!(std::fs::read(new.join("data.sqlite3")).unwrap(), b"db");
        assert!(old.join("data.sqlite3").exists());
        // 同じ場所なら何もしない（macOS と Linux）
        super::move_database(&new, &new).unwrap();
        assert_eq!(std::fs::read(new.join("data.sqlite3")).unwrap(), b"db");
    }

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
