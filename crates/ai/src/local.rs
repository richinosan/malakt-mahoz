//! 手元で動かす AI のサーバー（ローカル AI）。
//!
//! - Ollama: Ollama の API（`/api/chat`）。モデルの一覧は `/api/tags`、できること
//!   （ツール・画像・考える深さ）と一度に読める長さは `/api/show` から読む
//! - LM Studio: OpenAI 互換の API（`/v1/chat/completions`）。モデルの一覧とできることは
//!   `/api/v1/models`（古い版は `/api/v0/models`）から読む
//! - llama.cpp、vLLM、LocalAI、Jan、KoboldCpp、text-generation-webui、GPT4All など:
//!   OpenAI 互換の API。モデルの一覧は `/v1/models`（llama.cpp は画像を読めるかを `/props` から）
//!
//! できることが一覧に書いていないモデルには、ツールや画像を送ってみて、断られたら外して送り直す
//! （[`lacking`]）。

use serde_json::{json, Value};

use crate::provider::{CatalogModel, Effort, Endpoint};
use crate::wire::{error_message, Wire};

/// GET で読む関数。答えたら状態コードと本文、つながらなければ `Err`（[`crate::http::probe`]）。
pub type Probe<'a> = &'a (dyn Fn(&str, &[(&str, String)]) -> Result<(u16, String), String> + Sync);
/// JSON を POST する関数（[`crate::http::post_json`]）。
pub type Post<'a> =
    &'a (dyn Fn(&str, &[(&'static str, String)], &Value) -> Result<Value, String> + Sync);

/// サーバーの種類。画面に出す名前と、モデルの一覧の読み方が変わる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Ollama,
    LmStudio,
    LlamaCpp,
    Vllm,
    LocalAi,
    Jan,
    KoboldCpp,
    TextGen,
    Gpt4All,
    /// ほかの OpenAI 互換のサーバー
    Other,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Ollama => "Ollama",
            Kind::LmStudio => "LM Studio",
            Kind::LlamaCpp => "llama.cpp",
            Kind::Vllm => "vLLM",
            Kind::LocalAi => "LocalAI",
            Kind::Jan => "Jan",
            Kind::KoboldCpp => "KoboldCpp",
            Kind::TextGen => "text-generation-webui",
            Kind::Gpt4All => "GPT4All",
            Kind::Other => "OpenAI 互換",
        }
    }
}

/// 自動で探すポートと、そこで動くことが多いサーバー（どれもアプリの最初の設定のポート）。
pub const CANDIDATES: [(u16, Kind); 9] = [
    (11434, Kind::Ollama),
    (1234, Kind::LmStudio),
    (8080, Kind::LlamaCpp),
    (9931, Kind::LlamaCpp),
    (8000, Kind::Vllm),
    (1337, Kind::Jan),
    (5001, Kind::KoboldCpp),
    (5000, Kind::TextGen),
    (4891, Kind::Gpt4All),
];

/// 見つけたサーバー。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Server {
    pub kind: Kind,
    /// `http://127.0.0.1:11434` のような、`/v1` を付けない URL
    pub base: String,
}

impl Server {
    /// 画面に出す場所。`127.0.0.1:11434` のように、頭の `http://` を外す。
    pub fn address(&self) -> &str {
        self.base
            .split_once("://")
            .map_or(self.base.as_str(), |(_, rest)| rest)
    }
}

/// 入力された URL を、比べられる形にする。`/v1` や `/api` で終わっていれば外し、`http://` がなければ付ける。
pub fn normalize(url: &str) -> String {
    let mut url = url.trim().trim_end_matches('/').to_owned();
    if url.is_empty() {
        return url;
    }
    if !url.contains("://") {
        url = format!("http://{url}");
    }
    for tail in ["/v1", "/api"] {
        if let Some(stripped) = url.strip_suffix(tail) {
            url = stripped.trim_end_matches('/').to_owned();
            break;
        }
    }
    url
}

fn auth(key: &str) -> Vec<(&'static str, String)> {
    if key.is_empty() {
        Vec::new()
    } else {
        vec![("authorization", format!("Bearer {key}"))]
    }
}

fn json_of(text: &str) -> Option<Value> {
    serde_json::from_str(text).ok()
}

