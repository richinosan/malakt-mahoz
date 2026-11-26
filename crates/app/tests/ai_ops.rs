//! AI チャットから、アプリの操作（画面、カスタマイズ、設定、データ）を頼むシナリオテスト。
//! AI の返事だけを差し替え、ツールは本物の画面とデータベースで動かす。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext, Bounds, Entity, Point, TestAppContext, WindowBounds,
    WindowOptions,
};
use malakt_ai::keys::{KeyStore, MemoryKeys};
use malakt_ai::shell::Spec;
use malakt_ai::Provider;
use malakt_core::view::Sort;
use malakt_core::{Scope, Store, TaskDraft, TaskFilter};
use malakt_mahoz::ai::{self, Item, Session, ToolState};
use malakt_mahoz::state::Column;
use malakt_mahoz::theme::{self, Choice, Density, Scene, Slot};
use malakt_mahoz::{db, AiConfig, AiPane, MainView, Modal, Nav};
use serde_json::{json, Value};

type Sent = Arc<Mutex<Vec<Value>>>;

struct World {
    sent: Sent,
    replies: Arc<Mutex<VecDeque<Value>>>,
}

impl World {
    /// AI がツールを 1 つ呼び、その結果を受けて `done` と答える。
    fn call(&self, name: &str, input: Value, done: &str) {
        let mut replies = self.replies.lock().unwrap();
        let id = format!("toolu_{}", replies.len());
        replies.push_back(json!({"stop_reason": "tool_use", "content": [
            {"type": "tool_use", "id": id, "name": name, "input": input},
        ]}));
        replies.push_back(
            json!({"stop_reason": "end_turn", "content": [{"type": "text", "text": done}]}),
        );
    }

    /// 最後に送ったリクエストの、最後のツールの結果。
    fn last_result(&self) -> (String, bool) {
        let sent = self.sent.lock().unwrap();
        let body = sent.last().expect("送っている");
        let messages = body["messages"].as_array().unwrap();
        let result = messages
            .iter()
            .rev()
            .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
            .find(|c| c["type"] == "tool_result")
            .expect("ツールの結果がある");
        (
            result["content"].as_str().unwrap_or_default().to_owned(),
            result["is_error"] == true,
        )
    }
}

fn start(cx: &mut TestAppContext, store: Store) -> World {
    cx.executor().allow_parking();
    let world = World {
        sent: Sent::default(),
        replies: Arc::default(),
    };
    let keys = Arc::new(MemoryKeys::default());
    keys.set(Provider::Claude, "sk-ant-1").unwrap();
    let (sent, replies) = (world.sent.clone(), world.replies.clone());
    let config = AiConfig {
        keys,
        env: Arc::new(|_| None),
        post: Arc::new(move |_, _, body| {
            sent.lock().unwrap().push(body.clone());
            replies
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "テストに返事が用意されていません".into())
        }),
        get: Arc::new(|url, _| Err(format!("テストでは {url} を読めません"))),
        gh: Arc::new(|_| Spec::shell("exit 1")),
        slack: Arc::new(|_| Vec::new()),
        whoami: Arc::new(|_| vec![Spec::shell("exit 1")]),
        gh_progress: Arc::new(|_| vec![Spec::shell("exit 1")]),
    };
    cx.update(|cx| malakt_mahoz::init_with(store, config, cx));
    world
}

fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<MainView>) {
    cx.update(|cx| {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: size(px(2000.), px(1125.)),
            })),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| {
                let mut view = MainView::new(window, cx);
                view.motion = false;
                view
            })
        })
        .expect("ウィンドウを開く")
    })
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

fn open_chat(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    view: &Entity<MainView>,
) -> Entity<AiPane> {
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    cx.update(|cx| view.read(cx).ai_pane.clone())
        .expect("チャットが開く")
}

fn say(cx: &mut TestAppContext, window: AnyWindowHandle, pane: &Entity<AiPane>, text: &str) {
    ui(cx, window, |window, cx| {
        pane.update(cx, |pane, cx| pane.focus_input(window, cx));
        window.input(text, cx);
        window.click("ai-send", cx);
    });
}

