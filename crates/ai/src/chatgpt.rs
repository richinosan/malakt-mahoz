//! ChatGPT のアカウントでのサインイン（OpenAI の「Sign in with ChatGPT」の、オープンソースの
//! アプリ向けの流れ）。API キーの代わりに、ユーザーの ChatGPT のプランで AI を呼ぶ。
//!
//! 仕様: <https://developers.openai.com/siwc/token-sharing-open-source>
//!
//! - 最初のサインインは `client_id=dynamic_agent_client` で始め、OpenAI が返す `client_id` を保存して、
//!   次からはそれを使う。クライアントの秘密もパートナーの API キーも要らない
//! - ブラウザからの戻り先は `http://127.0.0.1:<port>/auth/callback`（PKCE の S256）
//! - 呼び出しは `https://api.openai.com/v1/responses`（`store: false`、`stream: true`）
//! - アクセストークンは 1 時間、リフレッシュトークンは 30 日で、更新のたびに新しいものに替わる
//!
//! OS のキーチェーンには、小さな値（ホストの ID、発行された client_id、メールアドレス、
//! リフレッシュトークン）だけを入れる。Windows の資格情報マネージャーは 1 つ 2560 バイトまでしか
//! 入らないため。アクセストークンはメモリの中だけに持ち、起動し直したら更新して取り直す。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::keys::KeyStore;
use crate::provider::Endpoint;
use crate::wire::Wire;

pub const ISSUER: &str = "https://auth.openai.com";
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
pub const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
/// サインアウトで使う `revocation_endpoint` が書いてある所。
pub const DISCOVERY_URL: &str = "https://auth.openai.com/.well-known/openid-configuration";
/// トークンを使う先（`resource`）。
pub const RESOURCE: &str = "https://api.openai.com/v1";
pub const RESPONSES_URL: &str = "https://api.openai.com/v1/responses";
pub const MODELS_URL: &str = "https://api.openai.com/v1/models";
/// ChatGPT の使用量の画面。上限に達したときに案内する。
pub const USAGE_URL: &str = "https://chatgpt.com/settings/usage";

/// 最初の登録の入口。保存する client_id ではない。
const REGISTER_CLIENT: &str = "dynamic_agent_client";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
/// ChatGPT のプランで呼んでよいという許可。ID トークンだけでは使えない。
const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
/// 同意の画面に出す、このアプリの名前。
pub const AGENT_NAME: &str = "Malakt Mahoz";
/// 戻り先のポート。使われていれば、空いているポートにする（変えてよいのはポートだけ）。
const PORT: u16 = 1455;
const CALLBACK_PATH: &str = "/auth/callback";
/// ブラウザでのサインインを待つ長さ。
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// アクセストークンは、切れるこの秒数前に更新する。
const REFRESH_MARGIN: u64 = 120;
/// AI に渡すツールをまとめる名前空間（ChatGPT のプランの呼び出しでは、ツールを名前空間に入れる）。
pub const NAMESPACE: &str = "malakt";

/// キーチェーンでの名前。
const ENTRY: &str = "chatgpt-account";
const WHAT: &str = "ChatGPT のサインイン";
/// [`Account::secret`] の目印。API キーとは混ざらない（API キーは `{` で始まらない）。
const SECRET_MARK: &str = "malakt-chatgpt";

