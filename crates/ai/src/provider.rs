use crate::wire::Wire;

/// API キーを設定できるサービス。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Claude,
    OpenAi,
    /// OpenCode の従量課金
    OpenCode,
    /// OpenCode Go（定額）
    OpenCodeGo,
    Cursor,
    /// 手元で動かす AI のサーバー（Ollama、LM Studio、llama.cpp など。[`crate::local`]）
    Local,
}

impl Provider {
    pub const ALL: [Provider; 6] = [
        Provider::Claude,
        Provider::OpenAi,
        Provider::OpenCode,
        Provider::OpenCodeGo,
        Provider::Cursor,
        Provider::Local,
    ];

    /// 設定やキーチェーンに使う名前。
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::OpenAi => "openai",
            Provider::OpenCode => "opencode",
            Provider::OpenCodeGo => "opencode-go",
            Provider::Cursor => "cursor",
            Provider::Local => "local",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Provider::Claude => "Claude",
            Provider::OpenAi => "ChatGPT",
            Provider::OpenCode => "OpenCode",
            Provider::OpenCodeGo => "OpenCode Go",
            Provider::Cursor => "Cursor",
            Provider::Local => "ローカル AI",
        }
    }

    /// キーチェーンにキーがないときに読む環境変数。
    pub fn env_var(self) -> &'static str {
        match self {
            Provider::Claude => "ANTHROPIC_API_KEY",
            Provider::OpenAi => "OPENAI_API_KEY",
            Provider::OpenCode | Provider::OpenCodeGo => "OPENCODE_API_KEY",
            Provider::Cursor => "CURSOR_API_KEY",
            Provider::Local => "LOCAL_AI_API_KEY",
        }
    }

    /// API キーがなければ使えないか。ローカルの AI はキーなしで動くものが多い（Jan などはキーが要る）。
    pub fn needs_key(self) -> bool {
        self != Provider::Local
    }

    /// すぐ選べるモデル。ほかのモデルは ID を入力して使う。
    pub fn models(self) -> &'static [&'static str] {
        match self {
            Provider::Claude => &[
                "claude-opus-5-5",
                "claude-fable-5-2",
                "claude-sonnet-5-5",
                "claude-haiku-5-5",
            ],
            Provider::OpenAi => &[
                "gpt-6.2-astra",
                "gpt-6.2-sol",
                "gpt-6.1-terra",
                "gpt-6-luna",
            ],
            Provider::OpenCode => &[
                "claude-sonnet-5-5",
                "gpt-6.2-sol",
                "kimi-k3",
                "glm-5.3",
                "big-pickle",
            ],
            Provider::OpenCodeGo => &[
                "glm-5.3",
                "kimi-k3.1-pro",
                "deepseek-v4.1-flash",
                "qwen4-max",
                "claude-haiku-5-5",
                "gpt-6-luna",
            ],
            // Cursor のモデルはアカウントごとに違う（`agent models` で一覧が出る）。
            // ローカルの AI は、サーバーに入っているモデルを一覧から選ぶ
            Provider::Cursor | Provider::Local => &[],
        }
    }

    /// 最初に選ばれているモデル。Cursor は空（Cursor 側の標準のモデル）。
    pub fn default_model(self) -> &'static str {
        self.models().first().copied().unwrap_or("")
    }

    /// effort を選べるか。Cursor の CLI には指定する方法がない。
    pub fn supports_effort(self) -> bool {
        self != Provider::Cursor
    }

    /// OpenCode のモデルの一覧の URL。OpenCode でなければ `None`。
    pub fn catalog_url(self) -> Option<&'static str> {
        match self {
            Provider::OpenCode => Some("https://opencode.ai/inference/v1/models"),
            Provider::OpenCodeGo => Some("https://opencode.ai/inference/go/v1/models"),
            _ => None,
        }
    }
}

/// 考える深さ。`Default` は何も指定せず、モデルの標準に任せる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Effort {
    Default,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    pub const ALL: [Effort; 6] = [
        Effort::Default,
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::XHigh,
        Effort::Max,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Default => "default",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Effort::Default => "標準",
            other => other.as_str(),
        }
    }

    /// API に送る値。`Default` は送らない。
    pub fn wire(self) -> Option<&'static str> {
        (self != Effort::Default).then(|| self.as_str())
    }
}

/// HTTP の送り先。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub wire: Wire,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    /// Claude が回答を断ったとき、サーバーの側で別のモデルに回す指定を付けるか。
    pub fallbacks: bool,
    /// Messages API で、ツールと最初の指示のキャッシュを残す時間（[`crate::wire::Request::prefix_ttl`]）。
    pub prefix_ttl: Option<&'static str>,
    /// ChatGPT のプランでの呼び出し（[`crate::wire::Request::plan`]）。
    pub plan: bool,
}

