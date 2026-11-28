//! Slack のリンクの検出と、中身を取りに行くコマンド。
//! Slack CLI の `api` コマンドで `conversations.replies` を呼ぶ。トークンは CLI が
//! 環境変数（`SLACK_USER_TOKEN` など）から読むので、このアプリは持たない。

use serde_json::Value;

use crate::shell::Spec;

/// 1 回のメッセージで取りに行くリンクの上限。
pub const MAX_LINKS: usize = 5;

/// 1 つのスレッドで受け取るメッセージの数。
const LIMIT: usize = 100;

const SCHEME: &str = "https://";
const DOMAIN: &str = ".slack.com";

/// WSL の中で動かすコマンドを入れる環境変数。引数にすると、`wsl.exe` を通る間に引用符が崩れる。
const ENV_RUN: &str = "MALAKT_MAHOZ_RUN";

/// Slack のメッセージへのリンク。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// `example.slack.com` のような、ワークスペースの名前
    pub host: String,
    pub channel: String,
    /// メッセージの時刻（`1700000000.123456`）
    pub ts: String,
    /// 返信へのリンクなら、スレッドの最初のメッセージの時刻
    pub thread_ts: Option<String>,
}

fn is_ts(s: &str) -> bool {
    let Some((secs, micros)) = s.split_once('.') else {
        return false;
    };
    let digits = |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit());
    digits(secs) && digits(micros) && micros.len() == 6
}

/// Slack のリンク 1 つを読む。メッセージへのリンクでなければ `None`。
pub fn parse_link(url: &str) -> Option<Link> {
    let rest = url.strip_prefix(SCHEME)?;
    let (rest, _) = rest.split_once('#').unwrap_or((rest, ""));
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (host, path) = rest.split_once('/')?;
    let name = host.strip_suffix(DOMAIN)?;
    if name.is_empty()
        || name.starts_with(['.', '-'])
        || name.contains("..")
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
    {
        return None;
    }
    let mut parts = path.split('/').filter(|p| !p.is_empty());
    if parts.next() != Some("archives") {
        return None;
    }
    let channel = parts.next()?;
    if !channel.starts_with(['C', 'G', 'D'])
        || !channel
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return None;
    }
    // `p1700000000123456` は、時刻の小数点を抜いたもの
    let stamp = parts.next()?.strip_prefix('p')?;
    if stamp.len() <= 6 || !stamp.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (secs, micros) = stamp.split_at(stamp.len() - 6);
    let ts = format!("{secs}.{micros}");
    let thread_ts = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("thread_ts="))
        .filter(|value| is_ts(value) && *value != ts)
        .map(str::to_owned);
    Some(Link {
        host: host.to_owned(),
        channel: channel.to_owned(),
        ts,
        thread_ts,
    })
}

/// 文章の中の Slack のリンクを、出てきた順に、重複なしで返す（[`MAX_LINKS`] 個まで）。
pub fn find_links(text: &str) -> Vec<Link> {
    let mut links: Vec<Link> = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(SCHEME) {
        let candidate = &rest[start..];
        // リンクは ASCII だけ。日本語の文に続けて書かれていても、そこで切る
        let end = candidate
            .find(|c: char| !c.is_ascii_graphic() || "<>\"'()[]{}`|".contains(c))
            .unwrap_or(candidate.len());
        let url = candidate[..end].trim_end_matches(['.', ',', ';', ':', '!']);
        if let Some(link) = parse_link(url) {
            if !links.contains(&link) && links.len() < MAX_LINKS {
                links.push(link);
            }
        }
        rest = &candidate[end.max(SCHEME.len())..];
    }
    links
}

impl Link {
    pub fn url(&self) -> String {
        let stamp = self.ts.replace('.', "");
        let base = format!("{SCHEME}{}/archives/{}/p{stamp}", self.host, self.channel);
        match &self.thread_ts {
            Some(thread) => format!("{base}?thread_ts={thread}&cid={}", self.channel),
            None => base,
        }
    }