/// その URL で答える AI のサーバーを見分ける。つながらない、AI のサーバーでないなら `None`。
pub fn identify(probe: Probe, base: &str, key: &str) -> Option<Server> {
    let base = normalize(base);
    let headers = auth(key);
    let server = |kind| {
        Some(Server {
            kind,
            base: base.clone(),
        })
    };
    // Ollama は版を答える。動いていないポートは、ここですぐ断られる
    let (status, text) = probe(&format!("{base}/api/version"), &headers).ok()?;
    if status == 200 && json_of(&text).is_some_and(|v| v["version"].is_string()) {
        return server(Kind::Ollama);
    }
    // LM Studio の API（v1 と、古い v0）
    if let Ok((200, text)) = probe(&format!("{base}/api/v1/models"), &headers) {
        if json_of(&text).is_some_and(|v| v["models"].is_array()) {
            return server(Kind::LmStudio);
        }
    }
    if let Ok((200, text)) = probe(&format!("{base}/api/v0/models"), &headers) {
        if json_of(&text).is_some_and(|v| v["data"].is_array()) {
            return server(Kind::LmStudio);
        }
    }
    let (status, text) = probe(&format!("{base}/v1/models"), &headers).ok()?;
    // キーが要るサーバー（Jan など）は、キーがないと断る。キーを入れれば使える
    let by_port = || {
        let port = base.rsplit(':').next().and_then(|p| p.parse::<u16>().ok());
        CANDIDATES.iter().find(|(p, _)| Some(*p) == port).map_or(
            Kind::Other,
            |(_, kind)| match kind {
                // 8080 は llama.cpp か LocalAI。llama.cpp なら `owned_by` で分かる
                Kind::LlamaCpp if port == Some(8080) => Kind::LocalAi,
                kind => *kind,
            },
        )
    };
    if matches!(status, 401 | 403) {
        return server(by_port());
    }
    let value = json_of(&text).filter(|v| status == 200 && v["data"].is_array())?;
    let owner = value["data"][0]["owned_by"].as_str().unwrap_or_default();
    server(match owner {
        "llamacpp" => Kind::LlamaCpp,
        "vllm" => Kind::Vllm,
        _ => by_port(),
    })
}

/// 手元のよく使われるポートを、まとめて探す。見つけた順は [`CANDIDATES`] の順。
pub fn detect(probe: Probe, key: &str) -> Vec<Server> {
    let found: Vec<Option<Server>> = std::thread::scope(|scope| {
        let handles: Vec<_> = CANDIDATES
            .iter()
            .map(|(port, _)| {
                scope.spawn(move || identify(probe, &format!("http://127.0.0.1:{port}"), key))
            })
            .collect();
        handles
            .into_iter()
            // 探す途中で落ちたスレッドは、見つからなかったポートと同じ
            .map(|h| h.join().ok().flatten())
            .collect()
    });
    found.into_iter().flatten().collect()
}

/// GET して JSON を読む。エラーの状態コードなら、理由を付けて `Err`。
fn get_json(probe: Probe, url: &str, key: &str) -> Result<Value, String> {
    let (status, text) = probe(url, &auth(key))?;
    let value = json_of(&text);
    if !(200..300).contains(&status) {
        let detail = value
            .as_ref()
            .and_then(error_message)
            .unwrap_or_else(|| text.chars().take(300).collect());
        let hint = if matches!(status, 401 | 403) {
            "API キーを確認してください。"
        } else {
            ""
        };
        return Err(format!(
            "{url} がエラーを返しました（{status}）。{hint}{detail}"
        ));
    }
    value.ok_or_else(|| format!("{url} の返事が JSON として読めません"))
}

/// サーバーが選べると書いている考える深さのうち、このアプリで送れるもの。
fn efforts_of(values: Option<&Value>) -> Vec<String> {
    values
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|v| Effort::parse(v).is_some_and(|e| e != Effort::Default))
        .map(str::to_owned)
        .collect()
}

fn model(id: String, wire: Wire, base: &str) -> CatalogModel {
    let api = match wire {
        Wire::Ollama => format!("{base}/api"),
        _ => format!("{base}/v1"),
    };
    CatalogModel {
        id,
        name: None,
        api: Some((wire, api)),
        efforts: Vec::new(),
        tools: None,
        vision: None,
        context: None,
    }
}