fn session(cx: &mut TestAppContext) -> Entity<Session> {
    cx.update(|cx| ai::shared(cx))
}

/// 最後のツールの呼び出しの表示。
fn last_tool(cx: &mut TestAppContext) -> (String, ToolState) {
    let session = session(cx);
    cx.update(|cx| {
        session
            .read(cx)
            .items
            .iter()
            .rev()
            .find_map(|item| match item {
                Item::Tool { summary, state } => Some((summary.to_string(), state.clone())),
                _ => None,
            })
            .expect("ツールを呼んでいる")
    })
}

fn tasks(cx: &mut TestAppContext) -> Vec<malakt_core::Task> {
    cx.update(|cx| {
        db::shared(cx)
            .read(cx)
            .store
            .tasks(Scope::All, &TaskFilter::default())
            .unwrap()
    })
}

/// 「ダークフォレストにして」「行を詰めて、アクセントを青に」のように頼むと、AI がその場で見た目を変える。
/// 今の見た目を自分のテーマとして保存でき、削除だけは確認が出る。
#[gpui_kit::test]
fn the_ai_changes_the_customization(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, view) = open(cx);
    let pane = open_chat(cx, window, &view);

    world.call(
        "apply_theme",
        json!({"name": "ダークフォレスト"}),
        "変えました。",
    );
    say(cx, window, &pane, "ダークフォレストにして");
    assert_eq!(
        last_tool(cx),
        ("テーマを変更: ダークフォレスト".into(), ToolState::Done)
    );
    cx.update(|cx| {
        assert_eq!(theme::choice(cx), Choice::Dark);
        assert_eq!(theme::scene(cx), Scene::Forest);
        assert!(theme::floating(cx));
    });

    world.call(
        "set_appearance",
        json!({"density": "compact", "accent": "#2f8fe0", "mode": "light", "cards": true}),
        "変えました。",
    );
    say(
        cx,
        window,
        &pane,
        "ライトにして、行を詰めて、アクセントを青に、一覧はカードで",
    );
    cx.update(|cx| {
        assert_eq!(theme::choice(cx), Choice::Light);
        assert_eq!(theme::density(cx), Density::Compact);
        assert!(theme::cards(cx));
        assert_eq!(theme::shown(Slot::Accent, cx).hex(), "#2f8fe0");
    });
    let (result, error) = world.last_result();
    assert!(!error, "{result}");
    assert!(result.contains("\"density\":\"compact\""), "{result}");

    // 正しくない値が 1 つでもあれば、何も変えずに理由を返す
    world.call(
        "set_appearance",
        json!({"density": "roomy", "font": "存在しない書体 XYZ"}),
        "その書体はありませんでした。",
    );
    say(cx, window, &pane, "書体を XYZ に");
    let (result, error) = world.last_result();
    assert!(error && result.contains("list_fonts"), "{result}");
    cx.update(|cx| assert_eq!(theme::density(cx), Density::Compact));

    world.call("save_theme", json!({"name": "仕事用"}), "保存しました。");
    say(cx, window, &pane, "今の見た目を「仕事用」で保存して");
    cx.update(|cx| {
        let own = theme::own_presets(cx).unwrap();
        assert_eq!(own.len(), 1);
        assert_eq!(own[0].name, "仕事用");
    });

    world.call("apply_layout", json!({"name": "3 カラム"}), "並べました。");
    say(cx, window, &pane, "3 カラムにして");
    cx.update(|cx| assert!(view.read(cx).ai_docked(cx), "チャットも右に並ぶ"));

    // 削除は確認が出て、許可するまで消えない
    world.call("delete_theme", json!({"name": "仕事用"}), "削除しました。");
    say(cx, window, &pane, "仕事用のテーマを消して");
    let session = session(cx);
    let asked = cx.update(|cx| {
        session
            .read(cx)
            .pending
            .as_ref()
            .map(|p| p.text.to_string())
    });
    assert!(asked.is_some_and(|t| t.contains("仕事用")));
    cx.update(|cx| assert_eq!(theme::own_presets(cx).unwrap().len(), 1));
    ui(cx, window, |window, cx| window.click("ai-allow", cx));
    cx.update(|cx| assert!(theme::own_presets(cx).unwrap().is_empty()));
}

