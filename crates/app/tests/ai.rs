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
    self, Item, KeyState, LineKind, RunKind, Session, ShellPlace, ToolState, Watch,
    DEFAULT_WATCH_INSTRUCTION, MAX_CHATS,
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

    fn fail(&self, error: &str) {
        self.replies.lock().unwrap().push_back(Err(error.into()));
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
        menu(cx, window, 0);
    }
}

/// チャットの「…」メニューの `n` 番目（0 から）を選ぶ。
fn menu(cx: &mut TestAppContext, window: AnyWindowHandle, n: usize) {
    ui(cx, window, |window, cx| window.click("ai-menu", cx));
    ui(cx, window, |window, cx| {
        for _ in 0..=n {
            window.press("down", cx);
        }
        window.press("enter", cx);
    });
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

    menu(cx, window, 0);
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
    // 今の画面は、最初の指示ではなくメッセージに添える
    let first_message = first["messages"][0]["content"].as_str().unwrap();
    assert!(first_message.contains("すべてのタスク"), "{first_message}");
    assert!(!first["system"].as_str().unwrap().contains("すべてのタスク"));
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
    assert!(messages[3]["content"]
        .as_str()
        .unwrap()
        .starts_with("もういいよ\n\n（アプリからの補足）"));
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
    menu(cx, window, 0);

    // Claude のモデルと effort を変える
    // モデルは、開いた一覧から選ぶ。選ぶと一覧は閉じる
    ui(cx, window, |window, cx| window.click("ai-model-open", cx));
    ui(cx, window, |window, cx| {
        window.click(("ai-model", 2usize), cx)
    });
    ui(cx, window, |window, cx| {
        assert!(window.try_find("ai-model-list").is_none());
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
    ui(cx, window, |window, cx| window.click("ai-model-open", cx));
    ui(cx, window, |window, cx| {
        window.click("ai-model-input", cx);
        window.input("haiku", cx);
    });
    // 検索の言葉に合うものだけが残る
    ui(cx, window, |window, cx| {
        assert!(window.try_find(("ai-model", 1usize)).is_none());
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
    ui(cx, window, |window, cx| {
        window.click("ai-settings-toggle", cx)
    });
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
    // モデルの一覧を開いた 2 回は Claude の一覧を、送るときは OpenCode Go の一覧を読む
    assert_eq!(
        *world.fetched.lock().unwrap(),
        [
            "https://api.anthropic.com/v1/models?limit=100",
            "https://api.anthropic.com/v1/models?limit=100",
            "https://opencode.ai/inference/go/v1/models"
        ]
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
    menu(cx, window, 2);
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
    menu(cx, chat_window, 2);
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

/// 最初の大きさのウィンドウでは、上のバーとタスクの画面を残して右に重ねる。
/// 上のバーのボタンでも、チャットの × でも閉じられる。
#[gpui_kit::test]
fn the_chat_leaves_the_bar_and_tasks_in_a_small_window(cx: &mut TestAppContext) {
    start(cx);
    for width in [1280., 1125., 600.] {
        let (window, view) = open(cx, size(px(width), px(720.)));
        open_chat(cx, window, &view);
        ui(cx, window, |window, cx| {
            let chat = window.find("ai-chat").bounds();
            let toggle = window.find("ai-toggle").bounds();
            // 上のバー（ウィンドウを動かす・閉じる所）を隠さない
            assert!(chat.origin.y >= toggle.origin.y + toggle.size.height);
            assert_eq!(chat.origin.x + chat.size.width, px(width));
            // タスクの画面が左に残る
            assert!(chat.origin.x >= px(width) - px(475.));
            window.click("ai-toggle", cx);
        });
        cx.update(|cx| assert!(!view.read(cx).ai_open));
        open_chat(cx, window, &view);
        ui(cx, window, |window, cx| window.click("ai-close", cx));
        cx.update(|cx| assert!(!view.read(cx).ai_open));
        ui(cx, window, |window, _| {
            assert!(window.try_find("ai-chat").is_none())
        });
    }
}

/// ロック中は、チャットで送れない。
#[gpui_kit::test]
fn a_locked_app_does_not_talk_to_the_ai(cx: &mut TestAppContext) {
    let mut store = Store::open_in_memory().unwrap();
    malakt_core::lock::enable(&mut store).unwrap();
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
    // 定期チェックだけを見るので、新しいタスクを整えるほうは切る
    let arrange = session(cx);
    cx.update(|cx| arrange.update(cx, |s, cx| s.set_arrange(false, cx)));
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
    // 会話には混ぜず、実行の記録に残す
    assert!(read(cx, |s| s.items.is_empty()));
    let run = read(cx, |s| s.runs.last().cloned().unwrap());
    assert_eq!(run.kind, RunKind::Watch);
    assert_eq!(run.tasks, vec![(task.clone(), "ログインの不具合".into())]);
    assert_eq!(run.error, None);
    assert_eq!(
        run.items.last(),
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
    menu(cx, window, 0);
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
    menu(cx, window, 0);
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

fn new_task(cx: &mut TestAppContext, title: &str) -> String {
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            let draft = TaskDraft {
                title: title.into(),
                ..Default::default()
            };
            let task = db.store.create_task(None, &draft).unwrap();
            cx.notify();
            task.id
        })
    })
}

/// タスクを作ると、AI がリンクを読んでタスクを整える。整えている間は印が出る。
#[gpui_kit::test]
fn a_new_task_is_arranged_by_the_ai(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    cx.run_until_parked();

    // gh が少しかかる間に、印を見る
    *world.github.lock().unwrap() = if cfg!(windows) {
        " & ping -n 2 127.0.0.1 > NUL".into()
    } else {
        "; sleep 0.5".into()
    };
    let task = new_task(cx, "https://github.com/o/r/pull/7 のレビュー");
    wait(cx, "AI が動き始める", |s| !s.arranging.is_empty());
    // 始まったら、タスクの行に印が出る
    assert_eq!(read(cx, |s| s.arranging.clone()), vec![task.clone()]);
    ui(cx, window, |window, _| {
        assert!(window.try_find(("ai-working", 0usize)).is_some());
    });
    world.reply(tool_reply(
        "t1",
        "update_task",
        json!({"id": task, "title": "ログインを直す PR のレビュー"}),
    ));
    world.reply(text_reply("タイトルを付け直しました。"));
    wait(cx, "AI が整え終わる", |s| !s.busy);

    let sent = world.sent();
    assert_eq!(sent.len(), 2);
    let message = sent[0].2["messages"][0]["content"].as_str().unwrap();
    assert!(message.contains(&format!("## タスク（id: {task}）")));
    assert!(message.contains("fetched https://github.com/o/r/pull/7"));
    // 今見ている画面も伝える
    assert!(message.contains("すべてのタスク"), "{message}");
    assert!(message.contains("のレビュー（ID: "), "{message}");
    assert_eq!(all_tasks(cx)[0].title, "ログインを直す PR のレビュー");
    assert!(read(cx, |s| s.arranging.is_empty()));
    ui(cx, window, |window, _| {
        assert!(window.try_find(("ai-working", 0usize)).is_none());
    });

    // AI が書き換えた分では、もう一度動かない
    cx.run_until_parked();
    assert_eq!(world.sent().len(), 2);
    let _ = view;
}

/// 設定に書いた「いつも AI に伝えること」は、毎回 AI に渡り、起動し直しても残る。
#[gpui_kit::test]
fn the_custom_context_reaches_the_ai(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    let pane = open_chat(cx, window, &view);
    open_settings(cx, window, &pane);
    ui(cx, window, |window, cx| {
        window.click("ai-context", cx);
        window.input("PR のタスクは優先度を高にする", cx);
        window.click("ai-context-save", cx);
    });
    assert_eq!(
        setting(cx, "ai.context").as_deref(),
        Some("PR のタスクは優先度を高にする")
    );
    world.reply(text_reply("はい。"));
    say(cx, window, &pane, "こんにちは");
    wait(cx, "返事が来る", |s| !s.busy && s.items.len() >= 2);
    let system = world.sent()[0].2["system"].as_str().unwrap().to_owned();
    assert!(
        system.ends_with("いつも従ってください。\nPR のタスクは優先度を高にする"),
        "{system}"
    );

    // 長すぎるものは保存しない
    let long = "あ".repeat(malakt_mahoz::ai::MAX_CONTEXT_CHARS + 1);
    let session = session(cx);
    cx.update(|cx| session.update(cx, |s, cx| s.set_context(&long, cx)));
    assert!(read(cx, |s| s.error.clone()).unwrap().contains("文字まで"));
    assert_eq!(
        read(cx, |s| s.context.clone()),
        "PR のタスクは優先度を高にする"
    );
}

/// ワークスペースの設定に書いた「AI に伝えること」は、そのワークスペースを開いている間だけ AI に渡る。
#[gpui_kit::test]
fn the_workspace_context_reaches_the_ai(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let workspace = cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            let workspace = db.store.create_workspace(None, "仕事").unwrap();
            cx.notify();
            workspace.id
        })
    });
    let (window, view) = open(cx, wide());
    let session = session(cx);
    cx.update(|cx| session.update(cx, |s, cx| s.set_context("敬語で答える", cx)));

    // ワークスペースの設定で書いて保存する
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| {
            v.open_workspace_settings(&workspace, window, cx)
        });
    });
    ui(cx, window, |window, cx| {
        window.click("ws-context", cx);
        window.input("PR のタスクは優先度を高にする", cx);
        window.click("ws-settings-save", cx);
    });
    assert_eq!(
        setting(cx, &malakt_mahoz::ai::workspace_context_key(&workspace)).as_deref(),
        Some("PR のタスクは優先度を高にする")
    );

    // ほかの場所では、全体の分だけ
    let pane = open_chat(cx, window, &view);
    world.reply(text_reply("はい。"));
    say(cx, window, &pane, "こんにちは");
    wait(cx, "返事が来る", |s| !s.busy && s.items.len() >= 2);
    let system = world.sent()[0].2["system"].as_str().unwrap().to_owned();
    assert!(system.ends_with("敬語で答える"), "{system}");
    let first = world.sent()[0].2["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!first.contains("前提と指示"), "{first}");

    // そのワークスペースを開くと、両方が渡る
    ui(cx, window, |_, cx| {
        view.update(cx, |v, cx| {
            v.go(malakt_mahoz::state::Nav::Workspace(workspace.clone()), cx)
        });
    });
    world.reply(text_reply("はい。"));
    say(cx, window, &pane, "もう一度");
    wait(cx, "返事が来る", |s| !s.busy && s.items.len() >= 4);
    // 最初の指示は変えず、ワークスペースの分はメッセージに添える
    let second = world.sent()[1].2.clone();
    assert_eq!(second["system"].as_str().unwrap(), system);
    let message = second["messages"][2]["content"].as_str().unwrap();
    assert!(
        message.ends_with(
            "ワークスペース「仕事」についての前提と指示:\nPR のタスクは優先度を高にする"
        ),
        "{message}"
    );
    // 前のメッセージは、送ったときのまま
    assert_eq!(second["messages"][0]["content"].as_str().unwrap(), first);

    // 長すぎるものは保存しない
    let long = "あ".repeat(malakt_mahoz::ai::MAX_WORKSPACE_CONTEXT_CHARS + 1);
    assert!(malakt_mahoz::ai::workspace_context_error(&long).is_some());
}

