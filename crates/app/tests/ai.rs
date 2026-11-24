//! AI チャットの、画面を通したシナリオテスト。
//! API とキーチェーンと gh は差し替え、通信はしない。シェルのコマンドは実際に動かす。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext, Bounds, Entity, Pixels, Point, Size, TestAppContext,
    WindowBounds, WindowOptions,
};
use malakt_ai::keys::{KeyStore, MemoryKeys, Source};
use malakt_ai::shell::Spec;
use malakt_ai::{Effort, Provider};
use malakt_core::{Scope, Status, Store, TaskDraft, TaskFilter};
use malakt_mahoz::ai::{
    self, Item, KeyState, LineKind, Session, ShellPlace, ToolState, Watch,
    DEFAULT_WATCH_INSTRUCTION,
};
use malakt_mahoz::{db, AiConfig, AiPane, MainView};
use serde_json::{json, Value};

/// 送られたリクエスト（URL、ヘッダー、本文）。
/// 送ったリクエスト（URL、ヘッダー、本文）。
type Request = (String, Vec<(String, String)>, Value);
type Sent = Arc<Mutex<Vec<Request>>>;

struct World {
    keys: Arc<MemoryKeys>,
    sent: Sent,
    /// gh が返す中身に付け足す文字。変えると「GitHub 側で変化があった」ことになる。
    github: Arc<Mutex<String>>,
    replies: Arc<Mutex<VecDeque<Result<Value, String>>>>,
    fetched: Arc<Mutex<Vec<String>>>,
}

impl World {
    fn reply(&self, value: Value) {
        self.replies.lock().unwrap().push_back(Ok(value));
    }

    fn sent(&self) -> Vec<Request> {
        self.sent.lock().unwrap().clone()
    }
}

fn start(cx: &mut TestAppContext) -> World {
    start_with(cx, Store::open_in_memory().unwrap())
}

fn start_with(cx: &mut TestAppContext, store: Store) -> World {
    // コマンドの出力は、別のスレッドから届く
    cx.executor().allow_parking();
    let world = World {
        keys: Arc::new(MemoryKeys::default()),
        sent: Sent::default(),
        github: Arc::default(),
        replies: Arc::default(),
        fetched: Arc::default(),
    };
    let (sent, replies, fetched) = (
        world.sent.clone(),
        world.replies.clone(),
        world.fetched.clone(),
    );
    let github = world.github.clone();
    let config = AiConfig {
        keys: world.keys.clone(),
        env: Arc::new(|_| None),
        post: Arc::new(move |url, headers, body| {
            let headers = headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect();
            sent.lock()
                .unwrap()
                .push((url.to_owned(), headers, body.clone()));
            replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err("テストに返事が用意されていません".into()))
        }),
        get: Arc::new(move |url, _| {
            fetched.lock().unwrap().push(url.to_owned());
            if url.ends_with("/go/v1/models") {
                return Ok(json!({"data": [
                    {"id": "glm-5.3", "type": "chat", "recommended_api": "chat-completions",
                     "capabilities": {"reasoning": {"effort": ["low", "high", "max"]}},
                     "api": {"chat-completions": {"base_url": "https://opencode.ai/inference/go/openai/v1"}}},
                ]})
                .to_string());
            }
            Err(format!("テストでは {url} を読めません"))
        }),
        gh: Arc::new(move |link| {
            let state = github.lock().unwrap().clone();
            Spec::shell(&format!("echo fetched {}{state}", link.url()))
        }),
    };
    cx.update(|cx| malakt_mahoz::init_with(store, config, cx));
    world
}

fn open(cx: &mut TestAppContext, window_size: Size<Pixels>) -> (AnyWindowHandle, Entity<MainView>) {
    cx.update(|cx| {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: window_size,
            })),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| MainView::new(window, cx))
        })
        .expect("ウィンドウを開く")
    })
}

fn wide() -> Size<Pixels> {
    size(px(2000.), px(1125.))
}

