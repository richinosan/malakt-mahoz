//! すべてのウィンドウで共有するデータ。

use gpui_kit::*;
use malakt_core::{lock, Store};

pub struct Db {
    pub store: Store,
    /// ロックを解除済みか。ロックが無効なら最初から `true`。
    pub unlocked: bool,
    /// パスキーの確認の答えを待っているか。待っている間は、もう 1 つ確認を出さない。
    pub verifying: bool,
    /// 添付を外のアプリで開くときに、中身を一時的に書き出す場所。
    pub opened_dir: std::path::PathBuf,
    /// 最後に見た、データベースの外からの変更の番号。
    data_version: i64,
    /// 外のコマンドに向けた「解除している」印を最後に書いた時刻（UNIX 秒）。書いていなければ `None`。
    open_mark: Option<i64>,
}

/// 解除の印を書き直す間隔（秒）。[`lock::OPEN_FOR`] より十分短くする。
const OPEN_MARK_EVERY: i64 = 60;

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
            verifying: false,
            opened_dir,
            data_version,
            open_mark: None,
        }
    }

    /// 外のコマンド（MCP サーバーなど）に向けた「解除している」印を、今の状態に合わせる。
    /// ロックが有効で解除している間は印を書き直し、ロックしている間は印を消す。
    /// 起動時、解除・ロックのあと、それと定期的に呼ぶ。
    pub fn sync_open_mark(&mut self, now: i64) {
        let open = self.unlocked && lock::is_enabled(&self.store).unwrap_or(false);
        if !open {
            // ロックが無効なら印は要らない。前に書いた印や、落ちた実行が残した印も消す
            if lock::mark_closed(&mut self.store).is_ok() {
                self.open_mark = None;
            }
        } else if self
            .open_mark
            .is_none_or(|at| (now - at).abs() >= OPEN_MARK_EVERY)
            && lock::mark_open(&mut self.store, now).is_ok()
        {
            self.open_mark = Some(now);
        }
    }

    /// 解除の印を消す。アプリを閉じるときに呼ぶ。
    pub fn close_open_mark(&mut self) {
        if lock::mark_closed(&mut self.store).is_ok() {
            self.open_mark = None;
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

pub fn init(mut store: Store, cx: &mut App) {
    // 自分はいつでも担当に選べるようにしておく
    let _ = store.ensure_self();
    let unlocked = !lock::is_enabled(&store).unwrap_or(false);
    let db = cx.new(|_| {
        let mut db = Db::new(store, unlocked);
        db.sync_open_mark(unix_now());
        db
    });
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
            db.sync_open_mark(unix_now());
            db.clear_opened();
            cx.notify();
        }
    });
}

/// ロックの解除・有効化のあとに呼ぶ。外のコマンドに向けた印を、今の状態に合わせる。
pub fn sync_open_mark(cx: &mut App) {
    shared(cx).update(cx, |db, _| db.sync_open_mark(unix_now()));
}

/// アプリを閉じるときに呼ぶ。閉じたあとは、ロックが有効なら外のコマンドから読めない。
pub fn close_open_mark(cx: &mut App) {
    if let Some(shared) = cx.try_global::<Shared>() {
        shared.0.clone().update(cx, |db, _| db.close_open_mark());
    }
}

/// 外からの変更を見張る間隔。MCP サーバーやコマンドからの変更を、すぐ画面に出す。
const WATCH_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// 外からの変更と日付の変わり目を見張る。2 秒ごとに確かめ、変わっていたら全ウィンドウを読み直させる
/// （読み直しのときに、期限が今日の通知も作られる）。
pub fn watch(cx: &mut App) {
    cx.spawn(async move |cx| {
        let mut day = malakt_core::view::today();
        loop {
            cx.background_executor().timer(WATCH_EVERY).await;
            let today = malakt_core::view::today();
            let new_day = std::mem::replace(&mut day, today.clone()) != today;
            cx.update(|cx| {
                shared(cx).update(cx, |db, cx| {
                    db.sync_open_mark(unix_now());
                    if db.changed_outside() || new_day {
                        cx.notify();
                    }
                })
            });
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::{Db, OPEN_MARK_EVERY};
    use malakt_core::{lock, Store};

    #[test]
    fn the_open_mark_follows_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.sqlite3");
        // 外のコマンドと同じように、別の接続から見る
        let outside = Store::open(&path).unwrap();
        let allows = |now| lock::allows_outside(&outside, now).unwrap();

        let mut store = Store::open(&path).unwrap();
        lock::enable(&mut store).unwrap();
        // 落ちた実行が残した印は、起動したとき（ロックしている）に消える
        lock::mark_open(&mut store, 1_000).unwrap();
        let mut db = Db::new(store, false);
        db.sync_open_mark(1_000);
        assert!(!allows(1_000));

        // 解除すると読める。印は 1 分ごとに書き直す
        db.unlocked = true;
        db.sync_open_mark(1_000);
        assert!(allows(1_000));
        db.sync_open_mark(1_000 + OPEN_MARK_EVERY - 1);
        assert!(!allows(1_001 + lock::OPEN_FOR));
        db.sync_open_mark(1_000 + OPEN_MARK_EVERY);
        assert!(allows(1_001 + lock::OPEN_FOR));

        // ロックすると、すぐ読めなくなる
        db.unlocked = false;
        db.sync_open_mark(1_100);
        assert!(!allows(1_100));

        // 閉じるときも消す
        db.unlocked = true;
        db.sync_open_mark(1_200);
        assert!(allows(1_200));
        db.close_open_mark();
        assert!(!allows(1_200));

        // ロックが無効なら、印は書かない（読めるのは変わらない）
        lock::disable(&mut db.store).unwrap();
        db.sync_open_mark(1_300);
        assert!(db.open_mark.is_none());
        assert!(allows(9_999));
    }
}