/// 設定を変えて最初の指示が変わると、前の考えのブロックをサーバーが断る。
/// 考えのブロックを外してやり直し、エラーは出さない。
#[gpui_kit::test]
fn a_rejected_thinking_block_is_dropped_and_retried(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    let pane = open_chat(cx, window, &view);
    world.reply(json!({"stop_reason": "end_turn", "content": [
        {"type": "thinking", "thinking": "", "signature": "sig"},
        {"type": "text", "text": "はい。"},
    ]}));
    say(cx, window, &pane, "こんにちは");
    wait(cx, "返事が来る", |s| !s.busy && s.items.len() >= 2);

    let session = session(cx);
    cx.update(|cx| session.update(cx, |s, cx| s.set_context("敬語で答える", cx)));
    world.fail(
        "サーバーがエラーを返しました（400）。messages.1.content.0: Invalid `signature` in `thinking` block. The block is bound to a different conversation.",
    );
    world.reply(text_reply("かしこまりました。"));
    say(cx, window, &pane, "もう一度");
    wait(cx, "返事が来る", |s| !s.busy && s.items.len() >= 4);

    let sent = world.sent();
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[1].2["messages"][1]["content"][0]["type"], "thinking");
    assert_eq!(
        sent[2].2["messages"][1]["content"],
        json!([{"type": "text", "text": "はい。"}])
    );
    let items = read(cx, |s| s.items.clone());
    assert!(
        !items.iter().any(|item| matches!(item, Item::Error(_))),
        "{items:?}"
    );
    assert_eq!(
        items.last(),
        Some(&Item::Assistant("かしこまりました。".into()))
    );
}

/// タスクをチャットにドラッグして落とすと入力欄に添えられ、送ると中身が AI に渡る。
#[gpui_kit::test]
fn a_task_dropped_on_the_chat_is_attached(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let session = session(cx);
    cx.update(|cx| session.update(cx, |s, cx| s.set_arrange(false, cx)));
    let task = new_task(cx, "発表資料を作る");
    let (window, view) = open(cx, wide());
    let pane = open_chat(cx, window, &view);
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    cx.run_until_parked();

    // 行を押して動かし、チャットの上で離す
    let drop = |cx: &mut TestAppContext| {
        ui(cx, window, |window, cx| {
            let at = window.find("ai-input").bounds().center();
            view.update(cx, |v, cx| {
                v.start_task_drag(&task, gpui_kit::point(px(500.), px(400.)), cx);
                v.drag_task(at, cx);
                v.drag_task_over(malakt_mahoz::state::DropTarget::Chat, at, cx);
            });
        });
        ui(cx, window, |_, cx| {
            view.update(cx, |v, cx| v.finish_task_drag(cx));
        });
    };
    drop(cx);
    assert_eq!(
        read(cx, |s| s.attached.clone()),
        vec![(task.clone(), "発表資料を作る".into())]
    );
    // 同じタスクは 1 つだけ。× で外せる
    drop(cx);
    assert_eq!(read(cx, |s| s.attached.len()), 1);
    ui(cx, window, |window, cx| {
        assert!(window.find(("ai-attached", 0usize)).visible());
        window.click(("ai-attached-remove", 0usize), cx);
    });
    assert!(read(cx, |s| s.attached.is_empty()));

    drop(cx);
    world.reply(text_reply("見ました。"));
    say(cx, window, &pane, "これを見て");
    wait(cx, "返事が来る", |s| !s.busy && s.items.len() >= 2);
    let message = world.sent()[0].2["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        message.starts_with("これを見て\n\n[添えたタスク「発表資料を作る」]\n"),
        "{message}"
    );
    assert!(message.contains(&task), "{message}");
    assert!(read(cx, |s| s.attached.is_empty()));
    assert_eq!(
        read(cx, |s| s.items[0].clone()),
        Item::User("「発表資料を作る」\nこれを見て".into())
    );
}