fn ui(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    f: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App),
) {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        f(window, cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
}

fn session(cx: &mut TestAppContext) -> Entity<Session> {
    cx.update(|cx| ai::shared(cx))
}

fn read<R>(cx: &mut TestAppContext, f: impl FnOnce(&Session) -> R) -> R {
    let session = session(cx);
    cx.update(|cx| f(session.read(cx)))
}

/// コマンドは別のスレッドで動くので、条件がそろうまで待つ。
fn wait(cx: &mut TestAppContext, what: &str, f: impl Fn(&Session) -> bool) {
    for _ in 0..1500 {
        cx.run_until_parked();
        if read(cx, &f) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("待っても起きませんでした: {what}");
}

/// AI チャットを開き、右のパネルを返す。
fn open_chat(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    view: &Entity<MainView>,
) -> Entity<AiPane> {
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    cx.update(|cx| view.read(cx).ai_pane.clone())
        .expect("チャットが開く")
}

/// 入力欄に書いて、送信を押す。
fn say(cx: &mut TestAppContext, window: AnyWindowHandle, pane: &Entity<AiPane>, text: &str) {
    // 設定を開いている間は入力欄が出ないので、開いていれば閉じる
    if cx.update(|cx| pane.read(cx).settings_open) {
        ui(cx, window, |window, cx| {
            window.click("ai-settings-toggle", cx)
        });
    }
    ui(cx, window, |window, cx| {
        pane.update(cx, |pane, cx| pane.focus_input(window, cx));
        window.input(text, cx);
        window.click("ai-send", cx);
    });
}

/// 設定を開く（開いていればそのまま）。
fn open_settings(cx: &mut TestAppContext, window: AnyWindowHandle, pane: &Entity<AiPane>) {
    if !cx.update(|cx| pane.read(cx).settings_open) {
        ui(cx, window, |window, cx| {
            window.click("ai-settings-toggle", cx)
        });
    }
}

fn text_reply(text: &str) -> Value {
    json!({"stop_reason": "end_turn", "content": [{"type": "text", "text": text}]})
}

fn tool_reply(id: &str, name: &str, input: Value) -> Value {
    json!({"stop_reason": "tool_use", "content": [
        {"type": "text", "text": "やります"},
        {"type": "tool_use", "id": id, "name": name, "input": input},
    ]})
}

fn all_tasks(cx: &mut TestAppContext) -> Vec<malakt_core::Task> {
    cx.update(|cx| {
        db::shared(cx)
            .read(cx)
            .store
            .tasks(Scope::All, &TaskFilter::default())
            .unwrap()
    })
}

/// キーを保存していなければ、送っても通信しない。キーチェーンにもデータベースにもキーは入らない。
#[gpui_kit::test]
fn nothing_is_sent_without_a_key(cx: &mut TestAppContext) {
    let world = start(cx);
    let (window, view) = open(cx, wide());
    cx.update(|cx| assert!(!view.read(cx).ai_open, "最初は閉じている"));

    let pane = open_chat(cx, window, &view);
    assert_eq!(read(cx, |s| s.key.clone()), KeyState::Missing);
    ui(cx, window, |window, _| {
        assert!(window.find("ai-empty").visible());
        assert!(
            window.find("ai-shell-prompt").visible(),
            "シェルも右に出ている"
        );
    });

    say(cx, window, &pane, "タスクを作って");
    let items = read(cx, |s| s.items.clone());
    assert_eq!(items[0], Item::User("タスクを作って".into()));
    assert!(matches!(&items[1], Item::Error(e) if e.contains("API キーが設定されていません")));
    assert!(!read(cx, |s| s.busy));
    assert!(world.sent().is_empty());
    assert!(world.fetched.lock().unwrap().is_empty());
}

/// 設定で API キーを保存すると、キーチェーンに入り、画面には末尾だけが出る。
#[gpui_kit::test]
fn the_key_is_saved_to_the_keychain_from_settings(cx: &mut TestAppContext) {
    let world = start(cx);
    let (window, view) = open(cx, wide());
    open_chat(cx, window, &view);

    ui(cx, window, |window, cx| {
        window.click("ai-settings-toggle", cx)
    });
    ui(cx, window, |window, cx| {
        window.click("ai-key-input", cx);
        window.input("sk-ant-test-12345678", cx);
        window.click("ai-key-save", cx);
    });
    assert_eq!(
        world.keys.get(Provider::Claude).unwrap().as_deref(),
        Some("sk-ant-test-12345678")
    );
    assert_eq!(
        read(cx, |s| s.key.clone()),
        KeyState::Present {
            masked: "••••5678".into(),
            source: Source::Keychain
        }
    );
    ui(cx, window, |window, _| {
        let input = window.find("ai-key-input");
        assert!(
            input.value().is_none_or(|v| v.is_empty()),
            "入力欄は空に戻る"
        );
    });
    // データベースには入れない（エクスポートにも入らない）
    let leaked = cx.update(|cx| {
        let store = &db::shared(cx).read(cx).store;
        [
            "ai.provider",
            "ai.effort",
            "ai.model.claude",
            "ai.key",
            "ai.key.claude",
        ]
        .iter()
        .filter_map(|k| store.setting(k).unwrap())
        .any(|v| v.contains("sk-ant"))
    });
    assert!(!leaked);

    ui(cx, window, |window, cx| window.click("ai-key-delete", cx));
    assert_eq!(world.keys.get(Provider::Claude).unwrap(), None);
    assert_eq!(read(cx, |s| s.key.clone()), KeyState::Missing);
}

/// 頼むと AI がツールでタスクを作り、開いている一覧にすぐ出る。
#[gpui_kit::test]
fn the_ai_creates_a_task_and_the_list_updates(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "sk-ant-1").unwrap();
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    let pane = open_chat(cx, window, &view);

    world.reply(tool_reply(
        "toolu_1",
        "create_task",
        json!({"title": "牛乳を買う", "due": "2026-12-01", "priority": "high"}),
    ));
    world.reply(text_reply("牛乳を買うタスクを作りました。"));
    say(cx, window, &pane, "牛乳を買うタスクを 12/1 期限で");

    let tasks = all_tasks(cx);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].title, "牛乳を買う");
    assert_eq!(tasks[0].due.as_deref(), Some("2026-12-01"));
    cx.update(|cx| assert_eq!(view.read(cx).visible.len(), 1, "一覧に出る"));

    let items = read(cx, |s| s.items.clone());
    assert_eq!(
        items,
        vec![
            Item::User("牛乳を買うタスクを 12/1 期限で".into()),
            Item::Assistant("やります".into()),
            Item::Tool {
                summary: "タスクを作成: 牛乳を買う".into(),
                state: ToolState::Done
            },
            Item::Assistant("牛乳を買うタスクを作りました。".into()),
        ]
    );
    ui(cx, window, |window, _| {
        assert!(window.find(("ai-tool", 2usize)).visible());
        // 送ったあと、入力欄は空に戻る
    });

    let sent = world.sent();
    assert_eq!(sent.len(), 2);
    let (url, headers, first) = &sent[0];
    assert_eq!(url, "https://api.anthropic.com/v1/messages");
    assert!(headers.contains(&("x-api-key".into(), "sk-ant-1".into())));
    assert_eq!(first["model"], "claude-opus-5-5");
    assert!(first.get("output_config").is_none(), "effort は標準のまま");
    assert!(first["system"].as_str().unwrap().contains("すべてのタスク"));
    assert!(first["tools"].as_array().unwrap().len() >= 10);
    // 2 回目は、返ってきたブロックをそのまま返し、ツールの結果を付ける
    let messages = sent[1].2["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1]["content"][1]["id"], "toolu_1");
    assert_eq!(messages[2]["content"][0]["tool_use_id"], "toolu_1");
    assert_eq!(messages[2]["content"][0]["is_error"], false);
    assert!(messages[2]["content"][0]["content"]
        .as_str()
        .unwrap()
        .contains("牛乳を買う"));
}

