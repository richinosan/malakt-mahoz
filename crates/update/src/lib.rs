//! 自動アップデート。
//!
//! 新しい版があるかは GitHub Releases の最新のリリースだけで決める。あれば、この OS 用の
//! アーカイブを裏で取ってきて、同じリリースの `SHA256SUMS` と合うことを確かめ、中身を
//! 取り出して置いておく（[`stage`]）。アプリを閉じるときに、動いている実行ファイル
//! （macOS は `.app`）と入れ替える（[`install`]）。次に起動すると新しい版になる。
//!
//! ディスクに置くのは、取り出した新しい版 1 つだけ。入れ替えたら消す。
//! どの関数も答えが出るまで戻らないので、画面のスレッドからは [`install`] と [`staged`]、
//! [`discard`]、[`clear_leftovers`]（どれも手元のファイルだけを触る）のほかは呼ばない。

mod fetch;
mod place;

use std::path::{Path, PathBuf};

pub use fetch::{latest, stage};
pub use place::{clear_leftovers, install};

/// 新しい版を探す場所。
#[derive(Debug, Clone)]
pub struct Source {
    /// 最新のリリースを返す API の URL。
    pub latest: String,
    /// リリースのファイルの URL の頭。ここから始まらない URL からは取らない。
    pub downloads: String,
}

impl Source {
    pub fn github() -> Self {
        Self {
            latest: "https://api.github.com/repos/richinosan/malakt-mahoz/releases/latest".into(),
            downloads: "https://github.com/richinosan/malakt-mahoz/releases/download/".into(),
        }
    }
}

/// 失敗。
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// つながらなかった。オフラインで使うアプリなので、画面には出さず、次の確認でやり直す。
    Offline(String),
    /// 取ってきたものが使えなかった。画面に出す。
    Failed(String),
}

/// GitHub にある、今より新しい版。
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    /// この OS 用のアーカイブの名前と URL。
    pub archive: String,
    pub archive_url: String,
    pub sums_url: String,
}

/// 取り出して置いてある新しい版。
#[derive(Debug, Clone, PartialEq)]
pub struct Staged {
    pub version: String,
    /// 置いてあるフォルダー。入れ替えたら丸ごと消す。
    pub dir: PathBuf,
    /// 実行ファイル、または `.app`。
    pub payload: PathBuf,
}

/// リリースのアーカイブの名前に入っている、この OS の名前。配っていない OS なら `None`。
pub fn platform() -> Option<&'static str> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("linux-x86_64")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("macos-aarch64")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("windows-x86_64")
    } else {
        None
    }
}

/// 自動で入れ替えてよい場所で動いているか。
///
/// ソースからビルドしたもの（`target/` の下）と、macOS がダウンロードしたままのアプリを
/// 読み取り専用の場所に写して動かしているとき（App Translocation）は、入れ替えない。
pub fn replaceable(exe: &Path) -> bool {
    let parts: Vec<_> = exe
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    !parts
        .iter()
        .any(|p| p == "target" || p == "AppTranslocation")
}

/// `1.2.3` の形の版を数字にする。
fn parse(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.strip_prefix('v').unwrap_or(version).split('.');
    let numbers = [
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ];
    parts.next().is_none().then_some(numbers)
}

/// `candidate` が `current` より新しいか。読めない版は新しくないものとして扱う。
pub fn newer(candidate: &str, current: &str) -> bool {
    match (parse(candidate), parse(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

const READY: &str = "ready.json";

/// 前に取り出して置いたままの新しい版。今の版より新しくなければ（入れ替えが済んでいる、
/// 取ってくる途中で終わった）、置いてあるものを消して `None`。
pub fn staged(dir: &Path, current: &str) -> Option<Staged> {
    let found = read_ready(dir).filter(|s| newer(&s.version, current) && s.payload.exists());
    if found.is_none() && dir.exists() {
        // 消せなければ次の起動でもう一度消す
        let _ = std::fs::remove_dir_all(dir);
    }
    found
}

fn read_ready(dir: &Path) -> Option<Staged> {
    let text = std::fs::read_to_string(dir.join(READY)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let name = value["payload"].as_str()?;
    // 置いたフォルダーの外を指す名前は受け取らない
    if name.contains(['/', '\\']) || name.starts_with('.') {
        return None;
    }
    Some(Staged {
        version: value["version"].as_str()?.to_owned(),
        dir: dir.to_owned(),
        payload: dir.join("payload").join(name),
    })
}

fn write_ready(dir: &Path, version: &str, payload: &str) -> std::io::Result<()> {
    let value = serde_json::json!({ "version": version, "payload": payload });
    std::fs::write(dir.join(READY), value.to_string())
}

/// 置いてある新しい版を消す。
pub fn discard(staged: &Staged) -> std::io::Result<()> {
    match std::fs::remove_dir_all(&staged.dir) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("v0.10.0", "0.9.0"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
        // 読めない版には上げない
        assert!(!newer("0.2", "0.1.0"));
        assert!(!newer("0.2.0-beta", "0.1.0"));
        assert!(!newer("0.2.0.1", "0.1.0"));
        assert!(!newer("latest", "0.1.0"));
    }

    #[test]
    fn source_builds_and_translocated_apps_are_left_alone() {
        assert!(replaceable(Path::new(
            "/Applications/Malakt Mahoz.app/Contents/MacOS/malakt-mahoz"
        )));
        assert!(replaceable(Path::new("/home/a/.local/bin/malakt-mahoz")));
        assert!(!replaceable(Path::new(
            "/home/a/src/malakt-mahoz/target/release/malakt-mahoz"
        )));
        assert!(!replaceable(Path::new(
            "/private/var/folders/x/AppTranslocation/1234/d/Malakt Mahoz.app/Contents/MacOS/malakt-mahoz"
        )));
    }

    #[test]
    fn leftovers_from_an_unfinished_download_are_removed() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("update");
        // 何もなければ何もしない
        assert_eq!(staged(&dir, "0.1.0"), None);
        // 取ってくる途中で終わった（目印がない）
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("archive"), b"half").unwrap();
        assert_eq!(staged(&dir, "0.1.0"), None);
        assert!(!dir.exists());

        // そろっていれば使う
        std::fs::create_dir_all(dir.join("payload")).unwrap();
        std::fs::write(dir.join("payload/malakt-mahoz"), b"new").unwrap();
        write_ready(&dir, "0.2.0", "malakt-mahoz").unwrap();
        let found = staged(&dir, "0.1.0").unwrap();
        assert_eq!(found.version, "0.2.0");
        assert_eq!(found.payload, dir.join("payload/malakt-mahoz"));
        // 入れ替えが済んでいれば（今の版が同じか新しい）消す
        assert_eq!(staged(&dir, "0.2.0"), None);
        assert!(!dir.exists());
    }

    #[test]
    fn a_marker_pointing_outside_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("update");
        std::fs::create_dir_all(dir.join("payload")).unwrap();
        std::fs::write(root.path().join("elsewhere"), b"x").unwrap();
        write_ready(&dir, "0.2.0", "../../elsewhere").unwrap();
        assert_eq!(staged(&dir, "0.1.0"), None);
        assert!(root.path().join("elsewhere").exists());
    }

    #[test]
    fn discarding_twice_is_fine() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("update");
        std::fs::create_dir_all(dir.join("payload")).unwrap();
        let staged = Staged {
            version: "0.2.0".into(),
            payload: dir.join("payload/x"),
            dir: dir.clone(),
        };
        discard(&staged).unwrap();
        assert!(!dir.exists());
        discard(&staged).unwrap();
    }
}
