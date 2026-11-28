//! HTTPS の呼び出し。呼んだスレッドを止めるので、画面のスレッドからは呼ばない。

use std::time::Duration;

use serde_json::{json, Value};

use crate::wire::error_message;

/// 深く考えるモデルは、返事までに何分もかかることがある。
const TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// 手元のサーバーを探すときに待つ長さ。動いていないポートは、すぐに断られる。
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

fn agent() -> ureq::Agent {
    agent_with(TIMEOUT)
}

fn agent_with(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        // エラーの本文を読んで、理由を画面に出す
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        // OpenCode は、クライアントが自分の名前を名乗ることを求めている
        .user_agent(concat!("malakt-mahoz/", env!("CARGO_PKG_VERSION")))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                // 会社のプロキシなどの証明書も通るように、OS の証明書を使う
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into()
}

fn describe(status: u16, body: &str) -> String {
    let value = serde_json::from_str::<Value>(body).ok();
    // ChatGPT のプランの呼び出しのエラー（`error.code`）は、それに合った文にする
    if let Some(text) = value.as_ref().and_then(plan_error) {
        return text;
    }
    let detail = value
        .as_ref()
        .and_then(|v| {
            error_message(v).or_else(|| v.get("detail").and_then(Value::as_str).map(str::to_owned))
        })
        .unwrap_or_else(|| body.chars().take(300).collect());
    let hint = match status {
        401 | 403 => "API キーを確認してください。",
        404 => "モデルの名前を確認してください。",
        429 => "利用の上限に達しています。少し待ってから送り直してください。",
        _ => "",
    };
    format!("サーバーがエラーを返しました（{status}）。{hint}{detail}")
}

/// `{"error": {"code": …, "param": …}}` が ChatGPT のプランのエラーなら、その文。
fn plan_error(value: &Value) -> Option<String> {
    let error = value.get("error")?;
    crate::chatgpt::error_text(
        error.get("code").and_then(Value::as_str)?,
        error.get("param").and_then(Value::as_str),
    )
}

/// JSON を POST して、JSON の返事を受け取る。
///
/// 返事がストリーム（`text/event-stream`。Responses API の `stream: true`）なら、
/// 終わりのイベント（`response.completed`）まで読み、その `response` を返す。
pub fn post_json(url: &str, headers: &[(&str, String)], body: &Value) -> Result<Value, String> {
    let mut request = agent().post(url);
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let mut response = request
        .send_json(body)
        .map_err(|e| format!("{url} に接続できませんでした: {e}"))?;
    let status = response.status().as_u16();
    let stream = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"));
    if stream && (200..300).contains(&status) {
        let reader = std::io::BufReader::new(response.body_mut().as_reader());
        return read_events(reader);
    }
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("返事を読めませんでした: {e}"))?;
    if !(200..300).contains(&status) {
        return Err(describe(status, &text));
    }
    serde_json::from_str(&text).map_err(|e| format!("返事が JSON として読めません: {e}"))
}

