//! 各 API の形式への変換。会話は 1 つの形（[`Turn`]）で持ち、送るたびに組み立てる。

use serde_json::{json, Map, Value};

use crate::files::{self, FilePart, Kind};
use crate::provider::Effort;

/// API の形式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// Anthropic の Messages API
    Messages,
    /// OpenAI の Chat Completions API（互換のサービスを含む）
    Chat,
    /// OpenAI の Responses API
    Responses,
    /// Cursor の CLI（ツールの呼び出しは文章の中の決まった形で受け取る）
    Cursor,
    /// Ollama の API（`/api/chat`）
    Ollama,
}

/// 添えたファイルのうち、中身をそのまま送り先に渡せる種類。渡せないものは、置いた場所だけを伝える。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Media {
    /// 画像と PDF
    All,
    /// 画像だけ（画像を読めるローカルのモデル）
    Images,
    /// どちらも渡さない
    None,
}

impl Media {
    fn allows(self, kind: Kind) -> bool {
        match self {
            Media::All => true,
            Media::Images => kind == Kind::Image,
            Media::None => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
}

/// 会話の 1 つ分。
#[derive(Debug, Clone, PartialEq)]
pub enum Turn {
    User(String),
    /// ユーザーが添えたファイル。すぐ後ろの [`Turn::User`] と 1 つのメッセージにして送る。
    Files(Vec<FilePart>),
    Assistant {
        text: String,
        calls: Vec<ToolCall>,
        /// 返ってきたままの中身。同じ形式に送り返すときは、これをそのまま使う
        /// （考えた内容のブロックなどを、変えずに返す必要がある）。
        raw: Option<(Wire, Value)>,
    },
    Tool {
        id: String,
        name: String,
        output: String,
        is_error: bool,
    },
}

/// AI に見せるツールの定義。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    /// 引数の JSON Schema
    pub schema: Value,
}

/// AI の返事 1 回分。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reply {
    pub text: String,
    pub calls: Vec<ToolCall>,
    pub raw: Option<Value>,
    /// 途中で終わった理由（断られた、長さの上限に達した）。
    pub notice: Option<String>,
}

impl Reply {
    pub fn into_turn(self, wire: Wire) -> Turn {
        Turn::Assistant {
            text: self.text,
            calls: self.calls,
            raw: self.raw.map(|raw| (wire, raw)),
        }
    }
}

pub struct Request<'a> {
    pub model: &'a str,
    pub effort: Effort,
    pub system: &'a str,
    pub turns: &'a [Turn],
    pub tools: &'a [ToolDef],
    /// Messages API で、断られたときの回し先をサーバーに任せる（`fallbacks: "default"`）。
    pub fallbacks: bool,
    /// Messages API で、ツールと最初の指示のキャッシュを残す時間（`"1h"` など）。
    /// `None` なら API の標準（5 分）。
    pub prefix_ttl: Option<&'static str>,
    /// 中身をそのまま渡せるファイルの種類。
    pub media: Media,
    /// Responses API で、ChatGPT のプランで呼ぶ（[`crate::chatgpt`]）。返事はストリームで受け取り、
    /// ツールは名前空間 [`crate::chatgpt::NAMESPACE`] にまとめる。
    pub plan: bool,
}

/// Messages API の 1 回の返事の上限。ストリーミングなしで時間切れにならない大きさにする。
const MAX_TOKENS: u32 = 16_000;

/// リクエストの本文を組み立てる。
pub fn body(wire: Wire, request: &Request) -> Value {
    match wire {
        Wire::Messages => messages_body(request),
        Wire::Chat => chat_body(request),
        Wire::Responses => responses_body(request),
        Wire::Cursor => Value::String(cursor_prompt(request)),
        Wire::Ollama => ollama_body(request),
    }
}

/// 返事を読み取る。
pub fn parse(wire: Wire, response: &Value) -> Result<Reply, String> {
    match wire {
        Wire::Messages => messages_parse(response),
        Wire::Chat => chat_parse(response),
        Wire::Responses => responses_parse(response),
        Wire::Cursor => cursor_parse(response),
        Wire::Ollama => ollama_parse(response),
    }
}

/// API のエラーの本文から、読める文を取り出す。
pub fn error_message(response: &Value) -> Option<String> {
    let error = response.get("error")?;
    error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .map(str::to_owned)
}

fn text_of(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// 添えたファイルと、それを 1 つのメッセージにまとめるユーザーの発言。
/// `inline` はファイルごとに、中身をこのリクエストに入れるか。
struct Message<'a> {
    files: &'a [FilePart],
    inline: &'a [bool],
    text: &'a str,
}

/// 会話のファイルのうち、中身をこのリクエストに入れるもの。`Turn::Files` ごとに並ぶ。
fn inline_of(turns: &[Turn], media: Media) -> Vec<Vec<bool>> {
    let groups: Vec<&[FilePart]> = turns
        .iter()
        .filter_map(|t| match t {
            Turn::Files(parts) => Some(parts.as_slice()),
            _ => None,
        })
        .collect();
    files::inline_flags_where(groups.into_iter(), |part| media.allows(part.kind))
}

/// 会話の順に、ファイルをすぐ後ろの発言とまとめながら回る。
/// ファイルのあとに発言がなければ、文字が空の発言として渡す。
fn each_turn<'a>(turns: &'a [Turn], flags: &'a [Vec<bool>], mut f: impl FnMut(Step<'a>)) {
    let mut pending: Option<(&'a [FilePart], &'a [bool])> = None;
    let mut group = 0;
    for turn in turns {
        match turn {
            Turn::Files(parts) => {
                if let Some((files, inline)) = pending.take() {
                    f(Step::User(Message {
                        files,
                        inline,
                        text: "",
                    }));
                }
                pending = Some((parts.as_slice(), flags[group].as_slice()));
                group += 1;
            }
            Turn::User(text) => {
                let (files, inline) = pending.take().unwrap_or((&[], &[]));
                f(Step::User(Message {
                    files,
                    inline,
                    text,
                }));
            }
            other => {
                if let Some((files, inline)) = pending.take() {
                    f(Step::User(Message {
                        files,
                        inline,
                        text: "",
                    }));
                }
                f(Step::Other(other));
            }
        }
    }
    if let Some((files, inline)) = pending.take() {
        f(Step::User(Message {
            files,
            inline,
            text: "",
        }));
    }
}