/// Enter で送り、Shift+Enter は改行になる。
#[gpui_kit::test]
fn enter_sends_and_shift_enter_breaks_the_line(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    let pane = open_chat(cx, window, &view);
    world.reply(text_reply("はい。"));
    ui(cx, window, |window, cx| {
        pane.update(cx, |pane, cx| pane.focus_input(window, cx));
        window.input("1 行目", cx);
        window.press("shift-enter", cx);
        window.input("2 行目", cx);
    });
    assert!(world.sent().is_empty());
    ui(cx, window, |window, cx| window.press("enter", cx));
    wait(cx, "返事が来る", |s| !s.busy && s.items.len() >= 2);
    let sent = world.sent();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].2["messages"][0]["content"]
        .as_str()
        .unwrap()
        .starts_with("1 行目\n2 行目\n\n（アプリからの補足）"));
}

/// リンクを読めなかったタスクは AI に渡さず、元のまま残す。
#[gpui_kit::test]
fn a_task_whose_link_cannot_be_read_is_left_alone(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    *world.github.lock().unwrap() = if cfg!(windows) {
        " & exit 1".into()
    } else {
        "; exit 1".into()
    };
    let task = new_task(cx, "https://github.com/o/r/pull/7");
    wait(cx, "理由が出る", |s| {
        !s.busy && !s.arrange_notes.is_empty()
    });
    assert!(world.sent().is_empty());
    let note = read(cx, |s| s.arrange_notes[&task].clone());
    assert!(note.contains("読み込めませんでした"), "{note}");
    let tasks = all_tasks(cx);
    assert_eq!(tasks[0].title, "https://github.com/o/r/pull/7");
    assert_eq!(tasks[0].description, "");
    assert_eq!(
        malakt_mahoz::ai::arrange_note_label(&note),
        "リンクを読み込めませんでした"
    );
    assert_eq!(
        malakt_mahoz::ai::arrange_note_label(
            "AI がタスクを整えられませんでした: サーバーがエラーを返しました（500）。overloaded"
        ),
        "AI に接続できませんでした"
    );

    // 読めるようになってからやり直すと、AI が整える
    world.github.lock().unwrap().clear();
    world.reply(tool_reply(
        "t1",
        "update_task",
        json!({"id": task, "title": "ログインを直す"}),
    ));
    world.reply(text_reply("整えました。"));
    let session = session(cx);
    cx.update(|cx| session.update(cx, |s, cx| s.retry_arrange(&task, cx)));
    wait(cx, "AI が整え終わる", |s| {
        !s.busy && s.arrange_notes.is_empty()
    });
    assert_eq!(all_tasks(cx)[0].title, "ログインを直す");
}

/// AI が「取得できなかった」「未確認」と書いても、タスクには入れない。
#[gpui_kit::test]
fn fetch_notes_never_reach_the_task(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let task = new_task(cx, "https://github.com/o/r/pull/7");
    world.reply(tool_reply(
        "t1",
        "update_task",
        json!({
            "id": task,
            "title": "PR 7（題名は未確認）",
            "description": "PR: https://github.com/o/r/pull/7\n\n※リンク先の内容を取得できなかったため、概要と題名は未確認（タイトルはそのまま）。",
        }),
    ));
    world.reply(text_reply("整えました。"));
    wait(cx, "AI が整え終わる", |s| {
        !s.busy && !s.runs.is_empty()
    });
    assert_eq!(world.sent().len(), 2);
    let message = world.sent()[0].2["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(message.contains("注記を書かない"), "{message}");
    let tasks = all_tasks(cx);
    assert_eq!(tasks[0].title, "https://github.com/o/r/pull/7");
    assert_eq!(tasks[0].description, "PR: https://github.com/o/r/pull/7");
}

/// API キーがなければ、どこにも通信しない。理由を出し、キーを入れると動く。
#[gpui_kit::test]
fn a_new_task_waits_for_the_key(cx: &mut TestAppContext) {
    let world = start(cx);
    let task = new_task(cx, "https://github.com/o/r/issues/3");
    wait(cx, "理由が出る", |s| !s.arrange_notes.is_empty());
    assert!(world.sent().is_empty());
    assert!(read(cx, |s| s.shell.is_empty()));
    let note = read(cx, |s| s.arrange_notes[&task].clone());
    assert!(note.contains("API キーがない"), "{note}");

    world.reply(text_reply("そのままにしました。"));
    let session = session(cx);
    cx.update(|cx| session.update(cx, |s, cx| s.save_key("k", cx)));
    wait(cx, "AI が整え終わる", |s| {
        !s.busy && !s.runs.is_empty()
    });
    assert_eq!(world.sent().len(), 1);
    assert!(read(cx, |s| s.arrange_notes.is_empty()));
}

/// 前からあるタスクと、オフにしている間のタスクは整えない。
#[gpui_kit::test]
fn old_tasks_are_left_alone(cx: &mut TestAppContext) {
    let mut store = Store::open_in_memory().unwrap();
    let draft = TaskDraft {
        title: "https://github.com/o/r/issues/1".into(),
        ..Default::default()
    };
    let old = store.create_task(None, &draft).unwrap();
    let world = start_with(cx, store);
    world.keys.set(Provider::Claude, "k").unwrap();
    // 前からあるタスクを書き換えても動かない
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            db.store
                .set_task_status(&old.id, Status::InProcess)
                .unwrap();
            cx.notify();
        })
    });
    cx.run_until_parked();
    let session = session(cx);
    cx.update(|cx| session.update(cx, |s, cx| s.set_arrange(false, cx)));
    new_task(cx, "https://github.com/o/r/issues/2");
    cx.run_until_parked();
    cx.update(|cx| session.update(cx, |s, cx| s.set_arrange(true, cx)));
    cx.run_until_parked();
    assert!(world.sent().is_empty());
    assert!(read(cx, |s| s.shell.is_empty() && !s.busy));
}

/// 入力欄の下のボタンから、その場でサービス・モデル・effort を選べる。
#[gpui_kit::test]
fn model_and_effort_are_picked_under_the_input(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    open_chat(cx, window, &view);
    ui(cx, window, |window, _| {
        for id in ["ai-pick-provider", "ai-pick-model", "ai-pick-effort"] {
            assert!(window.find(id).visible(), "{id}");
        }
    });
    let before = read(cx, |s| (s.effort, s.model().to_owned()));
    ui(cx, window, |window, cx| window.click("ai-pick-effort", cx));
    ui(cx, window, |window, cx| {
        window.press("down", cx);
        window.press("down", cx);
        window.press("enter", cx);
    });
    assert_eq!(read(cx, |s| s.effort), malakt_ai::Effort::ALL[1]);
    assert_ne!(read(cx, |s| s.effort), before.0);

    ui(cx, window, |window, cx| window.click("ai-pick-model", cx));
    ui(cx, window, |window, cx| {
        window.press("down", cx);
        window.press("down", cx);
        window.press("enter", cx);
    });
    assert_eq!(
        read(cx, |s| s.model().to_owned()),
        Provider::Claude.models()[1]
    );
}

