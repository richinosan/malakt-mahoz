//! ターミナルにドロップしたファイルを、入力の行に入れる文字にする。

use std::path::PathBuf;

use crate::profile::{Kind, Profile};

/// シェルに渡すときの、ファイルの場所の書き方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathStyle {
    /// sh・bash・zsh など。空白や記号があれば `'...'` で囲む。
    Posix,
    /// PowerShell とコマンドプロンプト。空白や記号があれば `"..."` で囲む。
    Windows,
    /// WSL。Windows の場所を `/mnt/c/...` に読み替えてから、[`PathStyle::Posix`] と同じに囲む。
    Wsl,
}

impl Profile {
    /// このシェルでの、ファイルの場所の書き方。`windows` は、アプリが Windows で動いているか。
    pub fn path_style(&self, windows: bool) -> PathStyle {
        match self.kind {
            Kind::Wsl => PathStyle::Wsl,
            Kind::Shell if windows => PathStyle::Windows,
            Kind::Shell => PathStyle::Posix,
        }
    }
}

/// Windows の場所を、WSL の中から見た場所に読み替える。
/// `C:\a\b` は `/mnt/c/a/b`、`\\wsl$\Ubuntu\home\me` は `/home/me`。
pub fn wsl_path(path: &str) -> String {
    for prefix in [r"\\wsl$\", r"\\wsl.localhost\"] {
        let head = path.get(..prefix.len()).unwrap_or_default();
        if head.eq_ignore_ascii_case(prefix) {
            // 先頭はディストリビューションの名前
            let inside = path[prefix.len()..]
                .split_once('\\')
                .map_or("", |(_, inside)| inside);
            return format!("/{}", inside.replace('\\', "/"));
        }
    }
    let mut chars = path.chars();
    match (chars.next(), chars.next()) {
        (Some(drive), Some(':')) if drive.is_ascii_alphabetic() => {
            let rest = chars.as_str().replace('\\', "/");
            format!("/mnt/{}{rest}", drive.to_ascii_lowercase())
        }
        _ => path.replace('\\', "/"),
    }
}

fn posix_quote(path: &str) -> String {
    let plain = |c: char| c.is_alphanumeric() || "_@%+=:,./-".contains(c);
    if !path.is_empty() && path.chars().all(plain) && !path.starts_with('-') {
        path.to_owned()
    } else {
        format!("'{}'", path.replace('\'', r"'\''"))
    }
}

fn windows_quote(path: &str) -> String {
    let plain = |c: char| c.is_alphanumeric() || r"_@+=:,./\-".contains(c);
    if !path.is_empty() && path.chars().all(plain) {
        path.to_owned()
    } else {
        format!("\"{path}\"")
    }
}

/// ドロップしたファイルを、入力の行に入れる文字にする。
/// 空白で区切り、続けて打てるように最後にも空白を置く。
pub fn drop_text(paths: &[PathBuf], style: PathStyle) -> String {
    let mut out = String::new();
    for path in paths {
        let path = path.to_string_lossy();
        let quoted = match style {
            PathStyle::Posix => posix_quote(&path),
            PathStyle::Windows => windows_quote(&path),
            PathStyle::Wsl => posix_quote(&wsl_path(&path)),
        };
        out.push_str(&quoted);
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    fn shell(kind: Kind) -> Profile {
        Profile {
            id: "x".into(),
            name: "x".into(),
            kind,
            program: "x".into(),
            args: Vec::new(),
        }
    }

    #[test]
    fn plain_paths_are_typed_as_they_are() {
        let text = drop_text(
            &paths(&["/home/me/a.png", "/tmp/メモ.txt"]),
            PathStyle::Posix,
        );
        assert_eq!(text, "/home/me/a.png /tmp/メモ.txt ");
    }

    #[test]
    fn spaces_and_symbols_are_quoted() {
        let text = drop_text(
            &paths(&["/home/me/My Pictures/it's (1).png", "-rf"]),
            PathStyle::Posix,
        );
        assert_eq!(text, r"'/home/me/My Pictures/it'\''s (1).png' '-rf' ");
        let text = drop_text(
            &paths(&[r"C:\Users\me\a.png", r"C:\Program Files\b c.txt"]),
            PathStyle::Windows,
        );
        assert_eq!(text, r#"C:\Users\me\a.png "C:\Program Files\b c.txt" "#);
    }

    #[test]
    fn windows_paths_are_translated_for_wsl() {
        assert_eq!(wsl_path(r"C:\Users\me\a.png"), "/mnt/c/Users/me/a.png");
        assert_eq!(wsl_path(r"\\wsl$\Ubuntu\home\me\a.png"), "/home/me/a.png");
        assert_eq!(wsl_path(r"\\wsl.localhost\Ubuntu\home\me"), "/home/me");
        let text = drop_text(&paths(&[r"D:\My Photos\a.png"]), PathStyle::Wsl);
        assert_eq!(text, "'/mnt/d/My Photos/a.png' ");
    }

    #[test]
    fn the_style_follows_the_shell() {
        assert_eq!(shell(Kind::Wsl).path_style(true), PathStyle::Wsl);
        assert_eq!(shell(Kind::Shell).path_style(true), PathStyle::Windows);
        assert_eq!(shell(Kind::Shell).path_style(false), PathStyle::Posix);
    }
}