enum Step<'a> {
    User(Message<'a>),
    Other(&'a Turn),
}

/// ファイルの説明と発言を 1 つの文にする。
fn user_text(message: &Message, text_below: bool) -> String {
    if message.files.is_empty() {
        return message.text.to_owned();
    }
    let note = files::describe(message.files, message.inline, text_below);
    format!("{note}\n{}", message.text)
}

/// Messages API のユーザーのメッセージの中身。画像と PDF と文字にした文書のブロックを先に置く。
/// 文字列ではなくブロックで送る。最後のメッセージにはキャッシュの区切りを付けるので、
/// 次の回も同じ形にしておく
fn messages_user(message: &Message) -> Value {
    let mut blocks = Vec::new();
    for (part, &inline) in message.files.iter().zip(message.inline) {
        match (part.kind, &part.data, &part.text) {
            (Kind::Image, Some(data), _) if inline => blocks.push(json!({
                "type": "image",
                "source": {"type": "base64", "media_type": part.mime, "data": files::base64(data)},
            })),
            (Kind::Pdf, Some(data), _) if inline => blocks.push(json!({
                "type": "document",
                "source": {"type": "base64", "media_type": "application/pdf", "data": files::base64(data)},
                "title": part.name,
            })),
            (Kind::Text, _, Some(text)) => blocks.push(json!({
                "type": "document",
                "source": {"type": "text", "media_type": "text/plain", "data": &**text},
                "title": part.name,
            })),
            _ => {}
        }
    }
    blocks.push(json!({"type": "text", "text": user_text(message, false)}));
    Value::Array(blocks)
}

/// Chat Completions API のユーザーのメッセージの中身。文字にした文書は文の中に続ける。
fn chat_user(message: &Message) -> Value {
    if message.files.is_empty() {
        return Value::String(message.text.to_owned());
    }
    let mut parts = vec![json!({"type": "text", "text": user_text(message, true)})];
    for (part, &inline) in message.files.iter().zip(message.inline) {
        match (part.kind, &part.data) {
            (Kind::Image, Some(data)) if inline => parts.push(json!({
                "type": "image_url",
                "image_url": {"url": files::data_url(&part.mime, data)},
            })),
            (Kind::Pdf, Some(data)) if inline => parts.push(json!({
                "type": "file",
                "file": {"filename": part.name, "file_data": files::data_url(&part.mime, data)},
            })),
            _ => {}
        }
    }
    Value::Array(parts)
}

/// Responses API のユーザーのメッセージの中身。
fn responses_user(message: &Message) -> Value {
    if message.files.is_empty() {
        return Value::String(message.text.to_owned());
    }
    let mut parts = vec![json!({"type": "input_text", "text": user_text(message, true)})];
    for (part, &inline) in message.files.iter().zip(message.inline) {
        match (part.kind, &part.data) {
            (Kind::Image, Some(data)) if inline => parts.push(json!({
                "type": "input_image",
                "image_url": files::data_url(&part.mime, data),
            })),
            (Kind::Pdf, Some(data)) if inline => parts.push(json!({
                "type": "input_file",
                "filename": part.name,
                "file_data": files::data_url(&part.mime, data),
            })),
            _ => {}
        }
    }
    Value::Array(parts)
}

// ---- Messages API ----

fn messages_body(request: &Request) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    // ツールの結果は、続いているものを 1 つの user メッセージにまとめる
    let mut results: Vec<Value> = Vec::new();
    let flags = inline_of(request.turns, request.media);
    each_turn(request.turns, &flags, |step| {
        let turn = match step {
            Step::User(message) => {
                if !results.is_empty() {
                    messages.push(json!({"role": "user", "content": std::mem::take(&mut results)}));
                }
                messages.push(json!({"role": "user", "content": messages_user(&message)}));
                return;
            }
            Step::Other(turn) => turn,
        };
        if !matches!(turn, Turn::Tool { .. }) && !results.is_empty() {
            messages.push(json!({"role": "user", "content": std::mem::take(&mut results)}));
        }
        match turn {
            Turn::User(_) | Turn::Files(_) => {}
            Turn::Assistant { text, calls, raw } => {
                let content = match raw {
                    Some((Wire::Messages, raw)) => raw.clone(),
                    _ => {
                        let mut blocks = Vec::new();
                        if !text.is_empty() {
                            blocks.push(json!({"type": "text", "text": text}));
                        }
                        for call in calls {
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": call.id,
                                "name": call.name,
                                "input": call.args,
                            }));
                        }
                        Value::Array(blocks)
                    }
                };
                if content.as_array().is_some_and(|blocks| !blocks.is_empty()) {
                    messages.push(json!({"role": "assistant", "content": content}));
                }
            }
            Turn::Tool {
                id,
                output,
                is_error,
                ..
            } => results.push(json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": output,
                "is_error": is_error,
            })),
        }
    });
    if !results.is_empty() {
        messages.push(json!({"role": "user", "content": results}));
    }
    let mut tools: Vec<Value> = request
        .tools
        .iter()
        .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.schema}))
        .collect();
    // プロンプトキャッシュ。並びは tools → system → messages で、前から同じ部分を読み直さずに済む。
    // 区切りは 3 つ: ツールの最後（最初の指示を変えても、ツールの分は残る）、最初の指示の最後
    // （会話をまたいで同じ）、最後のメッセージの最後（同じ会話の次の回が、ここまでを読む）
    let mut prefix = json!({"type": "ephemeral"});
    if let Some(ttl) = request.prefix_ttl {
        prefix["ttl"] = ttl.into();
    }
    if let Some(last) = tools.last_mut() {
        last["cache_control"] = prefix.clone();
    }
    if let Some(block) = messages
        .last_mut()
        .filter(|m| m["role"] == "user")
        .and_then(|m| m["content"].as_array_mut())
        .and_then(|blocks| blocks.last_mut())
    {
        block["cache_control"] = json!({"type": "ephemeral"});
    }
    let mut body = json!({
        "model": request.model,
        "max_tokens": MAX_TOKENS,
        "messages": messages,
        "tools": tools,
    });
    // 空の文のブロックはサーバーが断るので、最初の指示がなければ送らない
    if !request.system.is_empty() {
        body["system"] = json!([{
            "type": "text",
            "text": request.system,
            "cache_control": prefix,
        }]);
    }
    if let Some(effort) = request.effort.wire() {
        body["output_config"] = json!({"effort": effort});
    }
    if request.fallbacks {
        body["fallbacks"] = "default".into();
    }
    body
}

