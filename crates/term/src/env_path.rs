//! Windows の PATH を、レジストリから読み直す。
//!
//! Windows のプロセスは、起動したときの PATH をずっと持ち続ける。アプリを起動したあとに
//! 入れたコマンド（`winget` や インストーラーが PATH に足したもの）は、アプリを再起動するまで
//! 見つからない。外のターミナルを開き直せば見つかるのに、アプリの中では見つからない。
//! それを避けるため、コマンドを起こすたびに、ログインしたときと同じ順（システム、ユーザー）で
//! PATH を組み直して渡す。

use std::ffi::OsString;

/// 今の PATH。Windows はレジストリのシステムとユーザーの PATH を読み直し、アプリが起動したときの
/// PATH にしかないもの（起動したターミナルが足したものなど）を後ろに足す。
/// Windows 以外は、アプリの PATH のまま（`None`）。
pub fn fresh() -> Option<OsString> {
    #[cfg(windows)]
    {
        let machine = registry::read(
            registry::HKEY_LOCAL_MACHINE,
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
        );
        let user = registry::read(registry::HKEY_CURRENT_USER, "Environment");
        if machine.is_none() && user.is_none() {
            return None;
        }
        let current = std::env::var("PATH").unwrap_or_default();
        Some(
            merge(
                &machine.unwrap_or_default(),
                &user.unwrap_or_default(),
                &current,
            )
            .into(),
        )
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// PATH の 1 つのフォルダーを、同じものか比べられる形にする（大文字小文字と、最後の `\` を無視する）。
fn key(dir: &str) -> String {
    dir.trim().trim_end_matches(['\\', '/']).to_lowercase()
}

/// システムの PATH、ユーザーの PATH、アプリの PATH の順に、重なりを除いて `;` でつなぐ。
pub fn merge(machine: &str, user: &str, current: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<&str> = Vec::new();
    for dir in [machine, user, current]
        .iter()
        .flat_map(|path| path.split(';'))
        .map(str::trim)
        .filter(|dir| !dir.is_empty())
    {
        if seen.insert(key(dir)) {
            out.push(dir);
        }
    }
    out.join(";")
}

/// PATH から、実行するファイルを探す。`exts` は、名前に足して試す拡張子（Windows の PATHEXT）。
pub fn find_in(name: &str, path: &std::ffi::OsStr, exts: &[&str]) -> Option<std::path::PathBuf> {
    std::env::split_paths(path).find_map(|dir| {
        std::iter::once("")
            .chain(exts.iter().copied())
            .map(|ext| dir.join(format!("{name}{ext}")))
            .find(|file| file.is_file())
    })
}

/// 今の PATH（[`fresh`]）に、そのプログラムがあるか。
pub fn has(name: &str) -> bool {
    let path = fresh().or_else(|| std::env::var_os("PATH"));
    path.is_some_and(|path| find_in(name, &path, &[]).is_some())
}

#[cfg(windows)]
mod registry {
    use std::ffi::c_void;

    // 決まった鍵の値は、32 ビットの値を符号つきで広げたもの
    pub const HKEY_LOCAL_MACHINE: isize = 0x8000_0002_u32 as i32 as isize;
    pub const HKEY_CURRENT_USER: isize = 0x8000_0001_u32 as i32 as isize;
    /// REG_SZ と REG_EXPAND_SZ を読む。REG_EXPAND_SZ の `%USERPROFILE%` などは、展開されて届く。
    const RRF_RT_REG_SZ: u32 = 0x0000_0002;
    const RRF_RT_REG_EXPAND_SZ: u32 = 0x0000_0004;
    const ERROR_SUCCESS: i32 = 0;
    const ERROR_MORE_DATA: i32 = 234;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            key: *mut c_void,
            sub_key: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut c_void,
            len: *mut u32,
        ) -> i32;
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// `key\sub_key` の `Path` の値。なければ `None`。
    pub fn read(key: isize, sub_key: &str) -> Option<String> {
        let sub_key = wide(sub_key);
        let value = wide("Path");
        // 読む間に書き換わって大きくなったら、もう一度測る
        for _ in 0..3 {
            let mut len = 0u32;
            // SAFETY: 文字列は 0 で終わり、data が null のときは大きさだけを返す
            let status = unsafe {
                RegGetValueW(
                    key as *mut c_void,
                    sub_key.as_ptr(),
                    value.as_ptr(),
                    RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut len,
                )
            };
            if status != ERROR_SUCCESS {
                return None;
            }
            let mut buf = vec![0u16; (len as usize).div_ceil(2) + 1];
            let mut len = (buf.len() * 2) as u32;
            // SAFETY: buf は len バイトの大きさがある
            let status = unsafe {
                RegGetValueW(
                    key as *mut c_void,
                    sub_key.as_ptr(),
                    value.as_ptr(),
                    RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
                    std::ptr::null_mut(),
                    buf.as_mut_ptr().cast(),
                    &mut len,
                )
            };
            match status {
                ERROR_SUCCESS => {
                    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                    return Some(String::from_utf16_lossy(&buf[..end]));
                }
                ERROR_MORE_DATA => continue,
                _ => return None,
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_path_comes_first_and_duplicates_are_dropped() {
        let machine = r"C:\Windows\system32;C:\Windows;C:\Program Files\PowerShell\7\";
        let user = r"C:\Users\a\AppData\Local\Microsoft\WindowsApps;C:\Users\a\.slack\bin;";
        // アプリを起動したときの PATH。あとから入れた slack のフォルダーはない
        let current = r"C:\WINDOWS\system32;C:\Program Files\PowerShell\7;C:\tools\from-terminal";
        assert_eq!(
            merge(machine, user, current),
            [
                r"C:\Windows\system32",
                r"C:\Windows",
                r"C:\Program Files\PowerShell\7\",
                r"C:\Users\a\AppData\Local\Microsoft\WindowsApps",
                r"C:\Users\a\.slack\bin",
                r"C:\tools\from-terminal",
            ]
            .join(";")
        );
        assert_eq!(merge("", "", ""), "");
        assert_eq!(merge(";;", " ", "a"), "a");
    }

    #[cfg(not(windows))]
    #[test]
    fn other_systems_keep_the_path_of_the_app() {
        assert_eq!(fresh(), None);
        assert!(has("sh"));
        assert!(!has("malakt-mahoz-no-such-program"));
    }
}