/// 画面の移動、タスクを開く、一覧の絞り込みと並び順、表の列、左のメニューも AI に頼める。
#[gpui_kit::test]
fn the_ai_moves_around_the_screen(cx: &mut TestAppContext) {
    let mut store = Store::open_in_memory().unwrap();
    let ws = store.create_workspace(None, "冬のイベント").unwrap();
    for title in ["b 会場", "a 受付", "c 片付け"] {
        let draft = TaskDraft {
            title: title.into(),
            ..Default::default()
        };
        store.create_task(Some(&ws.id), &draft).unwrap();
    }
    let world = start(cx, store);
    let (window, view) = open(cx);
    let pane = open_chat(cx, window, &view);

    world.call("navigate", json!({"to": ws.id}), "開きました。");
    say(cx, window, &pane, "冬のイベントを開いて");
    cx.update(|cx| assert_eq!(view.read(cx).nav, Nav::Workspace(ws.id.clone())));

    world.call(
        "set_list_view",
        json!({"sort": "title", "descending": true, "query": "会場"}),
        "絞り込みました。",
    );
    say(cx, window, &pane, "会場で絞り込んで、タイトルの逆順に");
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.query.sort, Sort::Title);
        assert!(v.query.descending);
        assert_eq!(v.visible.len(), 1);
    });
    cx.update(|cx| {
        let search = view.read(cx).inputs.search.read(cx).value().to_string();
        assert_eq!(search, "会場", "検索欄にも出る");
    });
    world.call(
        "set_list_view",
        json!({"clear_filters": true}),
        "外しました。",
    );
    say(cx, window, &pane, "絞り込みを外して");
    cx.update(|cx| assert_eq!(view.read(cx).visible.len(), 3));

    let task = tasks(cx).into_iter().find(|t| t.title == "a 受付").unwrap();
    world.call("open_task", json!({"id": task.id}), "開きました。");
    say(cx, window, &pane, "受付のタスクを開いて");
    cx.update(|cx| assert_eq!(view.read(cx).panel.as_deref(), Some(task.id.as_str())));

    world.call(
        "set_columns",
        json!({"columns": [{"column": "updated", "shown": false}, {"column": "due", "width": 200}], "order": ["due"]}),
        "変えました。",
    );
    say(cx, window, &pane, "更新日の列を隠して、期限を先頭に広く");
    cx.update(|cx| {
        let columns = &view.read(cx).columns;
        assert_eq!(columns[0].column, Column::Due);
        assert_eq!(columns[0].width, 200.);
        assert!(columns
            .iter()
            .any(|c| c.column == Column::Updated && c.hidden));
    });

    world.call(
        "set_panels",
        json!({"sidebar": false, "detail_width": 700}),
        "変えました。",
    );
    say(cx, window, &pane, "左のメニューを閉じて、詳細を広く");
    cx.update(|cx| {
        let v = view.read(cx);
        assert!(v.sidebar_hidden);
        assert_eq!(v.panel_width, 700.);
    });

    // 今の画面を読める
    world.call("get_screen", json!({}), "読みました。");
    say(cx, window, &pane, "今どこ？");
    let (result, _) = world.last_result();
    let screen: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(screen["shown"]["workspace_id"], ws.id.as_str());
    assert_eq!(screen["open_task_id"], task.id.as_str());
    assert_eq!(screen["sidebar"], false);

    world.call("open_dialog", json!({"name": "customize"}), "開きました。");
    say(cx, window, &pane, "カスタマイズを開いて");
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::Appearance));
}