    /// Slack CLI に渡す引数。スレッドの最初のメッセージから、返信をまとめて読む。
    /// 返信のないメッセージなら、そのメッセージだけが返る。
    /// どれも英数字と決まった記号だけなので、シェルを通しても崩れない。
    fn args(&self) -> Vec<String> {
        vec![
            "api".into(),
            "conversations.replies".into(),
            "--method".into(),
            "GET".into(),
            "--skip-update".into(),
            "--no-color".into(),
            format!("channel={}", self.channel),
            format!("ts={}", self.thread_ts.as_ref().unwrap_or(&self.ts)),
            format!("limit={LIMIT}"),
        ]
    }

    /// 取りに行くコマンド。前のものが起動できなければ、次のものを試す。
    pub fn commands(&self) -> Vec<Spec> {
        commands(&self.args())
    }

    /// CLI の出力を、AI に渡す文章にする。`time` は、秒を画面に出す時刻にする。
    /// 読めなかったときは、Slack が返した理由を返す。
    pub fn format_response(
        &self,
        output: &str,
        time: impl Fn(i64) -> String,
    ) -> Result<String, String> {
        // シェルの設定が何か書いていても、JSON の所だけを読む
        let json = match (output.find('{'), output.rfind('}')) {
            (Some(start), Some(end)) if start < end => &output[start..=end],
            _ => return Err("Slack CLI の出力を読めませんでした".into()),
        };
        let value: Value = serde_json::from_str(json)
            .map_err(|_| "Slack CLI の出力を読めませんでした".to_owned())?;
        if value.get("ok").and_then(Value::as_bool) != Some(true) {
            let reason = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("理由なし");
            return Err(format!("Slack が断りました: {reason}"));
        }
        let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
        let messages = value
            .get("messages")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let threaded = messages.len() > 1;
        let mut out = String::new();
        for message in messages {
            let ts = text(message, "ts").unwrap_or_default();
            let who = ["user", "username", "bot_id"]
                .iter()
                .find_map(|key| text(message, key))
                .unwrap_or_default();
            let at = ts
                .split('.')
                .next()
                .and_then(|secs| secs.parse::<i64>().ok())
                .map(&time)
                .unwrap_or_default();
            let mark = if threaded && ts == self.ts {
                "（リンクのメッセージ）"
            } else {
                ""
            };
            out.push_str(&format!("{who} {at}{mark}:\n"));
            out.push_str(text(message, "text").unwrap_or_default().trim_end());
            out.push('\n');
            let files: Vec<String> = message
                .get("files")
                .and_then(Value::as_array)
                .map(|files| {
                    files
                        .iter()
                        .filter_map(|f| text(f, "name").or_else(|| text(f, "title")))
                        .collect()
                })
                .unwrap_or_default();
            if !files.is_empty() {
                out.push_str(&format!("添付: {}\n", files.join("、")));
            }
            out.push('\n');
        }
        if value.get("has_more").and_then(Value::as_bool) == Some(true) {
            out.push_str(&format!("（最初の {LIMIT} 件まで）\n"));
        }
        let out = out.trim_end().to_owned();
        if out.is_empty() {
            return Err("Slack のメッセージが空でした".into());
        }
        Ok(out)
    }
}

/// シェルの中で Slack CLI を呼ぶ 1 行。sh・bash・zsh・fish のどれでも通る書き方にする。
/// `slack` の名前でも呼ぶのは、その名前が Slack のアプリを指さない所（macOS と WSL）だけ。
fn script(args: &[String], also_slack: bool) -> String {
    let args = args.join(" ");
    if also_slack {
        format!(
            "command -v slack-cli >/dev/null 2>&1 && exec slack-cli {args} || exec slack {args}"
        )
    } else {
        format!("exec slack-cli {args}")
    }
}

fn display(args: &[String]) -> String {
    format!("slack-cli {}", args.join(" "))
}

