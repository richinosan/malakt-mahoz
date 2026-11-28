//! MPRIS（D-Bus）。

use std::collections::HashMap;
use std::sync::Mutex;

use zbus::blocking::{fdo::DBusProxy, Connection, Proxy};
use zbus::zvariant::{OwnedValue, Value};

use crate::{file_url_path, shown, targets, Command, Failure, NowPlaying, Status, MAX_ARTWORK};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

/// 最後に止めたプレイヤー。次に鳴らすときは、これを鳴らす。
static LAST: Mutex<Option<String>> = Mutex::new(None);

/// D-Bus のつなぎ。曲を 2 秒ごとに読むので、つなぎ直さずに使い回す。
static CONNECTION: Mutex<Option<Connection>> = Mutex::new(None);

fn connection() -> Result<Connection, Failure> {
    let mut held = CONNECTION.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(conn) = held.as_ref() {
        return Ok(conn.clone());
    }
    let conn = Connection::session().map_err(os)?;
    *held = Some(conn.clone());
    Ok(conn)
}

/// つなぎが切れていたら、次はつなぎ直す。
fn forget<T>(result: Result<T, Failure>) -> Result<T, Failure> {
    if result.is_err() {
        *CONNECTION.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    result
}

fn os(error: zbus::Error) -> Failure {
    Failure::Os(error.to_string())
}

/// 開いているプレイヤーの名前と状態。
fn players(conn: &Connection) -> Result<Vec<(String, Status)>, Failure> {
    let names = DBusProxy::new(conn)
        .map_err(os)?
        .list_names()
        .map_err(|e| Failure::Os(e.to_string()))?;
    let mut players = Vec::new();
    for name in names {
        let name = name.to_string();
        if !name.starts_with(PREFIX) {
            continue;
        }
        let proxy = Proxy::new(conn, name.clone(), PATH, PLAYER).map_err(os)?;
        // 状態を読めないプレイヤーは、止まっているものとして扱う
        let status = match proxy.get_property::<String>("PlaybackStatus") {
            Ok(s) if s == "Playing" => Status::Playing,
            Ok(s) if s == "Paused" => Status::Paused,
            _ => Status::Stopped,
        };
        players.push((name, status));
    }
    Ok(players)
}

pub fn send(command: Command) -> Result<(), Failure> {
    forget(send_on(&connection()?, command))
}

fn send_on(conn: &Connection, command: Command) -> Result<(), Failure> {
    let players = players(conn)?;
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
        let proxy = Proxy::new(conn, name.to_owned(), PATH, PLAYER).map_err(os)?;
        proxy.call_method(method, &()).map_err(os)?;
        if command == Command::Pause {
            *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some(name.to_owned());
        }
    }
    Ok(())
}

/// 文字列の値。アーティストのように配列のときは「, 」でつなぐ。
fn text(value: Option<&OwnedValue>) -> String {
    match value.map(|v| &**v) {
        Some(Value::Str(s)) => s.to_string(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    }
}

pub fn now_playing() -> Result<Option<NowPlaying>, Failure> {
    forget(now_playing_on(&connection()?))
}

fn now_playing_on(conn: &Connection) -> Result<Option<NowPlaying>, Failure> {
    let players = players(conn)?;
    let last = LAST.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(name) = shown(&players, last.as_deref()) else {
        return Ok(None);
    };
    let playing = players
        .iter()
        .any(|(n, s)| n == name && *s == Status::Playing);
    let proxy = Proxy::new(conn, name.to_owned(), PATH, PLAYER).map_err(os)?;
    let metadata: HashMap<String, OwnedValue> = proxy.get_property("Metadata").map_err(os)?;
    let title = text(metadata.get("xesam:title"));
    if title.is_empty() {
        return Ok(None);
    }
    // ジャケットは手元のファイルだけ読む（ネットには取りに行かない）
    let artwork = match file_url_path(&text(metadata.get("mpris:artUrl"))) {
        Some(path) => match std::fs::metadata(&path) {
            Ok(meta) if meta.len() as usize <= MAX_ARTWORK => Some(
                std::fs::read(&path)
                    .map_err(|e| Failure::Os(format!("{}: {e}", path.display())))?,
            ),
            // 大きすぎるものは出さない
            Ok(_) => None,
            // プレイヤーが消したあとのファイルは、ジャケットなしで出す
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(Failure::Os(format!("{}: {e}", path.display()))),
        },
        None => None,
    };
    Ok(Some(NowPlaying {
        title,
        artist: text(metadata.get("xesam:artist")),
        album: text(metadata.get("xesam:album")),
        playing,
        artwork,
    }))
}
