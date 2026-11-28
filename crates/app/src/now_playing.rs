//! OS で再生中の曲（曲名・アーティスト・アルバム・ジャケット）。ポモドーロのウィンドウが開いている間だけ、
//! 2 秒ごとに [`malakt_media::now_playing`] で読む。読むのは裏のスレッドで、画面は止めない。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::{App, AppContext, Context, Entity, Global, Image, ImageFormat, SharedString};
use malakt_media::{Failure, NowPlaying};

/// 読み直す間隔。
pub const EVERY: Duration = Duration::from_secs(2);

/// 再生中の曲の読み方。テストでは差し替える。
#[derive(Clone)]
pub struct NowPlayingReader(pub Arc<dyn Fn() -> Result<Option<NowPlaying>, Failure> + Send + Sync>);

impl Global for NowPlayingReader {}

/// 画面に出す曲。
#[derive(Clone, PartialEq)]
pub struct Track {
    pub title: SharedString,
    pub artist: SharedString,
    pub album: SharedString,
    pub playing: bool,
    pub artwork: Option<Arc<Image>>,
}

/// 最後に読んだ再生中の曲。
#[derive(Default)]
pub struct Music {
    pub track: Option<Track>,
    /// 読めなかったときの文。次に読めたら消える。
    pub error: Option<SharedString>,
    /// 前のジャケット。中身が同じなら、同じ画像を使い続ける（描き直しで読み込み直さない）。
    artwork: Option<(u64, Arc<Image>)>,
    /// 頼んだ回と、画面に出した回。遅れて届いた古い答えは捨てる。
    asked: u64,
    shown: u64,
}

struct Shared(Entity<Music>);

impl Global for Shared {}

pub fn init(cx: &mut App) {
    let entity = cx.new(|_| Music::default());
    cx.set_global(Shared(entity));
    if !cx.has_global::<NowPlayingReader>() {
        cx.set_global(NowPlayingReader(Arc::new(malakt_media::now_playing)));
    }
}

pub fn shared(cx: &App) -> Entity<Music> {
    cx.global::<Shared>().0.clone()
}

/// ジャケットの中身を、画面に出せる画像にする。読めない形式は出さない。
fn image(bytes: Vec<u8>) -> Option<Image> {
    let format = match malakt_media::artwork_format(&bytes)? {
        "png" => ImageFormat::Png,
        "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::Webp,
        "bmp" => ImageFormat::Bmp,
        _ => return None,
    };
    Some(Image::from_bytes(format, bytes))
}

impl Music {
    /// 裏で読み直す。
    pub fn read(&mut self, cx: &mut Context<Self>) {
        self.asked += 1;
        let ticket = self.asked;
        let reader = cx.global::<NowPlayingReader>().clone();
        let task = cx.background_executor().spawn(async move { (reader.0)() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            // 曲の状態はアプリが終わるまで生きている。終わりかけで消えていれば、出す先もない
            let _ = this.update(cx, |this, cx| this.apply(ticket, result, cx));
        })
        .detach();
    }

    /// 少し待ってから読み直す。操作を送った直後は、OS の答えがまだ前の曲のことがある。
    pub fn read_soon(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            // 上と同じく、消えていれば出す先もない
            let _ = this.update(cx, |this, cx| this.read(cx));
        })
        .detach();
    }

    fn apply(
        &mut self,
        ticket: u64,
        result: Result<Option<NowPlaying>, Failure>,
        cx: &mut Context<Self>,
    ) {
        if ticket < self.shown {
            return;
        }
        self.shown = ticket;
        let (track, error) = match result {
            Ok(Some(playing)) => (Some(self.track(playing)), None),
            // 何も開いていない、この OS では読めない: 曲を出さない
            Ok(None) | Err(Failure::NoPlayer | Failure::Unsupported) => (None, None),
            Err(e) => (
                None,
                Some(SharedString::from(format!(
                    "再生中の曲を読めませんでした: {e}"
                ))),
            ),
        };
        if track != self.track || error != self.error {
            self.track = track;
            self.error = error;
            cx.notify();
        }
    }

    fn track(&mut self, playing: NowPlaying) -> Track {
        let artwork = playing.artwork.and_then(|bytes| {
            let mut hasher = DefaultHasher::new();
            bytes.hash(&mut hasher);
            let key = hasher.finish();
            match &self.artwork {
                Some((old, image)) if *old == key => Some(image.clone()),
                _ => {
                    let image = Arc::new(image(bytes)?);
                    self.artwork = Some((key, image.clone()));
                    Some(image)
                }
            }
        });
        Track {
            title: playing.title.into(),
            artist: playing.artist.into(),
            album: playing.album.into(),
            playing: playing.playing,
            artwork,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui_kit::test]
    fn 遅れて届いた古い答えは出さない(cx: &mut gpui_kit::TestAppContext) {
        let music = cx.new(|_| Music::default());
        let song = |title: &str| {
            Ok(Some(NowPlaying {
                title: title.into(),
                playing: true,
                ..Default::default()
            }))
        };
        music.update(cx, |m, cx| {
            m.apply(2, song("新しい曲"), cx);
            m.apply(1, song("古い曲"), cx);
            assert_eq!(m.track.as_ref().unwrap().title, "新しい曲");
            m.apply(3, Err(Failure::Os("切れた".into())), cx);
            assert!(m.track.is_none());
            assert!(m.error.as_ref().unwrap().contains("切れた"));
            m.apply(4, Err(Failure::NoPlayer), cx);
            assert!(m.track.is_none() && m.error.is_none());
        });
    }
}
