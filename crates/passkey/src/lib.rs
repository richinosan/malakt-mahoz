//! OS の本人確認。
//!
//! Windows は Windows Hello（顔・指紋・PIN）、macOS は Touch ID かログインのパスワード、
//! Linux は fprintd の指紋で確かめる。秘密はこのアプリに保存しない。

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as os;
#[cfg(target_os = "macos")]
use macos as os;
#[cfg(windows)]
use windows as os;

/// 本人確認が通らなかった理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// この端末にパスキーがない。
    Unavailable,
    /// 本人がキャンセルしたか、確認が通らなかった。
    Denied,
    /// OS のエラー。
    Os(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => f.write_str("この端末にはパスキーがありません"),
            Self::Denied => f.write_str("パスキーで確認できませんでした"),
            Self::Os(detail) => write!(f, "パスキーのエラー: {detail}"),
        }
    }
}

impl std::error::Error for Failure {}

/// 何のための確認かと、OS のダイアログを重ねるウィンドウ。
#[derive(Debug, Clone)]
pub struct Request {
    /// アプリの名前。
    pub app: String,
    /// 確認の目的（「ロックを解除」など）。OS のダイアログに出る。
    pub action: String,
    /// Windows のウィンドウ（HWND）。ほかの OS では使わない。
    pub window: Option<isize>,
}

/// OS のダイアログで本人確認をする。答えが出るまで戻らないので、画面のスレッドでは呼ばない。
pub fn verify(request: &Request) -> Result<(), Failure> {
    os::verify(request)
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod os {
    pub fn verify(_: &super::Request) -> Result<(), super::Failure> {
        Err(super::Failure::Unavailable)
    }
}
