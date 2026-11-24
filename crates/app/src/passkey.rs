//! パスキー（OS の本人確認）への入口。テストでは差し替える。

use std::sync::Arc;

use gpui_kit::*;
pub use malakt_passkey::{Failure, Request};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// 本人確認をする関数。OS のダイアログに答えるまで戻らない。
pub type Verify = Arc<dyn Fn(Request) -> Result<(), Failure> + Send + Sync>;

pub struct Passkey(pub Verify);

impl Global for Passkey {}

pub fn init(cx: &mut App) {
    cx.set_global(Passkey(Arc::new(|request| {
        malakt_passkey::verify(&request)
    })));
}

/// Windows Hello のダイアログを重ねるウィンドウ。
pub fn native_window(window: &Window) -> Option<isize> {
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}
