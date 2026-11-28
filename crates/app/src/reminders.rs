//! OS のリマインダーとの連携。
//!
//! macOS は「リマインダー」、Windows は期限の時刻に出る通知の予約。標準はオフで、
//! 設定のメニューで選んだときだけ動く。データが変わるたびと 1 分ごとに、OS の側を合わせる。

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::*;
use malakt_core::remind::{Link, Outcome, Snapshot};
use malakt_core::Status;
pub use malakt_reminders::Target;

use crate::db::{self, Db};

/// OS の側を合わせる関数。答えが出るまで戻らないので、裏のスレッドで呼ぶ。
pub type Run = Arc<dyn Fn(Snapshot, Vec<Link>) -> Result<Outcome, String> + Send + Sync>;

/// OS への入口。テストでは差し替える。
pub struct Reminders {
    /// この OS での連携の相手。なければ、設定のメニューに出さない。
    pub target: Option<Target>,
    pub run: Run,
}

impl Global for Reminders {}

/// 連携の進み具合。すべてのウィンドウで共有する。
pub struct Mirror {
    db: Entity<Db>,
    running: bool,
    /// 合わせている間に、もう一度合わせる必要が出た。
    again: bool,
    /// 始められなかった（許可がないなど）。設定を選び直すまで、やり直さない。
    stopped: bool,
    /// 最後に合わせたときの「あってほしいもの」。同じなら、データの変更では合わせ直さない。
    last: Option<Snapshot>,
    _watch: Subscription,
}

/// 連携で失敗したこと。画面に出す。
pub struct Failed(pub SharedString);

impl EventEmitter<Failed> for Mirror {}

struct Shared(Entity<Mirror>);

impl Global for Shared {}

pub fn shared(cx: &App) -> Entity<Mirror> {
    cx.global::<Shared>().0.clone()
}

/// 設定のメニューに出す文。この OS に連携の相手がなければ `None`。
pub fn menu_label(cx: &App) -> Option<&'static str> {
    let on = enabled(cx);
    Some(match (cx.global::<Reminders>().target?, on) {
        (Target::Reminders, false) => "リマインダーと連携する",
        (Target::Reminders, true) => "リマインダーとの連携を解除する",
        (Target::Toasts, false) => "期限の通知を予約する",
        (Target::Toasts, true) => "期限の通知の予約を解除する",
    })
}

/// 「設定」の通知に出す、連携のスイッチの名前。この OS に連携する先がなければ `None`。
pub fn setting_label(cx: &App) -> Option<&'static str> {
    Some(match cx.global::<Reminders>().target? {
        Target::Reminders => "リマインダーと連携",
        Target::Toasts => "期限の通知を予約",
    })
}

/// 「リマインダー」アプリと同期できる OS か（macOS）。設定の「連携と同期」のアカウントに出す。
pub fn is_app(cx: &App) -> bool {
    cx.global::<Reminders>().target == Some(Target::Reminders)
}

pub fn enabled(cx: &App) -> bool {
    db::shared(cx)
        .read(cx)
        .store
        .reminders_enabled()
        .unwrap_or(false)
}

/// 本物の OS のリマインダーにつなぐ。アプリの起動のときに 1 回呼ぶ。
/// テストが OS のリマインダーに触らないよう、[`init`] だけではつながない。
pub fn connect_system(cx: &mut App) {
    cx.set_global(Reminders {
        target: malakt_reminders::target(),
        run: Arc::new(|snapshot, links| {
            malakt_reminders::run(crate::APP_ID, crate::APP_NAME, &snapshot, &links)
        }),
    });
}

pub fn init(cx: &mut App) {
    cx.set_global(Reminders {
        target: None,
        run: Arc::new(|_, _| Err("この OS にはリマインダーがありません".into())),
    });
    let db = db::shared(cx);
    let sync = cx.new(|cx| Mirror {
        _watch: cx.observe(&db, |this: &mut Mirror, _, cx| this.request(false, cx)),
        db,
        running: false,
        again: false,
        stopped: false,
        last: None,
    });
    cx.set_global(Shared(sync.clone()));
    // 起動したときと、そのあと 1 分ごとに、OS の側で完了にされたものを受け取る
    let weak = sync.downgrade();
    cx.spawn(async move |cx| loop {
        if weak.update(cx, |this, cx| this.request(true, cx)).is_err() {
            break;
        }
        cx.background_executor()
            .timer(Duration::from_secs(60))
            .await;
    })
    .detach();
}