/// 続けて実行したツールは 1 行にまとまり、押すと中身が開く。
#[gpui_kit::test]
fn tool_runs_fold_into_one_row(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    let pane = open_chat(cx, window, &view);
    world.reply(json!({"stop_reason": "tool_use", "content": [
        {"type": "tool_use", "id": "a", "name": "create_task", "input": {"title": "牛乳を買う"}},
        {"type": "tool_use", "id": "b", "name": "create_task", "input": {"title": "卵を買う"}},
    ]}));
    world.reply(text_reply("2 つ作りました。"));
    say(cx, window, &pane, "牛乳と卵");
    ui(cx, window, |window, cx| {
        assert!(window.find(("ai-tools", 1usize)).visible());
        assert!(window.try_find(("ai-tool", 1usize)).is_none());
        window.click(("ai-tools", 1usize), cx);
    });
    ui(cx, window, |window, _| {
        assert!(window.find(("ai-tool", 1usize)).visible());
        assert!(window.find(("ai-tool", 2usize)).visible());
    });
}

fn setting(cx: &mut TestAppContext, key: &str) -> Option<String> {
    cx.update(|cx| db::shared(cx).read(cx).store.setting(key).unwrap())
}

/// チャットの開閉、シェルの場所、会話は保存され、次に起動したときに戻る。
#[gpui_kit::test]
fn the_chat_comes_back_after_a_restart(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    let pane = open_chat(cx, window, &view);
    world.reply(text_reply("はい"));
    say(cx, window, &pane, "こんにちは");
    menu(cx, window, 1);
    cx.run_until_parked();
    let ui_state: Value = serde_json::from_str(&setting(cx, "ai.ui").unwrap()).unwrap();
    assert_eq!(
        (&ui_state["open"], &ui_state["chat"], &ui_state["shell"]),
        (&json!(true), &json!("docked"), &json!("closed"))
    );
    let index: Value = serde_json::from_str(&setting(cx, "ai.chats").unwrap()).unwrap();
    let id = index["current"].as_str().unwrap().to_owned();
    assert_eq!(index["chats"][0]["id"], json!(id));
    assert_eq!(index["chats"][0]["title"], json!("こんにちは"));
    let key = format!("ai.chat.{id}");
    let chat: Value = serde_json::from_str(&setting(cx, &key).unwrap()).unwrap();
    assert_eq!(chat["items"][0], json!({"k": "user", "t": "こんにちは"}));
    assert_eq!(chat["items"][1], json!({"k": "assistant", "t": "はい"}));
    assert_eq!(chat["turns"].as_array().unwrap().len(), 2);

    // 新しいチャットにしても、前の会話は一覧に残り、開き直せる
    ui(cx, window, |window, cx| window.click("ai-new-chat", cx));
    cx.run_until_parked();
    assert!(read(cx, |s| s.items.is_empty() && s.chats.len() == 1));
    assert!(setting(cx, &key).is_some());
    let index: Value = serde_json::from_str(&setting(cx, "ai.chats").unwrap()).unwrap();
    assert_ne!(index["current"], json!(id));
    ui(cx, window, |window, cx| window.click("ai-chats", cx));
    ui(cx, window, |window, cx| {
        window.click(("ai-chat-row", 0usize), cx)
    });
    assert_eq!(
        read(cx, |s| s.items.clone()),
        vec![
            Item::User("こんにちは".into()),
            Item::Assistant("はい".into())
        ]
    );
    // 続きを送ると、前の会話も AI に渡る
    world.reply(text_reply("続きです"));
    say(cx, window, &pane, "それで？");
    wait(cx, "返事が来る", |s| !s.busy && s.items.len() >= 4);
    let sent = world.sent();
    assert_eq!(
        sent.last().unwrap().2["messages"].as_array().unwrap().len(),
        3
    );
    assert_eq!(read(cx, |s| s.chats.len()), 1);

    // 消すと、保存した中身も消える
    ui(cx, window, |window, cx| window.click("ai-chats", cx));
    ui(cx, window, |window, cx| {
        window.click(("ai-chat-delete", 0usize), cx)
    });
    assert!(read(cx, |s| s.items.is_empty() && s.chats.is_empty()));
    assert_eq!(setting(cx, &key), None);
    ui(cx, window, |window, _| {
        assert!(window.find("ai-chats-empty").visible())
    });
}

/// 会話は決めた数までしか残さない。古いものから消える。
#[gpui_kit::test]
fn old_chats_are_dropped(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let session = session(cx);
    let mut first = String::new();
    for n in 0..MAX_CHATS + 2 {
        world.reply(text_reply("はい"));
        cx.update(|cx| {
            session.update(cx, |s, cx| {
                assert!(s.send(&format!("会話 {n}"), String::new(), cx));
            })
        });
        wait(cx, "返事が来る", |s| !s.busy && s.items.len() >= 2);
        if n == 0 {
            first = read(cx, |s| s.chat_id().to_owned());
        }
        cx.update(|cx| session.update(cx, |s, cx| s.clear(cx)));
    }
    cx.run_until_parked();
    let chats = read(cx, |s| s.chats.clone());
    assert_eq!(chats.len(), MAX_CHATS);
    assert_eq!(chats[0].title, format!("会話 {}", MAX_CHATS + 1));
    assert_eq!(setting(cx, &format!("ai.chat.{first}")), None);
}

