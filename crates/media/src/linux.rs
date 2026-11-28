//! MPRIS（D-Bus）。

use std::sync::Mutex;

use zbus::blocking::{fdo::DBusProxy, Connection, Proxy};

use crate::{targets, Command, Failure, Status};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

/// 最後に止めたプレイヤー。次に鳴らすときは、これを鳴らす。
static LAST: Mutex<Option<String>> = Mutex::new(None);

fn os(error: zbus::Error) -> Failure {
    Failure::Os(error.to_string())
}

pub fn send(command: Command) -> Result<(), Failure> {
    let conn = Connection::session().map_err(os)?;
    let names = DBusProxy::new(&conn)
        .map_err(os)?
        .list_names()
        .map_err(|e| Failure::Os(e.to_string()))?;
    let mut players = Vec::new();
    for name in names {
        let name = name.to_string();
        if !name.starts_with(PREFIX) {
            continue;
        }
        let proxy = Proxy::new(&conn, name.clone(), PATH, PLAYER).map_err(os)?;
        // 状態を読めないプレイヤーは、止まっているものとして扱う
        let status = match proxy.get_property::<String>("PlaybackStatus") {
            Ok(s) if s == "Playing" => Status::Playing,
            Ok(s) if s == "Paused" => Status::Paused,
            _ => Status::Stopped,
        };
        players.push((name, status));
    }
    if players.is_empty() {
        return Err(Failure::NoPlayer);
    }
    let last = LAST.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for (name, command) in targets(command, &players, last.as_deref()) {
        let method = match command {
            Command::Play => "Play",
            Command::Pause => "Pause",
            Command::Toggle => "PlayPause",
            Command::Next => "Next",
            Command::Previous => "Previous",
        };
        let proxy = Proxy::new(&conn, name.to_owned(), PATH, PLAYER).map_err(os)?;
        proxy.call_method(method, &()).map_err(os)?;
        if command == Command::Pause {
            *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some(name.to_owned());
        }
    }
    Ok(())
}