/// Responses API のストリームを、終わりのイベントまで読む。
fn read_events(reader: impl std::io::BufRead) -> Result<Value, String> {
    let mut data = String::new();
    // 終わりのイベントに output が入っていなかったときのために、出てきた項目を残す
    let mut items: Vec<Value> = Vec::new();
    let mut handle = |data: &str| -> Option<Result<Value, String>> {
        let event: Value = match serde_json::from_str(data) {
            Ok(event) => event,
            Err(e) => return Some(Err(format!("返事のイベントが JSON として読めません: {e}"))),
        };
        match event.get("type").and_then(Value::as_str) {
            Some("response.output_item.done") => {
                if let Some(item) = event.get("item") {
                    items.push(item.clone());
                }
                None
            }
            Some("response.completed") | Some("response.incomplete") => {
                let mut response = event.get("response").cloned().unwrap_or(Value::Null);
                let empty = response
                    .get("output")
                    .and_then(Value::as_array)
                    .is_none_or(|o| o.is_empty());
                if empty && !items.is_empty() {
                    response["output"] = Value::Array(std::mem::take(&mut items));
                }
                Some(Ok(response))
            }
            Some("response.failed") => {
                let response = event.get("response").cloned().unwrap_or(Value::Null);
                Some(Err(plan_error(&response).unwrap_or_else(|| {
                    format!(
                        "サーバーが返事の途中でエラーを返しました。{}",
                        error_message(&response).unwrap_or_default()
                    )
                    .trim_end()
                    .to_owned()
                })))
            }
            Some("error") => Some(Err(plan_error(&json!({"error": event.clone()}))
                .or_else(|| plan_error(&event))
                .unwrap_or_else(|| {
                    format!(
                        "サーバーが返事の途中でエラーを返しました。{}",
                        event
                            .get("message")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                            .or_else(|| error_message(&event))
                            .unwrap_or_default()
                    )
                    .trim_end()
                    .to_owned()
                }))),
            _ => None,
        }
    };
    for line in reader.lines() {
        let line = line.map_err(|e| format!("返事を読めませんでした: {e}"))?;
        if line.is_empty() {
            if !data.is_empty() && data != "[DONE]" {
                if let Some(done) = handle(&data) {
                    return done;
                }
            }
            data.clear();
        } else if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    if !data.is_empty() && data != "[DONE]" {
        if let Some(done) = handle(&data) {
            return done;
        }
    }
    Err("返事が途中で切れました。もう一度送ってください。".into())
}

/// フォームを POST して、HTTP の状態と JSON の本文（空なら `Null`）を返す（[`crate::chatgpt::FormFn`]）。
/// `Err` は、つながらなかったときと、本文が読めなかったとき。
pub fn post_form(url: &str, pairs: &[(&str, String)]) -> Result<(u16, Value), String> {
    let mut response = agent()
        .post(url)
        .header("content-type", "application/x-www-form-urlencoded")
        .send(crate::chatgpt::form_encode(pairs))
        .map_err(|e| format!("{url} に接続できませんでした: {e}"))?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("返事を読めませんでした: {e}"))?;
    if text.trim().is_empty() {
        return Ok((status, Value::Null));
    }
    match serde_json::from_str(&text) {
        Ok(value) => Ok((status, value)),
        // エラーの本文が JSON でなければ、そのまま文として渡す
        Err(_) if !(200..300).contains(&status) => Ok((
            status,
            json!({"detail": text.chars().take(300).collect::<String>()}),
        )),
        Err(e) => Err(format!("返事が JSON として読めません: {e}")),
    }
}

/// 文字列を GET する。
pub fn get_text(url: &str, headers: &[(&str, String)]) -> Result<String, String> {
    let mut request = agent().get(url);
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let mut response = request
        .call()
        .map_err(|e| format!("{url} に接続できませんでした: {e}"))?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("返事を読めませんでした: {e}"))?;
    if !(200..300).contains(&status) {
        return Err(describe(status, &text));
    }
    Ok(text)
}