/// 自動の実行は会話に混ぜない。動いている間は入力欄の上に出て、終わるとタスクの記録に残る。
#[gpui_kit::test]
fn automatic_runs_stay_out_of_the_chat(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    let pane = open_chat(cx, window, &view);
    world.reply(text_reply("はい"));
    say(cx, window, &pane, "こんにちは");
    wait(cx, "返事が来る", |s| !s.busy && s.items.len() >= 2);

    // gh が少しかかる間に、入力欄の上の表示を見る
    *world.github.lock().unwrap() = if cfg!(windows) {
        " & ping -n 2 127.0.0.1 > NUL".into()
    } else {
        "; sleep 0.5".into()
    };
    let task = new_task(cx, "https://github.com/o/r/issues/3");
    wait(cx, "AI が整え始める", |s| s.auto_running().is_some());
    ui(cx, window, |window, _| {
        assert!(window.find("ai-running").visible());
        assert!(window.try_find("ai-working").is_none());
    });
    assert_eq!(read(cx, |s| s.shown_items().len()), 2);
    world.reply(tool_reply(
        "t1",
        "update_task",
        json!({"id": task, "title": "ログインできない"}),
    ));
    world.reply(text_reply("タイトルを付け直しました。"));
    wait(cx, "AI が整え終わる", |s| {
        !s.busy && !s.runs.is_empty()
    });
    assert_eq!(read(cx, |s| s.items.len()), 2);
    ui(cx, window, |window, _| {
        assert!(window.try_find("ai-tray").is_none())
    });
    let run = read(cx, |s| s.runs_for(&task)[0].clone());
    assert_eq!((run.kind, run.error.clone()), (RunKind::Arrange, None));
    assert_eq!(
        run.items.last(),
        Some(&Item::Assistant("タイトルを付け直しました。".into()))
    );
    // 次に送る会話にも、自動の実行は入らない
    world.reply(text_reply("どうも"));
    say(cx, window, &pane, "ありがとう");
    wait(cx, "返事が来る", |s| !s.busy && s.items.len() >= 4);
    assert_eq!(
        world.sent().last().unwrap().2["messages"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    // タスクの詳細で、記録を開ける
    ui(cx, window, |window, cx| {
        view.update(cx, |view, cx| view.open_panel(&task, window, cx));
    });
    ui(cx, window, |window, cx| window.click("ai-log", cx));
    ui(cx, window, |window, _| {
        assert!(window.find(("ai-log-run", 0usize)).visible())
    });
    // 実行履歴にも出る
    menu(cx, window, 3);
    ui(cx, window, |window, cx| {
        window.click(("ai-run-row", 0usize), cx)
    });
    ui(cx, window, |window, _| {
        assert!(window.find(("ai-run-detail", 0usize)).visible())
    });

    // 記録は次の起動にも残る
    let saved: Value = serde_json::from_str(&setting(cx, "ai.runs").unwrap()).unwrap();
    assert_eq!(saved[0]["kind"], json!("arrange"));
    assert_eq!(saved[0]["tasks"][0]["id"], json!(task));
}

/// 失敗した実行は、入力欄の上に残る。やり直すか、消すまで出ている。
#[gpui_kit::test]
fn a_failed_run_stays_until_it_is_dismissed(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    let _pane = open_chat(cx, window, &view);
    world.fail("529 overloaded");
    let task = new_task(cx, "会場を予約する");
    wait(cx, "失敗が記録される", |s| {
        !s.busy && !s.runs.is_empty()
    });
    let run = read(cx, |s| s.runs[0].clone());
    assert!(run.error.as_ref().unwrap().contains("529"), "{run:?}");
    assert!(!run.seen);
    assert!(read(cx, |s| s.items.is_empty()));
    ui(cx, window, |window, _| {
        assert!(window.find(("ai-failed", 0usize)).visible())
    });
    // 押すと、そのタスクが開く
    ui(cx, window, |window, cx| {
        window.click(("ai-failed", 0usize), cx)
    });
    cx.update(|cx| assert_eq!(view.read(cx).panel.as_deref(), Some(task.as_str())));
    ui(cx, window, |window, cx| {
        window.click(("ai-failed-dismiss", 0usize), cx)
    });
    ui(cx, window, |window, _| {
        assert!(window.try_find("ai-tray").is_none())
    });
    // 履歴には残り、そこからやり直せる
    menu(cx, window, 3);
    world.reply(text_reply("そのままにしました。"));
    ui(cx, window, |window, cx| {
        window.click(("ai-run-retry", 0usize), cx)
    });
    wait(cx, "やり直しが終わる", |s| {
        !s.busy && s.runs.len() == 2
    });
    assert_eq!(read(cx, |s| s.runs[1].error.clone()), None);
}

/// タスクの詳細から、そのタスクについての会話を開ける。
#[gpui_kit::test]
fn a_task_has_its_own_chat(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    let task = new_task(cx, "会場を予約する");
    world.reply(text_reply("そのままにしました。"));
    wait(cx, "AI が整え終わる", |s| {
        !s.busy && !s.runs.is_empty()
    });
    ui(cx, window, |window, cx| {
        view.update(cx, |view, cx| view.open_panel(&task, window, cx));
    });
    ui(cx, window, |window, cx| window.click("ai-task-chat", cx));
    cx.update(|cx| assert!(view.read(cx).ai_open));
    assert_eq!(
        read(cx, |s| s.attached.clone()),
        vec![(task.clone(), "会場を予約する".into())]
    );
    let pane = cx.update(|cx| view.read(cx).ai_pane.clone().unwrap());
    world.reply(text_reply("予約の手順です"));
    say(cx, window, &pane, "どう進める？");
    wait(cx, "返事が来る", |s| !s.busy && s.items.len() >= 2);
    cx.run_until_parked();
    let chats = read(cx, |s| s.chats.clone());
    assert_eq!(chats[0].title, "会場を予約する");
    assert_eq!(chats[0].task.as_deref(), Some(task.as_str()));

    // ほかの会話に移っても、タスクからは同じ会話に戻る
    ui(cx, window, |window, cx| window.click("ai-new-chat", cx));
    assert!(read(cx, |s| s.items.is_empty()));
    ui(cx, window, |window, cx| window.click("ai-task-chat", cx));
    assert_eq!(read(cx, |s| s.items.len()), 2);
    assert_eq!(read(cx, |s| s.chats.len()), 1);
}

/// 入力欄に貼ったタスクのリンクは、ドラッグしたときと同じように入力欄に添えられる。
#[gpui_kit::test]
fn a_pasted_task_link_is_attached(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    let task = new_task(cx, "会場を予約する");
    world.reply(text_reply("そのままにしました。"));
    wait(cx, "AI が整え終わる", |s| {
        !s.busy && !s.runs.is_empty()
    });
    let pane = open_chat(cx, window, &view);

    // タスクが見つからないリンクは、文字のまま残る
    world.reply(text_reply("予約の手順です"));
    say(
        cx,
        window,
        &pane,
        &format!("malakt-mahoz://task/{task} malakt-mahoz://task/nothing どう進める？"),
    );
    wait(cx, "返事が来る", |s| !s.busy && s.items.len() >= 2);
    assert_eq!(
        read(cx, |s| s.items[0].clone()),
        Item::User("「会場を予約する」\nmalakt-mahoz://task/nothing どう進める？".into())
    );
    let sent = world.sent();
    let message = sent.last().unwrap().2["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        message.contains("[添えたタスク「会場を予約する」]"),
        "{message}"
    );
    assert!(
        !message.contains(&format!("malakt-mahoz://task/{task}")),
        "{message}"
    );
}

/// モデルは名前で出す。サービスの一覧にあった名前は、次の起動でも使う。
#[gpui_kit::test]
fn models_are_shown_by_name(cx: &mut TestAppContext) {
    let mut store = Store::open_in_memory().unwrap();
    store
        .set_setting("ai.model_names", Some(r#"{"glm-5.3":"GLM 5.3"}"#))
        .unwrap();
    let world = start_with(cx, store);
    world.keys.set(Provider::Claude, "k").unwrap();
    assert_eq!(read(cx, |s| s.model_label("glm-5.3")), "GLM 5.3");
    assert_eq!(
        read(cx, |s| s.model_label("claude-opus-5-5")),
        "Claude Opus 5.5"
    );
    assert_eq!(read(cx, |s| s.model_label("kimi-k3")), "kimi-k3");
}

#[gpui_kit::test]
fn a_saved_chat_is_shown_at_startup(cx: &mut TestAppContext) {
    let mut store = Store::open_in_memory().unwrap();
    store
        .set_setting(
            "ai.ui",
            Some(r#"{"open":true,"chat":"docked","shell":"closed"}"#),
        )
        .unwrap();
    store
        .set_setting(
            "ai.chat",
            Some(
                &json!({"id": "c1", "items": [
                    {"k": "user", "t": "こんにちは"}, {"k": "assistant", "t": "はい"}],
                    "turns": [{"k": "user", "t": "こんにちは"},
                        {"k": "assistant", "t": "はい", "calls": []}]})
                .to_string(),
            ),
        )
        .unwrap();
    let world = start_with(cx, store);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    cx.run_until_parked();
    // 押さなくても、チャットが開いていて、前の会話が出ている。シェルは閉じたまま
    cx.update(|cx| assert!(view.read(cx).ai_open));
    ui(cx, window, |window, _| {
        assert!(window.find(("ai-user", 0usize)).visible());
        assert!(window.find(("ai-assistant", 1usize)).visible());
        assert!(window.try_find("ai-shell").is_none());
    });
    // 続きを送ると、前の会話も AI に渡る
    let pane = cx.update(|cx| view.read(cx).ai_pane.clone().unwrap());
    world.reply(text_reply("続きです"));
    say(cx, window, &pane, "続けて");
    let messages = world.sent()[0].2["messages"].as_array().unwrap().clone();
    assert_eq!(messages.len(), 3);
}

/// 境界で決めたチャットとシェルの幅は保存され、次の起動で戻る。
#[gpui_kit::test]
fn pane_widths_are_kept(cx: &mut TestAppContext) {
    let mut store = Store::open_in_memory().unwrap();
    store
        .set_setting(
            "ai.ui",
            Some(r#"{"open":true,"chat":"docked","shell":"docked","cw":500,"sw":300}"#),
        )
        .unwrap();
    start_with(cx, store);
    let (window, _view) = open(cx, size(px(2400.), px(1125.)));
    cx.run_until_parked();
    assert_eq!(read(cx, |s| (s.chat_width, s.shell_width)), (500., 300.));
    ui(cx, window, |window, _| {
        // 拡大率 1.25 を掛けた幅で並ぶ
        let chat = window.find("ai-chat").bounds();
        let shell = window.find("ai-shell").bounds();
        // 左の境界の線の分だけ狭い
        assert!(chat.size.width + shell.size.width >= px(996.));
        assert!(chat.size.width + shell.size.width <= px(1000.));
        assert!(shell.size.width >= px(373.) && shell.size.width <= px(375.));
        assert!(window.find("ai-resize").visible());
        assert!(window.find("ai-shell-resize").visible());
    });

    // 範囲の外には出ない。変えた幅は保存される
    let session = session(cx);
    cx.update(|cx| session.update(cx, |s, cx| s.set_widths(100., 5000., cx)));
    cx.run_until_parked();
    assert_eq!(read(cx, |s| (s.chat_width, s.shell_width)), (320., 900.));
    let saved: Value = serde_json::from_str(&setting(cx, "ai.ui").unwrap()).unwrap();
    assert_eq!(
        (saved["cw"].as_f64(), saved["sw"].as_f64()),
        (Some(320.), Some(900.))
    );
}

/// 前からあるタスクを 1 つ入れて始める（新しいタスクを整えるほうが動かないようにする）。
fn start_with_task(cx: &mut TestAppContext, title: &str) -> (World, String) {
    let mut store = Store::open_in_memory().unwrap();
    let draft = TaskDraft {
        title: title.into(),
        ..Default::default()
    };
    let task = store.create_task(None, &draft).unwrap();
    (start_with(cx, store), task.id)
}

fn add_comment(cx: &mut TestAppContext, task: &str, body: &str) -> String {
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            let comment = db.store.add_comment(task, None, body).unwrap();
            cx.notify();
            comment.id
        })
    })
}

/// コメントを書いてからの待ち時間を進める。
fn settle_comments(cx: &mut TestAppContext) {
    cx.run_until_parked();
    cx.executor().advance_clock(ai::TRACK_DELAY);
    wait(cx, "AI が更新し終わる", |s| !s.busy);
}

/// 画面でコメントを書くと、少し待ってから AI が進捗と状態を更新する。
/// 変えたところは詳細に出て、ボタンで元に戻せる。
#[gpui_kit::test]
fn a_comment_updates_the_progress(cx: &mut TestAppContext) {
    let (world, task) = start_with_task(cx, "記事を書く");
    world.keys.set(Provider::Claude, "k").unwrap();
    // コメント欄は詳細の下のほうにあるので、縦に長いウィンドウで開く
    let (window, view) = open(cx, size(px(2000.), px(2400.)));
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |v, cx| {
            v.inputs.comment.update(cx, |s, cx| {
                s.set_value("導入と本文は書いた。あとは図を 2 枚".to_owned(), window, cx)
            })
        })
    })
    .unwrap();
    ui(cx, window, |window, cx| window.click("comment-submit", cx));

    // 待ち時間の前には送らない
    assert!(world.sent().is_empty());
    world.reply(tool_reply(
        "t1",
        "update_task",
        json!({"id": task, "percent": 60, "status": "in_process", "due": "2026-12-01"}),
    ));
    world.reply(text_reply("進捗を 60% にしました。"));
    settle_comments(cx);

    let sent = world.sent();
    assert_eq!(sent.len(), 2);
    let message = sent[0].2["messages"][0]["content"].as_str().unwrap();
    assert!(
        message.contains(&format!("## タスク（id: {task}）")),
        "{message}"
    );
    assert!(
        message.contains("導入と本文は書いた。あとは図を 2 枚"),
        "{message}"
    );
    let now = all_tasks(cx).remove(0);
    assert_eq!((now.percent, now.status), (60, Status::InProcess));
    let label = read(cx, |s| s.tracked[&task].label.clone());
    assert_eq!(label.as_ref(), "0% → 60%・進行中・期限 2026-12-01");
    // 開いている詳細の期限の欄にも出る
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).inputs.due.read(cx).value().as_ref(),
            "2026-12-01"
        );
    });
    ui(cx, window, |window, _| {
        assert!(window.try_find("ai-tracked-undo").is_some());
    });

    // AI が変えた分では、もう一度動かない
    settle_comments(cx);
    assert_eq!(world.sent().len(), 2);

    // 元に戻すと、前の進捗と状態になる
    ui(cx, window, |window, cx| window.click("ai-tracked-undo", cx));
    let now = all_tasks(cx).remove(0);
    assert_eq!(
        (now.percent, now.status, now.due),
        (0, Status::NeedsAction, None)
    );
    cx.update(|cx| {
        assert_eq!(view.read(cx).inputs.due.read(cx).value().as_ref(), "");
    });
    ui(cx, window, |window, _| {
        assert!(window.try_find("ai-tracked-undo").is_none());
    });
    assert_eq!(world.sent().len(), 2);
}

