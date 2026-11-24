//! すべてのウィンドウで共有するデータ。

use gpui_kit::*;
use malakt_core::{lock, Store};

pub struct Db {
    pub store: Store,
    /// ロックを解除済みか。ロックが無効なら最初から `true`。
    pub unlocked: bool,
    /// 添付を外のアプリで開くときに、中身を一時的に書き出す場所。
    pub opened_dir: std::path::PathBuf,
    /// 最後に見た、データベースの外からの変更の番号。
    data_version: i64,
}

/// 共有データへの入口。どのウィンドウも同じ [`Db`] を見て、変更の通知を受ける。
pub struct Shared(pub Entity<Db>);

impl Global for Shared {}

/// 添付を外のアプリで開くときに、中身を一時的に書き出す場所の親。
fn opened_root() -> std::path::PathBuf {
    std::env::temp_dir().join("malakt-mahoz-opened")
}

/// 前回の実行が残した一時ファイルを消す。起動時に 1 回呼ぶ。
pub fn clear_stale_opened() {
    let _ = std::fs::remove_dir_all(opened_root());
}

impl Db {
    fn new(store: Store, unlocked: bool) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let opened_dir = opened_root().join(format!("{}-{n}", std::process::id()));
        let data_version = store.data_version().unwrap_or(0);
        Self {
            store,
            unlocked,
            opened_dir,
            data_version,
        }
    }

    /// 期限が今日のタスクの通知を作り、まだ OS に出していない通知を取り出す。
    /// 取り出したものは「出した」と記録するので、どのウィンドウから呼んでも 1 回しか出ない。
    /// 戻り値は、出す通知と、ほかのウィンドウに知らせる変更があったか。
    pub fn take_notices(&mut self, today: &str) -> (Vec<malakt_core::notice::Notice>, bool) {
        let made = self.store.sweep_due_today(today).unwrap_or(0);
        let pending = self.store.undelivered_notices().unwrap_or_default();
        if !pending.is_empty() {
            let ids: Vec<_> = pending.iter().map(|n| n.id.clone()).collect();
            let _ = self.store.mark_notices_delivered(&ids);
        }
        (pending, made > 0)
    }

    /// ほかのプロセス（完了チケットのコマンドなど）がデータベースを書き換えたか。
    pub fn changed_outside(&mut self) -> bool {
        let version = self.store.data_version().unwrap_or(self.data_version);
        std::mem::replace(&mut self.data_version, version) != version
    }

    /// 開くために書き出したファイルを消す。ロック時と終了時に呼び、ディスクに残さない。
    pub fn clear_opened(&self) {
        let _ = std::fs::remove_dir_all(&self.opened_dir);
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        self.clear_opened();
    }
}

pub fn init(store: Store, cx: &mut App) {
    let unlocked = !lock::is_enabled(&store).unwrap_or(false);
    let db = cx.new(|_| Db::new(store, unlocked));
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
            db.clear_opened();
            cx.notify();
        }
    });
}

/// 外からの変更と日付の変わり目を見張る。1 分ごとに確かめ、変わっていたら全ウィンドウを読み直させる
/// （読み直しのときに、期限が今日の通知も作られる）。
pub fn watch(cx: &mut App) {
    cx.spawn(async move |cx| {
        let mut day = malakt_core::view::today();
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(60))
                .await;
            let today = malakt_core::view::today();
            let new_day = std::mem::replace(&mut day, today.clone()) != today;
            cx.update(|cx| {
                shared(cx).update(cx, |db, cx| {
                    if db.changed_outside() || new_day {
                        cx.notify();
                    }
                })
            });
        }
    })
    .detach();
}