/// フォームの POST。返事の HTTP の状態と、JSON の本文（空なら `Null`）を返す。
/// `Err` は、つながらなかったときだけ。
pub type FormFn = dyn Fn(&str, &[(&str, String)]) -> Result<(u16, Value), String> + Send + Sync;
/// 文字列の GET（[`crate::http::get_text`] と同じ形）。
pub type GetFn = dyn Fn(&str, &[(&'static str, String)]) -> Result<String, String> + Send + Sync;

/// キーチェーンに入れる、ChatGPT のアカウント。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// この PC の ID（`urn:uuid:…`）。同じ PC では、サインインし直しても同じものを送る。
    pub host_id: String,
    /// 最初の登録で発行された client_id。サインアウトしても残し、次のサインインで使う。
    pub client_id: Option<String>,
    pub email: Option<String>,
    /// サインアウトしたら消す。
    pub refresh_token: Option<String>,
}

impl Account {
    /// この PC の新しい ID を持つ、サインインしていないアカウント。
    pub fn for_this_pc() -> Self {
        Self {
            host_id: new_host_id(),
            client_id: None,
            email: None,
            refresh_token: None,
        }
    }

    pub fn signed_in(&self) -> bool {
        self.client_id.is_some() && self.refresh_token.is_some()
    }

    fn to_json(&self) -> Value {
        json!({
            "host_id": self.host_id,
            "client_id": self.client_id,
            "email": self.email,
            "refresh_token": self.refresh_token,
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        Some(Self {
            host_id: text("host_id")?,
            client_id: text("client_id"),
            email: text("email"),
            refresh_token: text("refresh_token"),
        })
    }

    /// キーの代わりに持ち回る文字列（[`crate::keys::lookup`] が返す）。トークンは入れない。
    pub fn secret(&self) -> String {
        json!({
            SECRET_MARK: true,
            "client_id": self.client_id,
            "email": self.email,
        })
        .to_string()
    }

    /// [`Account::secret`] から読む。API キーなら `None`。
    pub fn from_secret(secret: &str) -> Option<Self> {
        if !secret.starts_with('{') {
            return None;
        }
        let value: Value = serde_json::from_str(secret).ok()?;
        if value.get(SECRET_MARK) != Some(&Value::Bool(true)) {
            return None;
        }
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        Some(Self {
            host_id: String::new(),
            client_id: text("client_id"),
            email: text("email"),
            refresh_token: None,
        })
    }

    pub fn load(store: &dyn KeyStore) -> Result<Option<Self>, String> {
        let Some(text) = store.read(ENTRY, WHAT)? else {
            return Ok(None);
        };
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| format!("保存されている ChatGPT のサインインを読めませんでした: {e}"))?;
        Self::from_json(&value)
            .map(Some)
            .ok_or_else(|| "保存されている ChatGPT のサインインの形が正しくありません".into())
    }

    pub fn save(&self, store: &dyn KeyStore) -> Result<(), String> {
        store.write(ENTRY, WHAT, &self.to_json().to_string())
    }
}

/// サインインを待つのをやめたときの `Err`。
pub const CANCELED: &str = "サインインをキャンセルしました";

/// サインインし直さないと使えなくなったときの文。
pub const SIGNED_OUT: &str =
    "ChatGPT のサインインが切れました。設定の「ChatGPT アカウント」でもう一度サインインしてください。";

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    // OS の乱数が使えない PC では、そもそもアプリが動かない（TLS も使えない）
    getrandom::fill(&mut bytes).expect("OS の乱数を読めません");
    bytes
}

fn random_text() -> String {
    URL_SAFE_NO_PAD.encode(random_bytes::<32>())
}

/// UUID の版 4 を `urn:uuid:…` の形で。
fn new_host_id() -> String {
    let mut b = random_bytes::<16>();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "urn:uuid:{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// PKCE の `code_challenge`（S256）。
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// フォームや URL の問い合わせの形にする。
pub fn form_encode(pairs: &[(&str, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = |b: u8| (b as char).to_digit(16);
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(high), Some(low)) => {
                        out.push((high * 16 + low) as u8);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            other => out.push(other),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| match part.split_once('=') {
            Some((k, v)) => (decode(k), decode(v)),
            None => (decode(part), String::new()),
        })
        .collect()
}

/// JWT の中身（2 つ目の部分）を読む。署名は見ない（[`check_id_token`] を参照）。
fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// ID トークンを確かめて、メールアドレスを返す。
///
/// トークンはトークンの窓口から TLS で直接受け取ったものなので、署名の代わりに TLS の
/// サーバーの確認で発行元を確かめる（OpenID Connect Core 1.0 の 3.1.3.7 の 6）。
/// 発行元・宛先・期限・nonce は、ここで確かめる。
fn check_id_token(token: &str, client_id: &str, nonce: &str) -> Result<Option<String>, String> {
    let claims = jwt_claims(token).ok_or("ChatGPT の ID トークンを読めませんでした")?;
    if claims.get("iss").and_then(Value::as_str) != Some(ISSUER) {
        return Err("ChatGPT の ID トークンの発行元が正しくありません".into());
    }
    let audience_ok = match claims.get("aud") {
        Some(Value::String(aud)) => aud == client_id,
        Some(Value::Array(list)) => list.iter().any(|a| a.as_str() == Some(client_id)),
        _ => false,
    };
    if !audience_ok {
        return Err("ChatGPT の ID トークンの宛先が、このアプリではありません".into());
    }
    if claims.get("nonce").and_then(Value::as_str) != Some(nonce) {
        return Err("ChatGPT の ID トークンが、このサインインのものではありません".into());
    }
    if claims
        .get("exp")
        .and_then(Value::as_u64)
        .is_some_and(|exp| exp < now())
    {
        return Err("ChatGPT の ID トークンの期限が切れています".into());
    }
    Ok(claims
        .get("email")
        .and_then(Value::as_str)
        .map(str::to_owned))
}

fn oauth_error(body: &Value) -> (String, String) {
    let code = body
        .get("error")
        .and_then(|e| e.as_str().or_else(|| e.get("code").and_then(Value::as_str)))
        .unwrap_or_default()
        .to_owned();
    let detail = body
        .get("error_description")
        .and_then(Value::as_str)
        .or_else(|| body.pointer("/error/message").and_then(Value::as_str))
        .or_else(|| body.get("detail").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned();
    (code, detail)
}

/// リフレッシュトークンがもう使えないことを示すエラー。サインインし直す。
const DEAD_REFRESH: [&str; 6] = [
    "invalid_grant",
    "invalid_refresh_token",
    "token_expired",
    "refresh_token_expired",
    "refresh_token_invalidated",
    "refresh_token_reused",
];

/// メモリの中だけに持つアクセストークン（client_id → トークンと切れる時刻）。
static TOKENS: Mutex<Option<HashMap<String, (String, u64)>>> = Mutex::new(None);
/// 更新は 1 つずつ（リフレッシュトークンは 1 回しか使えない）。
static REFRESH: Mutex<()> = Mutex::new(());

fn tokens() -> MutexGuard<'static, Option<HashMap<String, (String, u64)>>> {
    TOKENS.lock().unwrap_or_else(|e| e.into_inner())
}

fn remember(client_id: &str, access: &str, expires_in: u64) {
    tokens().get_or_insert_with(HashMap::new).insert(
        client_id.to_owned(),
        (access.to_owned(), now() + expires_in),
    );
}

fn forget(client_id: &str) {
    if let Some(map) = tokens().as_mut() {
        map.remove(client_id);
    }
}

fn cached(client_id: &str) -> Option<String> {
    tokens()
        .as_ref()?
        .get(client_id)
        .filter(|(_, expires)| *expires > now() + REFRESH_MARGIN)
        .map(|(token, _)| token.clone())
}

/// 使えるアクセストークンを返す。切れそうなら、リフレッシュトークンで更新して保存し直す。
pub fn access_token(store: &dyn KeyStore, form: &FormFn) -> Result<String, String> {
    let _one = REFRESH.lock().unwrap_or_else(|e| e.into_inner());
    // 更新したリフレッシュトークンはキーチェーンにあるので、毎回そこから読む
    let mut account = Account::load(store)?.filter(Account::signed_in).ok_or(
        "ChatGPT にサインインしていません。設定の「ChatGPT アカウント」でサインインしてください。",
    )?;
    let (Some(client_id), Some(refresh)) =
        (account.client_id.clone(), account.refresh_token.clone())
    else {
        return Err(SIGNED_OUT.into());
    };
    if let Some(token) = cached(&client_id) {
        return Ok(token);
    }
    let (status, body) = form(
        TOKEN_URL,
        &[
            ("grant_type", "refresh_token".into()),
            ("client_id", client_id.clone()),
            ("refresh_token", refresh),
            ("resource", RESOURCE.into()),
        ],
    )
    .map_err(|e| format!("ChatGPT のトークンを更新できませんでした: {e}"))?;
    if !(200..300).contains(&status) {
        let (code, detail) = oauth_error(&body);
        if DEAD_REFRESH.contains(&code.as_str()) || code == "invalid_client" {
            // もう使えないトークンは消す。登録（client_id）が使えないときは、登録からやり直す
            account.refresh_token = None;
            if code == "invalid_client" {
                account.client_id = None;
            }
            forget(&client_id);
            account.save(store)?;
            return Err(SIGNED_OUT.into());
        }
        return Err(format!(
            "ChatGPT のトークンを更新できませんでした（{status}）。{code} {detail}"
        )
        .trim_end()
        .to_owned());
    }
    let access = body
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or("ChatGPT のトークンの更新の返事に access_token がありません")?
        .to_owned();
    let expires_in = body
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(3600);
    if let Some(next) = body.get("refresh_token").and_then(Value::as_str) {
        account.refresh_token = Some(next.to_owned());
        account.save(store)?;
    }
    remember(&client_id, &access, expires_in);
    Ok(access)
}

/// ChatGPT のプランでの送り先。
pub fn endpoint(access_token: &str) -> Endpoint {
    Endpoint {
        wire: Wire::Responses,
        url: RESPONSES_URL.into(),
        headers: vec![("authorization", format!("Bearer {access_token}"))],
        fallbacks: false,
        prefix_ttl: None,
        plan: true,
    }
}

/// ChatGPT のプランで使えるモデル（ID と表示名）。
pub fn models(access_token: &str, get: &GetFn) -> Result<Vec<(String, Option<String>)>, String> {
    let text = get(
        MODELS_URL,
        &[("authorization", format!("Bearer {access_token}"))],
    )?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| format!("モデルの一覧が JSON として読めません: {e}"))?;
    parse_models(&value)
}

fn parse_models(value: &Value) -> Result<Vec<(String, Option<String>)>, String> {
    let list = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or("モデルの一覧に models がありません")?;
    Ok(list
        .iter()
        .filter(|m| m.get("visibility").and_then(Value::as_str) == Some("list"))
        .filter_map(|m| {
            Some((
                m.get("slug")?.as_str()?.to_owned(),
                m.get("display_name")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            ))
        })
        .collect())
}

/// ChatGPT のプランの呼び出しが返したエラーの `code` を、読める文にする。知らない `code` なら `None`。
pub fn error_text(code: &str, param: Option<&str>) -> Option<String> {
    Some(match code {
        "subscription_sharing_usage_limit_exceeded" => {
            format!("ChatGPT のプランの利用上限に達しました。使用量は {USAGE_URL} で確認できます。")
        }
        "subscription_sharing_user_not_eligible" => {
            "この ChatGPT アカウントのプランでは、ほかのアプリから ChatGPT を使えません。".into()
        }
        "subscription_sharing_usage_unavailable" | "subscription_sharing_user_unavailable" => {
            "ChatGPT のプランが一時的に使えません。少し待ってから送り直してください。".into()
        }
        "subscription_sharing_unsupported_capability" => format!(
            "ChatGPT のプランでは使えない指定があります（{}）。",
            param.unwrap_or("不明")
        ),
        "subscription_sharing_invalid_user" => SIGNED_OUT.into(),
        "subscription_sharing_route_not_supported"
        | "chatpass_v2_scope_not_authorized"
        | "chatpass_v2_invalid_authorization_context" => {
            format!("ChatGPT のプランでの呼び出しが受け付けられませんでした（{code}）。")
        }
        _ => return None,
    })
}

/// ブラウザでのサインインの途中。[`SignIn::begin`] で戻り先を開けてから、ブラウザで [`SignIn::url`] を開く。
pub struct SignIn {
    listener: TcpListener,
    account: Account,
    redirect_uri: String,
    state: String,
    nonce: String,
    verifier: String,
    /// ブラウザで開く URL。
    pub url: String,
}

/// ブラウザから戻ってきた中身。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Callback {
    code: String,
    client_id: Option<String>,
}

impl SignIn {
    /// 戻り先を開けて、サインインの URL を作る。保存済みの登録があれば、それを使う。
    pub fn begin(store: &dyn KeyStore) -> Result<Self, String> {
        let account = match Account::load(store)? {
            Some(account) => account,
            None => {
                // 同じ PC では、いつも同じ ID を送る
                let account = Account::for_this_pc();
                account.save(store)?;
                account
            }
        };
        let listener = TcpListener::bind(("127.0.0.1", PORT))
            .or_else(|_| TcpListener::bind(("127.0.0.1", 0)))
            .map_err(|e| format!("サインインの戻り先を開けませんでした: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("サインインの戻り先を開けませんでした: {e}"))?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
        let (state, nonce, verifier) = (random_text(), random_text(), random_text());
        let mut params = vec![
            (
                "client_id",
                account
                    .client_id
                    .clone()
                    .unwrap_or_else(|| REGISTER_CLIENT.into()),
            ),
            ("ext_agent_host_id", account.host_id.clone()),
            ("response_type", "code".into()),
            ("redirect_uri", redirect_uri.clone()),
            ("scope", SCOPE.into()),
            ("resource", RESOURCE.into()),
            ("state", state.clone()),
            ("nonce", nonce.clone()),
            ("code_challenge", challenge(&verifier)),
            ("code_challenge_method", "S256".into()),
        ];
        match &account.client_id {
            None => params.push(("agent_name_hint", AGENT_NAME.into())),
            Some(_) => {
                if let Some(email) = &account.email {
                    params.push(("login_hint", email.clone()));
                }
            }
        }
        let url = format!("{AUTHORIZE_URL}?{}", form_encode(&params));
        Ok(Self {
            listener,
            account,
            redirect_uri,
            state,
            nonce,
            verifier,
            url,
        })
    }

    /// ブラウザが戻ってくるのを待つ。`cancel` が立つか、`timeout` を過ぎたら止める。
    pub fn wait(&self, cancel: &AtomicBool, timeout: Duration) -> Result<Callback, String> {
        self.listener
            .set_nonblocking(true)
            .map_err(|e| format!("サインインの戻り先を開けませんでした: {e}"))?;
        let until = Instant::now() + timeout;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(CANCELED.into());
            }
            if Instant::now() > until {
                return Err("ChatGPT のサインインが時間内に終わりませんでした。もう一度サインインしてください。".into());
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(result) = self.answer(stream) {
                        return result;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(format!("サインインの戻り先で受け取れませんでした: {e}")),
            }
        }
    }

    /// 1 つのリクエストに答える。戻り先でなければ `None`（待ち続ける）。
    fn answer(&self, mut stream: TcpStream) -> Option<Result<Callback, String>> {
        // 受け取ったつなぎは、ふつうの（待つ）読み書きにする
        stream.set_nonblocking(false).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).ok()?;
        let target = line.split_whitespace().nth(1).unwrap_or_default();
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        if path != CALLBACK_PATH {
            // ブラウザが favicon などを取りに来ても、待ち続ける。返せなくても困らない
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            );
            return None;
        }
        let query = parse_query(query);
        let result = self.read_callback(&query);
        let page = match &result {
            Ok(_) => {
                "サインインしました。このタブを閉じて、Malakt Mahoz に戻ってください。".to_owned()
            }
            Err(e) => e.clone(),
        };
        let html = format!(
            "<!doctype html><meta charset=\"utf-8\"><title>Malakt Mahoz</title>\
             <body style=\"font-family:sans-serif;padding:48px\"><p>{}</p></body>",
            page.replace('&', "&amp;").replace('<', "&lt;")
        );
        // ブラウザに出す文が届かなくても、サインインの結果は変わらない
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{html}",
            html.len()
        );
        Some(result)
    }

    fn read_callback(&self, query: &HashMap<String, String>) -> Result<Callback, String> {
        // state が違えば、ほかのだれかの戻りなので何も使わない
        if query.get("state") != Some(&self.state) {
            return Err("サインインの戻りが、このサインインのものではありません。もう一度サインインしてください。".into());
        }
        if let Some(error) = query.get("error") {
            return Err(if error == "access_denied" {
                "ChatGPT でのサインインが許可されませんでした。".into()
            } else {
                format!(
                    "ChatGPT でサインインできませんでした（{error}）。{}",
                    query.get("error_description").cloned().unwrap_or_default()
                )
                .trim_end()
                .to_owned()
            });
        }
        let code = query
            .get("code")
            .filter(|c| !c.is_empty())
            .ok_or("サインインの戻りに code がありません")?;
        Ok(Callback {
            code: code.clone(),
            client_id: query.get("client_id").filter(|c| !c.is_empty()).cloned(),
        })
    }

    /// 受け取った code をトークンに替えて、アカウントを保存する。
    pub fn finish(
        self,
        callback: Callback,
        store: &dyn KeyStore,
        form: &FormFn,
    ) -> Result<Account, String> {
        let client_id = match (&self.account.client_id, callback.client_id) {
            (None, Some(issued)) => issued,
            (None, None) => {
                return Err(
                    "ChatGPT からアプリの登録（client_id）が返ってきませんでした。もう一度サインインしてください。"
                        .into(),
                )
            }
            (Some(saved), Some(issued)) if *saved != issued => {
                return Err("ChatGPT から、保存している登録と違う client_id が返ってきました。もう一度サインインしてください。".into())
            }
            (Some(saved), _) => saved.clone(),
        };
        let (status, body) = form(
            TOKEN_URL,
            &[
                ("grant_type", "authorization_code".into()),
                ("client_id", client_id.clone()),
                ("code", callback.code),
                ("code_verifier", self.verifier.clone()),
                ("redirect_uri", self.redirect_uri.clone()),
                ("resource", RESOURCE.into()),
            ],
        )
        .map_err(|e| format!("ChatGPT のトークンを受け取れませんでした: {e}"))?;
        if !(200..300).contains(&status) {
            let (code, detail) = oauth_error(&body);
            return Err(format!(
                "ChatGPT のトークンを受け取れませんでした（{status}）。もう一度サインインしてください。{code} {detail}"
            )
            .trim_end()
            .to_owned());
        }
        let text = |key: &str| body.get(key).and_then(Value::as_str).map(str::to_owned);
        let scope = text("scope").unwrap_or_default();
        if !scope.split_whitespace().any(|s| s == PLAN_SCOPE) {
            return Err(
                "ChatGPT のプランを使う許可がありませんでした。もう一度サインインして、許可してください。"
                    .into(),
            );
        }
        let access = text("access_token").ok_or("ChatGPT の返事に access_token がありません")?;
        let refresh = text("refresh_token").ok_or("ChatGPT の返事に refresh_token がありません")?;
        let email = match text("id_token") {
            Some(id_token) => check_id_token(&id_token, &client_id, &self.nonce)?,
            None => None,
        };
        let account = Account {
            host_id: self.account.host_id.clone(),
            client_id: Some(client_id.clone()),
            email: email.or(self.account.email.clone()),
            refresh_token: Some(refresh),
        };
        account.save(store)?;
        let expires_in = body
            .get("expires_in")
            .and_then(Value::as_u64)
            .unwrap_or(3600);
        remember(&client_id, &access, expires_in);
        Ok(account)
    }
}

