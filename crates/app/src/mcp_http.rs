//! MCP を HTTP のポートで開く（MCP の Streamable HTTP）。
//!
//! ```text
//! http://127.0.0.1:39217/mcp
//! ```
//!
//! アプリが動いている間だけ、この PC（127.0.0.1）からの接続だけを受ける。標準はオフで、設定の
//! 「MCP の接続」でオンにする。中身は `malakt-mahoz mcp`（標準入出力）と同じ [`crate::mcp::Server`] で、
//! アクセスキーは `Authorization: Bearer mmak_…` で渡す。キーなしの接続は、キーなしの接続の決まりで決まる。
//!
//! - `POST /mcp` に JSON-RPC を 1 つ（または配列）送ると、`application/json` で答える。通知だけなら 202
//! - `initialize` に答えるときに `Mcp-Session-Id` を返す。以後その ID を付けると、名乗った名前が履歴に残る
//! - `DELETE /mcp` でセッションを閉じる。サーバーからの通知はないので、`GET` は 405
//! - ほかのサイトのページから送られたもの（`Origin` が違う）や、`Host` が違うもの（DNS の付け替え）は断る

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use malakt_core::Store;
use serde_json::{json, Value};

use crate::mcp::Server;

/// 標準のポート。
pub const DEFAULT_PORT: u16 = 39217;
/// MCP を受ける道。
pub const PATH: &str = "/mcp";
const ON_KEY: &str = "mcp.http";
const PORT_KEY: &str = "mcp.http.port";
/// 頭（リクエスト行とヘッダー）の大きさの上限。
const MAX_HEAD: usize = 64 * 1024;
/// 本文の大きさの上限。
const MAX_BODY: usize = 16 * 1024 * 1024;
/// 覚えておくセッションの数。超えたら古いものから忘れる。
const MAX_SESSIONS: usize = 64;
/// つないだまま何も来ないとき、閉じるまでの時間。
const IDLE: Duration = Duration::from_secs(120);

/// ポートの決め方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortMode {
    /// 前に使ったポート（はじめは標準のポート）が空いていればそれ、使われていれば空いているポート。
    /// 選んだポートは覚えて、次も使う
    Auto,
    /// 決めたポートだけ。使われていれば開かない
    Fixed,
}

impl PortMode {
    pub fn label(self) -> &'static str {
        match self {
            PortMode::Auto => "自動",
            PortMode::Fixed => "固定",
        }
    }
}

/// HTTP の設定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub on: bool,
    pub mode: PortMode,
    /// 固定ならそのポート、自動なら前に使ったポート
    pub port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            on: false,
            mode: PortMode::Auto,
            port: DEFAULT_PORT,
        }
    }
}

const MODE_KEY: &str = "mcp.http.port_mode";

/// 保存した設定。読めない値は標準のもの。
pub fn config(store: &Store) -> malakt_core::Result<Config> {
    Ok(Config {
        on: store.setting(ON_KEY)?.as_deref() == Some("on"),
        mode: match store.setting(MODE_KEY)?.as_deref() {
            Some("fixed") => PortMode::Fixed,
            _ => PortMode::Auto,
        },
        port: store
            .setting(PORT_KEY)?
            .and_then(|p| p.parse().ok())
            .filter(|p| *p != 0)
            .unwrap_or(DEFAULT_PORT),
    })
}

pub fn save(store: &mut Store, config: &Config) -> malakt_core::Result<()> {
    store.set_setting(ON_KEY, config.on.then_some("on"))?;
    store.set_setting(
        MODE_KEY,
        (config.mode == PortMode::Fixed).then_some("fixed"),
    )?;
    let port = config.port.to_string();
    store.set_setting(
        PORT_KEY,
        (config.port != DEFAULT_PORT).then_some(port.as_str()),
    )
}

/// MCP の URL。
pub fn url(port: u16) -> String {
    format!("http://127.0.0.1:{port}{PATH}")
}

/// Claude Code に HTTP で登録するコマンド。
pub fn claude_add_command(port: u16, key: &str) -> String {
    format!(
        "claude mcp add --scope user --transport http {} {} --header \"Authorization: Bearer {key}\"",
        crate::mcp::NAME,
        url(port)
    )
}

