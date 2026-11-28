//! OS で再生中のメディア（音楽や動画）を操作する。どのアプリが鳴らしていても、OS の仕組みで送る。
//!
//! - Windows: SMTC（`GlobalSystemMediaTransportControlsSessionManager`）の今のセッション
//! - macOS: 「再生中」（Now Playing）のアプリ。`MediaRemote.framework` の `MRMediaRemoteSendCommand`
//! - Linux: MPRIS（D-Bus の `org.mpris.MediaPlayer2.*`）。鳴っているプレイヤー、なければ最後に止めたもの
//!
//! 再生中の曲の名前・アーティスト・アルバム・ジャケットも読める（[`now_playing`]）。
//!
//! どれも、待つ間は呼んだスレッドを止める。画面のスレッドからは呼ばない。

use std::fmt;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

/// 送る操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    Play,
    Pause,
    /// 鳴っていれば止め、止まっていれば鳴らす。
    Toggle,
    Next,
    Previous,
}

impl Command {
    /// 画面に出す名前。
    pub fn label(self) -> &'static str {
        match self {
            Command::Play => "再生",
            Command::Pause => "一時停止",
            Command::Toggle => "再生 / 一時停止",
            Command::Next => "次の曲",
            Command::Previous => "前の曲",
        }
    }
}

/// 送れなかった理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// 操作できるメディアがない（何も開いていない）。
    NoPlayer,
    /// この OS では送れない。
    Unsupported,
    /// OS が断った。
    Os(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::NoPlayer => f.write_str("操作できる再生中のメディアがありません"),
            Failure::Unsupported => f.write_str("この OS では再生中のメディアを操作できません"),
            Failure::Os(text) => write!(f, "OS がメディアの操作を断りました: {text}"),
        }
    }
}

impl std::error::Error for Failure {}

/// この OS で送れるか。
pub fn supported() -> bool {
    cfg!(any(windows, target_os = "macos", target_os = "linux"))
}

/// 操作を送る。
pub fn send(command: Command) -> Result<(), Failure> {
    #[cfg(windows)]
    return windows::send(command);
    #[cfg(target_os = "macos")]
    return macos::send(command);
    #[cfg(target_os = "linux")]
    return linux::send(command);
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        let _ = command;
        Err(Failure::Unsupported)
    }
}

/// ジャケットの画像の大きさの上限。これより大きいものは読まない。
pub const MAX_ARTWORK: usize = 8 * 1024 * 1024;

/// 再生中（または一時停止中）の曲。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// 鳴っているか。止まっているときは false。
    pub playing: bool,
    /// ジャケットの画像（PNG・JPEG など、ファイルの中身そのまま）。
    pub artwork: Option<Vec<u8>>,
}

/// 再生中の曲を読む。何も開いていなければ `Ok(None)`。
pub fn now_playing() -> Result<Option<NowPlaying>, Failure> {
    #[cfg(windows)]
    return windows::now_playing();
    #[cfg(target_os = "macos")]
    return macos::now_playing();
    #[cfg(target_os = "linux")]
    return linux::now_playing();
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    Err(Failure::Unsupported)
}

/// ジャケットの画像の形式を、中身の先頭から見分ける。
pub fn artwork_format(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else if bytes.starts_with(b"BM") {
        Some("bmp")
    } else {
        None
    }
}

/// `file://` の URL をファイルのパスにする（`%20` などを戻す）。ほかの URL は None。
pub fn file_url_path(url: &str) -> Option<std::path::PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // file://localhost/… と file:///… のどちらも受ける
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
        match (
            bytes[i],
            bytes.get(i + 1).copied().and_then(hex),
            bytes.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(hi), Some(lo)) => {
                out.push(hi * 16 + lo);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(std::ffi::OsString::from_vec(out).into())
    }
    #[cfg(not(unix))]
    String::from_utf8(out).ok().map(Into::into)
}

/// プレイヤーが何人もいるとき、どれの曲を見せるか。鳴っているもの、なければ最後に止めたもの、
/// なければ一時停止の最初のもの、なければ最初のもの。
pub fn shown<'a>(players: &'a [(String, Status)], last: Option<&str>) -> Option<&'a str> {
    players
        .iter()
        .find(|(_, s)| *s == Status::Playing)
        .or_else(|| players.iter().find(|(n, _)| Some(n.as_str()) == last))
        .or_else(|| players.iter().find(|(_, s)| *s == Status::Paused))
        .or_else(|| players.first())
        .map(|(n, _)| n.as_str())
}