/// その人のログインシェルで呼ぶ。シェルの設定にある PATH とトークンを読ませる。
fn login(args: &[String], shell: &str, also_slack: bool) -> Spec {
    let mut spec = Spec::direct(shell, vec!["-lic".into(), script(args, also_slack)]);
    spec.display = display(args);
    spec
}

/// Windows で、`slack-cli` をそのまま呼ぶ。
fn native(args: &[String]) -> Spec {
    Spec::direct("slack-cli", args.to_vec())
}

/// WSL の中の、その人のログインシェルで呼ぶ。`wslenv` は、今の `WSLENV`。
fn wsl(args: &[String], wslenv: Option<&str>) -> Spec {
    let names = match wslenv.filter(|names| !names.is_empty()) {
        Some(names) => format!("{names}:{ENV_RUN}"),
        None => ENV_RUN.to_owned(),
    };
    Spec {
        program: "wsl.exe".into(),
        args: vec!["--".into(), "sh".into()],
        display: format!("wsl.exe -- {}", display(args)),
        env: vec![
            (ENV_RUN.into(), script(args, true)),
            ("WSLENV".into(), names),
        ],
        stdin: Some(format!("exec \"${{SHELL:-bash}}\" -lic \"${ENV_RUN}\"\n")),
        ..Default::default()
    }
}

/// Slack CLI を呼ぶコマンド。前のものが起動できなければ、次のものを試す。
fn commands(args: &[String]) -> Vec<Spec> {
    if cfg!(windows) {
        let wslenv = std::env::var("WSLENV").ok();
        return vec![native(args), wsl(args, wslenv.as_deref())];
    }
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| "sh".into());
    vec![login(args, &shell, cfg!(target_os = "macos"))]
}

/// Slack の Web API の 1 回の呼び出し（`slack-cli api <method> key=value …`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Api {
    pub method: String,
    pub params: Vec<(String, String)>,
}

impl Api {
    /// 値は英数字と `-._` だけにする（シェルを通しても崩れない）。ほかの文字が入る値は渡さない。
    pub fn new(method: &str, params: &[(&str, &str)]) -> Option<Self> {
        let safe = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-._".contains(c))
        };
        if !safe(method) || !params.iter().all(|(k, v)| safe(k) && safe(v)) {
            return None;
        }
        Some(Api {
            method: method.to_owned(),
            params: params
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        })
    }

    fn args(&self) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "api".into(),
            self.method.clone(),
            "--skip-update".into(),
            "--no-color".into(),
        ];
        args.extend(self.params.iter().map(|(k, v)| format!("{k}={v}")));
        args
    }

    pub fn commands(&self) -> Vec<Spec> {
        commands(&self.args())
    }

    /// 呼んで、出力（標準出力）を返す。待つので、画面のスレッドでは呼ばない。
    pub fn call(&self) -> Result<String, String> {
        for spec in self.commands() {
            match crate::shell::run(&spec) {
                Ok((Some(0), stdout, _)) => return Ok(stdout),
                Ok((Some(127), _, _)) => {
                    return Err("Slack CLI（slack-cli）が見つかりませんでした".into())
                }
                Ok((code, stdout, all)) => {
                    // Slack が断ったときも、JSON の答えは標準出力に出る
                    if stdout.contains("\"ok\"") {
                        return Ok(stdout);
                    }
                    return Err(format!(
                        "Slack CLI が失敗しました（終了コード {}）: {}",
                        code.map_or("なし".to_owned(), |c| c.to_string()),
                        all.trim().chars().take(1000).collect::<String>()
                    ));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("Slack CLI を起動できませんでした: {e}")),
            }
        }
        Err("Slack CLI（slack-cli）が見つかりませんでした".into())
    }
}

/// Slack のリマインダーを、取り込む中身にする。待つので、画面のスレッドでは呼ばない。
pub fn import_reminders() -> Result<malakt_core::intake::Batch, String> {
    use malakt_core::intake::slack;
    let api = Api::new("reminders.list", &[]).ok_or("呼び出しを作れませんでした")?;
    let value = slack::json_of(&api.call()?).map_err(|e| e.to_string())?;
    Ok(slack::reminders(&value))
}