/// 手元のサーバーが答えるかを、短く待って確かめる。答えたら状態コードと本文を返す
/// （エラーの状態コードでも `Ok`）。`Err` は、つながらなかったとき。
pub fn probe(url: &str, headers: &[(&str, String)]) -> Result<(u16, String), String> {
    let mut request = agent_with(PROBE_TIMEOUT).get(url);
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let mut response = request
        .call()
        .map_err(|e| format!("{url} に接続できませんでした: {e}"))?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("返事を読めませんでした: {e}"))?;
    Ok((status, text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// 1 回だけ返事をする、手元の HTTP サーバー。受け取ったリクエストを返す。
    fn serve(status: &str, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/test", listener.local_addr().unwrap());
        let status = status.to_owned();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let n = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..n]);
                let text = String::from_utf8_lossy(&request);
                let Some(head_end) = text.find("\r\n\r\n") else {
                    continue;
                };
                let length = text
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if request.len() >= head_end + 4 + length {
                    break;
                }
            }
            write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            String::from_utf8(request).unwrap()
        });
        (url, handle)
    }

    #[test]
    fn json_is_posted_with_headers_and_the_reply_is_parsed() {
        let (url, server) = serve("200 OK", r#"{"ok":true}"#);
        let reply = post_json(
            &url,
            &[("x-api-key", "secret".into())],
            &serde_json::json!({"model": "m", "text": "こんにちは"}),
        )
        .unwrap();
        assert_eq!(reply["ok"], true);
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /v1/test HTTP/1.1"));
        assert!(request.to_ascii_lowercase().contains("x-api-key: secret"));
        assert!(request.contains("こんにちは"));
        assert!(request
            .to_ascii_lowercase()
            .contains("user-agent: malakt-mahoz/"));
    }

    #[test]
    fn error_statuses_carry_the_reason_and_a_hint() {
        let (url, server) = serve(
            "401 Unauthorized",
            r#"{"type":"error","error":{"message":"invalid x-api-key"}}"#,
        );
        let error = post_json(&url, &[], &serde_json::json!({})).unwrap_err();
        server.join().unwrap();
        assert!(error.contains("401"));
        assert!(error.contains("API キーを確認してください"));
        assert!(error.contains("invalid x-api-key"));

        let (url, server) = serve("404 Not Found", "nothing here");
        let error = get_text(&url, &[]).unwrap_err();
        server.join().unwrap();
        assert!(error.contains("404") && error.contains("nothing here"));
    }

    #[test]
    fn text_is_fetched_and_broken_json_is_reported() {
        let (url, server) = serve("200 OK", "plain");
        assert_eq!(
            get_text(&url, &[("accept", "text/plain".into())]).unwrap(),
            "plain"
        );
        assert!(server.join().unwrap().starts_with("GET "));
        let (url, server) = serve("200 OK", "plain");
        assert!(post_json(&url, &[], &serde_json::json!({}))
            .unwrap_err()
            .contains("JSON"));
        server.join().unwrap();
    }

    #[test]
    fn streams_are_read_to_the_completed_response() {
        let stream = "event: response.created\ndata: {\"type\":\"response.created\"}\n\n\
            data: {\"type\":\"response.output_text.delta\",\"delta\":\"は\"}\n\n\
            data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\"}]}}\n\n";
        let response = read_events(stream.as_bytes()).unwrap();
        assert_eq!(response["output"][0]["type"], "message");

        // 終わりのイベントに output がなければ、途中の項目を使う
        let stream = "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\"}}\n\n\
            data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n";
        let response = read_events(stream.as_bytes()).unwrap();
        assert_eq!(response["output"][0]["type"], "function_call");

        let failed = "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"subscription_sharing_usage_limit_exceeded\",\"message\":\"limit\"}}}\n\n";
        assert!(read_events(failed.as_bytes())
            .unwrap_err()
            .contains("利用上限"));
        assert!(
            read_events("data: {\"type\":\"response.created\"}\n\n".as_bytes())
                .unwrap_err()
                .contains("途中で切れました")
        );
    }

    #[test]
    fn plan_errors_before_the_stream_are_readable() {
        let error = describe(
            403,
            r#"{"error":{"code":"subscription_sharing_user_not_eligible","message":"no"}}"#,
        );
        assert!(error.contains("ChatGPT アカウントのプラン"));
        assert!(describe(503, r#"{"detail":"unavailable"}"#).contains("unavailable"));
    }

    #[test]
    fn forms_are_posted_and_empty_replies_read_as_null() {
        let (url, server) = serve("200 OK", "");
        let (status, body) = post_form(&url, &[("token", "a b".into())]).unwrap();
        assert_eq!((status, body), (200, Value::Null));
        let request = server.join().unwrap();
        assert!(request.ends_with("token=a%20b"));
        assert!(request
            .to_ascii_lowercase()
            .contains("content-type: application/x-www-form-urlencoded"));
        let (url, server) = serve("400 Bad Request", r#"{"error":"invalid_grant"}"#);
        let (status, body) = post_form(&url, &[]).unwrap();
        server.join().unwrap();
        assert_eq!(status, 400);
        assert_eq!(body["error"], "invalid_grant");
    }

    #[test]
    fn connection_failures_name_the_url() {
        // すぐ閉じたポートには、だれも待っていない
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let url = format!("http://127.0.0.1:{port}/");
        assert!(get_text(&url, &[]).unwrap_err().contains(&url));
    }
}