/// 動いている HTTP のサーバー。落とすと、ポートを閉じる。
pub struct Http {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    accept: Option<std::thread::JoinHandle<()>>,
}

impl Http {
    /// 127.0.0.1 の `port` で受け始める。`0` なら空いているポート（テスト用）。
    pub fn start(db: PathBuf, port: u16) -> std::io::Result<Http> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared {
            db,
            port: addr.port(),
            sessions: Mutex::new(Sessions::default()),
        });
        let stopping = stop.clone();
        let accept = std::thread::Builder::new()
            .name("mcp-http".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stopping.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(stream) = stream else { continue };
                    let shared = shared.clone();
                    let spawned = std::thread::Builder::new()
                        .name("mcp-http-conn".into())
                        .spawn(move || serve_connection(stream, &shared));
                    if let Err(error) = spawned {
                        eprintln!("MCP の HTTP の接続を受けられませんでした: {error}");
                    }
                }
            })?;
        Ok(Http {
            addr,
            stop,
            accept: Some(accept),
        })
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }
}

impl Drop for Http {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // 受け待ちの accept を起こす。つながらなくても、止める印は付いている
        if TcpStream::connect_timeout(&self.addr, Duration::from_millis(500)).is_ok() {
            // ポートが閉じるまで待つ（すぐ同じポートで開き直せるように）
            if let Some(accept) = self.accept.take() {
                let _ = accept.join();
            }
        }
    }
}

static RUNNING: Mutex<Option<Http>> = Mutex::new(None);

/// 開けなかった理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// ほかのものがそのポートで受けている。`ours` なら、ほかに起動している Malakt Mahoz
    Busy { port: u16, ours: bool },
    /// OS が使わせない（Windows が予約しているポートなど）
    Denied { port: u16 },
    /// 空いているポートが見つからない、ほか
    Failed { port: u16 },
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Busy { port, ours: true } => write!(
                f,
                "ポート {port} は、ほかに起動している Malakt Mahoz が使っています。"
            ),
            OpenError::Busy { port, ours: false } => write!(
                f,
                "ポート {port} はほかのアプリが使っています。ポートを自動にするか、別のポートにしてください。"
            ),
            OpenError::Denied { port } => write!(
                f,
                "ポート {port} は OS が使わせないポートです（Windows が予約しているなど）。ポートを自動にするか、別のポートにしてください。"
            ),
            OpenError::Failed { port } => write!(f, "ポート {port} で MCP を開けませんでした。"),
        }
    }
}

/// そのポートで受けているのが Malakt Mahoz の MCP か。答えの `Server` を見る。
fn answered_by_us(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) else {
        return false;
    };
    // 時間切れを付けられなくても、読むときに待つだけ
    let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(800)));
    let ask = format!("GET {PATH} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(ask.as_bytes()).is_err() {
        return false;
    }
    let mut head = Vec::new();
    let _ = stream.take(4096).read_to_end(&mut head);
    let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
    head.contains(&format!("\r\nserver: {}\r\n", crate::mcp::NAME))
}

/// `port` で開く。開けなければ理由。
fn open_on(db: &Path, port: u16) -> Result<Http, OpenError> {
    Http::start(db.to_owned(), port).map_err(|error| match error.kind() {
        std::io::ErrorKind::AddrInUse => OpenError::Busy {
            port,
            ours: answered_by_us(port),
        },
        // Windows の予約（除外）しているポートは、ここに来る
        std::io::ErrorKind::PermissionDenied => OpenError::Denied { port },
        _ => OpenError::Failed { port },
    })
}

/// 自動のときに試すポート。前に使ったポート、標準のポートから 100 個、OS の選ぶ空きポート。
fn auto_ports(preferred: u16) -> impl Iterator<Item = u16> {
    std::iter::once(preferred)
        .chain((DEFAULT_PORT..DEFAULT_PORT + 100).filter(move |p| *p != preferred))
        .chain(std::iter::once(0))
}