/// OpenCode に送る Messages API のバージョン。
const ANTHROPIC_VERSION_COMPAT: &str = "2023-06-01";
const ANTHROPIC_VERSION: &str = "2026-10-21";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Claude に送る、ツールと最初の指示のキャッシュを残す時間。
const PREFIX_TTL: &str = "1h";

/// サーバーの側の回し先（`fallbacks`）を使えるモデル。
fn claude_has_fallbacks(model: &str) -> bool {
    [
        "claude-fable-5-2",
        "claude-fable-5-1",
        "claude-opus-5-5",
        "claude-opus-5",
        "claude-sonnet-5-5",
    ]
    .contains(&model)
}

/// モデルの ID から、画面に出す名前を作る。`claude-opus-5-5` なら「Claude Opus 5.5」。
/// Claude のモデルでなければ、ID のまま。
pub fn display_name(id: &str) -> String {
    if !id.starts_with("claude-") {
        return id.to_owned();
    }
    let mut out = String::new();
    let mut after_number = false;
    for part in id.split('-') {
        let number = !part.is_empty() && part.chars().all(|c| c.is_ascii_digit());
        // 終わりに付く日付（20251101 など）は出さない
        if number && part.len() == 8 {
            continue;
        }
        if number && after_number {
            out.push('.');
        } else if !out.is_empty() {
            out.push(' ');
        }
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
        after_number = number;
    }
    out
}

/// OpenCode のモデル 1 つ。モデルの一覧（`/v1/models`）から読む。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogModel {
    pub id: String,
    /// 画面に出す名前。一覧に書いてあるときだけ。
    pub name: Option<String>,
    /// このアプリから呼べる形式と、そのベース URL。呼べない形式（Gemini など）なら `None`。
    pub api: Option<(Wire, String)>,
    /// 選べる effort。空なら effort を指定できない。
    pub efforts: Vec<String>,
    /// ツールを呼べるか。`None` は一覧に書いていない（送ってみて、断られたら外す）。
    pub tools: Option<bool>,
    /// 画像を読めるか。`None` は一覧に書いていない。
    pub vision: Option<bool>,
    /// 一度に読める長さ（トークン）。
    pub context: Option<u32>,
}