/// MPRIS のプレイヤーの状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Playing,
    Paused,
    Stopped,
}

/// どのプレイヤーに何を送るか（Linux のように、プレイヤーが何人もいる OS で使う）。
/// `players` はプレイヤーの名前と状態、`last` は最後に止めたプレイヤー。
/// 鳴っているものがあれば、止める・送るのはそれ全部。鳴らすのは、最後に止めたものか、
/// 止まっている最初のもの。鳴っているものがあるときに「再生」を送る相手はいない。
pub fn targets<'a>(
    command: Command,
    players: &'a [(String, Status)],
    last: Option<&str>,
) -> Vec<(&'a str, Command)> {
    let playing: Vec<&str> = players
        .iter()
        .filter(|(_, s)| *s == Status::Playing)
        .map(|(n, _)| n.as_str())
        .collect();
    let resume = || {
        players
            .iter()
            .find(|(n, s)| Some(n.as_str()) == last && *s != Status::Playing)
            .or_else(|| players.iter().find(|(_, s)| *s == Status::Paused))
            .or_else(|| players.first())
            .map(|(n, _)| n.as_str())
    };
    match command {
        Command::Pause => playing.into_iter().map(|n| (n, Command::Pause)).collect(),
        Command::Play if playing.is_empty() => {
            resume().map(|n| (n, Command::Play)).into_iter().collect()
        }
        Command::Play => Vec::new(),
        Command::Toggle if playing.is_empty() => {
            resume().map(|n| (n, Command::Play)).into_iter().collect()
        }
        Command::Toggle => playing.into_iter().map(|n| (n, Command::Pause)).collect(),
        Command::Next | Command::Previous => match playing.first() {
            Some(n) => vec![(*n, command)],
            None => resume().map(|n| (n, command)).into_iter().collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn players(list: &[(&str, Status)]) -> Vec<(String, Status)> {
        list.iter().map(|(n, s)| (n.to_string(), *s)).collect()
    }

    #[test]
    fn 鳴っているものを止め_最後に止めたものを鳴らす() {
        use Status::*;
        let list = players(&[("a", Paused), ("b", Playing), ("c", Playing)]);
        assert_eq!(
            targets(Command::Pause, &list, None),
            [("b", Command::Pause), ("c", Command::Pause)]
        );
        assert!(targets(Command::Play, &list, None).is_empty());
        assert_eq!(targets(Command::Next, &list, None), [("b", Command::Next)]);
        let list = players(&[("a", Paused), ("b", Paused), ("c", Stopped)]);
        assert_eq!(
            targets(Command::Play, &list, Some("b")),
            [("b", Command::Play)]
        );
        assert_eq!(targets(Command::Play, &list, None), [("a", Command::Play)]);
        assert_eq!(
            targets(Command::Toggle, &list, Some("c")),
            [("c", Command::Play)]
        );
        assert!(targets(Command::Pause, &list, None).is_empty());
        assert!(targets(Command::Play, &[], None).is_empty());
    }

    #[test]
    fn 見せる曲は鳴っているものから選ぶ() {
        use Status::*;
        let list = players(&[("a", Paused), ("b", Stopped), ("c", Playing)]);
        assert_eq!(shown(&list, Some("b")), Some("c"));
        let list = players(&[("a", Stopped), ("b", Paused), ("c", Paused)]);
        assert_eq!(shown(&list, Some("c")), Some("c"));
        assert_eq!(shown(&list, None), Some("b"));
        assert_eq!(shown(&players(&[("a", Stopped)]), None), Some("a"));
        assert_eq!(shown(&[], None), None);
    }

    #[test]
    fn ジャケットの形式とファイルの_url() {
        assert_eq!(artwork_format(b"\x89PNG\r\n\x1a\n...."), Some("png"));
        assert_eq!(artwork_format(&[0xff, 0xd8, 0xff, 0xe0]), Some("jpeg"));
        assert_eq!(artwork_format(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(artwork_format(b"<svg"), None);
        assert_eq!(
            file_url_path("file:///tmp/%E6%9B%B2%20a.png"),
            Some("/tmp/曲 a.png".into())
        );
        assert_eq!(
            file_url_path("file://localhost/a/b.jpg"),
            Some("/a/b.jpg".into())
        );
        assert_eq!(file_url_path("https://example.com/a.jpg"), None);
    }
}