/// タスクの削除は、実行の前に確認が出る。「実行しない」なら消えず、AI にもそう伝わる。
#[gpui_kit::test]
fn deleting_asks_first(cx: &mut TestAppContext) {
    let mut store = Store::open_in_memory().unwrap();
    let task = store
        .create_task(
            None,
            &malakt_core::TaskDraft {
                title: "消さないで".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let world = start_with(cx, store);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    let pane = open_chat(cx, window, &view);

    world.reply(tool_reply("t1", "delete_task", json!({"id": task.id})));
    world.reply(text_reply("削除しませんでした。"));
    say(cx, window, &pane, "全部消して");
    let asked = read(cx, |s| {
        s.pending.as_ref().map(|p| (p.text.clone(), p.shell))
    });
    let (text, shell) = asked.expect("確認が出る");
    assert!(text.contains("消さないで"));
    assert!(!shell);
    assert_eq!(all_tasks(cx).len(), 1, "答えるまで消えない");
    assert_eq!(world.sent().len(), 1);

    ui(cx, window, |window, cx| window.click("ai-deny", cx));
    assert_eq!(all_tasks(cx).len(), 1);
    let items = read(cx, |s| s.items.clone());
    assert!(items.contains(&Item::Tool {
        summary: "タスクを削除".into(),
        state: ToolState::Denied
    }));
    let result = &world.sent()[1].2["messages"][2]["content"][0];
    assert_eq!(result["is_error"], true);
    assert_eq!(result["content"], "ユーザーが拒否しました");

    // もう一度頼んで、今度は実行する
    world.reply(tool_reply("t2", "delete_task", json!({"id": task.id})));
    world.reply(text_reply("削除しました。"));
    say(cx, window, &pane, "やっぱり消して");
    ui(cx, window, |window, cx| window.click("ai-allow", cx));
    assert!(all_tasks(cx).is_empty());
    assert!(!read(cx, |s| s.busy));
}

/// シェルのコマンドは確認のあとで動き、コマンドと出力がシェルに出て、AI に結果が返る。
#[gpui_kit::test]
fn shell_commands_run_after_confirmation(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    let pane = open_chat(cx, window, &view);

    world.reply(tool_reply(
        "t1",
        "run_shell",
        json!({"command": "echo hello-shell"}),
    ));
    world.reply(text_reply("動きました。"));
    say(cx, window, &pane, "echo して");
    let pending = read(cx, |s| {
        s.pending.as_ref().map(|p| (p.text.clone(), p.shell))
    });
    let (text, shell) = pending.expect("確認が出る");
    assert!(text.contains("echo hello-shell"));
    assert!(shell);
    assert!(read(cx, |s| s.shell.is_empty()), "答えるまで動かない");

    ui(cx, window, |window, cx| window.click("ai-allow", cx));
    wait(cx, "コマンドが終わって返事が出る", |s| {
        !s.busy
    });
    let lines = read(cx, |s| s.shell.clone());
    assert_eq!(lines[0].kind, LineKind::Command);
    assert_eq!(lines[0].text, "$ echo hello-shell");
    assert!(lines
        .iter()
        .any(|l| l.kind == LineKind::Out && l.text.trim() == "hello-shell"));
    ui(cx, window, |window, _| {
        assert!(window.find(("ai-shell-line", 0usize)).visible());
    });
    let result = world.sent()[1].2["messages"][2]["content"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(result.contains("終了コード: 0"));
    assert!(result.contains("hello-shell"));

    // 「この会話では確認なしで実行」を選ぶと、次からは聞かれない
    world.reply(tool_reply(
        "t2",
        "run_shell",
        json!({"command": "echo second"}),
    ));
    world.reply(tool_reply(
        "t3",
        "run_shell",
        json!({"command": "echo third"}),
    ));
    world.reply(text_reply("終わり。"));
    say(cx, window, &pane, "もう 2 回");
    ui(cx, window, |window, cx| window.click("ai-allow-always", cx));
    wait(cx, "2 つとも終わる", |s| !s.busy);
    let lines = read(cx, |s| s.shell.clone());
    assert!(lines.iter().any(|l| l.text == "$ echo third"));
    assert!(read(cx, |s| s.pending.is_none()));
    // 新しい会話にすると、また聞かれる
    ui(cx, window, |window, cx| window.click("ai-new-chat", cx));
    assert!(!read(cx, |s| s.allow_shell));
    assert!(read(cx, |s| s.items.is_empty()));
}

/// 動いているコマンドは、シェルで Ctrl+C を押すと強制終了し、AI の返事も止まる。
#[gpui_kit::test]
fn ctrl_c_kills_the_running_command(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    let pane = open_chat(cx, window, &view);

    let command = if cfg!(windows) {
        "ping -n 120 127.0.0.1"
    } else {
        "sleep 120"
    };
    world.reply(tool_reply("t1", "run_shell", json!({"command": command})));
    say(cx, window, &pane, "待って");
    ui(cx, window, |window, cx| window.click("ai-allow", cx));
    wait(cx, "コマンドが動き出す", |s| s.command_running());

    let started = std::time::Instant::now();
    ui(cx, window, |window, cx| {
        assert!(window.find("ai-shell-kill").visible());
        window.click("ai-shell", cx);
        window.press("ctrl-c", cx);
    });
    wait(cx, "止まる", |s| !s.busy && !s.command_running());
    assert!(started.elapsed() < Duration::from_secs(30));
    let lines = read(cx, |s| s.shell.clone());
    assert!(lines.iter().any(|l| l.text == "強制終了しました"));
    let items = read(cx, |s| s.items.clone());
    assert_eq!(items.last(), Some(&Item::Notice("中止しました。".into())));
    assert!(items.contains(&Item::Tool {
        summary: format!("コマンドを実行: {command}").into(),
        state: ToolState::Failed("中止しました".into())
    }));
    assert_eq!(world.sent().len(), 1, "止めたあとは AI に送らない");

    // 続けて送れる。止めた呼び出しには、中止の結果が付いている
    world.reply(text_reply("了解です。"));
    say(cx, window, &pane, "もういいよ");
    let messages = world.sent()[1].2["messages"].as_array().unwrap().clone();
    assert_eq!(messages[2]["content"][0]["tool_use_id"], "t1");
    assert_eq!(messages[2]["content"][0]["is_error"], true);
    assert_eq!(messages[3]["content"], "もういいよ");
}

/// 「止める」ボタンでも止まる。確認を待っている間でも止められる。
#[gpui_kit::test]
fn the_stop_button_cancels_a_pending_confirmation(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    let pane = open_chat(cx, window, &view);
    world.reply(tool_reply(
        "t1",
        "run_shell",
        json!({"command": "echo never"}),
    ));
    say(cx, window, &pane, "実行して");
    assert!(read(cx, |s| s.pending.is_some()));
    ui(cx, window, |window, cx| window.click("ai-stop", cx));
    assert!(read(cx, |s| !s.busy && s.pending.is_none()));
    assert!(read(cx, |s| s.shell.is_empty()), "コマンドは動いていない");
}

/// メッセージに GitHub のリンクがあると、gh で中身を取ってきて AI に渡す。
#[gpui_kit::test]
fn github_links_are_fetched_before_sending(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    let pane = open_chat(cx, window, &view);

    world.reply(text_reply("読みました。"));
    say(
        cx,
        window,
        &pane,
        "https://github.com/o/r/issues/3 をタスクにして",
    );
    wait(cx, "返事が出る", |s| !s.busy);
    let lines = read(cx, |s| s.shell.clone());
    assert_eq!(
        lines[0].text,
        "$ echo fetched https://github.com/o/r/issues/3"
    );
    let sent = world.sent();
    let message = sent[0].2["messages"][0]["content"].as_str().unwrap();
    assert!(message.starts_with("https://github.com/o/r/issues/3 をタスクにして"));
    assert!(message.contains("[https://github.com/o/r/issues/3 の中身]"));
    assert!(message.contains("fetched https://github.com/o/r/issues/3"));
    // 画面には、ユーザーが書いたとおりに出る
    assert_eq!(
        read(cx, |s| s.items[0].clone()),
        Item::User("https://github.com/o/r/issues/3 をタスクにして".into())
    );

    // AI が自分で取りに行くこともできる（確認は出ない）
    world.reply(tool_reply(
        "t1",
        "github_fetch",
        json!({"url": "https://github.com/o/r/pull/4"}),
    ));
    world.reply(text_reply("PR も読みました。"));
    say(cx, window, &pane, "関連の PR も見て");
    wait(cx, "返事が出る", |s| !s.busy);
    assert!(read(cx, |s| s.pending.is_none()));
    let result = world.sent()[2].2["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["content"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(result.contains("fetched https://github.com/o/r/pull/4"));
}

/// サービス・モデル・effort は設定で選べ、保存されて、次のリクエストに使われる。
#[gpui_kit::test]
fn provider_model_and_effort_are_chosen_in_settings(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    world.keys.set(Provider::OpenCodeGo, "oc_sk_1").unwrap();
    let (window, view) = open(cx, wide());
    let pane = open_chat(cx, window, &view);
    ui(cx, window, |window, cx| {
        window.click("ai-settings-toggle", cx)
    });

    // Claude のモデルと effort を変える
    ui(cx, window, |window, cx| {
        window.click(("ai-model", 2usize), cx);
        window.click(("ai-effort", Effort::High as usize), cx);
    });
    assert_eq!(
        read(cx, |s| (s.model().to_owned(), s.effort)),
        ("claude-sonnet-5-5".into(), Effort::High)
    );
    world.reply(text_reply("はい"));
    say(cx, window, &pane, "こんにちは");
    let body = world.sent()[0].2.clone();
    assert_eq!(body["model"], "claude-sonnet-5-5");
    assert_eq!(body["output_config"], json!({"effort": "high"}));
    assert_eq!(body["fallbacks"], "default");

    // モデルの ID を入力して選ぶ
    open_settings(cx, window, &pane);
    ui(cx, window, |window, cx| {
        window.click("ai-model-input", cx);
        window.press("secondary-a", cx);
        window.input("claude-haiku-5-5", cx);
        window.press("enter", cx);
    });
    assert_eq!(read(cx, |s| s.model().to_owned()), "claude-haiku-5-5");

    // OpenCode Go に切り替える。送り先はモデルの一覧から決まる
    open_settings(cx, window, &pane);
    ui(cx, window, |window, cx| {
        window.click(("ai-provider", Provider::OpenCodeGo as usize), cx)
    });
    assert_eq!(read(cx, |s| s.model().to_owned()), "glm-5.3");
    assert!(matches!(
        read(cx, |s| s.key.clone()),
        KeyState::Present { .. }
    ));
    world.reply(json!({"choices": [{"finish_reason": "stop", "message": {"content": "やあ"}}]}));
    ui(cx, window, |window, cx| window.click("ai-new-chat", cx));
    say(cx, window, &pane, "こんにちは");
    let (url, headers, body) = world.sent()[1].clone();
    assert_eq!(
        url,
        "https://opencode.ai/inference/go/openai/v1/chat/completions"
    );
    assert!(headers.contains(&("authorization".into(), "Bearer oc_sk_1".into())));
    assert!(headers
        .iter()
        .any(|(k, v)| k == "x-opencode-session" && !v.is_empty()));
    assert_eq!(body["model"], "glm-5.3");
    assert_eq!(body["reasoning_effort"], "high");
    assert_eq!(
        read(cx, |s| s.items.last().cloned()),
        Some(Item::Assistant("やあ".into()))
    );
    assert_eq!(
        *world.fetched.lock().unwrap(),
        ["https://opencode.ai/inference/go/v1/models"]
    );

    // このモデルで選べない effort は、送る前にエラーになる
    open_settings(cx, window, &pane);
    ui(cx, window, |window, cx| {
        window.click(("ai-effort", Effort::Medium as usize), cx)
    });
    say(cx, window, &pane, "もう一度");
    assert_eq!(world.sent().len(), 2);
    assert!(matches!(
        read(cx, |s| s.items.last().cloned()),
        Some(Item::Error(e)) if e.contains("low / high / max")
    ));

    // 選んだ内容は保存され、起動し直しても戻る
    let saved = cx.update(|cx| {
        let store = &db::shared(cx).read(cx).store;
        (
            store.setting("ai.provider").unwrap(),
            store.setting("ai.effort").unwrap(),
            store.setting("ai.model.claude").unwrap(),
        )
    });
    assert_eq!(
        saved,
        (
            Some("opencode-go".into()),
            Some("medium".into()),
            Some("claude-haiku-5-5".into())
        )
    );
}

/// チャットとシェルは、別のウィンドウに出して、また戻せる。会話はそのまま続く。
#[gpui_kit::test]
fn chat_and_shell_detach_into_their_own_windows(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    let pane = open_chat(cx, window, &view);
    world.reply(text_reply("はい"));
    say(cx, window, &pane, "こんにちは");

    // シェルを別ウィンドウへ
    ui(cx, window, |window, cx| window.click("ai-shell-detach", cx));
    let shell_window = match read(cx, |s| s.shell_place) {
        ShellPlace::Window(handle) => handle,
        other => panic!("シェルが別ウィンドウに出る: {other:?}"),
    };
    assert_eq!(cx.update(|cx| cx.windows().len()), 2);
    ui(cx, window, |window, _| {
        assert!(window.try_find("ai-shell").is_none(), "右からは消える");
    });
    ui(cx, shell_window, |window, cx| {
        assert!(window.find("ai-shell").visible());
        window.click("ai-shell-attach", cx);
    });
    assert_eq!(read(cx, |s| s.shell_place), ShellPlace::Docked);
    assert_eq!(cx.update(|cx| cx.windows().len()), 1);

    // チャットを別ウィンドウへ。シェルはチャットの右に付いていく
    ui(cx, window, |window, cx| window.click("ai-detach", cx));
    let chat_window = read(cx, |s| s.chat_window).expect("チャットが別ウィンドウに出る");
    cx.update(|cx| assert!(!view.read(cx).ai_docked(cx)));
    ui(cx, window, |window, _| {
        assert!(window.try_find("ai-chat").is_none());
    });
    world.reply(text_reply("続きです"));
    ui(cx, chat_window, |window, cx| {
        assert!(window.find("ai-chat").visible());
        assert!(window.find("ai-shell").visible());
        assert!(
            window.find(("ai-assistant", 1usize)).visible(),
            "会話は続いている"
        );
        // 別ウィンドウでも、同じように送れる
        window.click("ai-input", cx);
        window.input("続けて", cx);
        window.click("ai-send", cx);
    });
    assert_eq!(
        read(cx, |s| s.items.last().cloned()),
        Some(Item::Assistant("続きです".into()))
    );

    // 戻すと、メインのウィンドウの右に出る
    ui(cx, chat_window, |window, cx| window.click("ai-attach", cx));
    assert_eq!(read(cx, |s| s.chat_window), None);
    ui(cx, window, |window, _| {
        assert!(window.find("ai-chat").visible());
        assert!(window.find(("ai-assistant", 3usize)).visible());
    });
}

/// 最初のウィンドウの大きさでチャットを開いても、タスクの画面はそのまま使える。
#[gpui_kit::test]
fn the_task_screen_stays_usable_beside_the_chat(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, size(px(1475.), px(950.)));
    open_chat(cx, window, &view);
    ui(cx, window, |window, _| {
        let chat = window.find("ai-chat").bounds();
        // 右の 400（左の罫線 1 を含む）がチャット。寸法は 1.25 倍の拡大後の値
        assert_eq!(chat.origin.x, px(1076.));
        assert_eq!(chat.size.width, px(399.));
        // シェルはチャットの下に入る
        let shell = window.find("ai-shell").bounds();
        assert_eq!(shell.origin.x, chat.origin.x);
        assert!(shell.origin.y > chat.origin.y);
        // メニューをたたむ表示にはならない
        assert!(window.find("nav-home").visible());
        assert!(window.find("ai-toggle").visible());
    });
}

/// 広いウィンドウでは、シェルはチャットの右に並ぶ。
#[gpui_kit::test]
fn the_shell_sits_right_of_the_chat_in_a_wide_window(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    open_chat(cx, window, &view);
    ui(cx, window, |window, _| {
        let chat = window.find("ai-chat").bounds();
        let shell = window.find("ai-shell").bounds();
        assert_eq!(chat.origin.x, px(1076.));
        assert_eq!(shell.origin.x + shell.size.width, px(2000.));
        assert_eq!(shell.size.width, px(449.));
        assert_eq!(shell.origin.y, chat.origin.y);
        assert!(window.find("nav-home").visible());
    });
}

/// 狭いウィンドウでは全面に重ね、閉じるとタスクの画面に戻る。
#[gpui_kit::test]
fn the_chat_covers_a_narrow_window_and_closes(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, size(px(1125.), px(875.)));
    open_chat(cx, window, &view);
    ui(cx, window, |window, cx| {
        let chat = window.find("ai-chat").bounds();
        assert_eq!(chat.size.width, px(1125.));
        window.click("ai-close", cx);
    });
    cx.update(|cx| assert!(!view.read(cx).ai_open));
    ui(cx, window, |window, _| {
        assert!(window.try_find("ai-chat").is_none())
    });
}

/// ロック中は、チャットで送れない。
#[gpui_kit::test]
fn a_locked_app_does_not_talk_to_the_ai(cx: &mut TestAppContext) {
    let mut store = Store::open_in_memory().unwrap();
    malakt_core::lock::set_passphrase(&mut store, None, "passphrase-1").unwrap();
    let world = start_with(cx, store);
    world.keys.set(Provider::Claude, "k").unwrap();
    let session = session(cx);
    let sent = cx.update(|cx| session.update(cx, |s, cx| s.send("こんにちは", String::new(), cx)));
    cx.run_until_parked();
    assert!(!sent);
    assert!(world.sent().is_empty());
    let _ = Status::Completed;
}

/// リンク付きのタスクを 1 つ作り、定期チェックをオンにする。
fn watched_task(cx: &mut TestAppContext, minutes: u32) -> String {
    let id = cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            let draft = TaskDraft {
                title: "ログインの不具合".into(),
                description: "https://github.com/o/r/issues/3 を直す".into(),
                ..Default::default()
            };
            let task = db.store.create_task(None, &draft).unwrap();
            cx.notify();
            task.id
        })
    });
    let session = session(cx);
    cx.update(|cx| {
        session.update(cx, |s, cx| {
            let watch = Watch {
                enabled: true,
                minutes,
                ..s.watch.clone()
            };
            s.set_watch(watch, cx);
        })
    });
    id
}

