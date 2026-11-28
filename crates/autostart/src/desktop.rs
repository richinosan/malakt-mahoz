//! Linux の XDG autostart（`~/.config/autostart/<ID>.desktop`）。

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::{Path, PathBuf};

/// `Exec=` の 1 つの引数。予約の文字があるので、いつも二重引用符で囲む。
fn quote(argument: &str) -> String {
    let mut quoted = String::from("\"");
    for c in argument.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            // `%` はフィールドコードの印なので、`%%` にする
            '%' => quoted.push_str("%%"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    // .desktop の文字列の値では、`\` をもう一度 `\\` にする
    quoted.replace('\\', "\\\\")
}

/// [`quote`] の逆。最初の引数だけを読む。
fn first_argument(exec: &str) -> Option<String> {
    let mut value = String::new();
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next()? {
                's' => value.push(' '),
                'n' => value.push('\n'),
                't' => value.push('\t'),
                'r' => value.push('\r'),
                other => value.push(other),
            },
            _ => value.push(c),
        }
    }
    let value = value.trim_start();
    let mut argument = String::new();
    let mut chars = value.chars().peekable();
    if value.starts_with('"') {
        chars.next();
        loop {
            match chars.next()? {
                '"' => break,
                '\\' => argument.push(chars.next()?),
                '%' if chars.peek() == Some(&'%') => {
                    chars.next();
                    argument.push('%');
                }
                c => argument.push(c),
            }
        }
    } else {
        argument = value.split_whitespace().next()?.replace("%%", "%");
    }
    (!argument.is_empty()).then_some(argument)
}

/// ログインしたときに `exe` を起動する .desktop。
pub fn entry(name: &str, exe: &Path) -> String {
    let name = name.replace('\\', "\\\\").replace('\n', " ");
    format!(
        "[Desktop Entry]\nType=Application\nName={name}\nExec={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        quote(&exe.to_string_lossy())
    )
}

/// .desktop が起動するもの。止めてあれば（`Hidden=true` など）`None`。
pub fn target(text: &str) -> Option<PathBuf> {
    let mut exec = None;
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match (key.trim(), value.trim()) {
            ("Hidden", "true") | ("X-GNOME-Autostart-enabled", "false") => return None,
            ("Exec", value) => exec = Some(value.to_owned()),
            _ => {}
        }
    }
    first_argument(&exec?).map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odd_paths_are_read_back() {
        for path in [
            "/opt/malakt mahoz/malakt-mahoz",
            "/home/me/$HOME/`a`/\"q\"/back\\slash/100%/malakt-mahoz",
            "/home/me/タスク/malakt-mahoz",
        ] {
            let text = entry("Malakt Mahoz", Path::new(path));
            assert_eq!(target(&text).as_deref(), Some(Path::new(path)), "{text}");
        }
    }

    #[test]
    fn the_entry_has_the_name_and_a_quoted_exec() {
        let text = entry("Malakt Mahoz", Path::new("/opt/m m/malakt-mahoz"));
        assert!(text.starts_with("[Desktop Entry]\n"));
        assert!(text.contains("\nName=Malakt Mahoz\n"));
        assert!(text.contains("\nExec=\"/opt/m m/malakt-mahoz\"\n"));
    }

    #[test]
    fn a_hidden_entry_does_not_start() {
        let text = entry("M", Path::new("/opt/m")) + "Hidden=true\n";
        assert_eq!(target(&text), None);
        let text = entry("M", Path::new("/opt/m")).replace(
            "X-GNOME-Autostart-enabled=true",
            "X-GNOME-Autostart-enabled=false",
        );
        assert_eq!(target(&text), None);
        // ほかの人が書いた、引用符のない Exec も読む
        let text = "[Desktop Entry]\nExec=/usr/bin/malakt-mahoz --flag\n";
        assert_eq!(
            target(text).as_deref(),
            Some(Path::new("/usr/bin/malakt-mahoz"))
        );
        assert_eq!(target("[Other]\nExec=/x\n"), None);
    }
}
