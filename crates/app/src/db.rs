//! すべてのウィンドウで共有するデータ。

use gpui_kit::*;
use malakt_core::{lock, Store};

pub struct Db {
    pub store: Store,
    /// ロックを解除済みか。ロックが無効なら最初から `true`。
    pub unlocked: bool,
}

/// 共有データへの入口。どのウィンドウも同じ [`Db`] を見て、変更の通知を受ける。
pub struct Shared(pub Entity<Db>);

impl Global for Shared {}

pub fn init(store: Store, cx: &mut App) {
    let unlocked = !lock::is_enabled(&store).unwrap_or(false);
    let db = cx.new(|_| Db { store, unlocked });
    cx.set_global(Shared(db));
}

pub fn shared(cx: &App) -> Entity<Db> {
    cx.global::<Shared>().0.clone()
}

/// すべてのウィンドウをロックする。ロックが無効なら何もしない。
pub fn lock_now(cx: &mut App) {
    shared(cx).update(cx, |db, cx| {
        if lock::is_enabled(&db.store).unwrap_or(false) {
            db.unlocked = false;
            cx.notify();
        }
    });
}