fn messages_parse(response: &Value) -> Result<Reply, String> {
    let content = response
        .get("content")
        .and_then(Value::as_array)
        .ok_or("返事に content がありません")?;
    let mut reply = Reply {
        raw: Some(Value::Array(content.clone())),
        ..Default::default()
    };
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => reply.text.push_str(&text_of(block, "text")),
            Some("tool_use") => reply.calls.push(ToolCall {
                id: text_of(block, "id"),
                name: text_of(block, "name"),
                args: block.get("input").cloned().unwrap_or_else(|| json!({})),
            }),
            _ => {}
        }
    }
    reply.notice = match response.get("stop_reason").and_then(Value::as_str) {
        Some("refusal") => Some("モデルがこの依頼への回答を断りました。".into()),
        Some("max_tokens") => Some("返事が長さの上限に達して、途中で終わりました。".into()),
        _ => None,
    };
    if reply.notice.is_some() {
        // 途中で切れたツールの呼び出しは実行しない
        reply.calls.clear();
        reply.raw = None;
    }
    Ok(reply)
}

// ---- Chat Completions API ----

fn chat_body(request: &Request) -> Value {
    let mut messages = vec![json!({"role": "system", "content": request.system})];
    let flags = inline_of(request.turns, request.media);
    each_turn(request.turns, &flags, |step| {
        let turn = match step {
            Step::User(message) => {
                messages.push(json!({"role": "user", "content": chat_user(&message)}));
                return;
            }
            Step::Other(turn) => turn,
        };
        match turn {
            Turn::User(_) | Turn::Files(_) => {}
            Turn::Assistant { text, calls, .. } => {
                if text.is_empty() && calls.is_empty() {
                    return;
                }
                let mut message = Map::new();
                message.insert("role".into(), "assistant".into());
                message.insert("content".into(), text.clone().into());
                if !calls.is_empty() {
                    let calls: Vec<Value> = calls
                        .iter()
                        .map(|c| {
                            json!({
                                "id": c.id,
                                "type": "function",
                                "function": {"name": c.name, "arguments": c.args.to_string()},
                            })
                        })
                        .collect();
                    message.insert("tool_calls".into(), calls.into());
                }
                messages.push(Value::Object(message));
            }
            Turn::Tool { id, output, .. } => {
                messages.push(json!({"role": "tool", "tool_call_id": id, "content": output}))
            }
        }
    });
    let tools: Vec<Value> = request
        .tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {"name": t.name, "description": t.description, "parameters": t.schema},
            })
        })
        .collect();
    let mut body = json!({"model": request.model, "messages": messages});
    // ツールを渡さないときは、空の一覧も送らない（空の tools を断るサーバーがある）
    if !tools.is_empty() {
        body["tools"] = tools.into();
    }
    if let Some(effort) = request.effort.wire() {
        body["reasoning_effort"] = effort.into();
    }
    body
}

/// 文字列で届く引数を JSON にする。壊れていたら、そのことを AI に返せるようにエラーにする。
fn parse_arguments(name: &str, arguments: Option<&Value>) -> Result<Value, String> {
    match arguments {
        Some(Value::String(s)) if s.trim().is_empty() => Ok(json!({})),
        Some(Value::String(s)) => serde_json::from_str(s)
            .map_err(|e| format!("ツール {name} の引数が JSON として読めません: {e}")),
        Some(Value::Object(map)) => Ok(Value::Object(map.clone())),
        _ => Ok(json!({})),
    }
}

fn chat_parse(response: &Value) -> Result<Reply, String> {
    let choice = response
        .get("choices")
        .and_then(|c| c.get(0))
        .ok_or("返事に choices がありません")?;
    let message = choice.get("message").ok_or("返事に message がありません")?;
    let mut reply = Reply {
        text: text_of(message, "content"),
        ..Default::default()
    };
    for call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let function = call
            .get("function")
            .ok_or("tool_calls に function がありません")?;
        let name = text_of(function, "name");
        reply.calls.push(ToolCall {
            id: text_of(call, "id"),
            args: parse_arguments(&name, function.get("arguments"))?,
            name,
        });
    }
    if let Some(refusal) = message.get("refusal").and_then(Value::as_str) {
        reply.notice = Some(format!("モデルがこの依頼への回答を断りました。{refusal}"));
    } else if choice.get("finish_reason").and_then(Value::as_str) == Some("length") {
        reply.notice = Some("返事が長さの上限に達して、途中で終わりました。".into());
        reply.calls.clear();
    }
    Ok(reply)
}

// ---- Responses API ----

fn responses_body(request: &Request) -> Value {
    let mut input: Vec<Value> = Vec::new();
    let flags = inline_of(request.turns, request.media);
    each_turn(request.turns, &flags, |step| {
        let turn = match step {
            Step::User(message) => {
                input.push(json!({"role": "user", "content": responses_user(&message)}));
                return;
            }
            Step::Other(turn) => turn,
        };
        match turn {
            Turn::User(_) | Turn::Files(_) => {}
            Turn::Assistant { text, calls, raw } => match raw {
                Some((Wire::Responses, Value::Array(items))) => input.extend(items.iter().cloned()),
                _ => {
                    if !text.is_empty() {
                        input.push(json!({"role": "assistant", "content": text}));
                    }
                    for call in calls {
                        let mut item = json!({
                            "type": "function_call",
                            "call_id": call.id,
                            "name": call.name,
                            "arguments": call.args.to_string(),
                        });
                        if request.plan {
                            item["namespace"] = json!(crate::chatgpt::NAMESPACE);
                        }
                        input.push(item);
                    }
                }
            },
            Turn::Tool { id, output, .. } => input.push(json!({
                "type": "function_call_output",
                "call_id": id,
                "output": output,
            })),
        }
    });
    let tools: Vec<Value> = request
        .tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "name": t.name,
                "description": t.description,
                "parameters": t.schema,
            })
        })
        .collect();
    let tools = if request.plan && !tools.is_empty() {
        // ChatGPT のプランでは、関数のツールを名前空間に入れて渡す
        vec![json!({
            "type": "namespace",
            "name": crate::chatgpt::NAMESPACE,
            "description": "Malakt Mahoz のタスクと画面を読み書きするツール",
            "tools": tools,
        })]
    } else {
        tools
    };
    let mut body = json!({
        "model": request.model,
        "instructions": request.system,
        "input": input,
        "tools": tools,
        // 会話はサーバーに残さず、毎回こちらから全部送る
        "store": false,
        "include": ["reasoning.encrypted_content"],
    });
    if request.plan {
        // ChatGPT のプランは、ストリームでしか受け付けない
        body["stream"] = json!(true);
    }
    if let Some(effort) = request.effort.wire() {
        body["reasoning"] = json!({"effort": effort});
    }
    body
}