/// 続けて書いたコメントと書き直しは、手が止まってから 1 回にまとめて送る。
/// 手で進捗を変えると、そのあとに元に戻しても手で変えた値が残る。
#[gpui_kit::test]
fn comments_are_sent_once_after_a_pause(cx: &mut TestAppContext) {
    let (world, task) = start_with_task(cx, "引っ越しの準備");
    world.keys.set(Provider::Claude, "k").unwrap();
    cx.run_until_parked();
    let first = add_comment(cx, &task, "箱を買った");
    cx.run_until_parked();
    cx.executor().advance_clock(ai::TRACK_DELAY / 2);
    cx.run_until_parked();
    add_comment(cx, &task, "台所を詰めた");
    cx.run_until_parked();
    cx.executor().advance_clock(ai::TRACK_DELAY / 2);
    cx.run_until_parked();
    assert!(world.sent().is_empty(), "書いている間は送らない");

    world.reply(tool_reply(
        "t1",
        "update_task",
        json!({"id": task, "percent": 30, "due": "2026-12-20"}),
    ));
    world.reply(text_reply("更新しました。"));
    settle_comments(cx);
    assert_eq!(world.sent().len(), 2);
    let message = world.sent()[0].2["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(message.contains("箱を買った") && message.contains("台所を詰めた"));
    let label = read(cx, |s| s.tracked[&task].label.clone());
    assert_eq!(label.as_ref(), "0% → 30%・期限 2026-12-20");

    // コメントを書き直しても動く。続けて更新した分は、最初の値まで戻せる
    std::thread::sleep(Duration::from_millis(2));
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            db.store
                .update_comment(&first, "箱を買って、半分詰めた")
                .unwrap();
            cx.notify();
        })
    });
    world.reply(tool_reply(
        "t2",
        "update_task",
        json!({"id": task, "percent": 50}),
    ));
    world.reply(text_reply("更新しました。"));
    settle_comments(cx);
    assert_eq!(world.sent().len(), 4);
    let label = read(cx, |s| s.tracked[&task].label.clone());
    assert_eq!(label.as_ref(), "0% → 50%・期限 2026-12-20");

    // 手で進捗を変えたあとに戻すと、手で変えた進捗は残り、期限だけが戻る
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            let mut draft = TaskDraft::from(&db.store.task(&task).unwrap());
            draft.percent = 75;
            db.store.update_task(&task, &draft).unwrap();
            cx.notify();
        })
    });
    // 手で変えたあとも、残りの分（期限）は元に戻せる
    let now = all_tasks(cx).remove(0);
    let left = read(cx, |s| s.tracked_for(&now));
    assert_eq!(left.as_deref(), Some("期限 2026-12-20"));
    let session = session(cx);
    cx.update(|cx| session.update(cx, |s, cx| s.undo_track(&task, cx)));
    let now = all_tasks(cx).remove(0);
    assert_eq!((now.percent, now.due), (75, None));
}

