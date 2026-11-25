//! 自動アップデート。
//!
//! 起動したときと、そのあと 6 時間ごとに、GitHub Releases の最新のリリースを見に行く。
//! 新しい版があれば裏で取ってきて確かめ、最後のウィンドウを閉じたとき（終了のとき）に
//! 入れ替える。設定のメニューで無効にできる。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::*;
pub use malakt_update::{Error, Source, Staged};

use crate::db;

/// 新しい版を探し、あれば取ってきて置く関数。引数は、入れ替えに失敗して見送っている版。
/// 答えが出るまで戻らないので、裏のスレッドで呼ぶ。
pub type Check = Arc<dyn Fn(Option<String>) -> Result<Option<Staged>, Error> + Send + Sync>;
/// 置いてある版を、動いている実行ファイルと入れ替える関数。
pub type Install = Arc<dyn Fn(&Staged) -> Result<(), String> + Send + Sync>;

/// 外への入口。テストでは差し替える。
pub struct Updater {
    /// ここでは自動で入れ替えられる（配ったものを、書き換えられる場所で動かしている）。
    /// そうでなければ、設定のメニューにも出さず、どこにも見に行かない。
    pub available: bool,
    pub check: Check,
    pub install: Install,
}

impl Global for Updater {}

/// 進み具合。すべてのウィンドウで共有する。
pub struct Progress {
    /// 取ってきて置いてある版。閉じるときに入れ替える。
    ready: Option<Staged>,
    running: bool,
}

/// 新しい版を取ってくる途中で失敗したこと。画面に出す。
pub struct Failed(pub SharedString);

impl EventEmitter<Failed> for Progress {}

struct Shared(Entity<Progress>);

impl Global for Shared {}

pub fn shared(cx: &App) -> Entity<Progress> {
    cx.global::<Shared>().0.clone()
}

/// 「off」なら見に行かない。なければ有効。
const SETTING: &str = "update.auto";
/// 閉じるときの入れ替えで失敗したこと。次に起動したときに画面に出す。
const FAILURE: &str = "update.failure";
/// 入れ替えに失敗した版。同じ版をもう一度取ってこない。
const SKIPPED: &str = "update.skipped";
const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

pub fn init(cx: &mut App) {
    cx.set_global(Updater {
        available: false,
        check: Arc::new(|_| Ok(None)),
        install: Arc::new(|_| Err("ここでは更新できません".into())),
    });
    let progress = cx.new(|_| Progress {
        ready: None,
        running: false,
    });
    cx.set_global(Shared(progress));
    cx.spawn(async move |cx| loop {
        cx.background_executor().timer(INTERVAL).await;
        cx.update(check_now);
    })
    .detach();
}

/// `source` のリリースを見に行き、`exe` を入れ替えるようにつなぐ。取ってきたものは `dir` に置く。
pub fn connect(source: Source, platform: &'static str, exe: PathBuf, dir: PathBuf, cx: &mut App) {
    let current = env!("CARGO_PKG_VERSION");
    cx.set_global(Updater {
        available: true,
        check: Arc::new(move |skipped| {
            malakt_update::check(&source, current, platform, skipped.as_deref(), &dir)
        }),
        install: Arc::new(move |staged| malakt_update::install(staged, &exe)),
    });
    check_now(cx);
}

/// GitHub Releases につなぐ。アプリの起動のときに 1 回呼ぶ。
///
/// 開発用のビルド、ソースからビルドしたもの、配っていない OS では、つながない。
pub fn connect_system(cx: &mut App) {
    if cfg!(debug_assertions) {
        return;
    }
    let Some(platform) = malakt_update::platform() else {
        return;
    };
    let Ok(exe) = std::env::current_exe().and_then(|exe| exe.canonicalize()) else {
        return;
    };
    if !malakt_update::replaceable(&exe) {
        return;
    }
    malakt_update::clear_leftovers(&exe);
    let dir = crate::database_path().with_file_name("update");
    connect(Source::github(), platform, exe, dir, cx);
}

fn setting(key: &str, cx: &App) -> Option<String> {
    // 読めなければ、ないものとして扱う（有効のまま、見送りなし）
    db::shared(cx).read(cx).store.setting(key).ok().flatten()
}

fn set_setting(key: &str, value: Option<&str>, cx: &mut App) -> malakt_core::Result<()> {
    db::shared(cx).update(cx, |db, _| db.store.set_setting(key, value))
}

pub fn enabled(cx: &App) -> bool {
    setting(SETTING, cx).as_deref() != Some("off")
}

/// 設定のメニューに出す文。ここで自動の更新ができなければ `None`。
pub fn menu_label(cx: &App) -> Option<&'static str> {
    if !cx.global::<Updater>().available {
        return None;
    }
    Some(if enabled(cx) {
        "自動アップデートを無効にする"
    } else {
        "自動アップデートを有効にする"
    })
}