fn responses_parse(response: &Value) -> Result<Reply, String> {
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or("返事に output がありません")?;
    let mut reply = Reply {
        raw: Some(Value::Array(output.clone())),
        ..Default::default()
    };
    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                for part in item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    match part.get("type").and_then(Value::as_str) {
                        Some("output_text") => reply.text.push_str(&text_of(part, "text")),
                        Some("refusal") => {
                            reply.notice = Some(format!(
                                "モデルがこの依頼への回答を断りました。{}",
                                text_of(part, "refusal")
                            ))
                        }
                        _ => {}
                    }
                }
            }
            Some("function_call") => {
                let name = text_of(item, "name");
                reply.calls.push(ToolCall {
                    id: text_of(item, "call_id"),
                    args: parse_arguments(&name, item.get("arguments"))?,
                    name,
                });
            }
            _ => {}
        }
    }
    if response.get("status").and_then(Value::as_str) == Some("incomplete") {
        reply.notice = Some("返事が途中で終わりました。".into());
        reply.calls.clear();
        reply.raw = None;
    }
    Ok(reply)
}

// ---- Ollama の API ----

/// ツールの呼び出しに付ける ID。Ollama の返事には ID がないので、会話の中で重ならないものを作る。
fn ollama_call_id(index: usize) -> String {
    format!("ollama-{}-{index}", malakt_core::new_id())
}

fn ollama_body(request: &Request) -> Value {
    let mut messages = vec![json!({"role": "system", "content": request.system})];
    let flags = inline_of(request.turns, request.media);
    each_turn(request.turns, &flags, |step| {
        let turn = match step {
            Step::User(message) => {
                let mut user = json!({"role": "user", "content": user_text(&message, true)});
                let images: Vec<Value> = message
                    .files
                    .iter()
                    .zip(message.inline)
                    .filter_map(|(part, &inline)| match (part.kind, &part.data) {
                        (Kind::Image, Some(data)) if inline => Some(files::base64(data).into()),
                        _ => None,
                    })
                    .collect();
                if !images.is_empty() {
                    user["images"] = images.into();
                }
                messages.push(user);
                return;
            }
            Step::Other(turn) => turn,
        };
        match turn {
            Turn::User(_) | Turn::Files(_) => {}
            Turn::Assistant { text, calls, .. } => {
                if text.is_empty() && calls.is_empty() {
                    return;
                }
                let mut message = json!({"role": "assistant", "content": text});
                if !calls.is_empty() {
                    let calls: Vec<Value> = calls
                        .iter()
                        .map(|c| json!({"type": "function", "function": {"name": c.name, "arguments": c.args}}))
                        .collect();
                    message["tool_calls"] = calls.into();
                }
                messages.push(message);
            }
            // Ollama は、結果をツールの名前で呼び出しと結ぶ
            Turn::Tool { name, output, .. } => {
                messages.push(json!({"role": "tool", "tool_name": name, "content": output}))
            }
        }
    });
    let mut body = json!({"model": request.model, "messages": messages, "stream": false});
    if !request.tools.is_empty() {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {"name": t.name, "description": t.description, "parameters": t.schema},
                })
            })
            .collect();
        body["tools"] = tools.into();
    }
    // 「標準」は送らず、モデルの標準に任せる（考えるモデルは、標準で考える）
    if let Some(effort) = request.effort.wire() {
        body["think"] = effort.into();
    }
    body
}

fn ollama_parse(response: &Value) -> Result<Reply, String> {
    let message = response
        .get("message")
        .ok_or("返事に message がありません")?;
    let mut reply = Reply {
        text: text_of(message, "content"),
        ..Default::default()
    };
    for (index, call) in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let function = call
            .get("function")
            .ok_or("tool_calls に function がありません")?;
        let name = text_of(function, "name");
        reply.calls.push(ToolCall {
            id: ollama_call_id(index),
            args: parse_arguments(&name, function.get("arguments"))?,
            name,
        });
    }
    if response.get("done_reason").and_then(Value::as_str) == Some("length") {
        reply.notice = Some("返事が長さの上限に達して、途中で終わりました。".into());
        reply.calls.clear();
    }
    Ok(reply)
}

// ---- Cursor の CLI ----

/// ツールの呼び出しを書いてもらうコードブロックの名前。
const CURSOR_FENCE: &str = "malakt-tool";

/// CLI には会話もツールも渡せないので、全部を 1 つの文章にして渡す。
fn cursor_prompt(request: &Request) -> String {
    let mut prompt = String::new();
    prompt.push_str(request.system);
    prompt.push_str(
        "\n\n# ツールの使い方\n\
         あなた自身のファイル操作やシェルは使わず、下のツールだけを使ってください。\n\
         ただし、ユーザーが添えたファイルは、書いてあるパスのファイルを読んでかまいません。\n\
         ツールを使うときは、次の形のコードブロックを返事に書きます（1 回の返事にいくつ書いてもかまいません）。\n\
         ```malakt-tool\n{\"name\": \"ツール名\", \"arguments\": {}}\n```\n\
         ブロックを書いたら、そこで返事を終えてください。結果が次の入力で届きます。\n\
         ツールが要らなければ、ふつうに返事を書きます。\n\n# ツール\n",
    );
    for tool in request.tools {
        prompt.push_str(&format!(
            "- {}: {}\n  引数: {}\n",
            tool.name, tool.description, tool.schema
        ));
    }
    prompt.push_str("\n# これまでの会話\n");
    // CLI には画像や PDF を渡せないので、すべてパスで伝え、文字にしたものは文の中に続ける
    let flags: Vec<Vec<bool>> = inline_of(request.turns, request.media)
        .into_iter()
        .map(|g| vec![false; g.len()])
        .collect();
    let mut texts: Vec<String> = Vec::new();
    each_turn(request.turns, &flags, |step| {
        if let Step::User(message) = step {
            texts.push(user_text(&message, true));
        }
    });
    let mut texts = texts.into_iter();
    for turn in request.turns {
        match turn {
            Turn::Files(_) => {}
            Turn::User(_) => {
                let text = texts.next().unwrap_or_default();
                prompt.push_str(&format!("## ユーザー\n{text}\n\n"))
            }
            Turn::Assistant { text, calls, .. } => {
                prompt.push_str("## アシスタント\n");
                if !text.is_empty() {
                    prompt.push_str(text);
                    prompt.push('\n');
                }
                for call in calls {
                    let block = json!({"name": call.name, "arguments": call.args});
                    prompt.push_str(&format!("```{CURSOR_FENCE}\n{block}\n```\n"));
                }
                prompt.push('\n');
            }
            Turn::Tool {
                name,
                output,
                is_error,
                ..
            } => {
                let kind = if *is_error { "エラー" } else { "結果" };
                prompt.push_str(&format!("## ツール {name} の{kind}\n{output}\n\n"));
            }
        }
    }
    prompt.push_str("## アシスタント\n");
    prompt
}

