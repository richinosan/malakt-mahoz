//! Windows のスタートアップ（`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`）。
//!
//! タスク マネージャーや「設定 > アプリ > スタートアップ」で無効にすると、Windows は
//! `StartupApproved\Run` の同じ名前の値に、先頭のバイトが奇数の値を書く。これがあれば、
//! 登録されていても起動しないので、登録されていないものとして扱う。登録するときは消して、有効に戻す。

use std::io;
use std::path::{Path, PathBuf};

use windows_registry::CURRENT_USER;

use crate::App;

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
/// `HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)`。キーや値がない。
const NOT_FOUND: i32 = 0x8007_0002_u32 as i32;

fn missing(error: &windows_result::Error) -> bool {
    error.code().0 == NOT_FOUND
}

fn os(error: windows_result::Error) -> io::Error {
    io::Error::other(error.message())
}

/// 値を消す。キーも値もなければ何もしない。
fn remove(key: &str, name: &str) -> io::Result<()> {
    let key = match CURRENT_USER.options().read().write().open(key) {
        Ok(key) => key,
        Err(error) if missing(&error) => return Ok(()),
        Err(error) => return Err(os(error)),
    };
    match key.remove_value(name) {
        Err(error) if !missing(&error) => Err(os(error)),
        _ => Ok(()),
    }
}

/// `"C:\…\malakt-mahoz.exe" --引数` の最初の引数。
fn program(command: &str) -> Option<PathBuf> {
    let command = command.trim();
    let program = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => command.split(' ').next()?,
    };
    (!program.is_empty()).then(|| PathBuf::from(program))
}

pub fn registered(app: App) -> io::Result<Option<PathBuf>> {
    let run = match CURRENT_USER.open(RUN) {
        Ok(key) => key,
        Err(error) if missing(&error) => return Ok(None),
        Err(error) => return Err(os(error)),
    };
    let command = match run.get_string(app.name) {
        Ok(command) => command,
        Err(error) if missing(&error) => return Ok(None),
        Err(error) => return Err(os(error)),
    };
    let disabled = match CURRENT_USER.open(APPROVED) {
        Ok(key) => match key.get_value(app.name) {
            Ok(value) => value.first().is_some_and(|b| b & 1 == 1),
            Err(error) if missing(&error) => false,
            Err(error) => return Err(os(error)),
        },
        Err(error) if missing(&error) => false,
        Err(error) => return Err(os(error)),
    };
    Ok(if disabled { None } else { program(&command) })
}

pub fn enable(app: App, target: &Path) -> io::Result<()> {
    let command = format!("\"{}\"", target.display());
    CURRENT_USER
        .create(RUN)
        .and_then(|key| key.set_string(app.name, &command))
        .map_err(os)?;
    // 前に Windows の側で無効にしてあっても、ここで選んだので有効に戻す
    remove(APPROVED, app.name)
}

pub fn disable(app: App) -> io::Result<()> {
    remove(RUN, app.name)?;
    remove(APPROVED, app.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_is_read_from_the_command() {
        assert_eq!(
            program(r#""C:\Program Files\Malakt Mahoz\malakt-mahoz.exe" --x"#),
            Some(PathBuf::from(
                r"C:\Program Files\Malakt Mahoz\malakt-mahoz.exe"
            ))
        );
        assert_eq!(
            program(r"C:\m\malakt-mahoz.exe"),
            Some(PathBuf::from(r"C:\m\malakt-mahoz.exe"))
        );
        assert_eq!(program(""), None);
    }
}