/// AI の設定（モデル、自動の処理）も頼んで変えられる。オート実行をオンにするときだけ確認が出る。
#[gpui_kit::test]
fn the_ai_changes_its_own_settings(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, view) = open(cx);
    let pane = open_chat(cx, window, &view);

    world.call(
        "set_ai_settings",
        json!({"track": false, "arrange": false, "watch_minutes": 60, "context": "敬語で答える"}),
        "変えました。",
    );
    say(
        cx,
        window,
        &pane,
        "コメントからの更新とタスクの整理を止めて",
    );
    let session = session(cx);
    cx.update(|cx| {
        let s = session.read(cx);
        assert!(!s.track);
        assert!(!s.arrange);
        assert_eq!(s.watch.minutes, 60);
        assert_eq!(s.context, "敬語で答える");
    });

    world.call(
        "set_ai_settings",
        json!({"auto_run": true}),
        "オンにしました。",
    );
    say(cx, window, &pane, "オート実行をオンに");
    cx.update(|cx| {
        let s = session.read(cx);
        assert!(s
            .pending
            .as_ref()
            .is_some_and(|p| p.text.contains("オート実行")));
        assert!(!s.auto_run, "許可するまで変わらない");
    });
    ui(cx, window, |window, cx| window.click("ai-deny", cx));
    cx.update(|cx| assert!(!session.read(cx).auto_run));
    assert_eq!(last_tool(cx).1, ToolState::Denied);

    world.call(
        "set_ai_settings",
        json!({"watch_minutes": 7}),
        "できませんでした。",
    );
    say(cx, window, &pane, "7 分ごとに");
    let (result, error) = world.last_result();
    assert!(
        error && result.contains("60 / 180 / 360 / 720 / 1440"),
        "{result}"
    );
}

/// AI の変更は、ツールの呼び出し 1 回ずつ元に戻せる（AI に頼んでも、Ctrl+Z でも）。
#[gpui_kit::test]
fn ai_changes_are_undone_one_call_at_a_time(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, view) = open(cx);
    let pane = open_chat(cx, window, &view);

    world.call("create_task", json!({"title": "見積もり"}), "作りました。");
    say(cx, window, &pane, "見積もりのタスク");
    world.call("create_task", json!({"title": "請求書"}), "作りました。");
    say(cx, window, &pane, "請求書のタスク");
    assert_eq!(tasks(cx).len(), 2);

    world.call("undo", json!({}), "戻しました。");
    say(cx, window, &pane, "今のを元に戻して");
    let left: Vec<_> = tasks(cx).into_iter().map(|t| t.title).collect();
    assert_eq!(left, ["見積もり"]);

    world.call("redo", json!({}), "やり直しました。");
    say(cx, window, &pane, "やっぱりやり直して");
    assert_eq!(tasks(cx).len(), 2);
}

/// ワークスペースの名前・アイコン・設定、担当者、エクスポートも AI に頼める。
#[gpui_kit::test]
fn the_ai_edits_workspaces_people_and_exports(cx: &mut TestAppContext) {
    let mut store = Store::open_in_memory().unwrap();
    let ws = store.create_workspace(None, "仕事").unwrap();
    let person = store.create_person("佐藤", false).unwrap();
    let world = start(cx, store);
    let (window, view) = open(cx);
    let pane = open_chat(cx, window, &view);

    world.call(
        "update_workspace",
        json!({"id": ws.id, "name": "会社"}),
        "変えました。",
    );
    say(cx, window, &pane, "仕事を会社に改名して");
    cx.update(|cx| assert_eq!(view.read(cx).workspaces[0].name, "会社"));

    world.call(
        "find_icons",
        json!({"query": "briefcase"}),
        "見つけました。",
    );
    say(cx, window, &pane, "かばんのアイコンを探して");
    let (found, _) = world.last_result();
    assert!(found.contains("briefcase"), "{found}");
    world.call(
        "set_workspace_icon",
        json!({"id": ws.id, "icon": "briefcase", "color": "blue"}),
        "変えました。",
    );
    say(cx, window, &pane, "それにして、色は青");
    cx.update(|cx| {
        let w = &view.read(cx).workspaces[0];
        assert_eq!(w.icon.as_deref(), Some("briefcase"));
        assert_eq!(w.color.as_deref(), Some("blue"));
    });
    world.call(
        "set_workspace_icon",
        json!({"id": ws.id, "icon": "no-such-icon-zzz"}),
        "ありませんでした。",
    );
    say(cx, window, &pane, "存在しないアイコンに");
    let (result, error) = world.last_result();
    assert!(error && result.contains("find_icons"), "{result}");

    world.call(
        "update_person",
        json!({"id": person.id, "name": "佐藤さん"}),
        "変えました。",
    );
    say(cx, window, &pane, "佐藤を佐藤さんに");
    cx.update(|cx| assert!(view.read(cx).people.iter().any(|p| p.name == "佐藤さん")));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("backup.zip");
    world.call(
        "export_data",
        json!({"path": path.display().to_string()}),
        "書き出しました。",
    );
    say(cx, window, &pane, "バックアップを書き出して");
    assert!(path.is_file());
    assert_eq!(last_tool(cx), ("エクスポート".into(), ToolState::Done));
    // 同じ場所にもう一度書き出すときは、上書きの確認が出る
    world.call(
        "export_data",
        json!({"path": path.display().to_string()}),
        "書き出しました。",
    );
    say(cx, window, &pane, "もう一度");
    let session = session(cx);
    cx.update(|cx| {
        assert!(session
            .read(cx)
            .pending
            .as_ref()
            .is_some_and(|p| p.text.contains("上書き")))
    });
    ui(cx, window, |window, cx| window.click("ai-allow", cx));
    assert_eq!(last_tool(cx).1, ToolState::Done);
}

