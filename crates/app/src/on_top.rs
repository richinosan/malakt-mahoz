//! ウィンドウを常に最前面に表示する。どのウィンドウでも、上のバーの右クリックメニューで切り替える。
//!
//! 覚え方はウィンドウの種類で違う。タスクのウィンドウは、ほかの画面の状態と一緒に
//! `session.windows` に残す（[`crate::session::WindowState::on_top`]）。ポモドーロ、AI チャット、
//! シェルのウィンドウは、種類ごとに [`SETTING`] に残し、次に開いたときに戻す。
//! ブラウザの小さなウィンドウは、開いている間だけ覚える。
//!
//! MCP のサーバー（別のプロセス）は、切り替えたいことを [`ASK_PREFIX`] の設定に書く。
//! アプリは外からの変更を見張っていて（`db::watch`）、書かれたらそのウィンドウに当てて消す。

use std::collections::{HashMap, HashSet};

use gpui_kit::component::notification::Notification;
use gpui_kit::component::WindowExt;
use gpui_kit::{AnyWindowHandle, App, Global, Window, WindowId};
use malakt_core::Store;
use serde_json::{json, Map, Value};

/// ポモドーロ、AI チャット、シェルのウィンドウを最前面に出しておくか（種類の名前 → true / false）。
pub const SETTING: &str = "window.on_top";
/// MCP のサーバーからの切り替えの頼み。後ろに種類の名前が付き、中身は "1"（出す）か "0"（戻す）。
pub const ASK_PREFIX: &str = "window.on_top.ask.";

/// 最前面に出せるウィンドウの種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// タスクのウィンドウ。MCP からはメインのウィンドウ（最初に開いた 1 枚）
    Task,
    Pomodoro,
    /// AI チャットだけのウィンドウ
    Chat,
    /// シェル（ターミナルとブラウザのタブ）だけのウィンドウ
    Shell,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Task, Kind::Pomodoro, Kind::Chat, Kind::Shell];

    /// API（api/app.tsp と api/mcp.tsp の OnTopWindow）で使う名前。
    pub fn key(self) -> &'static str {
        match self {
            Kind::Task => "task",
            Kind::Pomodoro => "pomodoro",
            Kind::Chat => "chat",
            Kind::Shell => "shell",
        }
    }

    pub fn parse(key: &str) -> Result<Kind, String> {
        Kind::ALL
            .into_iter()
            .find(|kind| kind.key() == key)
            .ok_or_else(|| format!("window は task / pomodoro / chat / shell のどれかです: {key}"))
    }

    /// 文の中で使う名前。
    pub fn label(self) -> &'static str {
        match self {
            Kind::Task => "タスクのウィンドウ",
            Kind::Pomodoro => "ポモドーロのウィンドウ",
            Kind::Chat => "AI チャットのウィンドウ",
            Kind::Shell => "シェルのウィンドウ",
        }
    }
}

/// 今、最前面に出しているウィンドウと、開いているウィンドウの種類。
#[derive(Default)]
pub(crate) struct Pinned {
    on: HashSet<WindowId>,
    kinds: HashMap<WindowId, Kind>,
}

impl Global for Pinned {}

/// このウィンドウを最前面に出しているか。
pub fn is_on(window: &Window, cx: &App) -> bool {
    is_on_id(window.window_handle().window_id(), cx)
}

fn is_on_id(id: WindowId, cx: &App) -> bool {
    cx.try_global::<Pinned>()
        .is_some_and(|pinned| pinned.on.contains(&id))
}

/// 最前面に出す・戻す。OS のウィンドウを変えてから、覚える。OS が受け付けなかったら、何も変えずに理由を返す。
pub fn set(window: &mut Window, on: bool, cx: &mut App) -> Result<(), String> {
    native(window, on)?;
    let id = window.window_handle().window_id();
    let pinned = cx.default_global::<Pinned>();
    if on {
        pinned.on.insert(id);
    } else {
        pinned.on.remove(&id);
    }
    let kind = pinned.kinds.get(&id).copied();
    // ヘッダーのピンを出し直す
    window.refresh();
    match kind {
        Some(kind @ (Kind::Pomodoro | Kind::Chat | Kind::Shell)) => remember(kind, on, cx),
        // タスクのウィンドウは、画面の状態と一緒に覚える（MainView::track_session）
        _ => Ok(()),
    }
}