fn tick(cx: &mut TestAppContext, minutes: u64) {
    cx.executor()
        .advance_clock(Duration::from_secs(minutes * 60));
    wait(cx, "定期チェックが終わる", |s| !s.busy);
}

/// 定期チェックは、標準ではオフ。オンにするまで、GitHub にも AI にも何も送らない。
#[gpui_kit::test]
fn github_is_not_polled_until_turned_on(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    assert!(!read(cx, |s| s.watch.enabled));
    tick(cx, 600);
    assert!(read(cx, |s| s.shell.is_empty() && s.watch_status.is_none()));
    assert!(world.sent().is_empty());
}

/// オンにすると、選んだ間隔でタスクのリンクを見に行く。変化があったときだけ AI を呼び、
/// AI がタスクを更新する。
#[gpui_kit::test]
fn changes_on_github_are_reflected_into_tasks(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let task = watched_task(cx, 15);

    // 間隔より前には動かない
    tick(cx, 14);
    assert!(read(cx, |s| s.shell.is_empty()));

    // 1 回目は、比べる元を覚えるだけ
    tick(cx, 1);
    let lines = read(cx, |s| s.shell.clone());
    assert_eq!(
        lines[0].text,
        "$ echo fetched https://github.com/o/r/issues/3"
    );
    assert!(world.sent().is_empty());
    let status = read(cx, |s| s.watch_status.clone()).unwrap();
    assert!(status.contains("リンク 1 件中、更新 0 件"), "{status}");

    // 変化がなければ、AI は呼ばない
    tick(cx, 15);
    assert!(world.sent().is_empty());
    assert!(read(cx, |s| s.items.is_empty()));

    // GitHub 側で変わると、AI に渡して、タスクが更新される
    *world.github.lock().unwrap() = " state: CLOSED".into();
    world.reply(tool_reply(
        "t1",
        "update_task",
        json!({"id": task, "status": "completed"}),
    ));
    world.reply(text_reply("issue が閉じられたので、完了にしました。"));
    tick(cx, 15);
    let sent = world.sent();
    assert_eq!(sent.len(), 2);
    let message = sent[0].2["messages"][0]["content"].as_str().unwrap();
    assert!(message.contains("## https://github.com/o/r/issues/3"));
    assert!(message.contains(&format!("- ログインの不具合（id: {task}）")));
    assert!(message.contains("state: CLOSED"));
    assert!(message.contains(DEFAULT_WATCH_INSTRUCTION));
    assert_eq!(all_tasks(cx)[0].status, Status::Completed);
    let items = read(cx, |s| s.items.clone());
    assert_eq!(
        items[0],
        Item::Notice(
            "GitHub の定期チェック: https://github.com/o/r/issues/3 に更新がありました。AI がタスクに反映します。"
                .into()
        )
    );
    assert_eq!(
        items.last(),
        Some(&Item::Assistant(
            "issue が閉じられたので、完了にしました。".into()
        ))
    );

    // 完了したタスクのリンクは、もう見に行かない。覚えていた値も消える
    let before = read(cx, |s| s.shell.len());
    tick(cx, 15);
    assert_eq!(read(cx, |s| s.shell.len()), before);
    assert_eq!(world.sent().len(), 2);
    let seen = cx.update(|cx| {
        db::shared(cx)
            .read(cx)
            .store
            .setting("ai.watch.seen")
            .unwrap()
    });
    assert_eq!(seen.as_deref(), Some("{}"));
}

