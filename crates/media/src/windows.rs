//! Windows の SMTC（システムのメディア操作）。今のセッション（最後に操作したアプリ）に送る。

use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Storage::Streams::{DataReader, IRandomAccessStreamReference};
use windows_future::IAsyncOperation;

use crate::{Command, Failure, NowPlaying, MAX_ARTWORK};

fn os(error: windows::core::Error) -> Failure {
    Failure::Os(error.message())
}

pub fn send(command: Command) -> Result<(), Failure> {
    let manager = Manager::RequestAsync().map_err(os)?.join().map_err(os)?;
    // 何も開いていなければ、今のセッションはない
    let session = manager.GetCurrentSession().map_err(|_| Failure::NoPlayer)?;
    let operation: IAsyncOperation<bool> = match command {
        Command::Play => session.TryPlayAsync(),
        Command::Pause => session.TryPauseAsync(),
        Command::Toggle => session.TryTogglePlayPauseAsync(),
        Command::Next => session.TrySkipNextAsync(),
        Command::Previous => session.TrySkipPreviousAsync(),
    }
    .map_err(os)?;
    if operation.join().map_err(os)? {
        Ok(())
    } else {
        Err(Failure::Os(format!(
            "再生中のアプリが「{}」を受け付けませんでした",
            command.label()
        )))
    }
}

/// ジャケットの中身を読む。
fn read_artwork(reference: &IRandomAccessStreamReference) -> Result<Option<Vec<u8>>, Failure> {
    let stream = reference.OpenReadAsync().map_err(os)?.join().map_err(os)?;
    let size = stream.Size().map_err(os)? as usize;
    if size == 0 || size > MAX_ARTWORK {
        return Ok(None);
    }
    let reader =
        DataReader::CreateDataReader(&stream.GetInputStreamAt(0).map_err(os)?).map_err(os)?;
    let loaded = reader
        .LoadAsync(size as u32)
        .map_err(os)?
        .join()
        .map_err(os)? as usize;
    let mut bytes = vec![0; loaded];
    reader.ReadBytes(&mut bytes).map_err(os)?;
    Ok(Some(bytes))
}

pub fn now_playing() -> Result<Option<NowPlaying>, Failure> {
    let manager = Manager::RequestAsync().map_err(os)?.join().map_err(os)?;
    // 何も開いていなければ、今のセッションはない
    let Ok(session) = manager.GetCurrentSession() else {
        return Ok(None);
    };
    let properties = session
        .TryGetMediaPropertiesAsync()
        .map_err(os)?
        .join()
        .map_err(os)?;
    let title = properties.Title().map_err(os)?.to_string();
    if title.is_empty() {
        return Ok(None);
    }
    let playing = session
        .GetPlaybackInfo()
        .and_then(|info| info.PlaybackStatus())
        .map_err(os)?
        == PlaybackStatus::Playing;
    let artwork = match properties.Thumbnail() {
        Ok(reference) => read_artwork(&reference)?,
        // ジャケットのない曲
        Err(_) => None,
    };
    Ok(Some(NowPlaying {
        title,
        artist: properties.Artist().map_err(os)?.to_string(),
        album: properties.AlbumTitle().map_err(os)?.to_string(),
        playing,
        artwork,
    }))
}
