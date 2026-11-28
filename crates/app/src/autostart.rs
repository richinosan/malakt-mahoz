//! OS にログインしたときに、アプリを起動する（スタートアップ）。
//!
//! 設定の「スタートアップ」、アプリ内の AI（`set_app_options`）、MCP（`set_autostart`）から切り替える。
//! 登録しているかどうかは OS の側だけが持つ。Windows のタスク マネージャーなどで外したときも、
//! そのとおりに出る。登録するのは動いている実行ファイル（macOS は `.app`）の場所で、自動アップデートは
//! 同じ場所で入れ替えるので、登録はそのまま使える。アプリを別の場所へ動かしたときは、
//! 起動したときに登録し直す（[`connect_system`]）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use gpui_kit::*;

/// 登録しているか読む関数。
pub type Read = Arc<dyn Fn() -> Result<bool, String> + Send + Sync>;
/// 登録する・外す関数。
pub type Write = Arc<dyn Fn(bool) -> Result<(), String> + Send + Sync>;

/// OS への入口。テストでは差し替える。
#[derive(Clone)]
pub struct Launcher {
    /// この OS で登録できる。そうでなければ、設定に出さない。
    pub available: bool,
    pub read: Read,
    pub write: Write,
}

impl Global for Launcher {}

const APP: malakt_autostart::App = malakt_autostart::App {
    id: crate::APP_ID,
    name: crate::APP_NAME,
};

const UNAVAILABLE: &str = "この OS ではスタートアップに登録できません";

/// 動いている実行ファイル。macOS と Linux はリンクをたどった先。
fn current_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("動いているアプリの場所がわかりません: {e}"))?;
    // Windows で canonicalize すると `\\?\` の付いた場所になり、Run に書くと起動しない
    if cfg!(windows) {
        return Ok(exe);
    }
    exe.canonicalize().map_err(|e| {
        format!(
            "動いているアプリの場所（{}）を読めません: {e}",
            exe.display()
        )
    })
}

/// `exe` を登録してよいか。macOS でダウンロードしたフォルダーから動かしているときは、
/// 次に起動するときにはない仮の場所なので、登録できない。
fn registrable(exe: &Path) -> Result<(), String> {
    if exe
        .components()
        .any(|c| c.as_os_str() == "AppTranslocation")
    {
        return Err(
            "ダウンロードしたフォルダーから動かしているので、スタートアップに登録できません。「アプリケーション」フォルダーへ移してから、もう一度オンにしてください。"
                .into(),
        );
    }
    Ok(())
}

impl Launcher {
    /// どこにも登録しない（テストと、登録できない OS）。
    pub fn none() -> Self {
        Self {
            available: false,
            read: Arc::new(|| Ok(false)),
            write: Arc::new(|_| Err(UNAVAILABLE.into())),
        }
    }

    /// 本物の OS に登録する。
    pub fn system() -> Self {
        if !malakt_autostart::supported() {
            return Self::none();
        }
        Self {
            available: true,
            read: Arc::new(|| {
                malakt_autostart::registered(APP)
                    .map(|target| target.is_some())
                    .map_err(|e| format!("スタートアップの登録を読めませんでした: {e}"))
            }),
            write: Arc::new(|on| {
                if !on {
                    return malakt_autostart::disable(APP)
                        .map_err(|e| format!("スタートアップから外せませんでした: {e}"));
                }
                let exe = current_exe()?;
                registrable(&exe)?;
                malakt_autostart::enable(APP, &exe)
                    .map_err(|e| format!("スタートアップに登録できませんでした: {e}"))
            }),
        }
    }
}

/// コマンドと MCP サーバーが使う入口。[`use_system`] を呼ぶまでは、どこにも登録しない。
static PROCESS: OnceLock<Launcher> = OnceLock::new();

/// このプロセスのコマンドと MCP サーバーを、本物の OS につなぐ。起動してすぐ 1 回呼ぶ。
pub fn use_system() {
    // 2 回目に呼ばれても、もう同じものにつないである
    let _ = PROCESS.set(Launcher::system());
}

/// コマンドと MCP サーバーが使う入口。
pub fn process() -> Launcher {
    PROCESS.get().cloned().unwrap_or_else(Launcher::none)
}

pub fn init(cx: &mut App) {
    cx.set_global(Launcher::none());
}

/// 画面を本物の OS につなぐ。アプリの起動のときに 1 回呼ぶ。
/// 登録してある場所にアプリがもうなければ（アプリを動かしたとき）、今のアプリを登録し直す。
/// 直せなかったら、その理由を返す（画面に出す）。
pub fn connect_system(cx: &mut App) -> Result<(), String> {
    cx.set_global(Launcher::system());
    // 開発用のビルドは、配ったアプリの登録を自分に向け直さない
    if cfg!(debug_assertions) || !malakt_autostart::supported() {
        return Ok(());
    }
    let Some(target) = malakt_autostart::registered(APP)
        .map_err(|e| format!("スタートアップの登録を読めませんでした: {e}"))?
    else {
        return Ok(());
    };
    if target.exists() {
        return Ok(());
    }
    let exe = current_exe()?;
    registrable(&exe)?;
    malakt_autostart::enable(APP, &exe).map_err(|e| {
        format!(
            "スタートアップの登録を、動かしたアプリの場所（{}）に直せませんでした: {e}",
            exe.display()
        )
    })
}

pub fn available(cx: &App) -> bool {
    cx.global::<Launcher>().available
}

/// 登録してあるか。読めなければ、登録していないものとして出す（切り替えるときに理由が出る）。
pub fn enabled(cx: &App) -> bool {
    let launcher = cx.global::<Launcher>();
    launcher.available && (launcher.read)().unwrap_or(false)
}

/// 登録する・外す。
pub fn set_enabled(on: bool, cx: &App) -> Result<(), String> {
    let launcher = cx.global::<Launcher>();
    if !launcher.available {
        return Err(UNAVAILABLE.into());
    }
    (launcher.write)(on)
}

/// 今と逆にする。
pub fn toggle(cx: &App) -> Result<(), String> {
    let launcher = cx.global::<Launcher>();
    if !launcher.available {
        return Err(UNAVAILABLE.into());
    }
    let on = (launcher.read)()?;
    (launcher.write)(!on)
}

#[cfg(test)]
mod tests {
    // gpui_kit の `test` マクロを持ち込まないよう、使うものだけ読む
    use super::{process, registrable};
    use std::path::Path;

    #[test]
    fn a_translocated_app_is_not_registered() {
        let translocated = Path::new(
            "/private/var/folders/x/T/AppTranslocation/1234/d/Malakt Mahoz.app/Contents/MacOS/malakt-mahoz",
        );
        assert!(registrable(translocated).is_err());
        assert!(registrable(Path::new(
            "/Applications/Malakt Mahoz.app/Contents/MacOS/malakt-mahoz"
        ))
        .is_ok());
    }

    #[test]
    fn nothing_is_registered_until_the_process_is_connected() {
        let launcher = process();
        assert!(!launcher.available);
        assert_eq!((launcher.read)(), Ok(false));
        assert!((launcher.write)(true).is_err());
    }
}