/// 定期チェックからは、タスクの削除もコマンドの実行もしない（確認に答える人がいない）。
#[gpui_kit::test]
fn the_periodic_check_never_deletes_or_runs_commands(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let task = watched_task(cx, 30);
    tick(cx, 30);
    *world.github.lock().unwrap() = " state: CLOSED".into();
    world.reply(tool_reply("t1", "delete_task", json!({"id": task})));
    world.reply(tool_reply(
        "t2",
        "run_shell",
        json!({"command": "echo never"}),
    ));
    world.reply(text_reply("できませんでした。"));
    tick(cx, 30);
    assert_eq!(all_tasks(cx).len(), 1, "タスクは消えていない");
    assert!(read(cx, |s| s.pending.is_none()));
    let lines = read(cx, |s| s.shell.clone());
    assert!(lines.iter().all(|l| !l.text.contains("never")));
    let sent = world.sent();
    assert_eq!(sent.len(), 3);
    let result = &sent[1].2["messages"].as_array().unwrap().last().unwrap()["content"][0];
    assert_eq!(result["is_error"], true);
    assert_eq!(
        result["content"],
        "定期チェックからは、タスクの削除とコマンドの実行はできません"
    );
    // チャットから頼めば、いつもどおり確認が出る
    let (window, view) = open(cx, wide());
    let pane = open_chat(cx, window, &view);
    world.reply(tool_reply("t3", "delete_task", json!({"id": task})));
    say(cx, window, &pane, "消して");
    assert!(read(cx, |s| s.pending.is_some()));
}