/// メニューやピンのボタンから切り替える。できなかったら、そのウィンドウに知らせる。
pub fn toggle(window: &mut Window, cx: &mut App) {
    let on = !is_on(window, cx);
    if let Err(error) = set(window, on, cx) {
        fail(window, error, cx);
    }
}

/// 最前面に出しているときに、ヘッダーのウィンドウのボタンの左に出すピン。押すと解除する。
pub(crate) fn pin(
    size: gpui_kit::component::Size,
    window: &Window,
    cx: &App,
) -> Option<gpui_kit::AnyElement> {
    use gpui_kit::component::button::ButtonVariants;
    use gpui_kit::component::{ActiveTheme, Placement, Sizable};
    use gpui_kit::Styled;
    if !is_on(window, cx) {
        return None;
    }
    let primary = cx.theme().primary;
    Some(crate::feel::gloss(
        crate::press::button("window-on-top")
            .icon(crate::render::app_icon("pin").text_color(primary))
            .ghost()
            .with_size(size)
            .tooltip("最前面の表示を解除")
            .tooltip_placement(Placement::Bottom)
            .on_click(|_, window, cx| toggle(window, cx)),
    ))
}

fn fail(window: &mut Window, error: String, cx: &mut App) {
    window.push_notification(Notification::error(error).title("最前面の表示"), cx);
}

/// ウィンドウを開いたときに呼ぶ。種類を覚え、前に最前面に出していたなら、また出す。
/// タスクのウィンドウは `on` に保存してあった値を渡す。ほかの種類は保存した値を読む。
pub fn opened(handle: AnyWindowHandle, kind: Kind, on: Option<bool>, cx: &mut App) {
    cx.default_global::<Pinned>()
        .kinds
        .insert(handle.window_id(), kind);
    let on = on.unwrap_or_else(|| saved(kind, cx));
    if !on {
        return;
    }
    // ウィンドウが画面に出てから当てる（X11 は、出ているウィンドウにしか頼めない）
    cx.defer(move |cx| {
        // 開いてすぐ閉じたウィンドウなら、当てる先がない
        let _ = handle.update(cx, |_, window, cx| {
            if let Err(error) = set(window, true, cx) {
                fail(window, error, cx);
            }
        });
    });
}

/// 保存してある、その種類のウィンドウを最前面に出すか。
pub fn saved(kind: Kind, cx: &App) -> bool {
    stored(&crate::db::shared(cx).read(cx).store)
        .ok()
        .and_then(|all| all.get(kind.key()).and_then(Value::as_bool))
        .unwrap_or(false)
}

fn remember(kind: Kind, on: bool, cx: &mut App) -> Result<(), String> {
    crate::db::shared(cx).update(cx, |db, _| save_kind(&mut db.store, kind, on))
}

fn save_kind(store: &mut Store, kind: Kind, on: bool) -> Result<(), String> {
    let mut map: Map<String, Value> = store
        .setting(SETTING)
        .map_err(|e| e.to_string())?
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    map.insert(kind.key().into(), Value::Bool(on));
    store
        .set_setting(SETTING, Some(&Value::Object(map).to_string()))
        .map_err(|e| {
            format!(
                "{}を最前面に出すかを保存できませんでした: {e}",
                kind.label()
            )
        })
}

/// データベースに残してある、種類ごとの最前面の表示。タスクはメインのウィンドウのもの。
/// MCP の `get_on_top` が使う。
pub fn stored(store: &Store) -> Result<Map<String, Value>, String> {
    let core = |e: malakt_core::Error| e.to_string();
    let mut all: Map<String, Value> = store
        .setting(SETTING)
        .map_err(core)?
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let main = store
        .setting(crate::session::SETTING)
        .map_err(core)?
        .and_then(|text| serde_json::from_str::<Vec<crate::session::WindowState>>(&text).ok())
        .and_then(|windows| windows.first().map(|w| w.on_top))
        .unwrap_or(false);
    all.insert(Kind::Task.key().into(), Value::Bool(main));
    let mut out = Map::new();
    for kind in Kind::ALL {
        let on = all
            .get(kind.key())
            .and_then(Value::as_bool)
            .unwrap_or(false);
        out.insert(kind.key().into(), Value::Bool(on));
    }
    Ok(out)
}