/// 設定に合わせて開く・閉じる。開いていれば、そのポートを返す。
/// 開けなければ理由を返し、前の状態のまま（開いていたら、前のポートで開いたまま）。
pub fn apply(db: &Path, config: &Config) -> Result<Option<u16>, OpenError> {
    let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    if !config.on {
        *running = None;
        return Ok(None);
    }
    if let Some(now) = running.as_ref().map(Http::port) {
        // 開いているポートのままでよいなら、そのまま
        if now == config.port || config.mode == PortMode::Auto {
            return Ok(Some(now));
        }
    }
    // 同じポートで開き直すときのために、先に閉じる
    let before = running.take();
    let before_port = before.as_ref().map(Http::port);
    drop(before);
    let opened = match config.mode {
        PortMode::Fixed => open_on(db, config.port),
        PortMode::Auto => {
            let mut first = None;
            let mut found = None;
            for port in auto_ports(config.port) {
                match open_on(db, port) {
                    Ok(http) => {
                        found = Some(http);
                        break;
                    }
                    Err(error) => {
                        first.get_or_insert(error);
                    }
                }
            }
            found.ok_or_else(|| first.unwrap_or(OpenError::Failed { port: config.port }))
        }
    };
    match opened {
        Ok(http) => {
            let port = http.port();
            *running = Some(http);
            Ok(Some(port))
        }
        Err(error) => {
            if let Some(before) = before_port {
                // 前のポートで開き直す。開けなければ閉じたまま（理由は返す文のとおり）
                *running = Http::start(db.to_owned(), before).ok();
            }
            Err(error)
        }
    }
}

/// 今開いているポート。
pub fn running_port() -> Option<u16> {
    RUNNING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(Http::port)
}

/// 起動したときに、保存した設定で開く。自動で前と違うポートになったら、そのポートを覚える。
pub fn start_saved(store: &mut Store, db: &Path) -> Result<Option<u16>, String> {
    let mut saved =
        config(store).map_err(|e| format!("MCP の HTTP の設定を読めませんでした: {e}"))?;
    let port = apply(db, &saved).map_err(|e| format!("MCP を HTTP で開けませんでした。{e}"))?;
    if let Some(port) = port.filter(|p| *p != saved.port) {
        saved.port = port;
        save(store, &saved)
            .map_err(|e| format!("MCP の HTTP のポートを保存できませんでした: {e}"))?;
    }
    Ok(port)
}

type Opener = Box<dyn FnOnce() -> malakt_core::Result<Store> + Send>;

struct Shared {
    db: PathBuf,
    port: u16,
    sessions: Mutex<Sessions>,
}

#[derive(Default)]
struct Sessions {
    /// 作った順
    order: Vec<String>,
    by_id: HashMap<String, Arc<Mutex<Server<Opener>>>>,
}

impl Shared {
    fn server(&self) -> Server<Opener> {
        let db = self.db.clone();
        Server::new(Box::new(move || Store::open(&db)) as Opener)
    }

    fn session(&self, id: &str) -> Option<Arc<Mutex<Server<Opener>>>> {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.by_id.get(id).cloned()
    }

    fn open_session(&self, server: Server<Opener>) -> (String, Arc<Mutex<Server<Opener>>>) {
        let id = malakt_core::new_id();
        let server = Arc::new(Mutex::new(server));
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if sessions.order.len() >= MAX_SESSIONS {
            let oldest = sessions.order.remove(0);
            sessions.by_id.remove(&oldest);
        }
        sessions.order.push(id.clone());
        sessions.by_id.insert(id.clone(), server.clone());
        (id, server)
    }

    fn close_session(&self, id: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.order.retain(|s| s != id);
        sessions.by_id.remove(id).is_some()
    }

    /// 1 つのリクエストに答える。
    fn respond(&self, request: &Request) -> Response {
        if let Some(refused) = self.check_origin(request) {
            return refused;
        }
        let path = request.path.split('?').next().unwrap_or_default();
        if path != PATH {
            return Response::text(404, "Not Found", format!("MCP は {PATH} です。"));
        }
        let session = request.header("mcp-session-id").map(str::to_owned);
        match request.method.as_str() {
            "POST" => self.post(request, session),
            "DELETE" => match session {
                Some(id) if self.close_session(&id) => Response::empty(200, "OK"),
                _ => Response::text(404, "Not Found", "そのセッションはありません。".into()),
            },
            _ => Response::text(
                405,
                "Method Not Allowed",
                "POST か DELETE で送ってください。".into(),
            )
            .with_header("Allow", "POST, DELETE"),
        }
    }

