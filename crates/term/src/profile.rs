//! 開けるシェルの一覧。Windows は WSL のディストリビューション・PowerShell・コマンドプロンプト、
//! macOS と Linux はログインシェルと `/etc/shells` にあるシェル。

use std::path::{Path, PathBuf};

/// シェルの種類。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// ふつうのシェル。
    Shell,
    /// WSL のディストリビューション。
    Wsl,
}

/// 開けるシェル。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// 設定に保存する名前。
    pub id: String,
    /// 画面に出す名前。
    pub name: String,
    pub kind: Kind,
    pub program: String,
    pub args: Vec<String>,
}

impl Profile {
    fn shell(id: &str, name: &str, program: &str, args: &[&str]) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            kind: Kind::Shell,
            program: program.into(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    fn wsl(distro: &str) -> Self {
        Self {
            id: format!("wsl:{distro}"),
            name: distro.into(),
            kind: Kind::Wsl,
            program: "wsl.exe".into(),
            args: vec!["-d".into(), distro.into()],
        }
    }

    /// 起こすときの引数。WSL は、フォルダーの指定がなければ Linux のホームで始める。
    pub fn command_args(&self, has_cwd: bool) -> Vec<String> {
        let mut args = self.args.clone();
        if self.kind == Kind::Wsl && !has_cwd {
            args.extend(["--cd".to_owned(), "~".to_owned()]);
        }
        args
    }
}

/// `wsl.exe -l -q` の出力から、ディストリビューションの名前を取り出す。最初のものが標準。
/// 出力は UTF-16LE（`WSL_UTF8=1` なら UTF-8）。Docker Desktop の内部用は除く。
pub fn parse_wsl_list(bytes: &[u8]) -> Vec<String> {
    let text = if bytes.contains(&0) {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    text.lines()
        .map(|line| line.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}'))
        // 名前に空白は入らない。空白のある行は、WSL が入っていないときの案内
        .filter(|line| !line.is_empty() && !line.contains(char::is_whitespace))
        .filter(|line| !line.starts_with("docker-desktop"))
        .map(str::to_owned)
        .collect()
}

/// Windows のシェルの一覧。`has` は、そのプログラムが PATH にあるか。
pub fn windows_profiles(distros: &[String], has: impl Fn(&str) -> bool) -> Vec<Profile> {
    let mut out: Vec<Profile> = distros.iter().map(|d| Profile::wsl(d)).collect();
    if has("pwsh.exe") {
        out.push(Profile::shell(
            "pwsh",
            "PowerShell",
            "pwsh.exe",
            &["-NoLogo"],
        ));
    }
    if has("powershell.exe") {
        out.push(Profile::shell(
            "powershell",
            "Windows PowerShell",
            "powershell.exe",
            &["-NoLogo"],
        ));
    }
    out.push(Profile::shell("cmd", "コマンドプロンプト", "cmd.exe", &[]));
    out
}

/// macOS と Linux のシェルの一覧。`login` はログインシェル（`$SHELL`）、`etc_shells` は
/// `/etc/shells` の中身。macOS ではログインシェルとして起こす。
pub fn unix_profiles(
    login: &str,
    etc_shells: &str,
    macos: bool,
    exists: impl Fn(&Path) -> bool,
) -> Vec<Profile> {
    let args: &[&str] = if macos { &["-l"] } else { &[] };
    let name_of = |path: &str| {
        Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_owned())
    };
    let mut out = vec![Profile::shell(
        &name_of(login),
        &name_of(login),
        login,
        args,
    )];
    for line in etc_shells.lines() {
        let path = line.trim();
        if path.is_empty() || path.starts_with('#') || !exists(Path::new(path)) {
            continue;
        }
        let name = name_of(path);
        // 機能を絞ったシェル（rbash）は並べない
        if name == "rbash" || out.iter().any(|p| p.id == name) {
            continue;
        }
        out.push(Profile::shell(&name, &name, path, args));
    }
    out
}

/// PATH から実行ファイルを探す。アプリを起動したあとに入れたものも見つける。
#[cfg(windows)]
fn on_path(name: &str) -> bool {
    crate::env_path::has(name)
}

#[cfg(windows)]
fn wsl_distros() -> Vec<String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if !on_path("wsl.exe") {
        return Vec::new();
    }
    let output = Command::new("wsl.exe")
        .args(["-l", "-q"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match output {
        Ok(output) if output.status.success() => parse_wsl_list(&output.stdout),
        // WSL が入っていない、ディストリビューションがない
        _ => Vec::new(),
    }
}

/// この PC で開けるシェル。最初のものが標準（Windows は WSL があれば WSL、macOS と Linux は
/// ログインシェル）。Windows では `wsl.exe` を呼ぶので、画面のスレッドでは呼ばない。
pub fn detect() -> Vec<Profile> {
    #[cfg(windows)]
    {
        windows_profiles(&wsl_distros(), on_path)
    }
    #[cfg(not(windows))]
    {
        let login = std::env::var("SHELL")
            .ok()
            .filter(|shell| Path::new(shell).is_file())
            .unwrap_or_else(|| "/bin/sh".into());
        let etc = std::fs::read_to_string("/etc/shells").unwrap_or_default();
        unix_profiles(&login, &etc, cfg!(target_os = "macos"), |path| {
            path.is_file()
        })
    }
}

/// 設定に保存した名前のシェル。なくなっていれば標準（最初）のもの。
pub fn pick<'a>(profiles: &'a [Profile], id: Option<&str>) -> Option<&'a Profile> {
    id.and_then(|id| profiles.iter().find(|p| p.id == id))
        .or_else(|| profiles.first())
}