/// API キーがなければ、コメントを書いてもどこにも通信せず、何も出さない。オフにしても同じ。
#[gpui_kit::test]
fn comments_stay_offline_without_a_key_or_when_turned_off(cx: &mut TestAppContext) {
    let (world, task) = start_with_task(cx, "記事を書く");
    cx.run_until_parked();
    add_comment(cx, &task, "半分書いた");
    settle_comments(cx);
    assert!(world.sent().is_empty());
    assert!(read(cx, |s| s.track_notes.is_empty() && s.items.is_empty()));

    world.keys.set(Provider::Claude, "k").unwrap();
    let session = session(cx);
    cx.update(|cx| {
        session.update(cx, |s, cx| {
            s.refresh_key(cx);
            s.set_track(false, cx);
        })
    });
    add_comment(cx, &task, "8 割書いた");
    settle_comments(cx);
    assert!(world.sent().is_empty());
    assert_eq!(setting(cx, "ai.track").as_deref(), Some("0"));

    // オンに戻しても、オフの間のコメントでは動かない
    cx.update(|cx| session.update(cx, |s, cx| s.set_track(true, cx)));
    settle_comments(cx);
    assert!(world.sent().is_empty());
}

/// AI に届かなかったときは、短い言葉とやり直しのボタンを出す。やり直すと更新される。
#[gpui_kit::test]
fn a_failed_update_from_a_comment_can_be_retried(cx: &mut TestAppContext) {
    let (world, task) = start_with_task(cx, "記事を書く");
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, _view) = open(cx, wide());
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    add_comment(cx, &task, "半分書いた");
    world.fail("connection refused");
    settle_comments(cx);
    let note = read(cx, |s| s.track_notes[&task].clone());
    assert_eq!(ai::arrange_note_label(&note), "AI に接続できませんでした");
    ui(cx, window, |window, _| {
        assert!(window.try_find("ai-track-note").is_some());
    });

    world.reply(tool_reply(
        "t1",
        "update_task",
        json!({"id": task, "percent": 50}),
    ));
    world.reply(text_reply("更新しました。"));
    ui(cx, window, |window, cx| window.click("ai-track-retry", cx));
    wait(cx, "AI が更新し終わる", |s| !s.busy);
    assert_eq!(all_tasks(cx)[0].percent, 50);
    assert!(read(cx, |s| s.track_notes.is_empty()));
    ui(cx, window, |window, _| {
        assert!(window.try_find("ai-track-note").is_none());
    });
}

/// コメントからの更新では、タイトルと説明を変えず、ほかのタスクにも触らず、コメントも書かない。
#[gpui_kit::test]
fn an_update_from_a_comment_touches_only_that_task(cx: &mut TestAppContext) {
    let (world, task) = start_with_task(cx, "記事を書く");
    world.keys.set(Provider::Claude, "k").unwrap();
    let session = session(cx);
    cx.update(|cx| session.update(cx, |s, cx| s.set_arrange(false, cx)));
    let other = new_task(cx, "買い物");
    cx.run_until_parked();
    add_comment(cx, &task, "ほぼ終わり");
    world.reply(tool_reply(
        "t1",
        "update_task",
        json!({"id": task, "title": "記事（ほぼ完成）", "description": "メモ", "percent": 90,
               "categories": ["執筆"], "parent_id": other, "assignee_ids": []}),
    ));
    world.reply(tool_reply(
        "t2",
        "update_task",
        json!({"id": other, "percent": 90}),
    ));
    // 読むだけのツールは、渡した条件のまま使える
    world.reply(tool_reply("t4", "list_tasks", json!({"query": "買い物"})));
    world.reply(tool_reply(
        "t3",
        "add_comment",
        json!({"task_id": task, "body": "進捗を更新しました"}),
    ));
    world.reply(text_reply("更新しました。"));
    settle_comments(cx);
    let sent = world.sent();
    assert_eq!(sent.len(), 5);
    // `list_tasks` の結果は、次のリクエストの最後のメッセージで AI に返る
    let listed = sent[3].2["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .to_string();
    assert!(
        listed.contains("買い物") && !listed.contains("記事を書く"),
        "{listed}"
    );
    let tasks = all_tasks(cx);
    let find = |id: &str| tasks.iter().find(|t| t.id == id).unwrap();
    let now = find(&task);
    assert_eq!(
        (now.title.as_str(), now.description.as_str(), now.percent),
        ("記事を書く", "", 90)
    );
    assert!(now.categories.is_empty() && now.parent_id.is_none());
    assert_eq!(find(&other).percent, 0);
    let comments = cx.update(|cx| db::shared(cx).read(cx).store.comments(&task).unwrap());
    assert_eq!(comments.len(), 1);
}

/// 「設定」のスイッチで、コメントからの更新を切れる。
#[gpui_kit::test]
fn updates_from_comments_are_turned_off_in_settings(cx: &mut TestAppContext) {
    let (world, task) = start_with_task(cx, "記事を書く");
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    open_chat(cx, window, &view);
    menu(cx, window, 0);
    assert!(read(cx, |s| s.track));
    ui(cx, window, |window, cx| window.click("ai-track", cx));
    assert!(!read(cx, |s| s.track));
    assert_eq!(setting(cx, "ai.track").as_deref(), Some("0"));
    add_comment(cx, &task, "半分書いた");
    settle_comments(cx);
    assert!(world.sent().is_empty());
}

fn first_workspace(cx: &mut TestAppContext) -> malakt_core::Workspace {
    cx.update(|cx| db::shared(cx).read(cx).store.workspaces().unwrap())
        .remove(0)
}

/// 追加したワークスペースに名前を付けると、AI がアイコンを選ぶ。チャットには何も出ない。
#[gpui_kit::test]
fn a_named_workspace_gets_an_icon(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    world.reply(text_reply("penguin, bird, zoo"));
    ui(cx, window, |window, cx| window.click("add-workspace", cx));
    ui(cx, window, |window, cx| {
        window.input("ペンギンの世話", cx);
        window.press("enter", cx);
    });
    for _ in 0..500 {
        cx.run_until_parked();
        if first_workspace(cx).icon.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let saved = first_workspace(cx);
    assert_eq!(saved.name, "ペンギンの世話");
    assert_eq!(saved.icon.as_deref(), Some("emoji-penguin"));
    // 名前だけを送り、ツールは渡さない
    let sent = world.sent();
    assert_eq!(sent.len(), 1);
    let body = sent[0].2.to_string();
    assert!(body.contains("ペンギンの世話"), "{body}");
    assert!(!body.contains("update_task"), "{body}");
    assert!(read(cx, |s| s.items.is_empty() && !s.busy));
    // 左のメニューにも出る
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.workspaces[0].icon.as_deref(), Some("emoji-penguin"));
    });

    // 名前を変えただけでは、AI に聞かない
    let id = saved.id.clone();
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |v, cx| v.rename_workspace(id, window, cx))
    })
    .unwrap();
    cx.run_until_parked();
    ui(cx, window, |window, cx| {
        window.input("シロクマの世話", cx);
        window.press("enter", cx);
    });
    cx.run_until_parked();
    assert_eq!(first_workspace(cx).name, "シロクマの世話");
    assert_eq!(world.sent().len(), 1);
}