/// サーバーに入っている、チャットに使えるモデルの一覧を読む。
pub fn catalog(
    probe: Probe,
    post: Post,
    server: &Server,
    key: &str,
) -> Result<Vec<CatalogModel>, String> {
    let base = server.base.as_str();
    match server.kind {
        Kind::Ollama => {
            let tags = get_json(probe, &format!("{base}/api/tags"), key)?;
            let mut models = Vec::new();
            for entry in tags["models"].as_array().into_iter().flatten() {
                let Some(name) = entry["name"].as_str().or(entry["model"].as_str()) else {
                    continue;
                };
                let mut m = model(name.to_owned(), Wire::Ollama, base);
                // 詳しい情報が読めなくても、一覧にあるモデルは選べる。できることは送って確かめる
                if let Ok(show) = post(
                    &format!("{base}/api/show"),
                    &auth(key),
                    &json!({"model": name}),
                ) {
                    if let Some(caps) = show["capabilities"].as_array() {
                        let has = |c: &str| caps.iter().any(|v| v == c);
                        // 文の埋め込みだけのモデルは、チャットに使えない
                        if !has("completion") {
                            continue;
                        }
                        m.tools = Some(has("tools"));
                        m.vision = Some(has("vision"));
                        if has("thinking") {
                            m.efforts = efforts_of(show.pointer("/thinking/values"));
                        }
                    }
                    let info = &show["model_info"];
                    if let Some(arch) = info["general.architecture"].as_str() {
                        m.context = info[format!("{arch}.context_length")]
                            .as_u64()
                            .map(|n| n.min(u32::MAX as u64) as u32);
                    }
                }
                models.push(m);
            }
            Ok(models)
        }
        Kind::LmStudio => {
            if let Ok(list) = get_json(probe, &format!("{base}/api/v1/models"), key) {
                if let Some(entries) = list["models"].as_array() {
                    let mut models = Vec::new();
                    for entry in entries {
                        if entry["type"] != "llm" {
                            continue;
                        }
                        let Some(id) = entry["key"].as_str() else {
                            continue;
                        };
                        let mut m = model(id.to_owned(), Wire::Chat, base);
                        m.name = entry["display_name"].as_str().map(str::to_owned);
                        let caps = &entry["capabilities"];
                        m.vision = caps["vision"].as_bool();
                        // 学習していないモデルも、LM Studio の標準の書き方でツールを呼べる
                        m.tools = (caps["trained_for_tool_use"] == true).then_some(true);
                        m.efforts = efforts_of(caps.pointer("/reasoning/allowed_options"));
                        m.context = entry
                            .pointer("/loaded_instances/0/config/context_length")
                            .or(entry.get("max_context_length"))
                            .and_then(Value::as_u64)
                            .map(|n| n.min(u32::MAX as u64) as u32);
                        models.push(m);
                    }
                    return Ok(models);
                }
            }
            let list = get_json(probe, &format!("{base}/api/v0/models"), key)?;
            Ok(list["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|e| e["type"] == "llm" || e["type"] == "vlm")
                .filter_map(|entry| {
                    let mut m = model(entry["id"].as_str()?.to_owned(), Wire::Chat, base);
                    m.vision = Some(entry["type"] == "vlm");
                    let caps = entry["capabilities"].as_array();
                    m.tools = caps
                        .is_some_and(|c| c.iter().any(|v| v == "tool_use"))
                        .then_some(true);
                    m.context = entry
                        .get("loaded_context_length")
                        .or(entry.get("max_context_length"))
                        .and_then(Value::as_u64)
                        .map(|n| n.min(u32::MAX as u64) as u32);
                    Some(m)
                })
                .collect())
        }
        _ => {
            let list = get_json(probe, &format!("{base}/v1/models"), key)?;
            // llama.cpp は、読み込んだモデルが画像を読めるかを答える
            let vision = (server.kind == Kind::LlamaCpp)
                .then(|| get_json(probe, &format!("{base}/props"), key).ok())
                .flatten()
                .and_then(|props| props.pointer("/modalities/vision").and_then(Value::as_bool));
            Ok(list["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| {
                    let mut m = model(entry["id"].as_str()?.to_owned(), Wire::Chat, base);
                    m.vision = vision;
                    m.context = entry
                        .get("max_model_len")
                        .or(entry.pointer("/meta/n_ctx_train"))
                        .and_then(Value::as_u64)
                        .map(|n| n.min(u32::MAX as u64) as u32);
                    Some(m)
                })
                .collect())
        }
    }
}

/// サーバーへの送り先。
pub fn endpoint(server: &Server, key: &str) -> Endpoint {
    let (wire, path) = match server.kind {
        Kind::Ollama => (Wire::Ollama, "api/chat"),
        _ => (Wire::Chat, "v1/chat/completions"),
    };
    Endpoint {
        wire,
        url: format!("{}/{path}", server.base),
        headers: auth(key),
        fallbacks: false,
        // 手元のサーバーは、同じ頭の部分を自分で使い回す（キャッシュの指定はない）
        prefix_ttl: None,
        plan: false,
    }
}

