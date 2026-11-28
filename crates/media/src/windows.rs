//! Windows の SMTC（システムのメディア操作）。今のセッション（最後に操作したアプリ）に送る。

use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager as Manager;
use windows_future::IAsyncOperation;

use crate::{Command, Failure};

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