/// シェルを始めるフォルダー。なくなっていれば、ホームから始める。
pub fn start_dir(wanted: Option<&Path>) -> Option<PathBuf> {
    wanted.filter(|dir| dir.is_dir()).map(Path::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_list_is_read_from_utf16_and_utf8() {
        let utf16: Vec<u8> =
            "\u{feff}Ubuntu\r\nDebian\r\ndocker-desktop\r\ndocker-desktop-data\r\n"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect();
        assert_eq!(parse_wsl_list(&utf16), ["Ubuntu", "Debian"]);
        assert_eq!(parse_wsl_list(b"Ubuntu-24.04\n\n"), ["Ubuntu-24.04"]);
        // WSL にディストリビューションがないときの案内は、名前として読まない
        let help: Vec<u8> = "Windows Subsystem for Linux has no installed distributions.\r\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert!(parse_wsl_list(&help).is_empty());
    }

    #[test]
    fn windows_starts_with_wsl_when_it_exists() {
        let all = windows_profiles(&["Ubuntu".into()], |_| true);
        let ids: Vec<&str> = all.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["wsl:Ubuntu", "pwsh", "powershell", "cmd"]);
        assert_eq!(all[0].command_args(false), ["-d", "Ubuntu", "--cd", "~"]);
        assert_eq!(all[0].command_args(true), ["-d", "Ubuntu"]);
        assert_eq!(all[1].command_args(false), ["-NoLogo"]);

        let none = windows_profiles(&[], |name| name == "powershell.exe");
        let ids: Vec<&str> = none.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["powershell", "cmd"]);
    }

    #[test]
    fn unix_starts_with_the_login_shell() {
        let etc =
            "# shells\n/bin/sh\n/bin/bash\n/bin/rbash\n/bin/zsh\n/usr/bin/zsh\n/opt/gone/fish\n";
        let all = unix_profiles("/bin/zsh", etc, true, |p| !p.starts_with("/opt/gone"));
        let ids: Vec<&str> = all.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["zsh", "sh", "bash"]);
        assert_eq!(all[0].program, "/bin/zsh");
        assert_eq!(all[0].args, ["-l"]);
        let linux = unix_profiles("/bin/bash", "", false, |_| true);
        assert!(linux[0].args.is_empty());
    }

    #[test]
    fn a_saved_shell_falls_back_to_the_first() {
        let all = windows_profiles(&["Ubuntu".into()], |_| true);
        assert_eq!(pick(&all, Some("cmd")).unwrap().id, "cmd");
        assert_eq!(pick(&all, Some("wsl:Gone")).unwrap().id, "wsl:Ubuntu");
        assert_eq!(pick(&all, None).unwrap().id, "wsl:Ubuntu");
        assert_eq!(pick(&[], None), None);
    }

    #[test]
    fn a_missing_start_folder_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(start_dir(Some(dir.path())), Some(dir.path().to_owned()));
        assert_eq!(start_dir(Some(&dir.path().join("gone"))), None);
        assert_eq!(start_dir(None), None);
    }
}