/// キーがなければ、オンにしていても GitHub を見に行かない。
#[gpui_kit::test]
fn the_periodic_check_stays_offline_without_a_key(cx: &mut TestAppContext) {
    let world = start(cx);
    watched_task(cx, 15);
    tick(cx, 15);
    assert!(read(cx, |s| s.shell.is_empty()));
    assert!(world.sent().is_empty());
    let status = read(cx, |s| s.watch_status.clone()).unwrap();
    assert!(status.contains("API キーが設定されていない"), "{status}");
    assert!(read(cx, |s| s.items.is_empty()), "チャットには何も足さない");
}

/// 定期チェックは設定の画面でオンにでき、間隔と指示を変えられる。起動し直しても残る。
#[gpui_kit::test]
fn the_periodic_check_is_set_up_in_settings(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    open_chat(cx, window, &view);
    ui(cx, window, |window, cx| {
        window.click("ai-settings-toggle", cx)
    });
    ui(cx, window, |window, cx| window.click("ai-watch", cx));
    ui(cx, window, |window, cx| {
        window.click(("ai-watch-minutes", 60usize), cx)
    });
    ui(cx, window, |window, cx| {
        window.click("ai-watch-instruction", cx);
        window.press("secondary-a", cx);
        window.input("閉じられていたら完了にする", cx);
        window.click("ai-watch-save", cx);
    });
    assert_eq!(
        read(cx, |s| s.watch.clone()),
        Watch {
            enabled: true,
            minutes: 60,
            instruction: "閉じられていたら完了にする".into()
        }
    );
    let saved = cx.update(|cx| {
        let store = &db::shared(cx).read(cx).store;
        (
            store.setting("ai.watch.enabled").unwrap(),
            store.setting("ai.watch.minutes").unwrap(),
            store.setting("ai.watch.instruction").unwrap(),
        )
    });
    assert_eq!(
        saved,
        (
            Some("1".into()),
            Some("60".into()),
            Some("閉じられていたら完了にする".into())
        )
    );

    // 「今すぐ確認」で、間隔を待たずに見に行く。結果が設定に出る
    watched_task(cx, 60);
    ui(cx, window, |window, cx| window.click("ai-watch-now", cx));
    wait(cx, "確認が終わる", |s| !s.busy);
    ui(cx, window, |window, _| {
        assert!(window.find("ai-watch-status").visible());
    });
    assert_eq!(read(cx, |s| s.shell.len()), 2);

    // オフに戻すと、もう見に行かない
    ui(cx, window, |window, cx| window.click("ai-watch", cx));
    assert!(!read(cx, |s| s.watch.enabled));
    // カーソルの点滅で時間がかかるので、ウィンドウは閉じてから時計を進める
    ui(cx, window, |window, _| window.remove_window());
    tick(cx, 600);
    assert_eq!(read(cx, |s| s.shell.len()), 2);
    assert!(world.sent().is_empty());
}