/// MCP のサーバーから、切り替えを頼む。アプリが 2 秒以内に読んで当てる。
pub fn ask(store: &mut Store, kind: Kind, on: bool) -> Result<(), String> {
    store
        .set_setting(
            &format!("{ASK_PREFIX}{}", kind.key()),
            Some(if on { "1" } else { "0" }),
        )
        .map_err(|e| format!("{}の切り替えを頼めませんでした: {e}", kind.label()))
}

/// 開いている、その種類のウィンドウ。タスクはメインのウィンドウ。
pub fn find(kind: Kind, cx: &App) -> Option<AnyWindowHandle> {
    let open = cx.windows();
    let handle = match kind {
        Kind::Task => crate::session::main_window(cx),
        Kind::Pomodoro => crate::pomodoro::window(cx),
        Kind::Chat => crate::ai::shared(cx).read(cx).chat_window,
        Kind::Shell => match crate::ai::shared(cx).read(cx).shell_place {
            crate::ai::ShellPlace::Window(handle) => Some(handle),
            _ => None,
        },
    };
    handle.filter(|h| open.contains(h))
}

/// 種類のウィンドウを切り替える。開いていないウィンドウは、次に開いたときのために覚える。
pub fn set_kind(
    kind: Kind,
    target: Option<AnyWindowHandle>,
    on: bool,
    cx: &mut App,
) -> Result<(), String> {
    match target.or_else(|| find(kind, cx)) {
        Some(handle) => handle
            .update(cx, |_, window, cx| set(window, on, cx))
            .map_err(|_| format!("{}は閉じられました", kind.label()))?,
        None if kind == Kind::Task => Err("タスクのウィンドウが開いていません".into()),
        None => remember(kind, on, cx),
    }
}

/// 種類ごとの今の状態（AI の get_screen）。`task` は操作しているタスクのウィンドウの状態。
/// 開いていないウィンドウは、次に開いたときの状態。
pub fn states(task: bool, cx: &App) -> Value {
    let mut out = Map::new();
    for kind in Kind::ALL {
        let on = match (kind, find(kind, cx)) {
            (Kind::Task, _) => task,
            (_, Some(handle)) => is_on_id(handle.window_id(), cx),
            (_, None) => saved(kind, cx),
        };
        out.insert(kind.key().into(), json!(on));
    }
    Value::Object(out)
}

/// MCP のサーバーからの頼みを読んで当てる。外からの変更があったときに呼ぶ。
pub fn answer_asks(cx: &mut App) {
    let mut asks = Vec::new();
    let result = crate::db::shared(cx).update(cx, |db, _| {
        for kind in Kind::ALL {
            let key = format!("{ASK_PREFIX}{}", kind.key());
            let Some(value) = db.store.setting(&key)? else {
                continue;
            };
            db.store.set_setting(&key, None)?;
            asks.push((kind, value == "1"));
        }
        Ok::<_, malakt_core::Error>(())
    });
    let mut failed: Vec<String> = Vec::new();
    if let Err(e) = result {
        failed.push(format!(
            "MCP からの最前面の表示の切り替えを読めませんでした: {e}"
        ));
    }
    for (kind, on) in asks {
        if let Err(e) = set_kind(kind, None, on, cx) {
            failed.push(e);
        }
    }
    if failed.is_empty() {
        return;
    }
    // 知らせる先は、メインのウィンドウ
    if let Some(main) = crate::session::main_window(cx) {
        let text = failed.join("\n");
        // メインを閉じたあとなら、知らせる先がない
        let _ = main.update(cx, |_, window, cx| fail(window, text, cx));
    }
}

/// OS のウィンドウを、最前面に出す・戻す。
fn native(window: &Window, on: bool) -> Result<(), String> {
    use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle};
    let raw = match HasWindowHandle::window_handle(window) {
        Ok(handle) => handle.as_raw(),
        // テストのウィンドウは、OS のウィンドウを持たない
        Err(HandleError::NotSupported) => return Ok(()),
        Err(e) => return Err(format!("ウィンドウの番号を読めませんでした: {e}")),
    };
    match raw {
        #[cfg(target_os = "windows")]
        RawWindowHandle::Win32(handle) => windows(handle.hwnd.get(), on),
        #[cfg(target_os = "macos")]
        RawWindowHandle::AppKit(handle) => macos(handle.ns_view.as_ptr(), on),
        #[cfg(target_os = "linux")]
        RawWindowHandle::Xcb(handle) => x11(u32::from(handle.window), on),
        #[cfg(target_os = "linux")]
        RawWindowHandle::Xlib(handle) => x11(handle.window as u32, on),
        #[cfg(target_os = "linux")]
        RawWindowHandle::Wayland(_) => Err(
            "Wayland では、ウィンドウを最前面に表示できません（X11 のセッションで使えます）".into(),
        ),
        _ => Err("この OS では、ウィンドウを最前面に表示できません".into()),
    }
}