/// OpenCode のモデルの一覧を読む。チャット用のモデルだけを残す。
pub fn parse_catalog(response: &serde_json::Value) -> Result<Vec<CatalogModel>, String> {
    let data = response
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or("モデルの一覧に data がありません")?;
    let text =
        |v: &serde_json::Value, key: &str| v.get(key).and_then(|s| s.as_str()).map(str::to_owned);
    let mut models = Vec::new();
    for model in data {
        if model.get("type").and_then(|t| t.as_str()).unwrap_or("chat") != "chat" {
            continue;
        }
        let Some(id) = text(model, "id") else {
            continue;
        };
        let apis = model.get("api");
        let pick = |name: &str, wire: Wire| {
            apis.and_then(|a| a.get(name))
                .and_then(|a| text(a, "base_url"))
                .map(|base| (wire, base))
        };
        // すすめられている形式を先に見る
        let recommended = match model.get("recommended_api").and_then(|r| r.as_str()) {
            Some("messages") => pick("messages", Wire::Messages),
            Some("responses") => pick("responses", Wire::Responses),
            Some("chat-completions") => pick("chat-completions", Wire::Chat),
            _ => None,
        };
        let api = recommended
            .or_else(|| pick("chat-completions", Wire::Chat))
            .or_else(|| pick("messages", Wire::Messages))
            .or_else(|| pick("responses", Wire::Responses));
        let efforts = model
            .pointer("/capabilities/reasoning/effort")
            .and_then(|e| e.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|e| e.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let name = text(model, "display_name").or_else(|| text(model, "name"));
        models.push(CatalogModel {
            id,
            name,
            api,
            efforts,
            tools: None,
            vision: None,
            context: None,
        });
    }
    Ok(models)
}

/// OpenCode の送り先を、モデルの一覧から決める。
pub fn opencode_endpoint(
    catalog: &[CatalogModel],
    model: &str,
    effort: Effort,
    key: &str,
    session: &str,
) -> Result<Endpoint, String> {
    let entry = catalog.iter().find(|m| m.id == model).ok_or_else(|| {
        format!("{model} というモデルは、OpenCode のモデルの一覧にありません。モデルの名前を確認してください")
    })?;
    let (wire, base) = entry.api.clone().ok_or_else(|| {
        format!("{model} は、このアプリから呼び出せる形式の API がありません。ほかのモデルを選んでください")
    })?;
    if let Some(effort) = effort.wire() {
        if !entry.efforts.iter().any(|e| e == effort) {
            return Err(if entry.efforts.is_empty() {
                format!("{model} は effort を指定できません。effort を「標準」にしてください")
            } else {
                format!(
                    "{model} で選べる effort は {} です。effort を選び直してください",
                    entry.efforts.join(" / ")
                )
            });
        }
    }
    let mut headers = vec![
        ("authorization", format!("Bearer {key}")),
        // 会話ごとに同じ値を送る（OpenCode がキャッシュと振り分けに使う）
        ("x-opencode-session", session.to_owned()),
    ];
    let path = match wire {
        Wire::Messages => {
            headers.push(("anthropic-version", ANTHROPIC_VERSION_COMPAT.into()));
            "messages"
        }
        Wire::Responses => "responses",
        _ => "chat/completions",
    };
    Ok(Endpoint {
        wire,
        url: format!("{}/{path}", base.trim_end_matches('/')),
        headers,
        fallbacks: false,
        // 互換の API には、ttl のない標準の形（5 分）だけを送る
        prefix_ttl: None,
        plan: false,
    })
}

/// Claude と ChatGPT の送り先を決める。OpenCode は [`opencode_endpoint`]、Cursor は CLI で呼ぶ。
pub fn endpoint(provider: Provider, model: &str, key: &str) -> Result<Endpoint, String> {
    match provider {
        Provider::Claude => {
            let fallbacks = claude_has_fallbacks(model);
            let mut headers = vec![
                ("x-api-key", key.to_owned()),
                ("anthropic-version", ANTHROPIC_VERSION.into()),
            ];
            if fallbacks {
                headers.push(("anthropic-beta", FALLBACK_BETA.into()));
            }
            Ok(Endpoint {
                wire: Wire::Messages,
                url: "https://api.anthropic.com/v1/messages".into(),
                headers,
                fallbacks,
                // ツールと最初の指示は会話をまたいで同じ。チャットの合間が 5 分を超えても読めるように
                // 1 時間残す（書き込みは 5 分より高いが、読むのはふつうの入力よりずっと安い）
                prefix_ttl: Some(PREFIX_TTL),
                plan: false,
            })
        }
        Provider::OpenAi => Ok(Endpoint {
            wire: Wire::Chat,
            url: "https://api.openai.com/v1/chat/completions".into(),
            headers: vec![("authorization", format!("Bearer {key}"))],
            fallbacks: false,
            prefix_ttl: None,
            plan: false,
        }),
        Provider::OpenCode | Provider::OpenCodeGo => {
            Err("OpenCode の送り先は、モデルの一覧から決めます".into())
        }
        Provider::Cursor => Err("Cursor は CLI（agent コマンド）で呼び出します".into()),
        Provider::Local => Err("ローカル AI の送り先は、サーバーから決めます".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ids_become_readable_names() {
        assert_eq!(display_name("claude-opus-5-5"), "Claude Opus 5.5");
        assert_eq!(display_name("claude-fable-5-2"), "Claude Fable 5.2");
        assert_eq!(display_name("claude-opus-5"), "Claude Opus 5");
        assert_eq!(
            display_name("claude-haiku-4-5-20251001"),
            "Claude Haiku 4.5"
        );
        // ほかのサービスのモデルは、ID のまま
        assert_eq!(display_name("glm-5.3"), "glm-5.3");
        assert_eq!(display_name(""), "");
    }

    #[test]
    fn the_catalog_name_is_read() {
        let models = parse_catalog(&serde_json::json!({"data": [
            {"id": "glm-5.3", "name": "GLM 5.3"},
            {"id": "kimi-k3"},
        ]}))
        .unwrap();
        assert_eq!(models[0].name.as_deref(), Some("GLM 5.3"));
        assert_eq!(models[1].name, None);
    }

    #[test]
    fn names_round_trip() {
        for p in Provider::ALL {
            assert_eq!(Provider::parse(p.as_str()), Some(p));
        }
        assert_eq!(Provider::parse("gemini"), None);
        for e in Effort::ALL {
            assert_eq!(Effort::parse(e.as_str()), Some(e));
        }
        assert_eq!(Effort::Default.wire(), None);
        assert_eq!(Effort::XHigh.wire(), Some("xhigh"));
    }

    #[test]
    fn each_service_gets_its_own_endpoint_and_auth() {
        let claude = endpoint(Provider::Claude, "claude-opus-5-5", "k").unwrap();
        assert_eq!(claude.wire, Wire::Messages);
        assert_eq!(claude.url, "https://api.anthropic.com/v1/messages");
        assert!(claude.headers.contains(&("x-api-key", "k".into())));
        assert!(claude
            .headers
            .contains(&("anthropic-version", "2026-10-21".into())));
        // 断られたときの回し先は、対応しているモデルにだけ付ける
        assert!(claude.fallbacks);
        assert!(claude
            .headers
            .contains(&("anthropic-beta", "server-side-fallback-2026-07-01".into())));
        let haiku = endpoint(Provider::Claude, "claude-haiku-5-5", "k").unwrap();
        assert!(!haiku.fallbacks);
        // ツールと最初の指示のキャッシュは、どのモデルでも 1 時間
        assert_eq!(claude.prefix_ttl, Some("1h"));
        assert_eq!(haiku.prefix_ttl, Some("1h"));
        assert!(!haiku
            .headers
            .iter()
            .any(|(name, _)| *name == "anthropic-beta"));

        let openai = endpoint(Provider::OpenAi, "gpt-6.2-sol", "k").unwrap();
        assert_eq!(openai.wire, Wire::Chat);
        assert_eq!(openai.headers, vec![("authorization", "Bearer k".into())]);

        assert!(endpoint(Provider::Cursor, "", "k").is_err());
        assert!(endpoint(Provider::OpenCodeGo, "glm-5.3", "k").is_err());
    }

    fn catalog() -> Vec<CatalogModel> {
        parse_catalog(&serde_json::json!({"object": "list", "data": [
            {"id": "glm-5.3", "type": "chat", "recommended_api": "chat-completions",
             "capabilities": {"reasoning": {"supported": true, "effort": ["low", "high", "max"]}},
             "api": {"chat-completions": {"base_url": "https://opencode.ai/inference/go/openai/v1"}}},
            {"id": "claude-haiku-5-5", "type": "chat", "recommended_api": "messages",
             "capabilities": {"reasoning": {"effort": ["low", "medium"]}},
             "api": {"messages": {"base_url": "https://opencode.ai/inference/go/anthropic/v1"}}},
            {"id": "gpt-6-luna", "type": "chat", "recommended_api": "responses",
             "api": {"responses": {"base_url": "https://opencode.ai/inference/go/openai/v1/"}}},
            {"id": "gemini-4-argon", "type": "chat", "recommended_api": "interactions",
             "api": {"interactions": {"base_url": "https://opencode.ai/inference/go/google/v1beta"}}},
            {"id": "both", "recommended_api": "interactions",
             "api": {"interactions": {"base_url": "x"}, "messages": {"base_url": "https://m"}}},
            {"id": "text-embedding-3-small", "type": "embedding"},
        ]}))
        .unwrap()
    }

    #[test]
    fn the_catalog_keeps_chat_models_with_their_api_and_efforts() {
        let catalog = catalog();
        let ids: Vec<_> = catalog.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "glm-5.3",
                "claude-haiku-5-5",
                "gpt-6-luna",
                "gemini-4-argon",
                "both"
            ]
        );
        assert_eq!(catalog[0].efforts, ["low", "high", "max"]);
        assert!(catalog[2].efforts.is_empty());
        assert_eq!(catalog[3].api, None);
        // すすめられている形式で呼べなければ、呼べる形式を使う
        assert_eq!(catalog[4].api, Some((Wire::Messages, "https://m".into())));
        assert!(parse_catalog(&serde_json::json!({"error": "x"})).is_err());
    }

    #[test]
    fn opencode_routes_by_the_catalog() {
        let catalog = catalog();
        let route = |model, effort| opencode_endpoint(&catalog, model, effort, "k", "s1");
        let chat = route("glm-5.3", Effort::High).unwrap();
        assert_eq!(chat.wire, Wire::Chat);
        assert_eq!(
            chat.url,
            "https://opencode.ai/inference/go/openai/v1/chat/completions"
        );
        assert!(chat.headers.contains(&("authorization", "Bearer k".into())));
        assert!(chat.headers.contains(&("x-opencode-session", "s1".into())));
        assert!(!chat.fallbacks);

        let messages = route("claude-haiku-5-5", Effort::Default).unwrap();
        assert_eq!(messages.wire, Wire::Messages);
        // 互換の API には ttl を送らない
        assert_eq!(messages.prefix_ttl, None);
        assert_eq!(
            messages.url,
            "https://opencode.ai/inference/go/anthropic/v1/messages"
        );
        assert!(messages
            .headers
            .iter()
            .any(|(name, _)| *name == "anthropic-version"));

        let responses = route("gpt-6-luna", Effort::Default).unwrap();
        assert_eq!(responses.wire, Wire::Responses);
        assert_eq!(
            responses.url,
            "https://opencode.ai/inference/go/openai/v1/responses"
        );
    }

    #[test]
    fn opencode_refuses_what_the_model_cannot_do() {
        let catalog = catalog();
        let fail =
            |model, effort| opencode_endpoint(&catalog, model, effort, "k", "s").unwrap_err();
        assert!(fail("kimi-k9", Effort::Default).contains("kimi-k9"));
        assert!(fail("gemini-4-argon", Effort::Default).contains("gemini-4-argon"));
        assert!(fail("glm-5.3", Effort::Medium).contains("low / high / max"));
        assert!(fail("gpt-6-luna", Effort::Low).contains("標準"));
    }
}