/// オート実行は標準ではオフ。設定でオンにすると、コマンドの実行もタスクの削除も確認なしで進み、
/// オンであることがチャットの上にいつも出る。
#[gpui_kit::test]
fn auto_run_skips_the_confirmations(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    let pane = open_chat(cx, window, &view);
    assert!(!read(cx, |s| s.auto_run));
    ui(cx, window, |window, _| {
        assert!(window.try_find("ai-auto-badge").is_none());
    });

    // オフの間は確認が出る。待っている間にオンにすると、そのまま実行へ進む
    world.reply(tool_reply(
        "t1",
        "run_shell",
        json!({"command": "echo first-auto"}),
    ));
    world.reply(text_reply("実行しました。"));
    say(cx, window, &pane, "実行して");
    assert!(read(cx, |s| s.pending.is_some()));
    ui(cx, window, |window, cx| {
        window.click("ai-settings-toggle", cx)
    });
    ui(cx, window, |window, cx| window.click("ai-auto", cx));
    wait(cx, "コマンドが終わる", |s| !s.busy);
    assert!(read(cx, |s| s.auto_run));
    assert!(read(cx, |s| s.shell.iter().any(|l| l.text == "first-auto")));
    ui(cx, window, |window, _| {
        assert!(window.find("ai-auto-badge").visible());
    });

    // オンの間は、コマンドも削除も確認なしで進む
    world.reply(tool_reply(
        "t2",
        "create_task",
        json!({"title": "消すタスク"}),
    ));
    world.reply(text_reply("作りました。"));
    say(cx, window, &pane, "作って");
    wait(cx, "作り終わる", |s| !s.busy);
    let id = all_tasks(cx)[0].id.clone();
    world.reply(tool_reply("t3", "delete_task", json!({"id": id})));
    world.reply(tool_reply(
        "t4",
        "run_shell",
        json!({"command": "echo second-auto"}),
    ));
    world.reply(text_reply("消して、実行しました。"));
    say(cx, window, &pane, "消して実行して");
    wait(cx, "終わる", |s| !s.busy);
    assert!(read(cx, |s| s.pending.is_none()));
    assert!(all_tasks(cx).is_empty());
    assert!(read(cx, |s| s
        .shell
        .iter()
        .any(|l| l.text == "second-auto")));

    // 保存されていて、オフに戻すとまた確認が出る
    let saved = cx.update(|cx| {
        db::shared(cx)
            .read(cx)
            .store
            .setting("ai.auto_run")
            .unwrap()
    });
    assert_eq!(saved.as_deref(), Some("1"));
    open_settings(cx, window, &pane);
    ui(cx, window, |window, cx| window.click("ai-auto", cx));
    ui(cx, window, |window, _| {
        assert!(window.try_find("ai-auto-badge").is_none());
    });
    world.reply(tool_reply(
        "t5",
        "run_shell",
        json!({"command": "echo third-auto"}),
    ));
    say(cx, window, &pane, "もう一度");
    assert!(read(cx, |s| s.pending.is_some()));
    assert!(read(cx, |s| s.shell.iter().all(|l| l.text != "third-auto")));
}

