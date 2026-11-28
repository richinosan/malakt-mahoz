//! OS にログインしたときに、アプリを起動する（スタートアップ）。
//!
//! - Windows: レジストリの `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`。タスク マネージャーの
//!   「スタートアップ アプリ」で無効にされたもの（`StartupApproved\Run`）は、登録されていないものとして扱う
//! - macOS: `~/Library/LaunchAgents` の LaunchAgent。`.app` の中で動いているときは `open -a` で `.app` を開く
//! - Linux: `~/.config/autostart` の `.desktop`
//!
//! 登録するのは、そのとき動いている実行ファイル（macOS は `.app`）の場所。自動アップデートは
//! 同じ場所で入れ替えるので、登録はそのまま使える。アプリを動かしたときは、起動したアプリが
//! [`registered`] の場所がなくなっていることを見て、登録し直す。

use std::io;
use std::path::{Path, PathBuf};

mod desktop;
mod plist;
#[cfg(windows)]
mod windows;

/// 登録するアプリ。
#[derive(Debug, Clone, Copy)]
pub struct App<'a> {
    /// 逆ドメインの ID（`io.github.….malakt-mahoz`）。macOS と Linux のファイル名にする
    pub id: &'a str,
    /// 表示名。Windows の Run の名前、Linux の `Name=` にする
    pub name: &'a str,
}

/// この OS でスタートアップに登録できるか。
pub fn supported() -> bool {
    cfg!(any(windows, target_os = "macos", target_os = "linux"))
}

/// `.app` の中の実行ファイルの場所。
const BUNDLE_BINARY: &str = "Contents/MacOS";

/// 起動するときに登録するもの。macOS の `.app` の中の実行ファイルなら、その `.app`。
pub fn target(exe: &Path) -> PathBuf {
    let bundle = exe
        .parent()
        .filter(|dir| dir.ends_with(BUNDLE_BINARY))
        .and_then(|dir| dir.parent()?.parent())
        .filter(|bundle| bundle.extension().is_some_and(|e| e == "app"));
    match bundle {
        Some(bundle) if cfg!(target_os = "macos") => bundle.to_owned(),
        _ => exe.to_owned(),
    }
}

/// 登録されていて、OS で止められていなければ、ログインしたときに起動するもの
/// （実行ファイル、macOS は `.app`）。
pub fn registered(app: App) -> io::Result<Option<PathBuf>> {
    os::registered(app)
}

/// ログインしたときに `exe` を起動するように登録する。もう登録してあれば、置き換える。
pub fn enable(app: App, exe: &Path) -> io::Result<()> {
    os::enable(app, &target(exe))
}

/// 登録を外す。登録していなければ何もしない。
pub fn disable(app: App) -> io::Result<()> {
    os::disable(app)
}

/// ファイルを、書きかけのものが残らないように置く。
#[cfg_attr(windows, allow(dead_code))]
fn write_file(path: &Path, text: &str) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other(format!("{} の置き場がありません", path.display())))?;
    std::fs::create_dir_all(dir)?;
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path).inspect_err(|_| {
        // 置けなかった書きかけは要らない。消せなくても、次に書くときに上書きする
        let _ = std::fs::remove_file(&temporary);
    })
}

#[cfg_attr(windows, allow(dead_code))]
fn remove_file(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

#[cfg_attr(windows, allow(dead_code))]
fn read_file(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg_attr(windows, allow(dead_code))]
fn home() -> io::Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("ホームフォルダーがわかりません（HOME がありません）"))
}

#[cfg(target_os = "macos")]
mod os {
    use super::*;

    fn file(app: App) -> io::Result<PathBuf> {
        Ok(home()?
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", app.id)))
    }

    pub fn registered(app: App) -> io::Result<Option<PathBuf>> {
        Ok(read_file(&file(app)?)?.and_then(|text| plist::target(&text)))
    }

    pub fn enable(app: App, target: &Path) -> io::Result<()> {
        write_file(&file(app)?, &plist::agent(app.id, target))
    }

    pub fn disable(app: App) -> io::Result<()> {
        remove_file(&file(app)?)
    }
}

#[cfg(target_os = "linux")]
mod os {
    use super::*;

    fn file(app: App) -> io::Result<PathBuf> {
        let config = match std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
            Some(dir) => PathBuf::from(dir),
            None => home()?.join(".config"),
        };
        Ok(config.join("autostart").join(format!("{}.desktop", app.id)))
    }

    pub fn registered(app: App) -> io::Result<Option<PathBuf>> {
        Ok(read_file(&file(app)?)?.and_then(|text| desktop::target(&text)))
    }

    pub fn enable(app: App, target: &Path) -> io::Result<()> {
        write_file(&file(app)?, &desktop::entry(app.name, target))
    }

    pub fn disable(app: App) -> io::Result<()> {
        remove_file(&file(app)?)
    }
}

#[cfg(windows)]
use windows as os;

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod os {
    use super::*;

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "この OS ではスタートアップに登録できません",
        )
    }

    pub fn registered(_: App) -> io::Result<Option<PathBuf>> {
        Ok(None)
    }

    pub fn enable(_: App, _: &Path) -> io::Result<()> {
        Err(unsupported())
    }

    pub fn disable(_: App) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_is_registered_on_macos() {
        let exe = Path::new("/Applications/Malakt Mahoz.app/Contents/MacOS/malakt-mahoz");
        let expected = if cfg!(target_os = "macos") {
            Path::new("/Applications/Malakt Mahoz.app")
        } else {
            exe
        };
        assert_eq!(target(exe), expected);
        // `.app` の外に実行ファイルだけを置いたときは、実行ファイル
        let bare = Path::new("/opt/malakt/malakt-mahoz");
        assert_eq!(target(bare), bare);
        let odd = Path::new("/x/Contents/MacOS/malakt-mahoz");
        assert_eq!(target(odd), odd);
    }

    #[test]
    fn files_are_written_whole_and_removed_once() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("a/b/entry");
        assert_eq!(read_file(&path).unwrap(), None);
        write_file(&path, "one").unwrap();
        write_file(&path, "two").unwrap();
        assert_eq!(read_file(&path).unwrap().as_deref(), Some("two"));
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        remove_file(&path).unwrap();
        remove_file(&path).unwrap();
        assert_eq!(read_file(&path).unwrap(), None);
    }
}