/// `agent -p --output-format json` の出力を読む。
fn cursor_parse(response: &Value) -> Result<Reply, String> {
    let result = response
        .get("result")
        .and_then(Value::as_str)
        .ok_or("Cursor の出力に result がありません")?;
    let mut reply = Reply::default();
    let mut rest = result;
    let open = format!("```{CURSOR_FENCE}");
    while let Some(start) = rest.find(&open) {
        reply.text.push_str(&rest[..start]);
        let after = &rest[start + open.len()..];
        let Some(end) = after.find("```") else {
            return Err("Cursor の返事のツール呼び出しが、途中で切れています".into());
        };
        let block: Value = serde_json::from_str(after[..end].trim())
            .map_err(|e| format!("Cursor の返事のツール呼び出しが JSON として読めません: {e}"))?;
        reply.calls.push(ToolCall {
            id: format!("cursor-{}", reply.calls.len()),
            name: text_of(&block, "name"),
            args: block.get("arguments").cloned().unwrap_or_else(|| json!({})),
        });
        rest = &after[end + 3..];
    }
    reply.text.push_str(rest);
    reply.text = reply.text.trim().to_owned();
    Ok(reply)
}

/// 会話の中の、考えた内容のブロックをすべて外す。外したものがあれば `true`。
/// 最初の指示やツールが変わったあとは、前の考えのブロックを送り返すとサーバーが断る。
/// すべて外せば、そのまま続けられる。
pub fn strip_thinking(turns: &mut [Turn]) -> bool {
    let mut stripped = false;
    for turn in turns {
        let Turn::Assistant { raw, .. } = turn else {
            continue;
        };
        let Some((Wire::Messages, Value::Array(blocks))) = raw else {
            continue;
        };
        let before = blocks.len();
        blocks.retain(|block| {
            !matches!(
                block["type"].as_str(),
                Some("thinking" | "redacted_thinking")
            )
        });
        stripped |= blocks.len() != before;
        if blocks.is_empty() {
            // 残るものがなければ、文とツールの呼び出しから組み立て直す
            *raw = None;
        }
    }
    stripped
}