/// 取ってきて置いてある版。閉じると、この版になる。
pub fn ready_version(cx: &App) -> Option<SharedString> {
    let ready = shared(cx).read(cx).ready.as_ref()?;
    Some(ready.version.clone().into())
}

/// 自動アップデートを有効にする・無効にする。無効にすると、置いてある版を消す。
pub fn set_enabled(on: bool, cx: &mut App) -> Result<(), String> {
    set_setting(SETTING, (!on).then_some("off"), cx)
        .map_err(|e| format!("設定を保存できませんでした。{e}"))?;
    if on {
        check_now(cx);
        return Ok(());
    }
    let staged = shared(cx).update(cx, |progress, cx| {
        cx.notify();
        progress.ready.take()
    });
    match staged {
        Some(staged) => malakt_update::discard(&staged).map_err(|e| {
            format!(
                "取ってきた v{} を消せませんでした（{}）: {e}",
                staged.version,
                staged.dir.display()
            )
        }),
        None => Ok(()),
    }
}

/// 新しい版を探す。もう置いてある、探している途中、無効にしてある、なら何もしない。
pub fn check_now(cx: &mut App) {
    let updater = cx.global::<Updater>();
    if !updater.available || !enabled(cx) {
        return;
    }
    let check = updater.check.clone();
    let skipped = setting(SKIPPED, cx);
    let progress = shared(cx);
    let start = progress.update(cx, |progress, _| {
        let start = !progress.running && progress.ready.is_none();
        progress.running |= start;
        start
    });
    if !start {
        return;
    }
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move { check(skipped) })
            .await;
        cx.update(|cx| {
            // 待っている間に無効にされたら、置かない
            let keep = enabled(cx);
            progress.update(cx, |progress, cx| {
                progress.running = false;
                match result {
                    Ok(Some(staged)) if keep => progress.ready = Some(staged),
                    Ok(Some(staged)) => {
                        if let Err(error) = malakt_update::discard(&staged) {
                            cx.emit(Failed(
                                format!(
                                    "取ってきた v{} を消せませんでした（{}）: {error}",
                                    staged.version,
                                    staged.dir.display()
                                )
                                .into(),
                            ));
                        }
                    }
                    Ok(None) | Err(Error::Offline(_)) => {}
                    Err(Error::Failed(text)) => cx.emit(Failed(text.into())),
                }
                cx.notify();
            });
        });
    })
    .detach();
}

/// 置いてある版があれば、入れ替える。なければ `None`。
fn install(cx: &mut App) -> Option<Result<(), String>> {
    if !enabled(cx) {
        return None;
    }
    let staged = shared(cx).update(cx, |progress, cx| {
        cx.notify();
        progress.ready.take()
    })?;
    let install = cx.global::<Updater>().install.clone();
    // MCP サーバーが実行ファイルをつかんでいると入れ替えられないので、先に終わらせる
    let released = crate::mcp::release_for_install();
    let result = install(&staged);
    if result.is_err() {
        if released {
            if let Err(error) = crate::mcp::resume() {
                eprintln!("アプリが動いている印を付け直せませんでした: {error}");
            }
        }
        // 書き込めない場所に置いてあるときなどは、何度やっても同じなので、この版は見送る。
        // 残したものは次の起動で消えるので、ここで消せなくても続ける
        let _ = malakt_update::discard(&staged);
        if let Err(error) = set_setting(SKIPPED, Some(&staged.version), cx) {
            eprintln!("見送る版を保存できませんでした: {error}");
        }
    }
    Some(result)
}

/// 終了のときに呼ぶ。置いてある版があれば入れ替える。失敗は、次に起動したときに画面に出す。
pub fn install_on_quit(cx: &mut App) {
    if let Some(Err(text)) = install(cx) {
        if let Err(error) = set_setting(FAILURE, Some(&text), cx) {
            eprintln!("{text}（保存できませんでした: {error}）");
        }
    }
}

/// 今すぐ入れ替えて、起動し直す。
pub fn install_and_restart(cx: &mut App) -> Result<(), String> {
    match install(cx) {
        Some(Err(text)) => Err(text),
        Some(Ok(())) => {
            crate::session::quitting(cx);
            cx.restart();
            Ok(())
        }
        None => Ok(()),
    }
}

/// 前に閉じたときの入れ替えで失敗していれば、その文を 1 回だけ返す。
pub fn take_failure(cx: &mut App) -> Option<SharedString> {
    let text = setting(FAILURE, cx)?;
    if let Err(error) = set_setting(FAILURE, None, cx) {
        eprintln!("更新の失敗の控えを消せませんでした: {error}");
    }
    Some(text.into())
}