/// サインアウトする。ChatGPT の側でトークンを無効にしてから、キーチェーンのトークンを消す。
/// 登録（client_id）とこの PC の ID は、次のサインインのために残す。
///
/// ChatGPT の側で無効にできなかったときも、手元のトークンは消して、そのことを `Err` で返す。
pub fn sign_out(store: &dyn KeyStore, form: &FormFn, get: &GetFn) -> Result<(), String> {
    let _one = REFRESH.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut account) = Account::load(store)? else {
        return Ok(());
    };
    let (Some(client_id), Some(refresh)) =
        (account.client_id.clone(), account.refresh_token.take())
    else {
        return Ok(());
    };
    let revoked = revoke(&client_id, &refresh, form, get);
    forget(&client_id);
    account.save(store)?;
    revoked.map_err(|e| {
        format!("サインアウトしました。ChatGPT の側で接続を外せなかったので、ChatGPT の設定から Malakt Mahoz の接続を外してください。{e}")
    })
}

fn revoke(client_id: &str, refresh: &str, form: &FormFn, get: &GetFn) -> Result<(), String> {
    let discovery: Value = serde_json::from_str(&get(DISCOVERY_URL, &[])?)
        .map_err(|e| format!("OpenAI の設定が JSON として読めません: {e}"))?;
    let url = discovery
        .get("revocation_endpoint")
        .and_then(Value::as_str)
        .ok_or("OpenAI の設定に revocation_endpoint がありません")?;
    let (status, body) = form(
        url,
        &[
            ("token", refresh.to_owned()),
            ("token_type_hint", "refresh_token".into()),
            ("client_id", client_id.to_owned()),
        ],
    )?;
    if (200..300).contains(&status) {
        return Ok(());
    }
    let (code, detail) = oauth_error(&body);
    Err(format!("（{status} {code} {detail}）").replace(" ）", "）"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::MemoryKeys;
    use std::io::Read;
    use std::sync::Arc;

    fn id_token(claims: Value) -> String {
        format!(
            "e30.{}.sig",
            URL_SAFE_NO_PAD.encode(claims.to_string().as_bytes())
        )
    }

    #[test]
    fn query_strings_round_trip() {
        let pairs = [
            ("scope", "openid profile".to_owned()),
            ("redirect_uri", "http://127.0.0.1:1455/auth/callback".into()),
            ("名前", "あ&い=う".into()),
        ];
        let text = form_encode(&pairs);
        assert!(text.contains("scope=openid%20profile"));
        assert!(text.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback"));
        let back = parse_query(&text);
        for (k, v) in pairs {
            assert_eq!(back.get(k), Some(&v));
        }
        assert_eq!(parse_query("a=b+c&d=%")["a"], "b c");
        assert_eq!(parse_query("d=%")["d"], "%");
        assert_eq!(parse_query("d=%4")["d"], "%4");
    }

    #[test]
    fn pkce_uses_s256() {
        // base64url(SHA-256(verifier))、= なし
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r7wW1gXrFWFrcM"),
            "fdAuNR2jo16SMGm2HQfy2Bgso2xrfPCd2eFpk_94Iso"
        );
        let host = new_host_id();
        assert!(host.starts_with("urn:uuid:"));
        assert_eq!(host.len(), "urn:uuid:".len() + 36);
        assert_eq!(&host[9 + 14..9 + 15], "4");
        assert_ne!(new_host_id(), host);
    }

    #[test]
    fn the_secret_holds_no_tokens_and_is_not_an_api_key() {
        let account = Account {
            host_id: "urn:uuid:x".into(),
            client_id: Some("oaiapp_1".into()),
            email: Some("a@example.com".into()),
            refresh_token: Some("rt_secret".into()),
        };
        let secret = account.secret();
        assert!(!secret.contains("rt_secret"));
        let back = Account::from_secret(&secret).unwrap();
        assert_eq!(back.email.as_deref(), Some("a@example.com"));
        assert_eq!(back.client_id.as_deref(), Some("oaiapp_1"));
        assert_eq!(Account::from_secret("sk-proj-abc"), None);
        assert_eq!(Account::from_secret("{\"x\":1}"), None);
    }

    #[test]
    fn id_tokens_are_checked() {
        let good = json!({"iss": ISSUER, "aud": "oaiapp_1", "nonce": "n", "exp": now() + 600, "email": "a@example.com"});
        assert_eq!(
            check_id_token(&id_token(good.clone()), "oaiapp_1", "n"),
            Ok(Some("a@example.com".into()))
        );
        let mut list = good.clone();
        list["aud"] = json!(["x", "oaiapp_1"]);
        assert!(check_id_token(&id_token(list), "oaiapp_1", "n").is_ok());
        assert!(check_id_token(&id_token(good.clone()), "oaiapp_2", "n").is_err());
        assert!(check_id_token(&id_token(good.clone()), "oaiapp_1", "m").is_err());
        let mut issuer = good.clone();
        issuer["iss"] = json!("https://evil.example");
        assert!(check_id_token(&id_token(issuer), "oaiapp_1", "n").is_err());
        let mut old = good;
        old["exp"] = json!(1);
        assert!(check_id_token(&id_token(old), "oaiapp_1", "n").is_err());
        assert!(check_id_token("nope", "oaiapp_1", "n").is_err());
    }

    #[test]
    fn only_listed_models_are_offered() {
        let models = parse_models(&json!({"models": [
            {"slug": "gpt-6.2-sol", "display_name": "GPT-6.2 Sol", "visibility": "list"},
            {"slug": "hidden", "visibility": "hide"},
            {"slug": "gpt-6-luna", "visibility": "list"},
        ]}))
        .unwrap();
        assert_eq!(
            models,
            [
                ("gpt-6.2-sol".to_owned(), Some("GPT-6.2 Sol".to_owned())),
                ("gpt-6-luna".to_owned(), None)
            ]
        );
        assert!(parse_models(&json!({"data": []})).is_err());
    }

    #[test]
    fn plan_errors_are_readable() {
        assert!(
            error_text("subscription_sharing_usage_limit_exceeded", None)
                .unwrap()
                .contains(USAGE_URL)
        );
        assert!(error_text(
            "subscription_sharing_unsupported_capability",
            Some("temperature")
        )
        .unwrap()
        .contains("temperature"));
        assert_eq!(error_text("rate_limit_exceeded", None), None);
    }

    /// 送ったフォーム（URL と、名前と値）。
    type Sent = (String, Vec<(String, String)>);

    /// フォームの POST の台本。受け取ったものを残す。
    struct Script {
        replies: Mutex<Vec<(u16, Value)>>,
        sent: Mutex<Vec<Sent>>,
    }

    impl Script {
        fn new(replies: Vec<(u16, Value)>) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(replies),
                sent: Mutex::new(Vec::new()),
            })
        }

        fn form(self: &Arc<Self>) -> Box<FormFn> {
            let me = self.clone();
            Box::new(move |url, pairs| {
                me.sent.lock().unwrap().push((
                    url.to_owned(),
                    pairs
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.clone()))
                        .collect(),
                ));
                Ok(me.replies.lock().unwrap().remove(0))
            })
        }

        fn sent(&self, index: usize, key: &str) -> Option<String> {
            self.sent.lock().unwrap()[index]
                .1
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        }
    }

    fn browse(url: &str) -> String {
        let rest = url.strip_prefix("http://").unwrap();
        let (host, path) = rest.split_once('/').unwrap();
        let mut stream = TcpStream::connect(host).unwrap();
        write!(stream, "GET /{path} HTTP/1.1\r\nhost: {host}\r\n\r\n").unwrap();
        let mut page = String::new();
        stream.read_to_string(&mut page).unwrap();
        page
    }

    fn redirect_of(sign_in: &SignIn) -> String {
        let query = parse_query(sign_in.url.split_once('?').unwrap().1);
        query["redirect_uri"].clone()
    }

    fn query_of(sign_in: &SignIn) -> HashMap<String, String> {
        parse_query(sign_in.url.split_once('?').unwrap().1)
    }

    #[test]
    fn signing_in_registers_once_and_then_reuses_the_issued_client() {
        let keys = MemoryKeys::default();
        let sign_in = SignIn::begin(&keys).unwrap();
        let query = query_of(&sign_in);
        assert!(sign_in.url.starts_with(AUTHORIZE_URL));
        assert_eq!(query["client_id"], "dynamic_agent_client");
        assert_eq!(query["agent_name_hint"], "Malakt Mahoz");
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["resource"], RESOURCE);
        assert!(query["scope"].contains("chatgpt.tokens.use.direct"));
        assert!(query["redirect_uri"].starts_with("http://127.0.0.1:"));
        let host = query["ext_agent_host_id"].clone();

        // ブラウザが戻ってくる（ほかのパスは無視して待ち続ける）
        let redirect = redirect_of(&sign_in);
        let state = query["state"].clone();
        let nonce = query["nonce"].clone();
        let browser = std::thread::spawn(move || {
            let base = redirect.trim_end_matches(CALLBACK_PATH).to_owned();
            assert!(browse(&format!("{base}/favicon.ico")).contains("404"));
            browse(&format!(
                "{redirect}?code=c1&state={state}&client_id=oaiapp_9&scope=openid"
            ))
        });
        let callback = sign_in
            .wait(&AtomicBool::new(false), Duration::from_secs(10))
            .unwrap();
        assert!(browser.join().unwrap().contains("サインインしました"));
        let script = Script::new(vec![(
            200,
            json!({
                "access_token": "at_1", "refresh_token": "rt_1", "expires_in": 3600,
                "scope": "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
                "id_token": id_token(json!({"iss": ISSUER, "aud": "oaiapp_9", "nonce": nonce, "email": "a@example.com"})),
            }),
        )]);
        let verifier = sign_in.verifier.clone();
        let account = sign_in.finish(callback, &keys, &*script.form()).unwrap();
        assert_eq!(account.email.as_deref(), Some("a@example.com"));
        assert_eq!(script.sent(0, "client_id").as_deref(), Some("oaiapp_9"));
        assert_eq!(script.sent(0, "code").as_deref(), Some("c1"));
        assert_eq!(script.sent(0, "code_verifier"), Some(verifier));
        let saved = Account::load(&keys).unwrap().unwrap();
        assert!(saved.signed_in());
        assert_eq!(saved.host_id, host);
        // アクセストークンはキーチェーンに入れない
        assert!(!keys.read(ENTRY, "").unwrap().unwrap().contains("at_1"));
        assert_eq!(
            access_token(&keys, &*Script::new(vec![]).form()).unwrap(),
            "at_1"
        );

        // 2 回目は、発行された client_id と同じ PC の ID で始める
        let again = SignIn::begin(&keys).unwrap();
        let query = query_of(&again);
        assert_eq!(query["client_id"], "oaiapp_9");
        assert_eq!(query["ext_agent_host_id"], host);
        assert!(!query.contains_key("agent_name_hint"));
        assert_eq!(query["login_hint"], "a@example.com");
        forget("oaiapp_9");
    }

    #[test]
    fn a_wrong_state_or_a_refusal_stops_the_sign_in() {
        let keys = MemoryKeys::default();
        let sign_in = SignIn::begin(&keys).unwrap();
        let redirect = redirect_of(&sign_in);
        let browser = std::thread::spawn(move || browse(&format!("{redirect}?code=c&state=other")));
        let error = sign_in
            .wait(&AtomicBool::new(false), Duration::from_secs(10))
            .unwrap_err();
        assert!(error.contains("このサインインのものではありません"));
        browser.join().unwrap();

        let sign_in = SignIn::begin(&keys).unwrap();
        let redirect = redirect_of(&sign_in);
        let state = query_of(&sign_in)["state"].clone();
        let browser = std::thread::spawn(move || {
            browse(&format!("{redirect}?error=access_denied&state={state}"))
        });
        let error = sign_in
            .wait(&AtomicBool::new(false), Duration::from_secs(10))
            .unwrap_err();
        assert!(error.contains("許可されませんでした"));
        assert!(browser.join().unwrap().contains("許可されませんでした"));

        let sign_in = SignIn::begin(&keys).unwrap();
        assert!(sign_in
            .wait(&AtomicBool::new(true), Duration::from_secs(10))
            .unwrap_err()
            .contains("キャンセル"));
    }

    fn signed_in(keys: &MemoryKeys, client: &str) {
        Account {
            host_id: "urn:uuid:h".into(),
            client_id: Some(client.into()),
            email: Some("a@example.com".into()),
            refresh_token: Some("rt_old".into()),
        }
        .save(keys)
        .unwrap();
    }

    #[test]
    fn tokens_are_refreshed_and_the_new_refresh_token_is_kept() {
        let keys = MemoryKeys::default();
        signed_in(&keys, "oaiapp_refresh");
        let script = Script::new(vec![(
            200,
            json!({"access_token": "at_new", "refresh_token": "rt_new", "expires_in": 3600}),
        )]);
        assert_eq!(access_token(&keys, &*script.form()).unwrap(), "at_new");
        assert_eq!(
            script.sent(0, "grant_type").as_deref(),
            Some("refresh_token")
        );
        assert_eq!(script.sent(0, "refresh_token").as_deref(), Some("rt_old"));
        assert_eq!(script.sent(0, "resource").as_deref(), Some(RESOURCE));
        assert_eq!(
            Account::load(&keys)
                .unwrap()
                .unwrap()
                .refresh_token
                .as_deref(),
            Some("rt_new")
        );
        // 切れるまでは、更新しない
        assert_eq!(
            access_token(&keys, &*Script::new(vec![]).form()).unwrap(),
            "at_new"
        );
        forget("oaiapp_refresh");
    }

    #[test]
    fn a_dead_refresh_token_signs_out_but_keeps_the_registration() {
        let keys = MemoryKeys::default();
        signed_in(&keys, "oaiapp_dead");
        let script = Script::new(vec![(400, json!({"error": "refresh_token_reused"}))]);
        assert_eq!(
            access_token(&keys, &*script.form()).unwrap_err(),
            SIGNED_OUT
        );
        let account = Account::load(&keys).unwrap().unwrap();
        assert!(!account.signed_in());
        assert_eq!(account.client_id.as_deref(), Some("oaiapp_dead"));

        // 一時的な失敗では、サインインを消さない
        signed_in(&keys, "oaiapp_dead");
        let script = Script::new(vec![(503, json!({"detail": "busy"}))]);
        assert!(access_token(&keys, &*script.form())
            .unwrap_err()
            .contains("503"));
        assert!(Account::load(&keys).unwrap().unwrap().signed_in());
    }

    #[test]
    fn signing_out_revokes_the_refresh_token() {
        let keys = MemoryKeys::default();
        signed_in(&keys, "oaiapp_out");
        let script = Script::new(vec![(200, Value::Null)]);
        let get: Box<GetFn> = Box::new(|url, _| {
            assert_eq!(url, DISCOVERY_URL);
            Ok(r#"{"revocation_endpoint": "https://auth.openai.com/api/accounts/revoke"}"#.into())
        });
        sign_out(&keys, &*script.form(), &*get).unwrap();
        assert_eq!(
            script.sent.lock().unwrap()[0].0,
            "https://auth.openai.com/api/accounts/revoke"
        );
        assert_eq!(script.sent(0, "token").as_deref(), Some("rt_old"));
        assert_eq!(
            script.sent(0, "token_type_hint").as_deref(),
            Some("refresh_token")
        );
        let account = Account::load(&keys).unwrap().unwrap();
        assert!(!account.signed_in());
        assert_eq!(account.client_id.as_deref(), Some("oaiapp_out"));

        // ChatGPT の側で外せなくても、手元のトークンは消す
        signed_in(&keys, "oaiapp_out");
        let broken: Box<GetFn> = Box::new(|_, _| Err("つながりません".into()));
        let error = sign_out(&keys, &*Script::new(vec![]).form(), &*broken).unwrap_err();
        assert!(error.contains("つながりません"));
        assert!(!Account::load(&keys).unwrap().unwrap().signed_in());
    }
}