/// サーバーが、考えた内容のブロックを別の会話のものとして断ったか。
pub fn is_thinking_mismatch(error: &str) -> bool {
    error.contains("`signature` in `thinking` block")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools() -> Vec<ToolDef> {
        vec![ToolDef {
            name: "list_tasks",
            description: "タスクの一覧",
            schema: json!({"type": "object", "properties": {}}),
        }]
    }

    fn conversation(raw: Option<(Wire, Value)>) -> Vec<Turn> {
        vec![
            Turn::User("一覧を見せて".into()),
            Turn::Assistant {
                text: "調べます".into(),
                calls: vec![
                    ToolCall {
                        id: "a".into(),
                        name: "list_tasks".into(),
                        args: json!({"query": "牛乳"}),
                    },
                    ToolCall {
                        id: "b".into(),
                        name: "list_people".into(),
                        args: json!({}),
                    },
                ],
                raw,
            },
            Turn::Tool {
                id: "a".into(),
                name: "list_tasks".into(),
                output: "[]".into(),
                is_error: false,
            },
            Turn::Tool {
                id: "b".into(),
                name: "list_people".into(),
                output: "失敗".into(),
                is_error: true,
            },
            Turn::Assistant {
                text: "ありません".into(),
                calls: vec![],
                raw: None,
            },
        ]
    }

    fn request<'a>(turns: &'a [Turn], tools: &'a [ToolDef], effort: Effort) -> Request<'a> {
        Request {
            model: "m",
            effort,
            system: "system",
            turns,
            tools,
            fallbacks: false,
            prefix_ttl: None,
            media: Media::All,
            plan: false,
        }
    }

    #[test]
    fn messages_body_groups_tool_results_into_one_user_message() {
        let (turns, tools) = (conversation(None), tools());
        let body = body(Wire::Messages, &request(&turns, &tools, Effort::High));
        assert_eq!(body["model"], "m");
        assert_eq!(
            body["system"],
            json!([{"type": "text", "text": "system", "cache_control": {"type": "ephemeral"}}])
        );
        assert_eq!(body["max_tokens"], 16000);
        assert_eq!(body["output_config"], json!({"effort": "high"}));
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 4);
        assert_eq!(
            messages[0],
            json!({"role": "user", "content": [{"type": "text", "text": "一覧を見せて"}]})
        );
        assert_eq!(
            messages[1]["content"],
            json!([
                {"type": "text", "text": "調べます"},
                {"type": "tool_use", "id": "a", "name": "list_tasks", "input": {"query": "牛乳"}},
                {"type": "tool_use", "id": "b", "name": "list_people", "input": {}},
            ])
        );
        assert_eq!(
            messages[2],
            json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "a", "content": "[]", "is_error": false},
                {"type": "tool_result", "tool_use_id": "b", "content": "失敗", "is_error": true},
            ]})
        );
        assert_eq!(messages[3]["role"], "assistant");
    }

    /// プロンプトキャッシュの区切りは、ツールの最後・最初の指示・最後のメッセージの 3 つ。
    /// 区切りを外せば、前の回の本文が次の回の本文の頭とそのまま同じになる。
    #[test]
    fn messages_body_marks_cache_breakpoints() {
        let mut turns = vec![Turn::User("一覧を見せて".into())];
        let mut tools = tools();
        tools.push(ToolDef {
            name: "list_people",
            description: "担当者の一覧",
            schema: json!({"type": "object", "properties": {}}),
        });
        let mut long = request(&turns, &tools, Effort::Default);
        long.prefix_ttl = Some("1h");
        let first = body(Wire::Messages, &long);
        let hour = json!({"type": "ephemeral", "ttl": "1h"});
        assert!(first["tools"][0].get("cache_control").is_none());
        assert_eq!(first["tools"][1]["cache_control"], hour);
        assert_eq!(first["system"][0]["cache_control"], hour);
        assert_eq!(
            first["messages"][0]["content"][0]["cache_control"],
            json!({"type": "ephemeral"}),
            "会話の区切りは 5 分（長いものが先、の決まりを守る）"
        );
        let breakpoints = |body: &Value| body.to_string().matches("cache_control").count();
        assert_eq!(breakpoints(&first), 3, "区切りは 4 つまで");

        // ツールを呼んで、結果を返す 2 回目。区切りは最後のツールの結果に移る
        turns.extend(conversation(None)[1..4].iter().cloned());
        let mut long = request(&turns, &tools, Effort::Default);
        long.prefix_ttl = Some("1h");
        let second = body(Wire::Messages, &long);
        assert_eq!(breakpoints(&second), 3);
        let results = second["messages"][2]["content"].as_array().unwrap();
        assert!(results[0].get("cache_control").is_none());
        assert_eq!(results[1]["cache_control"], json!({"type": "ephemeral"}));

        fn strip(value: &mut Value) {
            match value {
                Value::Object(map) => {
                    map.remove("cache_control");
                    map.values_mut().for_each(strip);
                }
                Value::Array(items) => items.iter_mut().for_each(strip),
                _ => {}
            }
        }
        let (mut first, mut second) = (first, second);
        strip(&mut first);
        strip(&mut second);
        assert_eq!(first["tools"], second["tools"]);
        assert_eq!(first["system"], second["system"]);
        assert_eq!(first["messages"][0], second["messages"][0]);

        // 標準の 5 分なら ttl は送らない。最初の指示が空なら system ごと送らない
        let mut short = request(&turns, &tools, Effort::Default);
        short.system = "";
        let short = body(Wire::Messages, &short);
        assert_eq!(
            short["tools"][1]["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert!(short.get("system").is_none());
    }

    #[test]
    fn messages_body_replays_raw_blocks_unchanged_and_omits_default_effort() {
        let raw = json!([
            {"type": "thinking", "thinking": "", "signature": "sig"},
            {"type": "tool_use", "id": "a", "name": "list_tasks", "input": {}},
        ]);
        let turns = conversation(Some((Wire::Messages, raw.clone())));
        let tools = tools();
        let body = body(Wire::Messages, &request(&turns, &tools, Effort::Default));
        assert_eq!(body["messages"][1]["content"], raw);
        assert!(body.get("output_config").is_none());
        assert!(body.get("fallbacks").is_none());
        let mut with_fallbacks = request(&turns, &tools, Effort::Default);
        with_fallbacks.fallbacks = true;
        assert_eq!(
            super::body(Wire::Messages, &with_fallbacks)["fallbacks"],
            "default"
        );
        // ほかの形式で受け取った中身は、そのままは使わない
        let turns = conversation(Some((Wire::Responses, json!([{"type": "reasoning"}]))));
        let body = super::body(Wire::Messages, &request(&turns, &tools, Effort::Default));
        assert_eq!(body["messages"][1]["content"][0]["type"], "text");
    }

    #[test]
    fn messages_body_ends_with_pending_tool_results_and_skips_empty_turns() {
        let mut turns = conversation(None);
        turns.pop();
        turns.insert(
            1,
            Turn::Assistant {
                text: String::new(),
                calls: vec![],
                raw: None,
            },
        );
        let tools = tools();
        let body = body(Wire::Messages, &request(&turns, &tools, Effort::Default));
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2]["content"][1]["tool_use_id"], "b");
    }

    #[test]
    fn thinking_blocks_are_stripped_from_every_turn() {
        let raw = json!([
            {"type": "thinking", "thinking": "", "signature": "sig"},
            {"type": "redacted_thinking", "data": "x"},
            {"type": "tool_use", "id": "a", "name": "list_tasks", "input": {}},
        ]);
        let mut turns = conversation(Some((Wire::Messages, raw)));
        assert!(strip_thinking(&mut turns));
        let tools = tools();
        let body = body(Wire::Messages, &request(&turns, &tools, Effort::Default));
        assert_eq!(
            body["messages"][1]["content"],
            json!([{"type": "tool_use", "id": "a", "name": "list_tasks", "input": {}}])
        );
        assert!(!strip_thinking(&mut turns));
        assert!(is_thinking_mismatch(
            "サーバーがエラーを返しました（400）。messages.1.content.0: Invalid `signature` in `thinking` block."
        ));
        assert!(!is_thinking_mismatch(
            "サーバーがエラーを返しました（429）。"
        ));
    }

    #[test]
    fn messages_reply_is_read() {
        let response = json!({
            "stop_reason": "tool_use",
            "content": [
                {"type": "thinking", "thinking": "", "signature": "s"},
                {"type": "text", "text": "調べ"},
                {"type": "text", "text": "ます"},
                {"type": "tool_use", "id": "t1", "name": "get_task", "input": {"id": "x"}},
            ],
        });
        let reply = parse(Wire::Messages, &response).unwrap();
        assert_eq!(reply.text, "調べます");
        assert_eq!(
            reply.calls,
            vec![ToolCall {
                id: "t1".into(),
                name: "get_task".into(),
                args: json!({"id": "x"})
            }]
        );
        assert_eq!(reply.raw, Some(response["content"].clone()));
        assert_eq!(reply.notice, None);
        assert!(parse(Wire::Messages, &json!({"type": "error"})).is_err());
    }

    #[test]
    fn refusal_and_truncation_are_reported_and_run_no_tools() {
        for (reason, word) in [("refusal", "断りました"), ("max_tokens", "上限")] {
            let response = json!({
                "stop_reason": reason,
                "content": [{"type": "tool_use", "id": "t", "name": "delete_task", "input": {}}],
            });
            let reply = parse(Wire::Messages, &response).unwrap();
            assert!(reply.notice.unwrap().contains(word));
            assert!(reply.calls.is_empty());
            assert_eq!(reply.raw, None);
        }
        let response = json!({"choices": [{"finish_reason": "length", "message": {
            "content": "a",
            "tool_calls": [{"id": "1", "function": {"name": "x", "arguments": "{}"}}],
        }}]});
        let reply = parse(Wire::Chat, &response).unwrap();
        assert!(reply.notice.unwrap().contains("上限"));
        assert!(reply.calls.is_empty());
        let response = json!({"choices": [{"message": {"content": null, "refusal": "だめ"}}]});
        assert!(parse(Wire::Chat, &response)
            .unwrap()
            .notice
            .unwrap()
            .contains("だめ"));
        let response = json!({"status": "incomplete", "output": [
            {"type": "function_call", "call_id": "c", "name": "x", "arguments": "{}"},
        ]});
        let reply = parse(Wire::Responses, &response).unwrap();
        assert!(reply.notice.is_some());
        assert!(reply.calls.is_empty());
    }

    #[test]
    fn chat_body_and_reply() {
        let (turns, tools) = (conversation(None), tools());
        let body = body(Wire::Chat, &request(&turns, &tools, Effort::Low));
        assert_eq!(body["reasoning_effort"], "low");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "list_tasks");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 6);
        assert_eq!(messages[0], json!({"role": "system", "content": "system"}));
        assert_eq!(
            messages[2]["tool_calls"][0],
            json!({"id": "a", "type": "function",
                   "function": {"name": "list_tasks", "arguments": "{\"query\":\"牛乳\"}"}})
        );
        assert_eq!(
            messages[3],
            json!({"role": "tool", "tool_call_id": "a", "content": "[]"})
        );
        assert!(messages[5].get("tool_calls").is_none());
        let none = super::body(Wire::Chat, &request(&turns, &tools, Effort::Default));
        assert!(none.get("reasoning_effort").is_none());

        let response = json!({"choices": [{"finish_reason": "tool_calls", "message": {
            "content": null,
            "tool_calls": [
                {"id": "c1", "type": "function",
                 "function": {"name": "create_task", "arguments": "{\"title\":\"買い物\"}"}},
                {"id": "c2", "type": "function", "function": {"name": "list_people", "arguments": ""}},
            ],
        }}]});
        let reply = parse(Wire::Chat, &response).unwrap();
        assert_eq!(reply.text, "");
        assert_eq!(reply.calls[0].args, json!({"title": "買い物"}));
        assert_eq!(reply.calls[1].args, json!({}));
        assert_eq!(reply.raw, None);

        let broken = json!({"choices": [{"message": {"tool_calls": [
            {"id": "c", "function": {"name": "create_task", "arguments": "{\"title\":"}},
        ]}}]});
        assert!(parse(Wire::Chat, &broken)
            .unwrap_err()
            .contains("create_task"));
        assert!(parse(Wire::Chat, &json!({"choices": []})).is_err());
    }

    #[test]
    fn responses_body_and_reply() {
        let (turns, tools) = (conversation(None), tools());
        let body = body(Wire::Responses, &request(&turns, &tools, Effort::Max));
        assert_eq!(body["instructions"], "system");
        assert_eq!(body["store"], false);
        assert_eq!(body["reasoning"], json!({"effort": "max"}));
        assert_eq!(body["tools"][0]["name"], "list_tasks");
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 7);
        assert_eq!(
            input[1],
            json!({"role": "assistant", "content": "調べます"})
        );
        assert_eq!(
            input[2],
            json!({"type": "function_call", "call_id": "a", "name": "list_tasks",
                   "arguments": "{\"query\":\"牛乳\"}"})
        );
        assert_eq!(
            input[4],
            json!({"type": "function_call_output", "call_id": "a", "output": "[]"})
        );

        let response = json!({"status": "completed", "output": [
            {"type": "reasoning", "id": "r", "encrypted_content": "x"},
            {"type": "message", "content": [{"type": "output_text", "text": "はい"}]},
            {"type": "function_call", "call_id": "c1", "name": "get_task", "arguments": "{\"id\":\"1\"}"},
        ]});
        let reply = parse(Wire::Responses, &response).unwrap();
        assert_eq!(reply.text, "はい");
        assert_eq!(reply.calls[0].id, "c1");
        assert_eq!(reply.calls[0].args, json!({"id": "1"}));
        // 返ってきた項目は、次に送るときそのまま並べる
        let turns = vec![reply.into_turn(Wire::Responses)];
        let body = super::body(Wire::Responses, &request(&turns, &tools, Effort::Default));
        assert_eq!(body["input"], response["output"]);
        assert!(body.get("reasoning").is_none());
        assert!(body.get("stream").is_none());
    }

    #[test]
    fn the_chatgpt_plan_streams_and_puts_tools_in_a_namespace() {
        let (turns, tools) = (conversation(None), tools());
        let mut plan = request(&turns, &tools, Effort::Default);
        plan.plan = true;
        let body = body(Wire::Responses, &plan);
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        let namespaces = body["tools"].as_array().unwrap();
        assert_eq!(namespaces.len(), 1);
        assert_eq!(namespaces[0]["type"], "namespace");
        assert_eq!(namespaces[0]["name"], crate::chatgpt::NAMESPACE);
        assert_eq!(namespaces[0]["tools"][0]["name"], "list_tasks");
        assert_eq!(namespaces[0]["tools"][0]["type"], "function");
        // ほかの形式から続けた呼び出しにも、名前空間を付ける
        assert_eq!(body["input"][2]["namespace"], crate::chatgpt::NAMESPACE);
        assert_eq!(body["input"][2]["name"], "list_tasks");

        // ツールがなければ、名前空間も送らない
        let none = super::body(Wire::Responses, &Request { tools: &[], ..plan });
        assert_eq!(none["tools"], json!([]));

        // 名前空間の中の関数の呼び出しは、名前だけで読む
        let response = json!({"status": "completed", "output": [
            {"type": "function_call", "call_id": "c1", "name": "get_task",
             "namespace": "malakt", "arguments": "{\"id\":\"1\"}"},
        ]});
        let reply = parse(Wire::Responses, &response).unwrap();
        assert_eq!(reply.calls[0].name, "get_task");
    }

    #[test]
    fn cursor_prompt_carries_tools_and_history() {
        let (turns, tools) = (conversation(None), tools());
        let Value::String(prompt) = body(Wire::Cursor, &request(&turns, &tools, Effort::Default))
        else {
            panic!("文章になる");
        };
        assert!(prompt.starts_with("system"));
        assert!(prompt.contains("- list_tasks: タスクの一覧"));
        assert!(prompt.contains("## ユーザー\n一覧を見せて"));
        // キーの並びは serde_json の設定で変わる
        assert!(prompt.contains("```malakt-tool\n{"));
        assert!(prompt.contains("\"arguments\":{\"query\":\"牛乳\"}"));
        assert!(prompt.contains("\"name\":\"list_tasks\""));
        assert!(prompt.contains("## ツール list_tasks の結果\n[]"));
        assert!(prompt.contains("## ツール list_people のエラー\n失敗"));
        assert!(prompt.ends_with("## アシスタント\n"));
    }

    #[test]
    fn cursor_reply_is_split_into_text_and_calls() {
        let result = "確認します。\n```malakt-tool\n{\"name\": \"get_task\", \"arguments\": {\"id\": \"1\"}}\n```\n\
                      ```malakt-tool\n{\"name\": \"list_people\"}\n```";
        let reply = parse(Wire::Cursor, &json!({"type": "result", "result": result})).unwrap();
        assert_eq!(reply.text, "確認します。");
        assert_eq!(reply.calls.len(), 2);
        assert_eq!(reply.calls[0].name, "get_task");
        assert_eq!(reply.calls[0].args, json!({"id": "1"}));
        assert_eq!(reply.calls[1].args, json!({}));
        assert_ne!(reply.calls[0].id, reply.calls[1].id);

        let plain = parse(Wire::Cursor, &json!({"result": "こんにちは"})).unwrap();
        assert_eq!((plain.text.as_str(), plain.calls.len()), ("こんにちは", 0));
        assert!(parse(Wire::Cursor, &json!({"result": "```malakt-tool\n{"})).is_err());
        assert!(parse(Wire::Cursor, &json!({"result": "```malakt-tool\nx\n```"})).is_err());
        assert!(parse(Wire::Cursor, &json!({})).is_err());
    }

    #[test]
    fn error_bodies_are_readable() {
        let anthropic = json!({"type": "error", "error": {"type": "x", "message": "だめ"}});
        assert_eq!(error_message(&anthropic), Some("だめ".into()));
        assert_eq!(
            error_message(&json!({"error": "短い"})),
            Some("短い".into())
        );
        assert_eq!(error_message(&json!({"ok": true})), None);
    }

    fn attached() -> Vec<Turn> {
        let part = |name: &str, mime: &str, kind: Kind, data: Option<&[u8]>, text: Option<&str>| {
            FilePart {
                name: name.into(),
                mime: mime.into(),
                kind,
                size: 3,
                hash: "h".into(),
                path: std::path::PathBuf::from(format!("/files/{name}")),
                data: data.map(|d| std::sync::Arc::new(d.to_vec())),
                text: text.map(std::sync::Arc::from),
                note: None,
            }
        };
        vec![
            Turn::Files(vec![
                part("a.png", "image/png", Kind::Image, Some(b"png"), None),
                part("b.pdf", "application/pdf", Kind::Pdf, Some(b"pdf"), None),
                part("c.txt", "text/plain", Kind::Text, None, Some("中身")),
                part("d.mp3", "audio/mpeg", Kind::Other, None, None),
            ]),
            Turn::User("これを見て".into()),
        ]
    }

    #[test]
    fn files_go_into_the_user_message_of_each_format() {
        let (turns, tools) = (attached(), tools());
        let req = request(&turns, &tools, Effort::Default);

        let messages = body(Wire::Messages, &req);
        let content = &messages["messages"][0]["content"];
        assert_eq!(messages["messages"].as_array().unwrap().len(), 1);
        assert_eq!(content[0]["type"], "image");
        assert_eq!(content[0]["source"]["data"], "cG5n");
        assert_eq!(content[0]["source"]["media_type"], "image/png");
        assert_eq!(content[1]["type"], "document");
        assert_eq!(content[1]["source"]["media_type"], "application/pdf");
        assert_eq!(content[1]["title"], "b.pdf");
        assert_eq!(
            content[2]["source"],
            json!({"type": "text", "media_type": "text/plain", "data": "中身"})
        );
        let text = content[3]["text"].as_str().unwrap();
        assert!(text.contains("「d.mp3」"), "{text}");
        assert!(text.contains("/files/d.mp3"), "{text}");
        assert!(text.ends_with("これを見て"), "{text}");

        let chat = body(Wire::Chat, &req);
        let content = &chat["messages"][1]["content"];
        let text = content[0]["text"].as_str().unwrap();
        assert!(text.contains("[「c.txt」の中身]\n中身"), "{text}");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,cG5n");
        assert_eq!(
            content[2]["file"]["file_data"],
            "data:application/pdf;base64,cGRm"
        );

        let responses = body(Wire::Responses, &req);
        let content = &responses["input"][0]["content"];
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[1]["image_url"], "data:image/png;base64,cG5n");
        assert_eq!(content[2]["filename"], "b.pdf");

        let cursor = body(Wire::Cursor, &req);
        let prompt = cursor.as_str().unwrap();
        assert!(prompt.contains("パス: /files/a.png"), "{prompt}");
        assert!(prompt.contains("[「c.txt」の中身]\n中身"), "{prompt}");
        assert!(!prompt.contains("cG5n"));
    }

    #[test]
    fn ollama_body_and_reply() {
        let (turns, tools) = (conversation(None), tools());
        let body = body(Wire::Ollama, &request(&turns, &tools, Effort::High));
        assert_eq!(body["stream"], false);
        assert_eq!(body["think"], "high");
        assert_eq!(body["tools"][0]["function"]["name"], "list_tasks");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0], json!({"role": "system", "content": "system"}));
        // 引数は文字列ではなく、JSON のまま
        assert_eq!(
            messages[2]["tool_calls"][0],
            json!({"type": "function", "function": {"name": "list_tasks", "arguments": {"query": "牛乳"}}})
        );
        // 結果は、ツールの名前で呼び出しと結ぶ
        assert_eq!(
            messages[3],
            json!({"role": "tool", "tool_name": "list_tasks", "content": "[]"})
        );
        let plain = super::body(Wire::Ollama, &request(&turns, &[], Effort::Default));
        assert!(plain.get("think").is_none() && plain.get("tools").is_none());

        let reply = parse(
            Wire::Ollama,
            &json!({"done": true, "done_reason": "stop", "message": {"role": "assistant", "content": "",
                "tool_calls": [
                    {"type": "function", "function": {"index": 0, "name": "create_task", "arguments": {"title": "買い物"}}},
                    {"function": {"name": "list_people", "arguments": {}}},
                ]}}),
        )
        .unwrap();
        assert_eq!(reply.calls[0].args, json!({"title": "買い物"}));
        assert_eq!(reply.calls[1].name, "list_people");
        // ID は返ってこないので作る。会話の中で重ならない
        assert_ne!(reply.calls[0].id, reply.calls[1].id);
        assert!(!reply.calls[0].id.is_empty());
        let cut = parse(
            Wire::Ollama,
            &json!({"done_reason": "length", "message": {"content": "途中", "tool_calls": [
                {"function": {"name": "x", "arguments": {}}}]}}),
        )
        .unwrap();
        assert!(cut.notice.is_some() && cut.calls.is_empty());
        assert!(parse(Wire::Ollama, &json!({"error": "x"})).is_err());
    }

    #[test]
    fn files_the_model_cannot_read_are_sent_as_paths() {
        let (turns, tools) = (attached(), tools());
        let mut req = request(&turns, &tools, Effort::Default);
        req.media = Media::Images;
        let ollama = body(Wire::Ollama, &req);
        let user = &ollama["messages"][1];
        assert_eq!(user["images"], json!(["cG5n"]));
        let text = user["content"].as_str().unwrap();
        assert!(
            text.contains("「b.pdf」") && text.contains("中身はこのリクエストに入っていません"),
            "{text}"
        );
        let chat = body(Wire::Chat, &req);
        let parts = chat["messages"][1]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1]["type"], "image_url");

        req.media = Media::None;
        let ollama = body(Wire::Ollama, &req);
        assert!(ollama["messages"][1].get("images").is_none());
        let chat = body(Wire::Chat, &req);
        assert_eq!(chat["messages"][1]["content"].as_array().unwrap().len(), 1);
        // ツールを渡さないときは、tools を送らない
        req.tools = &[];
        assert!(body(Wire::Chat, &req).get("tools").is_none());
    }
}