/// モデルが受け付けなかったもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lack {
    Tools,
    Images,
}

/// サーバーのエラーが、ツールか画像を受け付けないというものか。
/// Ollama は「does not support tools」、llama.cpp は `--jinja` や画像の `mmproj`、
/// vLLM は `--enable-auto-tool-choice` を求める文を返す。
pub fn lacking(error: &str) -> Option<Lack> {
    let e = error.to_lowercase();
    let refused = ["not support", "unsupported", "not supported", "not enabled"]
        .iter()
        .any(|w| e.contains(w));
    if e.contains("tool")
        && (refused || e.contains("jinja") || e.contains("enable-auto-tool-choice"))
    {
        return Some(Lack::Tools);
    }
    if ["image", "vision", "multimodal", "mmproj"]
        .iter()
        .any(|w| e.contains(w))
        && (refused || e.contains("mmproj"))
    {
        return Some(Lack::Images);
    }
    None
}

/// Ollama に、一度に読む長さ（`num_ctx`）を付ける。Ollama の標準の長さはツールの説明より短く、
/// 超えた分は黙って前から切られるので、送る分が入る長さを 8,192 トークンごとに選ぶ
/// （長さを変えるとモデルを読み込み直すので、細かくは変えない）。モデルの上限は超えない。
pub fn fit_context(body: &mut Value, context: Option<u32>) {
    fn chars(value: &Value) -> usize {
        match value {
            Value::String(s) => s.chars().count(),
            Value::Array(items) => items.iter().map(chars).sum(),
            Value::Object(map) => map
                .iter()
                // 画像はトークンの数え方が違うので、文字としては数えない
                .filter(|(k, _)| k.as_str() != "images")
                .map(|(k, v)| k.chars().count() + chars(v))
                .sum(),
            _ => 4,
        }
    }
    const STEP: usize = 8192;
    // 日本語は 1 文字が 1 トークン近くになる。少なめに 2 文字で 1 トークンと見て、返事の分を足す
    let needed = chars(&body["messages"]) / 2 + chars(&body["tools"]) / 2 + 4096;
    let mut size = needed.div_ceil(STEP).max(1) * STEP;
    if let Some(max) = context.filter(|m| *m > 0) {
        size = size.min(max as usize);
    }
    body["options"] = json!({"num_ctx": size});
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    type Answer = Result<(u16, String), String>;

    /// URL ごとに決めた答えを返す、手元のサーバーの代わり。ないものはつながらない。
    fn fake(answers: &[(&str, u16, Value)]) -> impl Fn(&str, &[(&str, String)]) -> Answer + Sync {
        let map: HashMap<String, (u16, String)> = answers
            .iter()
            .map(|(url, status, body)| ((*url).to_owned(), (*status, body.to_string())))
            .collect();
        move |url: &str, _: &[(&str, String)]| {
            map.get(url)
                .cloned()
                .ok_or_else(|| format!("{url} に接続できませんでした"))
        }
    }

    fn no_post(_: &str, _: &[(&str, String)], _: &Value) -> Result<Value, String> {
        Err("送れません".into())
    }

    #[test]
    fn urls_are_normalized() {
        assert_eq!(normalize(" localhost:1234/v1/ "), "http://localhost:1234");
        assert_eq!(
            normalize("http://127.0.0.1:11434/api"),
            "http://127.0.0.1:11434"
        );
        assert_eq!(
            normalize("https://ai.example.com/v1"),
            "https://ai.example.com"
        );
        assert_eq!(normalize(""), "");
        let server = Server {
            kind: Kind::Ollama,
            base: "http://127.0.0.1:11434".into(),
        };
        assert_eq!(server.address(), "127.0.0.1:11434");
    }

    #[test]
    fn servers_are_told_apart() {
        let probe = fake(&[
            (
                "http://127.0.0.1:11434/api/version",
                200,
                json!({"version": "0.13.0"}),
            ),
            // LM Studio は知らない場所にも 200 でエラーを返す
            (
                "http://127.0.0.1:1234/api/version",
                200,
                json!({"error": "Unexpected endpoint"}),
            ),
            (
                "http://127.0.0.1:1234/api/v1/models",
                200,
                json!({"models": []}),
            ),
            ("http://127.0.0.1:8080/api/version", 404, json!({})),
            ("http://127.0.0.1:8080/api/v1/models", 404, json!({})),
            ("http://127.0.0.1:8080/api/v0/models", 404, json!({})),
            (
                "http://127.0.0.1:8080/v1/models",
                200,
                json!({"data": [{"id": "m", "owned_by": "llamacpp"}]}),
            ),
            ("http://127.0.0.1:1337/api/version", 401, json!({})),
            ("http://127.0.0.1:1337/api/v1/models", 401, json!({})),
            ("http://127.0.0.1:1337/api/v0/models", 401, json!({})),
            (
                "http://127.0.0.1:1337/v1/models",
                401,
                json!({"error": "unauthorized"}),
            ),
            ("http://127.0.0.1:8000/api/version", 404, json!({})),
            (
                "http://127.0.0.1:8000/v1/models",
                200,
                json!({"data": [{"id": "q", "owned_by": "vllm"}]}),
            ),
            // AI のサーバーでないもの
            ("http://127.0.0.1:5000/api/version", 404, json!({})),
            ("http://127.0.0.1:5000/v1/models", 404, json!({})),
        ]);
        let kinds: Vec<(Kind, String)> = detect(&probe, "")
            .into_iter()
            .map(|s| (s.kind, s.address().to_owned()))
            .collect();
        assert_eq!(
            kinds,
            [
                (Kind::Ollama, "127.0.0.1:11434".into()),
                (Kind::LmStudio, "127.0.0.1:1234".into()),
                (Kind::LlamaCpp, "127.0.0.1:8080".into()),
                (Kind::Vllm, "127.0.0.1:8000".into()),
                (Kind::Jan, "127.0.0.1:1337".into()),
            ]
        );
        assert_eq!(identify(&probe, "http://127.0.0.1:9", ""), None);
    }

    #[test]
    fn ollama_models_carry_what_they_can_do() {
        let probe = fake(&[(
            "http://o/api/tags",
            200,
            json!({"models": [{"name": "qwen3:8b"}, {"name": "gemma3:4b"}, {"name": "nomic-embed-text"}, {"name": "old"}]}),
        )]);
        let post = |url: &str, _: &[(&str, String)], body: &Value| -> Result<Value, String> {
            assert_eq!(url, "http://o/api/show");
            Ok(match body["model"].as_str().unwrap() {
                "qwen3:8b" => json!({"capabilities": ["completion", "tools", "thinking"],
                    "thinking": {"values": ["low", "medium", "high"]},
                    "model_info": {"general.architecture": "qwen3", "qwen3.context_length": 40960}}),
                "gemma3:4b" => json!({"capabilities": ["completion", "vision"],
                    "model_info": {"general.architecture": "gemma3", "gemma3.context_length": 131072}}),
                "nomic-embed-text" => json!({"capabilities": ["embedding"]}),
                _ => return Err("古い版".into()),
            })
        };
        let server = Server {
            kind: Kind::Ollama,
            base: "http://o".into(),
        };
        let models = catalog(&probe, &post, &server, "").unwrap();
        let ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["qwen3:8b", "gemma3:4b", "old"]);
        assert_eq!(models[0].tools, Some(true));
        assert_eq!(models[0].vision, Some(false));
        assert_eq!(models[0].efforts, ["low", "medium", "high"]);
        assert_eq!(models[0].context, Some(40960));
        assert_eq!(models[0].api, Some((Wire::Ollama, "http://o/api".into())));
        assert_eq!(models[1].tools, Some(false));
        assert_eq!(models[1].vision, Some(true));
        assert!(models[1].efforts.is_empty());
        // 詳しく読めなかったモデルは、送って確かめる
        assert_eq!((models[2].tools, models[2].vision), (None, None));
    }

    #[test]
    fn lm_studio_and_openai_compatible_models_are_listed() {
        let probe = fake(&[
            (
                "http://l/api/v1/models",
                200,
                json!({"models": [
                    {"type": "llm", "key": "qwen/qwen3-vl-8b", "display_name": "Qwen3 VL 8B",
                     "capabilities": {"vision": true, "trained_for_tool_use": true,
                                      "reasoning": {"allowed_options": ["off", "low", "high"], "default": "low"}},
                     "max_context_length": 262144,
                     "loaded_instances": [{"id": "x", "config": {"context_length": 16384}}]},
                    {"type": "llm", "key": "plain", "capabilities": {"vision": false, "trained_for_tool_use": false}},
                    {"type": "embedding", "key": "embed"},
                ]}),
            ),
            (
                "http://c/v1/models",
                200,
                json!({"data": [{"id": "gguf", "meta": {"n_ctx_train": 8192}}]}),
            ),
            (
                "http://c/props",
                200,
                json!({"modalities": {"vision": true}}),
            ),
            (
                "http://v/v1/models",
                200,
                json!({"data": [{"id": "Qwen/Qwen3-8B", "max_model_len": 32768}]}),
            ),
        ]);
        let list = |kind, base: &str| {
            catalog(
                &probe,
                &no_post,
                &Server {
                    kind,
                    base: base.into(),
                },
                "",
            )
            .unwrap()
        };
        let lm = list(Kind::LmStudio, "http://l");
        assert_eq!(lm.len(), 2);
        assert_eq!(lm[0].name.as_deref(), Some("Qwen3 VL 8B"));
        assert_eq!((lm[0].tools, lm[0].vision), (Some(true), Some(true)));
        assert_eq!(lm[0].efforts, ["low", "high"]);
        // 読み込んでいる長さが先
        assert_eq!(lm[0].context, Some(16384));
        assert_eq!(lm[0].api, Some((Wire::Chat, "http://l/v1".into())));
        assert_eq!((lm[1].tools, lm[1].vision), (None, Some(false)));

        let cpp = list(Kind::LlamaCpp, "http://c");
        assert_eq!(
            (cpp[0].id.as_str(), cpp[0].vision, cpp[0].context),
            ("gguf", Some(true), Some(8192))
        );
        let vllm = list(Kind::Vllm, "http://v");
        assert_eq!((vllm[0].vision, vllm[0].context), (None, Some(32768)));

        let error = catalog(
            &fake(&[(
                "http://j/v1/models",
                401,
                json!({"error": {"message": "invalid key"}}),
            )]),
            &no_post,
            &Server {
                kind: Kind::Jan,
                base: "http://j".into(),
            },
            "",
        )
        .unwrap_err();
        assert!(
            error.contains("401") && error.contains("API キー") && error.contains("invalid key")
        );
    }

    #[test]
    fn endpoints_follow_the_server() {
        let ollama = endpoint(
            &Server {
                kind: Kind::Ollama,
                base: "http://o".into(),
            },
            "",
        );
        assert_eq!(
            (ollama.wire, ollama.url.as_str()),
            (Wire::Ollama, "http://o/api/chat")
        );
        assert!(ollama.headers.is_empty());
        let jan = endpoint(
            &Server {
                kind: Kind::Jan,
                base: "http://j".into(),
            },
            "k",
        );
        assert_eq!(
            (jan.wire, jan.url.as_str()),
            (Wire::Chat, "http://j/v1/chat/completions")
        );
        assert_eq!(jan.headers, vec![("authorization", "Bearer k".into())]);
    }

    #[test]
    fn refusals_of_tools_and_images_are_recognized() {
        let tools = [
            "サーバーがエラーを返しました（400）。registry.ollama.ai/library/gemma2:2b does not support tools",
            "tools param requires --jinja flag",
            "\"auto\" tool choice requires --enable-auto-tool-choice and --tool-call-parser to be set",
        ];
        for e in tools {
            assert_eq!(lacking(e), Some(Lack::Tools), "{e}");
        }
        let images = [
            "image input is not supported - hint: if this is unexpected, you may need to provide the mmproj",
            "This model does not support vision",
        ];
        for e in images {
            assert_eq!(lacking(e), Some(Lack::Images), "{e}");
        }
        assert_eq!(lacking("model 'x' not found"), None);
        assert_eq!(lacking("connection refused"), None);
    }

    #[test]
    fn ollama_gets_a_context_that_fits() {
        let mut small = json!({"messages": [{"role": "user", "content": "こんにちは"}]});
        fit_context(&mut small, Some(131072));
        assert_eq!(small["options"]["num_ctx"], 8192);
        let long = "あ".repeat(40_000);
        let mut big = json!({"messages": [{"role": "user", "content": long, "images": ["x".repeat(100_000)]}],
                             "tools": [{"description": "t"}]});
        fit_context(&mut big, Some(131072));
        // 20,000 + 返事の分 4,096 が入る長さ
        assert_eq!(big["options"]["num_ctx"], 24576);
        // モデルの上限は超えない
        fit_context(&mut big, Some(16384));
        assert_eq!(big["options"]["num_ctx"], 16384);
    }
}
