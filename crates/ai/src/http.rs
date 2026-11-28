//! HTTPS の呼び出し。呼んだスレッドを止めるので、画面のスレッドからは呼ばない。

use std::time::Duration;

use serde_json::Value;

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
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| error_message(&v))
        .unwrap_or_else(|| body.chars().take(300).collect());
    let hint = match status {
        401 | 403 => "API キーを確認してください。",
        404 => "モデルの名前を確認してください。",
        429 => "利用の上限に達しています。少し待ってから送り直してください。",
        _ => "",
    };
    format!("サーバーがエラーを返しました（{status}）。{hint}{detail}")
}

/// JSON を POST して、JSON の返事を受け取る。
pub fn post_json(url: &str, headers: &[(&str, String)], body: &Value) -> Result<Value, String> {
    let mut request = agent().post(url);
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let mut response = request
        .send_json(body)
        .map_err(|e| format!("{url} に接続できませんでした: {e}"))?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("返事を読めませんでした: {e}"))?;
    if !(200..300).contains(&status) {
        return Err(describe(status, &text));
    }
    serde_json::from_str(&text).map_err(|e| format!("返事が JSON として読めません: {e}"))
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