    /// ほかのサイトのページや、ほかの名前（DNS の付け替え）から来たものを断る。
    fn check_origin(&self, request: &Request) -> Option<Response> {
        let local = |text: &str, scheme: &str| {
            let port = self.port;
            [
                format!("{scheme}127.0.0.1:{port}"),
                format!("{scheme}localhost:{port}"),
                format!("{scheme}[::1]:{port}"),
            ]
            .iter()
            .any(|ok| ok.eq_ignore_ascii_case(text))
        };
        match request.header("host") {
            Some(host) if local(host, "") => {}
            _ => {
                return Some(Response::text(
                    403,
                    "Forbidden",
                    "この PC の 127.0.0.1 からだけつなげます。".into(),
                ))
            }
        }
        match request.header("origin") {
            None => None,
            Some(origin) if local(origin, "http://") => None,
            Some(_) => Some(Response::text(
                403,
                "Forbidden",
                "ほかのサイトからはつなげません。".into(),
            )),
        }
    }

    fn post(&self, request: &Request, session: Option<String>) -> Response {
        let body: Value = match serde_json::from_slice(&request.body) {
            Ok(body) => body,
            Err(error) => {
                return Response::json(
                    400,
                    "Bad Request",
                    &json!({"jsonrpc": "2.0", "id": null,
                            "error": {"code": -32700, "message": error.to_string()}}),
                )
            }
        };
        let key = request
            .header("authorization")
            .and_then(|auth| {
                let (scheme, token) = auth.trim().split_once(' ')?;
                scheme
                    .eq_ignore_ascii_case("bearer")
                    .then(|| token.trim().to_owned())
            })
            .filter(|key| !key.is_empty());
        let (messages, batch) = match body {
            Value::Array(messages) => (messages, true),
            message => (vec![message], false),
        };
        let initialize = messages
            .iter()
            .any(|m| m.get("method").and_then(Value::as_str) == Some("initialize"));
        let (server, new_session) = match &session {
            Some(id) => match self.session(id) {
                Some(server) => (server, None),
                None => {
                    return Response::text(
                        404,
                        "Not Found",
                        "そのセッションはありません。initialize からやり直してください。".into(),
                    )
                }
            },
            None if initialize => {
                let (id, server) = self.open_session(self.server());
                (server, Some(id))
            }
            None => (Arc::new(Mutex::new(self.server())), None),
        };
        let mut server = server.lock().unwrap_or_else(|e| e.into_inner());
        server.set_key(key);
        let replies: Vec<Value> = messages
            .iter()
            .filter_map(|message| server.handle(&message.to_string()))
            .filter_map(|reply| serde_json::from_str(&reply).ok())
            .collect();
        let response = if replies.is_empty() {
            Response::empty(202, "Accepted")
        } else if batch {
            Response::json(200, "OK", &Value::Array(replies))
        } else {
            Response::json(200, "OK", &replies[0])
        };
        match new_session {
            Some(id) => response.with_header("Mcp-Session-Id", &id),
            None => response,
        }
    }
}