/// ユーザーが始めた会話では、画面と設定のツールも AI に渡す（自動の実行には渡さない）。
#[gpui_kit::test]
fn the_chat_offers_the_app_tools(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, view) = open(cx);
    let pane = open_chat(cx, window, &view);
    world.call("get_screen", json!({}), "読みました。");
    say(cx, window, &pane, "今の画面は？");
    let names: Vec<String> = world.sent.lock().unwrap()[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    for name in [
        "set_appearance",
        "apply_theme",
        "navigate",
        "set_ai_settings",
        "update_workspace",
    ] {
        assert!(names.contains(&name.to_owned()), "{name}");
    }
}

/// 自動でロックするまでの時間と、ロック中の通知も、AI に頼んで変えられる。
#[gpui_kit::test]
fn the_ai_changes_the_lock_options(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, view) = open(cx);
    let pane = open_chat(cx, window, &view);

    world.call(
        "lock",
        json!({"action": "set_options", "auto_lock_minutes": 30, "locked_notices": "hidden"}),
        "変えました。",
    );
    say(
        cx,
        window,
        &pane,
        "30 分でロックして、ロック中の通知はタイトルを出さないで",
    );
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.auto_lock(cx), Some(30));
        assert_eq!(
            v.locked_notices(cx),
            malakt_core::lock::LockedNotices::Hidden
        );
    });

    // 選べない時間は、何も変えずに失敗する
    world.call(
        "lock",
        json!({"action": "set_options", "auto_lock_minutes": 7, "locked_notices": "title"}),
        "できませんでした。",
    );
    say(cx, window, &pane, "7 分で");
    let (result, error) = world.last_result();
    assert!(error && result.contains("1 / 5 / 10"), "{result}");
    cx.update(|cx| {
        assert_eq!(view.read(cx).auto_lock(cx), Some(30));
        assert_eq!(
            view.read(cx).locked_notices(cx),
            malakt_core::lock::LockedNotices::Hidden
        );
    });

    world.call(
        "lock",
        json!({"action": "set_options", "auto_lock_minutes": 0}),
        "止めました。",
    );
    say(cx, window, &pane, "自動でロックしないで");
    cx.update(|cx| assert_eq!(view.read(cx).auto_lock(cx), None));

    world.call("get_screen", json!({}), "読みました。");
    say(cx, window, &pane, "ロックの設定は？");
    let (result, _) = world.last_result();
    let screen: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(screen["lock"]["auto_lock_minutes"], 0);
    assert_eq!(screen["lock"]["locked_notices"], "hidden");

    world.call("open_dialog", json!({"name": "lock"}), "開きました。");
    say(cx, window, &pane, "ロックの設定を開いて");
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.modal, Modal::Appearance);
        assert_eq!(
            v.appearance_section,
            malakt_mahoz::state::CustomizeSection::Lock
        );
    });
}