/// 連携を始める・解除する。解除すると、このアプリが OS に作ったものを消す。
pub fn set_enabled(on: bool, cx: &mut App) -> malakt_core::Result<()> {
    shared(cx).update(cx, |sync, _| sync.stopped = false);
    db::shared(cx).update(cx, |db, cx| {
        db.store.set_reminders_enabled(on)?;
        cx.notify();
        Ok(())
    })
}

impl Mirror {
    /// OS の側を合わせる。`poll` は、データが変わっていなくても OS の側を見に行く。
    pub fn request(&mut self, poll: bool, cx: &mut Context<Self>) {
        if self.stopped || cx.global::<Reminders>().target.is_none() {
            return;
        }
        let read = {
            let store = &self.db.read(cx).store;
            store.reminders_enabled().and_then(|on| {
                let links = store.reminder_links()?;
                let snapshot = if on {
                    store.reminder_snapshot()?
                } else {
                    Snapshot::default()
                };
                Ok((on, snapshot, links))
            })
        };
        let (on, snapshot, links) = match read {
            Ok(read) => read,
            Err(error) => {
                cx.emit(Failed(
                    format!("リマインダーに渡すタスクを読めませんでした。{error}").into(),
                ));
                return;
            }
        };
        if !on && links.is_empty() {
            self.last = None;
            return;
        }
        if self.running {
            self.again = true;
            return;
        }
        if !poll && self.last.as_ref() == Some(&snapshot) {
            return;
        }
        self.running = true;
        let run = cx.global::<Reminders>().run.clone();
        let wanted = snapshot.clone();
        let task = cx
            .background_executor()
            .spawn(async move { run(snapshot, links) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            // 終了の途中なら、受け取る相手がもういない
            let _ = this.update(cx, |this, cx| this.finish(wanted, result, cx));
        })
        .detach();
    }

    fn finish(
        &mut self,
        wanted: Snapshot,
        result: Result<Outcome, String>,
        cx: &mut Context<Self>,
    ) {
        self.running = false;
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                // 始められなかった。設定をオフに戻して、理由を出す
                self.stopped = true;
                self.again = false;
                let saved = self.db.update(cx, |db, cx| {
                    cx.notify();
                    db.store.set_reminders_enabled(false)
                });
                let text = match saved {
                    Ok(()) => error,
                    Err(save) => format!("{error}（設定も保存できませんでした。{save}）"),
                };
                cx.emit(Failed(text.into()));
                return;
            }
        };
        let mut errors = outcome.errors.clone();
        let changed = self.db.update(cx, |db, cx| {
            let store = &mut db.store;
            match store.reminder_links() {
                Ok(links) if links == outcome.links => {}
                _ => {
                    if let Err(error) = store.replace_reminder_links(&outcome.links) {
                        errors.push(format!("連携の控えを保存できませんでした。{error}"));
                    }
                }
            }
            let turns = outcome
                .completed
                .iter()
                .map(|id| (id, Status::Completed))
                .chain(outcome.reopened.iter().map(|id| (id, Status::NeedsAction)));
            let mut changed = false;
            for (id, status) in turns {
                match store.acting(malakt_core::Actor::Reminders, |s| {
                    s.set_task_status(id, status)
                }) {
                    Ok(_) => changed = true,
                    // 合わせている間に消されたタスクは、次の回でリマインダーも消える
                    Err(malakt_core::Error::NotFound(_)) => {}
                    Err(error) => errors.push(error.to_string()),
                }
            }
            if changed {
                cx.notify();
            }
            changed
        });
        // 失敗したものがあるときは、次の回にやり直す
        self.last = errors.is_empty().then_some(wanted);
        if !errors.is_empty() {
            cx.emit(Failed(failure_text(&errors).into()));
        }
        // 完了を受け取ったときは、データの変更の知らせでもう一度合わせる
        if std::mem::take(&mut self.again) && !changed {
            self.request(false, cx);
        }
    }
}

/// 失敗を 1 行にまとめる。いくつもあるときは、最初の 1 つと件数だけを出す。
fn failure_text(errors: &[String]) -> String {
    match errors {
        [only] => format!("リマインダーに反映できませんでした。{only}"),
        [first, ..] => format!(
            "リマインダーに反映できませんでした（{} 件）。{first}",
            errors.len()
        ),
        [] => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::failure_text;

    #[test]
    fn many_failures_show_the_first_and_the_count() {
        let one = ["「A」: 保存できませんでした".to_owned()];
        assert_eq!(
            failure_text(&one),
            "リマインダーに反映できませんでした。「A」: 保存できませんでした"
        );
        let many = [one[0].clone(), "「B」: 保存できませんでした".to_owned()];
        assert_eq!(
            failure_text(&many),
            "リマインダーに反映できませんでした（2 件）。「A」: 保存できませんでした"
        );
    }
}
