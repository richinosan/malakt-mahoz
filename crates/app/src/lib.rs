//! malakt-mahoz の画面。gpui で描く。

pub mod cli;
pub mod db;
mod render;
pub mod state;

use std::path::PathBuf;

use gpui_kit::*;

pub use db::{Db, Shared};
pub use state::{anchor_at, CommentMode, MainView, Modal, Nav};

actions!(malakt, [NewWindow, Quit, LockNow, Dismiss]);

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
    // OS の通知に出るアプリの名前。通知を出す前に決めておく
    cx.set_app_identity("io.github.richinosan.malakt-mahoz", "malakt-mahoz");
    gpui_kit::init(cx);
    db::init(store, cx);
    cx.bind_keys([
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
        KeyBinding::new("escape", Dismiss, Some("MainView")),
    ]);
    cx.on_action(|_: &NewWindow, cx| {
        let _ = open_main_window(cx);
    });
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &LockNow, cx| db::lock_now(cx));
    cx.set_menus([Menu {
        name: "malakt-mahoz".into(),
        items: vec![
            MenuItem::action("新しいウィンドウ", NewWindow),
            MenuItem::action("今すぐロック", LockNow),
            MenuItem::separator(),
            MenuItem::action("終了", Quit),
        ],
        disabled: false,
    }]);
}

/// メインのウィンドウを 1 枚開く。何枚でも開ける。
pub fn open_main_window(cx: &mut App) -> Result<(AnyWindowHandle, Entity<MainView>)> {
    // 2 枚目からは少しずらして重ならないようにする
    let offset = px(28.) * (cx.windows().len() % 8) as f32;
    let bounds = Bounds {
        origin: point(px(80.) + offset, px(60.) + offset),
        size: size(px(1180.), px(760.)),
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(360.), px(420.))),
        titlebar: Some(TitlebarOptions {
            title: Some("malakt-mahoz".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| MainView::new(window, cx))
    })
}