struct Request {
    method: String,
    path: String,
    /// 名前は小文字
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    /// 答えたら閉じる（HTTP/1.0 か Connection: close）
    close: bool,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

struct Response {
    status: u16,
    reason: &'static str,
    content_type: Option<&'static str>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn empty(status: u16, reason: &'static str) -> Self {
        Response {
            status,
            reason,
            content_type: None,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    fn text(status: u16, reason: &'static str, text: String) -> Self {
        Response {
            content_type: Some("text/plain; charset=utf-8"),
            body: text.into_bytes(),
            ..Self::empty(status, reason)
        }
    }

    fn json(status: u16, reason: &'static str, value: &Value) -> Self {
        Response {
            content_type: Some("application/json"),
            body: value.to_string().into_bytes(),
            ..Self::empty(status, reason)
        }
    }

    fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    fn write(&self, out: &mut dyn Write, close: bool) -> std::io::Result<()> {
        let mut head = format!("HTTP/1.1 {} {}\r\n", self.status, self.reason);
        if let Some(content_type) = self.content_type {
            head.push_str(&format!("Content-Type: {content_type}\r\n"));
        }
        head.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
        // 同じポートを開こうとしたほかの Malakt Mahoz が、相手を見分けられるように
        head.push_str(&format!("Server: {}\r\n", crate::mcp::NAME));
        for (name, value) in &self.headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str(if close {
            "Connection: close\r\n\r\n"
        } else {
            "Connection: keep-alive\r\n\r\n"
        });
        out.write_all(head.as_bytes())?;
        out.write_all(&self.body)?;
        out.flush()
    }
}

/// 1 つの接続で、閉じられるまでリクエストに答える。
fn serve_connection(stream: TcpStream, shared: &Shared) {
    // 読み書きの時間切れは、つないだまま止まった相手を閉じるため。付けられなくても答えはする
    let _ = stream.set_read_timeout(Some(IDLE));
    let _ = stream.set_write_timeout(Some(IDLE));
    let Ok(reader) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader);
    let mut out = stream;
    loop {
        let (response, close) = match read_request(&mut reader) {
            Ok(Some(request)) => (shared.respond(&request), request.close),
            // 相手が閉じた、または時間切れ
            Ok(None) => return,
            Err(Bad(status, reason, text)) => (Response::text(status, reason, text), true),
        };
        if response.write(&mut out, close).is_err() || close {
            return;
        }
    }
}

/// 読めなかったリクエスト。答えてから閉じる。
struct Bad(u16, &'static str, String);

fn read_request(reader: &mut impl BufRead) -> Result<Option<Request>, Bad> {
    // 頭は、空の行まで
    let mut head = Vec::new();
    loop {
        let before = head.len();
        let read = reader
            .by_ref()
            .take((MAX_HEAD + 1 - head.len()) as u64)
            .read_until(b'\n', &mut head);
        match read {
            Ok(0) if head.is_empty() => return Ok(None),
            Ok(0) => return Ok(None),
            Ok(_) => {}
            Err(_) => return Ok(None),
        }
        if head.len() > MAX_HEAD {
            return Err(Bad(
                431,
                "Request Header Fields Too Large",
                "ヘッダーが大きすぎます。".into(),
            ));
        }
        // リクエストの前の空行は読み飛ばす
        let line = &head[before..];
        if before == 0 && (line == b"\r\n" || line == b"\n") {
            head.clear();
            continue;
        }
        if head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n") {
            break;
        }
    }
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut parsed = httparse::Request::new(&mut headers);
    match parsed.parse(&head) {
        Ok(httparse::Status::Complete(_)) => {}
        _ => {
            return Err(Bad(
                400,
                "Bad Request",
                "HTTP のリクエストとして読めません。".into(),
            ))
        }
    }
    let method = parsed.method.unwrap_or_default().to_owned();
    let path = parsed.path.unwrap_or_default().to_owned();
    let version = parsed.version.unwrap_or(1);
    let headers: Vec<(String, String)> = parsed
        .headers
        .iter()
        .map(|h| {
            (
                h.name.to_ascii_lowercase(),
                String::from_utf8_lossy(h.value).trim().to_owned(),
            )
        })
        .collect();
    let get = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    };
    let close = version == 0 || get("connection").is_some_and(|c| c.eq_ignore_ascii_case("close"));
    let chunked =
        get("transfer-encoding").is_some_and(|t| t.to_ascii_lowercase().contains("chunked"));
    let body = if chunked {
        read_chunked(reader)?
    } else {
        let length: usize = match get("content-length") {
            None => 0,
            Some(n) => n
                .parse()
                .map_err(|_| Bad(400, "Bad Request", "Content-Length が読めません。".into()))?,
        };
        if length > MAX_BODY {
            return Err(Bad(413, "Payload Too Large", "本文が大きすぎます。".into()));
        }
        let mut body = vec![0; length];
        reader
            .read_exact(&mut body)
            .map_err(|_| Bad(400, "Bad Request", "本文が途中で切れています。".into()))?;
        body
    };
    Ok(Some(Request {
        method,
        path,
        headers,
        body,
        close,
    }))
}