/// API キーがなければ、名前を付けても通信しない。アイコンは最初のまま。
#[gpui_kit::test]
fn a_named_workspace_keeps_its_icon_without_a_key(cx: &mut TestAppContext) {
    let world = start(cx);
    let (window, _view) = open(cx, wide());
    ui(cx, window, |window, cx| window.click("add-workspace", cx));
    ui(cx, window, |window, cx| {
        window.input("ペンギンの世話", cx);
        window.press("enter", cx);
    });
    for _ in 0..20 {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
    let saved = first_workspace(cx);
    assert_eq!(saved.name, "ペンギンの世話");
    assert_eq!(saved.icon, None);
    assert!(world.sent().is_empty());
    assert!(read(cx, |s| s.items.is_empty()));
}

/// AI の答えに合うアイコンがなければ、チャットに理由を出して、アイコンは最初のままにする。
#[gpui_kit::test]
fn an_icon_that_cannot_be_picked_is_reported(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, _view) = open(cx, wide());
    world.reply(text_reply("zzzzqqq"));
    ui(cx, window, |window, cx| window.click("add-workspace", cx));
    ui(cx, window, |window, cx| {
        window.input("なぞ", cx);
        window.press("enter", cx);
    });
    wait(cx, "理由が出る", |s| !s.items.is_empty());
    let shown = read(cx, |s| match &s.items[0] {
        Item::Error(text) => text.to_string(),
        other => panic!("{other:?}"),
    });
    assert!(
        shown.contains("「なぞ」のアイコンを AI が選べませんでした"),
        "{shown}"
    );
    let saved = first_workspace(cx);
    assert_eq!(saved.icon, None);
}

/// 「新しいタスク」で追加したタスクは、タイトルを書き終えるまで AI が整えない。
#[gpui_kit::test]
fn a_task_being_named_waits_for_its_title(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    cx.run_until_parked();

    ui(cx, window, |window, cx| window.click("new-task-open", cx));
    for _ in 0..20 {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
    cx.update(|cx| assert!(view.read(cx).naming_task.is_some()));
    assert!(world.sent().is_empty(), "書いている間は送らない");

    world.reply(text_reply("そのままにしました。"));
    ui(cx, window, |window, cx| {
        window.input("会場を予約する", cx);
        window.press("enter", cx);
    });
    wait(cx, "AI が整え終わる", |s| {
        !s.busy && !s.runs.is_empty()
    });
    let sent = world.sent();
    assert_eq!(sent.len(), 1);
    let body = sent[0].2.to_string();
    assert!(body.contains("会場を予約する"), "{body}");
    assert!(!body.contains("新規タスク"), "{body}");
}

/// タイトルを書いている間に AI がほかの仕事でツールを使っても、書き終えたタスクは整えられる。
#[gpui_kit::test]
fn a_task_being_named_is_arranged_after_the_ai_used_a_tool(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    cx.run_until_parked();
    ui(cx, window, |window, cx| window.click("new-task-open", cx));
    for _ in 0..20 {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
    cx.update(|cx| assert!(view.read(cx).naming_task.is_some()));

    world.reply(tool_reply(
        "t1",
        "create_task",
        json!({"title": "牛乳を買う"}),
    ));
    world.reply(text_reply("作りました。"));
    let session = session(cx);
    cx.update(|cx| {
        session.update(cx, |s, cx| {
            s.send("牛乳のタスクを作って", String::new(), cx)
        })
    });
    wait(cx, "AI が作り終わる", |s| {
        !s.busy && !s.items.is_empty()
    });
    let before = world.sent().len();

    world.reply(text_reply("そのままにしました。"));
    ui(cx, window, |window, cx| {
        window.input("会場を予約する", cx);
        window.press("enter", cx);
    });
    for _ in 0..50 {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
    wait(cx, "AI が整え終わる", |s| !s.busy);
    let sent = world.sent();
    assert_eq!(sent.len(), before + 1, "書き終えたタスクを整える");
    assert!(sent[before].2.to_string().contains("会場を予約する"));
}

/// AI が更新している間に手で変えた分は、AI の変更に数えない。元に戻しても残る。
#[gpui_kit::test]
fn edits_made_during_an_update_are_not_undone(cx: &mut TestAppContext) {
    let (world, task) = start_with_task(cx, "記事を書く");
    world.keys.set(Provider::Claude, "k").unwrap();
    cx.run_until_parked();
    add_comment(cx, &task, "半分書いた");
    // AI に届かなかった間に、手で期限を入れる
    world.fail("connection refused");
    settle_comments(cx);
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            let mut draft = TaskDraft::from(&db.store.task(&task).unwrap());
            draft.due = Some("2026-12-24".into());
            db.store.update_task(&task, &draft).unwrap();
            cx.notify();
        })
    });
    assert!(read(cx, |s| s.tracked.is_empty()));

    world.reply(tool_reply(
        "t1",
        "update_task",
        json!({"id": task, "percent": 50}),
    ));
    world.reply(text_reply("更新しました。"));
    let session = session(cx);
    cx.update(|cx| session.update(cx, |s, cx| s.retry_track(&task, cx)));
    wait(cx, "AI が更新し終わる", |s| !s.busy);
    let label = read(cx, |s| s.tracked[&task].label.clone());
    assert_eq!(label.as_ref(), "0% → 50%");
    cx.update(|cx| session.update(cx, |s, cx| s.undo_track(&task, cx)));
    let now = all_tasks(cx).remove(0);
    assert_eq!((now.percent, now.due.as_deref()), (0, Some("2026-12-24")));
}

/// メインのウィンドウと一緒に閉じたチャットとシェルのウィンドウは、別ウィンドウのまま覚えていて、
/// 次の起動で同じ位置と大きさに開く。
#[gpui_kit::test]
fn windows_closed_with_the_main_window_are_remembered(cx: &mut TestAppContext) {
    let world = start(cx);
    world.keys.set(Provider::Claude, "k").unwrap();
    cx.update(malakt_mahoz::quit_when_main_closes);
    let (window, view) = cx.update(|cx| malakt_mahoz::open_main_window(cx).unwrap());
    open_chat(cx, window, &view);
    ui(cx, window, |window, cx| window.click("ai-shell-detach", cx));
    let shell_window = match read(cx, |s| s.shell_place) {
        ShellPlace::Window(handle) => handle,
        other => panic!("シェルが別ウィンドウに出る: {other:?}"),
    };
    menu(cx, window, 2);
    let chat_window = read(cx, |s| s.chat_window).expect("チャットが別ウィンドウに出る");
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.windows().len()), 3);
    let bounds = read(cx, |s| s.window_bounds);

    // メインを閉じると、チャットとシェルのウィンドウも閉じる
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.windows().len()), 0);
    assert!(cx.update_window(chat_window, |_, _, _| ()).is_err());
    assert!(cx.update_window(shell_window, |_, _, _| ()).is_err());

    // 別ウィンドウのまま、位置と大きさも一緒に残っている
    let saved = cx.update(|cx| {
        let store = &db::shared(cx).read(cx).store;
        store.setting("ai.ui").unwrap().expect("画面の状態が残る")
    });
    let saved: Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(saved["open"], true);
    assert_eq!(saved["chat"], "window");
    assert_eq!(saved["shell"], "window");
    assert_eq!(saved["chat_bounds"], json!(bounds[0]));
    assert_eq!(saved["shell_bounds"], json!(bounds[1]));
}
