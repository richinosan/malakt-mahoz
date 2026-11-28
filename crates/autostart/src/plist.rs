//! macOS の LaunchAgent（`~/Library/LaunchAgents/<ID>.plist`）。
//!
//! ログインしたときに launchd が読み、`RunAtLoad` で 1 回だけ起動する。`.app` は `open -a` で開く
//! （すでに動いていれば、新しく起動しない）。ファイルを置くだけで、今は起動しない。

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::path::{Path, PathBuf};

const OPEN: &str = "/usr/bin/open";

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// `target`（`.app` か実行ファイル）を、ログインしたときに起動する LaunchAgent。
pub fn agent(label: &str, target: &Path) -> String {
    let target = target.to_string_lossy();
    let arguments = if target.ends_with(".app") {
        vec![OPEN, "-a", &target]
    } else {
        vec![&*target]
    };
    let mut text = String::from(concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
        "\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
        "<plist version=\"1.0\">\n<dict>\n",
    ));
    text.push_str(&format!(
        "\t<key>Label</key>\n\t<string>{}</string>\n",
        escape(label)
    ));
    text.push_str("\t<key>ProgramArguments</key>\n\t<array>\n");
    for argument in arguments {
        text.push_str(&format!("\t\t<string>{}</string>\n", escape(argument)));
    }
    text.push_str(concat!(
        "\t</array>\n",
        "\t<key>RunAtLoad</key>\n\t<true/>\n",
        "\t<key>ProcessType</key>\n\t<string>Interactive</string>\n",
        "\t<key>LimitLoadToSessionType</key>\n\t<string>Aqua</string>\n",
        "</dict>\n</plist>\n",
    ));
    text
}

/// LaunchAgent が起動するもの。`Disabled` が付いていれば、起動しないので `None`。
pub fn target(text: &str) -> Option<PathBuf> {
    let compact: String = text.split_whitespace().collect();
    if compact.contains("<key>Disabled</key><true/>") {
        return None;
    }
    let (_, rest) = text.split_once("<key>ProgramArguments</key>")?;
    let (array, _) = rest.split_once("</array>")?;
    let arguments: Vec<String> = array
        .split("<string>")
        .skip(1)
        .filter_map(|part| part.split_once("</string>"))
        .map(|(value, _)| unescape(value))
        .collect();
    let target = match arguments.as_slice() {
        [open, flag, app] if open == OPEN && flag == "-a" => app,
        [exe, ..] => exe,
        [] => return None,
    };
    Some(PathBuf::from(target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_is_opened_and_read_back() {
        let app = Path::new("/Applications/Malakt & <Mahoz>.app");
        let text = agent("io.github.example.malakt-mahoz", app);
        assert!(text.contains("<string>/usr/bin/open</string>"));
        assert!(text.contains("<string>/Applications/Malakt &amp; &lt;Mahoz&gt;.app</string>"));
        assert!(text.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert_eq!(target(&text).as_deref(), Some(app));
    }

    #[test]
    fn a_bare_executable_is_run_directly() {
        let exe = Path::new("/Users/me/bin/malakt-mahoz");
        let text = agent("id", exe);
        assert!(!text.contains(OPEN));
        assert_eq!(target(&text).as_deref(), Some(exe));
    }

    #[test]
    fn a_disabled_agent_does_not_start() {
        let text = agent("id", Path::new("/Applications/M.app")).replace(
            "<key>RunAtLoad</key>",
            "<key>Disabled</key>\n\t<true/>\n\t<key>RunAtLoad</key>",
        );
        assert_eq!(target(&text), None);
        assert_eq!(target("<plist><dict></dict></plist>"), None);
    }
}