fn read_chunked(reader: &mut impl BufRead) -> Result<Vec<u8>, Bad> {
    let bad = || Bad(400, "Bad Request", "本文（chunked）が読めません。".into());
    let mut body = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|_| bad())?;
        let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16)
            .map_err(|_| bad())?;
        if size == 0 {
            // 後ろのヘッダーは読み捨てる
            loop {
                line.clear();
                reader.read_line(&mut line).map_err(|_| bad())?;
                if line.trim().is_empty() {
                    return Ok(body);
                }
            }
        }
        if body.len() + size > MAX_BODY {
            return Err(Bad(413, "Payload Too Large", "本文が大きすぎます。".into()));
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader.read_exact(&mut body[start..]).map_err(|_| bad())?;
        let mut end = [0u8; 2];
        reader.read_exact(&mut end).map_err(|_| bad())?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use malakt_core::lock;

    /// 開いたサーバーに、生の HTTP を送って答えを読む。
    fn send(port: u16, raw: &str) -> (u16, Vec<(String, String)>, String) {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(raw.as_bytes()).unwrap();
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        let code = status.split(' ').nth(1).unwrap().parse().unwrap();
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
            let (name, value) = line.split_once(':').unwrap();
            headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
        }
        let length: usize = headers
            .iter()
            .find(|(n, _)| n == "content-length")
            .unwrap()
            .1
            .parse()
            .unwrap();
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        (code, headers, String::from_utf8(body).unwrap())
    }

    fn post(port: u16, extra: &str, body: &Value) -> (u16, Vec<(String, String)>, String) {
        let body = body.to_string();
        send(
            port,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\n\
                 Accept: application/json, text/event-stream\r\n{extra}Content-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            ),
        )
    }

    fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn call(name: &str, args: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": name, "arguments": args}})
    }

    #[test]
    fn answers_mcp_over_http_with_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.sqlite3");
        let mut app = Store::open(&path).unwrap();
        let http = Http::start(path.clone(), 0).unwrap();
        let port = http.port();

        // initialize でセッションができる
        let (code, headers, body) = post(
            port,
            "",
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": {"protocolVersion": "2025-06-18",
                               "clientInfo": {"name": "Claude Code", "version": "1"}}}),
        );
        assert_eq!(code, 200, "{body}");
        let session = header(&headers, "mcp-session-id").unwrap().to_owned();
        let reply: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(reply["result"]["serverInfo"]["name"], crate::mcp::NAME);
        // 通知には 202 で、本文なし
        let (code, _, body) = post(
            port,
            &format!("Mcp-Session-Id: {session}\r\n"),
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        );
        assert_eq!((code, body.as_str()), (202, ""));

        // キーなし（標準の決まりで読み書きできる）
        let with_session = format!("Mcp-Session-Id: {session}\r\n");
        let (code, _, body) = post(
            port,
            &with_session,
            &call("create_task", json!({"title": "HTTP から"})),
        );
        assert_eq!(code, 200);
        let reply: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(reply["result"]["isError"], false, "{body}");
        let titles: Vec<String> = app
            .tasks(malakt_core::Scope::All, &Default::default())
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect();
        assert_eq!(titles, vec!["HTTP から".to_owned()]);

        // キーなしの決まりを外すと、キーが要る
        let rule = app.keyless_rules().unwrap().remove(0);
        app.remove_keyless_rule(&rule.id).unwrap();
        let (_, _, body) = post(port, &with_session, &call("list_tasks", json!({})));
        let reply: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(reply["result"]["content"][0]["text"], crate::mcp::NEEDS_KEY);
        // Bearer でキーを渡す。読み取りのみのキーは書けない
        let issued = app.issue_access_key("HTTP", true, false).unwrap();
        let bearer = format!("{with_session}Authorization: Bearer {}\r\n", issued.secret);
        let (_, _, body) = post(port, &bearer, &call("list_tasks", json!({})));
        let reply: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(reply["result"]["isError"], false, "{body}");
        let (_, _, body) = post(port, &bearer, &call("create_task", json!({"title": "x"})));
        let reply: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(reply["result"]["isError"], true);
        // ロック中は、ロック中も使えるキーでなければ使えない
        lock::enable(&mut app).unwrap();
        let (_, _, body) = post(port, &bearer, &call("list_tasks", json!({})));
        let reply: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(reply["result"]["content"][0]["text"], crate::mcp::LOCKED);

        // 配列で送ると、配列で答える
        let (code, _, body) = post(
            port,
            "",
            &json!([{"jsonrpc": "2.0", "id": 5, "method": "ping"},
                    {"jsonrpc": "2.0", "method": "notifications/initialized"}]),
        );
        assert_eq!(code, 200);
        let reply: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(reply.as_array().unwrap().len(), 1);
        assert_eq!(reply[0]["id"], 5);

        // セッションを閉じると、その ID は使えない
        let close = format!(
            "DELETE /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nMcp-Session-Id: {session}\r\n\
             Connection: close\r\n\r\n"
        );
        assert_eq!(send(port, &close).0, 200);
        assert_eq!(send(port, &close).0, 404);
        let (code, _, _) = post(port, &with_session, &call("list_tasks", json!({})));
        assert_eq!(code, 404);
        drop(http);
    }

    #[test]
    fn refuses_other_sites_and_other_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.sqlite3");
        Store::open(&path).unwrap();
        let http = Http::start(path, 0).unwrap();
        let port = http.port();
        let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
        // ほかのサイトのページ
        let (code, _, _) = post(port, "Origin: https://example.com\r\n", &ping);
        assert_eq!(code, 403);
        // この PC のページなら通す
        let (code, _, _) = post(port, &format!("Origin: http://localhost:{port}\r\n"), &ping);
        assert_eq!(code, 200);
        // Host が違う（DNS の付け替え）
        let body = ping.to_string();
        let raw = format!(
            "POST /mcp HTTP/1.1\r\nHost: evil.example:{port}\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        );
        assert_eq!(send(port, &raw).0, 403);
        // ほかの道と、GET
        let get = |path: &str| {
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
        };
        assert_eq!(send(port, &get("/")).0, 404);
        let (code, headers, _) = send(port, &get("/mcp"));
        assert_eq!(code, 405);
        assert_eq!(header(&headers, "allow"), Some("POST, DELETE"));
        // 読めない JSON
        let raw = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 2\r\n\
             Connection: close\r\n\r\n{{x"
        );
        let (code, _, body) = send(port, &raw);
        assert_eq!(code, 400);
        assert!(body.contains("-32700"), "{body}");
        // 知らないセッション
        let (code, _, _) = post(port, "Mcp-Session-Id: nope\r\n", &ping);
        assert_eq!(code, 404);
    }

    /// 1 つの接続で続けて送れる（keep-alive）。chunked の本文も読む。
    #[test]
    fn keeps_the_connection_and_reads_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.sqlite3");
        Store::open(&path).unwrap();
        let http = Http::start(path, 0).unwrap();
        let port = http.port();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();
        let (a, b) = body.split_at(10);
        let raw = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: {}\r\n\r\n{body}\
             POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nTransfer-Encoding: chunked\r\n\
             Connection: close\r\n\r\n{:x}\r\n{a}\r\n{:x}\r\n{b}\r\n0\r\n\r\n",
            body.len(),
            a.len(),
            b.len()
        );
        stream.write_all(raw.as_bytes()).unwrap();
        let mut all = String::new();
        stream.read_to_string(&mut all).unwrap();
        assert_eq!(all.matches("HTTP/1.1 200 OK").count(), 2, "{all}");
        assert_eq!(all.matches(r#""id":1"#).count(), 2, "{all}");
    }

    #[test]
    fn settings_and_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.sqlite3");
        let mut store = Store::open(&path).unwrap();
        assert_eq!(config(&store).unwrap(), Config::default());
        let fixed = Config {
            on: true,
            mode: PortMode::Fixed,
            port: 40001,
        };
        save(&mut store, &fixed).unwrap();
        assert_eq!(config(&store).unwrap(), fixed);
        save(&mut store, &Config::default()).unwrap();
        assert!(store.setting(PORT_KEY).unwrap().is_none());
        assert!(store.setting(MODE_KEY).unwrap().is_none());
        assert_eq!(url(40001), "http://127.0.0.1:40001/mcp");
        assert_eq!(
            claude_add_command(40001, "mmak_x"),
            "claude mcp add --scope user --transport http malakt-mahoz http://127.0.0.1:40001/mcp --header \"Authorization: Bearer mmak_x\""
        );
        assert_eq!(PortMode::Auto.label(), "自動");
        assert_eq!(PortMode::Fixed.label(), "固定");
    }

    /// 開こうとしたポートを、先にほかのものが使っている（Windows の os error 10048 と同じ）。
    #[test]
    fn opening_a_port_that_is_already_taken() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.sqlite3");
        let mut store = Store::open(&path).unwrap();
        let taken = TcpListener::bind("127.0.0.1:0").unwrap();
        let busy = taken.local_addr().unwrap().port();

        // 固定なら開かず、ほかのアプリが使っていると返す。OS の文は出さない
        let fixed = Config {
            on: true,
            mode: PortMode::Fixed,
            port: busy,
        };
        let error = apply(&path, &fixed).unwrap_err();
        assert_eq!(
            error,
            OpenError::Busy {
                port: busy,
                ours: false
            }
        );
        let text = error.to_string();
        assert!(
            text.contains(&format!("ポート {busy} はほかのアプリ")),
            "{text}"
        );
        assert!(!text.contains("os error"), "{text}");
        assert_eq!(running_port(), None);

        // 自動なら、空いているポートで開く
        let auto = Config {
            mode: PortMode::Auto,
            ..fixed
        };
        let port = apply(&path, &auto).unwrap().unwrap();
        assert_ne!(port, busy);
        assert_eq!(running_port(), Some(port));
        // 開いているポートで MCP に答える
        let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
        assert_eq!(post(port, "", &ping).0, 200);
        // 開いたまま自動でもう一度オンにしても、同じポート
        assert_eq!(apply(&path, &auto).unwrap(), Some(port));

        // 固定で使われているポートにしようとしたら、前のポートで開いたまま
        assert!(apply(&path, &fixed).is_err());
        assert_eq!(running_port(), Some(port));
        assert_eq!(post(port, "", &ping).0, 200);

        // ほかの Malakt Mahoz が使っているポートは、そう分かる
        let other = Http::start(path.clone(), 0).unwrap();
        let theirs = Config {
            port: other.port(),
            ..fixed
        };
        assert_eq!(
            apply(&path, &theirs).unwrap_err(),
            OpenError::Busy {
                port: other.port(),
                ours: true
            }
        );
        drop(other);

        // 起動したとき、自動で前のポートが使われていたら、空いているポートで開いて覚える
        apply(&path, &Config::default()).unwrap();
        save(&mut store, &auto).unwrap();
        let opened = start_saved(&mut store, &path).unwrap().unwrap();
        assert_ne!(opened, busy);
        assert_eq!(config(&store).unwrap().port, opened);
        // 起動したとき、固定で使われていたら、開かずに理由を返す
        apply(&path, &Config::default()).unwrap();
        save(&mut store, &fixed).unwrap();
        let error = start_saved(&mut store, &path).unwrap_err();
        assert!(error.contains("ほかのアプリが使っています"), "{error}");
        assert_eq!(running_port(), None);

        // 空いたら固定で開き、オフで閉じる
        drop(taken);
        assert_eq!(apply(&path, &fixed).unwrap(), Some(busy));
        assert_eq!(running_port(), Some(busy));
        assert_eq!(apply(&path, &Config::default()).unwrap(), None);
        assert_eq!(running_port(), None);
        assert!(TcpStream::connect(("127.0.0.1", busy)).is_err());
    }
}