#[cfg(target_os = "windows")]
fn windows(hwnd: isize, on: bool) -> Result<(), String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };
    let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
    // SAFETY: `hwnd` は、このウィンドウが開いている間ずっと生きている。メインスレッドで呼ぶ
    unsafe {
        SetWindowPos(
            HWND(hwnd as *mut core::ffi::c_void),
            Some(after),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
    }
    .map_err(|e| format!("ウィンドウを最前面に表示できませんでした: {e}"))
}

#[cfg(target_os = "macos")]
fn macos(view: *mut core::ffi::c_void, on: bool) -> Result<(), String> {
    /// NSNormalWindowLevel と NSFloatingWindowLevel。
    const NORMAL: isize = 0;
    const FLOATING: isize = 3;
    let view = view.cast::<objc2::runtime::AnyObject>();
    // SAFETY: `view` は、このウィンドウが開いている間ずっと生きている NSView。メインスレッドで呼ぶ
    unsafe {
        let native: *mut objc2::runtime::AnyObject = objc2::msg_send![view, window];
        if native.is_null() {
            return Err(
                "ウィンドウを最前面に表示できませんでした: ウィンドウが見つかりません".into(),
            );
        }
        let level = if on { FLOATING } else { NORMAL };
        let _: () = objc2::msg_send![native, setLevel: level];
    }
    Ok(())
}

/// X11 では、ウィンドウマネージャーに `_NET_WM_STATE_ABOVE` を頼む（EWMH）。
#[cfg(target_os = "linux")]
fn x11(window: u32, on: bool) -> Result<(), String> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ClientMessageEvent, ConnectionExt, EventMask};
    let failed =
        |e: &dyn std::fmt::Display| format!("ウィンドウを最前面に表示できませんでした: {e}");
    let (conn, screen) = x11rb::connect(None).map_err(|e| failed(&e))?;
    let root = conn.setup().roots[screen].root;
    let atom = |name: &[u8]| -> Result<u32, String> {
        Ok(conn
            .intern_atom(false, name)
            .map_err(|e| failed(&e))?
            .reply()
            .map_err(|e| failed(&e))?
            .atom)
    };
    let state = atom(b"_NET_WM_STATE")?;
    let above = atom(b"_NET_WM_STATE_ABOVE")?;
    // 1 = 足す、0 = 外す。最後の 1 は、ふつうのアプリからの頼み
    let event = ClientMessageEvent::new(32, window, state, [u32::from(on), above, 0, 1, 0]);
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        event,
    )
    .map_err(|e| failed(&e))?;
    conn.flush().map_err(|e| failed(&e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ask, stored, Kind, ASK_PREFIX};

    #[test]
    fn kinds_round_trip() {
        for kind in Kind::ALL {
            assert_eq!(Kind::parse(kind.key()), Ok(kind));
        }
        assert!(Kind::parse("popup").is_err());
    }

    #[test]
    fn stored_reads_the_main_window_and_the_other_kinds() {
        let mut store = malakt_core::Store::open_in_memory().unwrap();
        let all = stored(&store).unwrap();
        assert!(Kind::ALL.iter().all(|k| all[k.key()] == false));
        store
            .set_setting(super::SETTING, Some(r#"{"pomodoro":true}"#))
            .unwrap();
        store
            .set_setting(
                crate::session::SETTING,
                Some(r#"[{"on_top":true},{"on_top":false}]"#),
            )
            .unwrap();
        let all = stored(&store).unwrap();
        assert_eq!(all["task"], true);
        assert_eq!(all["pomodoro"], true);
        assert_eq!(all["chat"], false);
        ask(&mut store, Kind::Shell, true).unwrap();
        assert_eq!(
            store
                .setting(&format!("{ASK_PREFIX}shell"))
                .unwrap()
                .as_deref(),
            Some("1")
        );
    }
}
