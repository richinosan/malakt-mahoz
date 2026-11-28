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

/// HTTP で開くか。
pub fn enabled(store: &Store) -> malakt_core::Result<bool> {
    Ok(store.setting(ON_KEY)?.as_deref() == Some("on"))
}

/// 開くポート。読めない値なら標準のポート。
pub fn port(store: &Store) -> malakt_core::Result<u16> {
    Ok(store
        .setting(PORT_KEY)?
        .and_then(|p| p.parse().ok())
        .filter(|p| *p != 0)
        .unwrap_or(DEFAULT_PORT))
}

pub fn save(store: &mut Store, on: bool, port: u16) -> malakt_core::Result<()> {
    store.set_setting(ON_KEY, on.then_some("on"))?;
    let port = port.to_string();
    store.set_setting(
        PORT_KEY,
        (port != DEFAULT_PORT.to_string()).then_some(port.as_str()),
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

/// 設定に合わせて開く・閉じる。開けなければ（ポートが使われているなど）理由の文を返し、前の状態のまま。
pub fn apply(db: &Path, on: bool, port: u16) -> Result<(), String> {
    let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    if !on {
        *running = None;
        return Ok(());
    }
    if running.as_ref().is_some_and(|h| h.port() == port) {
        return Ok(());
    }
    // 同じポートで開き直すときのために、先に閉じる
    let before = running.as_ref().map(Http::port);
    *running = None;
    match Http::start(db.to_owned(), port) {
        Ok(http) => {
            *running = Some(http);
            Ok(())
        }
        Err(error) => {
            if let Some(before) = before {
                // 前のポートで開き直す。開けなければ閉じたまま（その理由は前に出ている）
                *running = Http::start(db.to_owned(), before).ok();
            }
            Err(format!(
                "MCP の HTTP をポート {port} で開けませんでした: {error}"
            ))
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

/// 起動したときに、保存した設定で開く。
pub fn start_saved(store: &Store, db: &Path) -> Result<(), String> {
    let on = enabled(store).map_err(|e| format!("MCP の HTTP の設定を読めませんでした: {e}"))?;
    let port = port(store).map_err(|e| format!("MCP の HTTP の設定を読めませんでした: {e}"))?;
    apply(db, on, port)
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
        assert!(!enabled(&store).unwrap());
        assert_eq!(port(&store).unwrap(), DEFAULT_PORT);
        save(&mut store, true, 40001).unwrap();
        assert!(enabled(&store).unwrap());
        assert_eq!(port(&store).unwrap(), 40001);
        save(&mut store, false, DEFAULT_PORT).unwrap();
        assert!(store.setting(PORT_KEY).unwrap().is_none());
        assert_eq!(url(40001), "http://127.0.0.1:40001/mcp");
        assert_eq!(
            claude_add_command(40001, "mmak_x"),
            "claude mcp add --scope user --transport http malakt-mahoz http://127.0.0.1:40001/mcp --header \"Authorization: Bearer mmak_x\""
        );

        // 使われているポートでは開けず、理由を返す
        let taken = TcpListener::bind("127.0.0.1:0").unwrap();
        let busy = taken.local_addr().unwrap().port();
        let error = apply(&path, true, busy).unwrap_err();
        assert!(error.contains(&format!("ポート {busy}")), "{error}");
        assert_eq!(running_port(), None);
        // 空いたポートで開き、閉じる
        drop(taken);
        apply(&path, true, busy).unwrap();
        assert_eq!(running_port(), Some(busy));
        apply(&path, false, busy).unwrap();
        assert_eq!(running_port(), None);
        assert!(TcpStream::connect(("127.0.0.1", busy)).is_err());
    }
}
