//! 本物の Claude API で、プロンプトキャッシュが効いているかを確かめる。
//! お金がかかるので、ふだんのテストでは動かさない。
//!
//! ```sh
//! ANTHROPIC_API_KEY=sk-ant-... mise run cache-check
//! ```
//!
//! モデルを変えるときは `MALAKT_CACHE_MODEL=claude-haiku-5-5` のように指定する。

use malakt_ai::provider::{self, Effort, Provider};
use malakt_ai::wire::{self, Media, Request, Turn, Wire};
use malakt_ai::{http, tools};
use serde_json::Value;

fn usage(response: &Value, key: &str) -> u64 {
    response["usage"][key].as_u64().unwrap_or(0)
}

#[test]
#[ignore = "本物の API に送る。ANTHROPIC_API_KEY を付けて -- --ignored で動かす"]
fn the_second_request_reads_the_cache() {
    let key = std::env::var(Provider::Claude.env_var()).expect("ANTHROPIC_API_KEY がありません");
    let model = std::env::var("MALAKT_CACHE_MODEL")
        .unwrap_or_else(|_| Provider::Claude.default_model().to_owned());
    let endpoint = provider::endpoint(Provider::Claude, &model, &key).unwrap();
    // アプリが始めた実行と同じ、ツールと最初の指示
    let system = tools::system_prompt("");
    let definitions = tools::definitions();
    let mut turns = vec![Turn::User("「はい」とだけ答えてください。".into())];

    let send = |turns: &[Turn]| {
        let request = Request {
            model: &model,
            effort: Effort::Low,
            system: &system,
            turns,
            tools: &definitions,
            fallbacks: endpoint.fallbacks,
            prefix_ttl: endpoint.prefix_ttl,
            media: Media::All,
        };
        let body = wire::body(Wire::Messages, &request);
        let response = http::post_json(&endpoint.url, &endpoint.headers, &body).unwrap();
        eprintln!(
            "input {} / cache_creation {} / cache_read {}",
            usage(&response, "input_tokens"),
            usage(&response, "cache_creation_input_tokens"),
            usage(&response, "cache_read_input_tokens"),
        );
        response
    };

    let first = send(&turns);
    let reply = wire::parse(Wire::Messages, &first).unwrap();
    turns.push(reply.into_turn(Wire::Messages));
    turns.push(Turn::User(
        "もう一度、「はい」とだけ答えてください。".into(),
    ));
    let second = send(&turns);

    // 1 時間残るので、少し前に同じものを送っていれば 1 回目から読める
    assert!(
        usage(&first, "cache_creation_input_tokens") + usage(&first, "cache_read_input_tokens") > 0,
        "1 回目でキャッシュに書いていない: {}",
        first["usage"]
    );
    assert!(
        usage(&second, "cache_read_input_tokens") > 0,
        "2 回目でキャッシュを読んでいない: {}",
        second["usage"]
    );
}
