//! OS で再生中のメディア（音楽や動画）を操作する。どのアプリが鳴らしていても、OS の仕組みで送る。
//!
//! - Windows: SMTC（`GlobalSystemMediaTransportControlsSessionManager`）の今のセッション
//! - macOS: 「再生中」（Now Playing）のアプリ。`MediaRemote.framework` の `MRMediaRemoteSendCommand`
//! - Linux: MPRIS（D-Bus の `org.mpris.MediaPlayer2.*`）。鳴っているプレイヤー、なければ最後に止めたもの
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
}