/// Slack リストの項目を、取り込む中身にする。`target` はリストの URL か ID。
/// 担当者の名前は `users.info` で調べる。調べられない人がいれば、取り込まずにエラーにする。
pub fn import_list(target: &str) -> Result<malakt_core::intake::Batch, String> {
    use malakt_core::intake::slack;
    let (id, host) = slack::list_target(target)
        .ok_or_else(|| format!("「{}」は Slack リストの URL ではありません", target.trim()))?;
    let mut items: Vec<serde_json::Value> = Vec::new();
    let mut first: Option<serde_json::Value> = None;
    let mut cursor = String::new();
    // 1 回に 1000 件まで、全部で 20 回まで
    for _ in 0..20 {
        let mut params = vec![
            ("list_id", id.as_str()),
            ("limit", "1000"),
            ("include_list_metadata", "true"),
        ];
        if !cursor.is_empty() {
            params.push(("cursor", cursor.as_str()));
        }
        let api = Api::new("slackLists.items.list", &params).ok_or("呼び出しを作れませんでした")?;
        let value = slack::json_of(&api.call()?).map_err(|e| e.to_string())?;
        if let Some(list) = value.get("items").and_then(serde_json::Value::as_array) {
            items.extend(list.iter().cloned());
        }
        cursor = value
            .pointer("/response_metadata/next_cursor")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if first.is_none() {
            first = Some(value);
        }
        if cursor.is_empty() {
            break;
        }
    }
    let mut all = first.ok_or("Slack リストを読めませんでした")?;
    all["items"] = serde_json::Value::Array(items);
    let mut names = std::collections::HashMap::new();
    let mut missing = Vec::new();
    for user in slack::list_users(&all) {
        let name = Api::new("users.info", &[("user", user.as_str())])
            .ok_or_else(|| format!("ユーザー {user}"))
            .and_then(|api| api.call())
            .ok()
            .and_then(|out| slack::json_of(&out).ok())
            .and_then(|v| slack::user_name(&v));
        match name {
            Some(n) => {
                names.insert(user, n);
            }
            None => missing.push(user),
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "Slack の担当者の名前を読めませんでした（{}）。トークンに users:read の権限があるか確かめてください",
            missing.join("・")
        ));
    }
    let url = host.map(|h| {
        let team = target
            .split("/lists/")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .unwrap_or_default();
        format!("https://{h}/lists/{team}/{id}")
    });
    Ok(slack::list_items(&all, &names, url.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(ts: &str, thread_ts: Option<&str>) -> Link {
        Link {
            host: "acme.slack.com".into(),
            channel: "C0123ABCD".into(),
            ts: ts.into(),
            thread_ts: thread_ts.map(str::to_owned),
        }
    }

    #[test]
    fn message_and_reply_links_are_recognized() {
        assert_eq!(
            parse_link("https://acme.slack.com/archives/C0123ABCD/p1700000000123456"),
            Some(link("1700000000.123456", None))
        );
        assert_eq!(
            parse_link(
                "https://acme.slack.com/archives/C0123ABCD/p1700000111000200?thread_ts=1700000000.123456&cid=C0123ABCD"
            ),
            Some(link("1700000111.000200", Some("1700000000.123456")))
        );
        // スレッドの最初のメッセージ自身へのリンク
        assert_eq!(
            parse_link(
                "https://acme.slack.com/archives/C0123ABCD/p1700000000123456?thread_ts=1700000000.123456"
            ),
            Some(link("1700000000.123456", None))
        );
        let grid = parse_link(
            "https://acme-corp.enterprise.slack.com/archives/G01ABC/p1700000000123456#x",
        )
        .unwrap();
        assert_eq!(grid.host, "acme-corp.enterprise.slack.com");
        assert_eq!(grid.channel, "G01ABC");
    }

    #[test]
    fn things_that_are_not_messages_are_ignored() {
        for url in [
            "https://slack.com/archives/C0123ABCD/p1700000000123456",
            "https://.slack.com/archives/C0123ABCD/p1700000000123456",
            "https://evil.com/acme.slack.com/archives/C0123ABCD/p1700000000123456",
            "https://acme.slack.com.evil.com/archives/C0123ABCD/p1700000000123456",
            "https://acme.slack.com@evil.com/archives/C0123ABCD/p1700000000123456",
            "http://acme.slack.com/archives/C0123ABCD/p1700000000123456",
            "https://acme.slack.com/archives/C0123ABCD",
            "https://acme.slack.com/archives/C0123ABCD/1700000000123456",
            "https://acme.slack.com/archives/C0123ABCD/p17000",
            "https://acme.slack.com/archives/C0123ABCD/p17000000001234x6",
            "https://acme.slack.com/archives/c0123abcd/p1700000000123456",
            "https://acme.slack.com/archives/C0123;rm/p1700000000123456",
            "https://acme.slack.com/client/T01/C0123ABCD",
            "https://acme.slack.com/files/U01/F01/a.png",
        ] {
            assert_eq!(parse_link(url), None, "{url}");
        }
        // 時刻として読めない thread_ts は使わない
        let odd = parse_link(
            "https://acme.slack.com/archives/C0123ABCD/p1700000000123456?thread_ts=1;rm%20-rf",
        )
        .unwrap();
        assert_eq!(odd.thread_ts, None);
    }

    #[test]
    fn links_are_found_inside_japanese_text() {
        let text = "これ見て https://acme.slack.com/archives/C0123ABCD/p1700000000123456、\
                    あと<https://acme.slack.com/archives/C0123ABCD/p1700000111000200?thread_ts=1700000000.123456&cid=C0123ABCD|返信>。\
                    同じもの https://acme.slack.com/archives/C0123ABCD/p1700000000123456 と https://github.com/o/r";
        assert_eq!(
            find_links(text),
            vec![
                link("1700000000.123456", None),
                link("1700000111.000200", Some("1700000000.123456")),
            ]
        );
        assert!(
            find_links("リンクなし acme.slack.com/archives/C0123ABCD/p1700000000123456").is_empty()
        );
        let many: String = (0..9)
            .map(|i| format!("https://acme.slack.com/archives/C0123ABCD/p170000000012345{i} "))
            .collect();
        assert_eq!(find_links(&many).len(), MAX_LINKS);
    }

    #[test]
    fn urls_round_trip() {
        for url in [
            "https://acme.slack.com/archives/C0123ABCD/p1700000000123456",
            "https://acme.slack.com/archives/C0123ABCD/p1700000111000200?thread_ts=1700000000.123456&cid=C0123ABCD",
        ] {
            assert_eq!(parse_link(url).unwrap().url(), url);
        }
    }

    #[test]
    fn the_whole_thread_is_read_from_its_first_message() {
        let args = link("1700000000.123456", None).args();
        assert_eq!(
            args,
            [
                "api",
                "conversations.replies",
                "--method",
                "GET",
                "--skip-update",
                "--no-color",
                "channel=C0123ABCD",
                "ts=1700000000.123456",
                "limit=100"
            ]
        );
        let reply = link("1700000111.000200", Some("1700000000.123456")).args();
        assert_eq!(reply[7], "ts=1700000000.123456");
        // シェルを通しても崩れない文字だけ
        assert!(reply.iter().all(|arg| arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-.=".contains(c))));
    }

    #[test]
    fn each_place_gets_its_own_command() {
        let link = link("1700000000.123456", None);
        let args = link.args().join(" ");

        let native = native(&link.args());
        assert_eq!(native.program, "slack-cli");
        assert_eq!(native.args, link.args());
        assert_eq!(native.stdin, None);

        let mac = login(&link.args(), "/bin/zsh", true);
        assert_eq!(mac.program, "/bin/zsh");
        assert_eq!(
            mac.args,
            [
                "-lic".to_owned(),
                format!("command -v slack-cli >/dev/null 2>&1 && exec slack-cli {args} || exec slack {args}")
            ]
        );
        assert_eq!(mac.display, format!("slack-cli {args}"));
        // Linux の `slack` は Slack のアプリのことがあるので、呼ばない
        let linux = login(&link.args(), "/bin/bash", false);
        assert_eq!(linux.args[1], format!("exec slack-cli {args}"));

        let wsl = wsl(&link.args(), Some("FOO/p"));
        assert_eq!(wsl.program, "wsl.exe");
        assert_eq!(wsl.args, ["--", "sh"]);
        assert_eq!(
            wsl.stdin.as_deref(),
            Some("exec \"${SHELL:-bash}\" -lic \"$MALAKT_MAHOZ_RUN\"\n")
        );
        assert_eq!(wsl.env[0], (ENV_RUN.to_owned(), mac.args[1].clone()));
        assert_eq!(
            wsl.env[1],
            ("WSLENV".to_owned(), "FOO/p:MALAKT_MAHOZ_RUN".to_owned())
        );
        assert_eq!(super::wsl(&link.args(), None).env[1].1, "MALAKT_MAHOZ_RUN");
        assert_eq!(
            super::wsl(&link.args(), Some("")).env[1].1,
            "MALAKT_MAHOZ_RUN"
        );
    }

    #[test]
    fn api_calls_take_only_safe_values() {
        let api = Api::new("reminders.list", &[("limit", "100")]).unwrap();
        assert_eq!(
            api.args(),
            [
                "api",
                "reminders.list",
                "--skip-update",
                "--no-color",
                "limit=100"
            ]
        );
        assert!(Api::new("users.info", &[("user", "U1; rm -rf ~")]).is_none());
        assert!(Api::new("a b", &[]).is_none());
    }

    #[test]
    fn a_thread_becomes_readable_text() {
        let body = r#"motd
{
  "ok": true,
  "messages": [
    {"user": "U01", "ts": "1700000000.123456", "text": "起動で落ちます\n"},
    {"user": "U02", "ts": "1700000111.000200", "text": "直しました", "files": [{"name": "log.txt"}, {"title": "画面"}]},
    {"bot_id": "B01", "ts": "1700000222.000300", "text": ""}
  ],
  "has_more": true
}"#;
        let time = |secs: i64| format!("t{secs}");
        let text = link("1700000111.000200", Some("1700000000.123456"))
            .format_response(body, time)
            .unwrap();
        assert_eq!(
            text,
            "U01 t1700000000:\n起動で落ちます\n\n\
             U02 t1700000111（リンクのメッセージ）:\n直しました\n添付: log.txt、画面\n\n\
             B01 t1700000222:\n\n\n\
             （最初の 100 件まで）"
        );
        // メッセージが 1 つなら、目印は付けない
        let one = link("1700000000.123456", None)
            .format_response(
                r#"{"ok":true,"messages":[{"user":"U01","ts":"1700000000.123456","text":"ひとつ"}]}"#,
                time,
            )
            .unwrap();
        assert_eq!(one, "U01 t1700000000:\nひとつ");
    }

    #[test]
    fn a_refusal_from_slack_is_an_error() {
        let link = link("1700000000.123456", None);
        let time = |_: i64| String::new();
        assert_eq!(
            link.format_response(r#"{"ok":false,"error":"not_authed"}"#, time),
            Err("Slack が断りました: not_authed".into())
        );
        assert_eq!(
            link.format_response(r#"{"ok":true,"messages":[]}"#, time),
            Err("Slack のメッセージが空でした".into())
        );
        for output in ["", "command not found", "} {", "{broken}"] {
            assert_eq!(
                link.format_response(output, time),
                Err("Slack CLI の出力を読めませんでした".into()),
                "{output}"
            );
        }
    }
}