/// オート実行がオンでも、定期チェックからは削除もコマンドの実行もしない。
#[gpui_kit::test]
fn auto_run_does_not_reach_the_periodic_check(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let task = watched_task(cx, 30);
    let session = session(cx);
    cx.update(|cx| session.update(cx, |s, cx| s.set_auto_run(true, cx)));
    tick(cx, 30);
    *world.github.lock().unwrap() = " state: CLOSED".into();
    world.reply(tool_reply("t1", "delete_task", json!({"id": task})));
    world.reply(tool_reply(
        "t2",
        "run_shell",
        json!({"command": "echo never"}),
    ));
    world.reply(text_reply("できませんでした。"));
    tick(cx, 30);
    assert_eq!(all_tasks(cx).len(), 1);
    assert!(read(cx, |s| s
        .shell
        .iter()
        .all(|l| !l.text.contains("never"))));
}

/// シェルの入力行に打って Enter を押す。
fn type_line(cx: &mut TestAppContext, window: AnyWindowHandle, line: &str) {
    ui(cx, window, |window, cx| {
        window.click("ai-shell-input", cx);
        window.input(line, cx);
        window.press("enter", cx);
    });
    wait(cx, "コマンドが終わる", |s| !s.busy);
}

fn shell_texts(cx: &mut TestAppContext) -> Vec<String> {
    read(cx, |s| s.shell.iter().map(|l| l.text.to_string()).collect())
}

#[gpui_kit::test]
fn the_shell_runs_typed_commands_like_a_terminal(cx: &mut TestAppContext) {
    // キーがなくても、自分で打つコマンドは動く
    let world = start(cx);
    let (window, view) = open(cx, wide());
    open_chat(cx, window, &view);

    type_line(cx, window, "echo hello");
    assert_eq!(shell_texts(cx), ["$ echo hello", "hello"]);
    assert!(world.sent().is_empty(), "AI には送らない");

    // cd は次のコマンドにも続く
    let dir = std::env::temp_dir().canonicalize().unwrap();
    type_line(cx, window, &format!("cd {}", dir.display()));
    assert_eq!(read(cx, |s| s.cwd.clone()), dir);
    type_line(cx, window, "cd no-such-folder-here");
    assert_eq!(read(cx, |s| s.cwd.clone()), dir, "行けなければ動かない");
    assert_eq!(read(cx, |s| s.shell.last().unwrap().kind), LineKind::Err);

    // ↑ で前のコマンドに戻り、↓ で進む
    ui(cx, window, |window, cx| {
        window.click("ai-shell-input", cx);
        for key in ["up", "up", "up", "up", "down", "up", "enter"] {
            window.press(key, cx);
        }
    });
    wait(cx, "コマンドが終わる", |s| !s.busy);
    let texts = shell_texts(cx);
    assert_eq!(&texts[texts.len() - 2..], ["$ echo hello", "hello"]);
    assert_eq!(
        read(cx, |s| s.history.clone()),
        [
            "echo hello".to_owned(),
            format!("cd {}", dir.display()),
            "cd no-such-folder-here".to_owned(),
            "echo hello".to_owned()
        ]
    );

    // Ctrl+L で画面を消す。履歴と場所は残る
    ui(cx, window, |window, cx| {
        assert!(window.find(("ai-shell-line", 0usize)).visible());
        window.click("ai-shell-input", cx);
        window.press("ctrl-l", cx);
    });
    assert!(shell_texts(cx).is_empty());
    ui(cx, window, |window, _| {
        assert!(window.try_find(("ai-shell-line", 0usize)).is_none());
    });
    assert_eq!(read(cx, |s| s.cwd.clone()), dir);
}

#[gpui_kit::test]
fn long_lines_wrap_and_the_clear_button_empties_the_screen(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    open_chat(cx, window, &view);

    let long = "x".repeat(300);
    type_line(cx, window, &format!("echo {long}"));
    assert_eq!(read(cx, |s| s.shell.len()), 2);
    ui(cx, window, |window, _| {
        // 2 行が、幅に合わせて 4 行より多く出ている
        assert!(window.find(("ai-shell-line", 4usize)).visible());
    });
    ui(cx, window, |window, cx| window.click("ai-shell-clear", cx));
    assert!(shell_texts(cx).is_empty());
}

#[cfg(unix)]
#[gpui_kit::test]
fn colours_from_the_command_are_kept(cx: &mut TestAppContext) {
    use malakt_ai::ansi::Color;
    start(cx);
    let (window, view) = open(cx, wide());
    open_chat(cx, window, &view);

    type_line(cx, window, r"printf '\033[31mred\033[0m plain\n'");
    let line = read(cx, |s| s.shell.last().unwrap().clone());
    assert_eq!(line.text, "red plain");
    assert_eq!(line.spans, [(0..3, Color::Red)]);
}

#[gpui_kit::test]
fn ctrl_c_in_the_input_line_kills_the_typed_command(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    open_chat(cx, window, &view);

    let command = if cfg!(windows) {
        "ping -n 120 127.0.0.1"
    } else {
        "sleep 120"
    };
    ui(cx, window, |window, cx| {
        window.click("ai-shell-input", cx);
        window.input(command, cx);
        window.press("enter", cx);
    });
    wait(cx, "コマンドが動き出す", |s| s.command_running());
    let started = std::time::Instant::now();
    ui(cx, window, |window, cx| window.press("ctrl-c", cx));
    wait(cx, "止まる", |s| !s.busy && !s.command_running());
    assert!(started.elapsed() < Duration::from_secs(30));
    assert!(shell_texts(cx).iter().any(|l| l == "強制終了しました"));
    assert!(read(cx, |s| s.items.is_empty()), "チャットには何も書かない");
}
