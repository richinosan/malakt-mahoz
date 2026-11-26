//! アプリ内ブラウザの、画面を通したシナリオテスト。OS のブラウザの部品は偽物に差し替え、
//! タブ・リンク・戻る進む・隠す見せる・AI のツールを、本物の画面で確かめる。

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui_kit::test::{ClickOptions, TestWindowExt};
use gpui_kit::{
    point, px, size, AnyWindowHandle, AppContext, Bounds, Entity, MouseButton, Pixels, Point,
    TestAppContext, WindowBounds, WindowOptions,
};
use malakt_ai::keys::{KeyStore, MemoryKeys};
use malakt_ai::shell::Spec;
use malakt_ai::Provider;
use malakt_core::{Store, TaskDraft};
use malakt_mahoz::ai::{self, ShellPlace};
use malakt_mahoz::browser::{self, Engine, Event, Factory, Setup};
use malakt_mahoz::term::{self, Terms};
use malakt_mahoz::{AiConfig, AiPane, MainView, Modal, Nav};
use serde_json::{json, Value};

/// 偽のページが受けた操作と、今の状態。
#[derive(Default)]
struct Fake {
    calls: RefCell<Vec<String>>,
    visible: Cell<bool>,
    bounds: Cell<Option<Bounds<Pixels>>>,
    history: Cell<(bool, bool)>,
    url: RefCell<String>,
    dropped: Cell<bool>,
}

struct FakeEngine(Rc<Fake>);

impl Drop for FakeEngine {
    fn drop(&mut self) {
        self.0.dropped.set(true);
    }
}

impl Engine for FakeEngine {
    fn load(&self, url: &str) -> Result<(), String> {
        self.0.calls.borrow_mut().push(format!("load {url}"));
        *self.0.url.borrow_mut() = url.to_owned();
        Ok(())
    }
    fn back(&self) -> Result<(), String> {
        self.0.calls.borrow_mut().push("back".into());
        Ok(())
    }
    fn forward(&self) -> Result<(), String> {
        self.0.calls.borrow_mut().push("forward".into());
        Ok(())
    }
    fn reload(&self) -> Result<(), String> {
        self.0.calls.borrow_mut().push("reload".into());
        Ok(())
    }
    fn stop(&self) -> Result<(), String> {
        self.0.calls.borrow_mut().push("stop".into());
        Ok(())
    }
    fn place(&self, bounds: Bounds<Pixels>) -> Result<(), String> {
        self.0.bounds.set(Some(bounds));
        Ok(())
    }
    fn show(&self, visible: bool) -> Result<(), String> {
        self.0.visible.set(visible);
        Ok(())
    }
    fn focus(&self) -> Result<(), String> {
        Ok(())
    }
    fn focus_parent(&self) -> Result<(), String> {
        self.0.calls.borrow_mut().push("focus_parent".into());
        Ok(())
    }
    fn url(&self) -> Option<String> {
        Some(self.0.url.borrow().clone())
    }
    fn history(&self) -> (bool, bool) {
        self.0.history.get()
    }
    fn read(&self, done: Box<dyn Fn(String) + Send + 'static>) -> Result<(), String> {
        let page = json!({
            "url": self.0.url.borrow().clone(),
            "title": "資料",
            "selection": "",
            "text": "ページの本文です",
        })
        .to_string();
        done(serde_json::to_string(&page).unwrap());
        Ok(())
    }
    fn clear_data(&self) -> Result<(), String> {
        self.0.calls.borrow_mut().push("clear".into());
        Ok(())
    }
}

/// 作ったページ（タブの番号、状態、知らせを送る口）。
type Made = Rc<RefCell<Vec<(u64, Rc<Fake>, async_channel::Sender<(u64, Event)>)>>>;

fn fake_browser(cx: &mut TestAppContext) -> Made {
    let made: Made = Rc::default();
    let log = made.clone();
    cx.update(|cx| {
        cx.set_global(Factory(Rc::new(move |_, setup: Setup| {
            let fake = Rc::new(Fake::default());
            *fake.url.borrow_mut() = setup.url.clone();
            fake.bounds.set(Some(setup.bounds));
            log.borrow_mut()
                .push((setup.id, fake.clone(), setup.events.clone()));
            Ok(Rc::new(FakeEngine(fake)) as Rc<dyn Engine>)
        })))
    });
    made
}

type Sent = Arc<Mutex<Vec<Value>>>;

struct World {
    sent: Sent,
    replies: Arc<Mutex<VecDeque<Value>>>,
    made: Made,
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

    fn fake(&self, tab: u64) -> Rc<Fake> {
        self.made
            .borrow()
            .iter()
            .rev()
            .find(|(id, _, _)| *id == tab)
            .map(|(_, fake, _)| fake.clone())
            .expect("ページを作っている")
    }

    fn send(&self, tab: u64, event: Event) {
        let made = self.made.borrow();
        let (_, _, events) = made
            .iter()
            .rev()
            .find(|(id, _, _)| *id == tab)
            .expect("ページを作っている");
        events.try_send((tab, event)).unwrap();
    }
}

fn start(cx: &mut TestAppContext, store: Store) -> World {
    cx.executor().allow_parking();
    let sent = Sent::default();
    let replies: Arc<Mutex<VecDeque<Value>>> = Arc::default();
    let keys = Arc::new(MemoryKeys::default());
    keys.set(Provider::Claude, "sk-ant-1").unwrap();
    let (posted, answers) = (sent.clone(), replies.clone());
    let config = AiConfig {
        keys,
        env: Arc::new(|_| None),
        post: Arc::new(move |_, _, body| {
            posted.lock().unwrap().push(body.clone());
            answers
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
    let made = fake_browser(cx);
    World {
        sent,
        replies,
        made,
    }
}

fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<MainView>) {
    let opened = cx.update(|cx| {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: size(px(2400.), px(1200.)),
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
    });
    cx.run_until_parked();
    opened
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
    // 部品を作るのは描き終わってから。作ったあとにもう一度描いて、見せるかを決める
    cx.update_window(window, |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.run_until_parked();
}

fn terms<R>(cx: &mut TestAppContext, f: impl FnOnce(&Terms) -> R) -> R {
    cx.update(|cx| f(term::shared(cx).read(cx)))
}

fn task_with(cx: &mut TestAppContext, description: &str) -> String {
    cx.update(|cx| {
        malakt_mahoz::db::shared(cx).update(cx, |db, _| {
            let draft = TaskDraft {
                title: "調べる".into(),
                description: description.into(),
                ..Default::default()
            };
            db.store.create_task(None, &draft).unwrap().id
        })
    })
}

fn open_task(cx: &mut TestAppContext, window: AnyWindowHandle, view: &Entity<MainView>, id: &str) {
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |v, cx| {
            v.reload(cx);
            v.go(Nav::All, cx);
            v.open_panel(id, window, cx)
        })
    })
    .unwrap();
    ui(cx, window, |_, _| {});
}

/// 説明の最初の文字（リンク）の上を押す場所。
const ON_LINK: Point<Pixels> = point(px(6.), px(6.));

/// タスクの詳細のリンクを押すと、シェルの場所にブラウザのタブが開き、ページが見える。
#[gpui_kit::test]
fn a_link_in_the_task_detail_opens_in_a_browser_tab(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, view) = open(cx);
    let id = task_with(cx, "[資料のページ](https://example.com/doc)");
    open_task(cx, window, &view, &id);
    ui(cx, window, |window, cx| {
        window.click_at("detail-description", ON_LINK, cx)
    });
    let (tab, url) = terms(cx, |t| {
        let page = t.active_page().expect("ブラウザのタブを見ている");
        (page.id, page.url.clone())
    });
    assert_eq!(url, "https://example.com/doc");
    // チャットとシェルが開き、タブとページが出る
    assert!(cx.update(|cx| view.read(cx).ai_open));
    assert_eq!(
        cx.update(|cx| ai::shared(cx).read(cx).shell_place),
        ShellPlace::Docked
    );
    ui(cx, window, |window, _| {
        assert!(window.find(("browser-tab", tab as usize)).visible());
        assert!(window.find("browser-page").visible());
        assert!(window.find("browser-url").visible());
    });
    let fake = world.fake(tab);
    assert!(fake.visible.get(), "ページが見えている");
    let placed = fake.bounds.get().expect("場所に置いた");
    assert!(placed.size.width > px(100.) && placed.size.height > px(100.));
}

/// リンクの右クリックで、アプリ内で開く・ブラウザで開く・コピーを選べる。メニューの間はページを隠す。
#[gpui_kit::test]
fn a_right_click_on_a_link_offers_where_to_open(cx: &mut TestAppContext) {
    let _world = start(cx, Store::open_in_memory().unwrap());
    let (window, view) = open(cx);
    let id = task_with(cx, "[資料のページ](https://example.com/doc)");
    open_task(cx, window, &view, &id);
    ui(cx, window, |window, cx| {
        window.click_with_options(
            "detail-description",
            ClickOptions::new()
                .with_offset(ON_LINK)
                .with_button(MouseButton::Right),
            cx,
        )
    });
    // 右クリックでは開かず、メニューが出る
    assert!(terms(cx, |t| t.pages.is_empty()));
    assert!(cx.update(|cx| browser::menus_open(cx)));
    ui(cx, window, |window, cx| {
        window.press("down", cx);
        window.press("enter", cx);
    });
    assert_eq!(
        terms(cx, |t| t.active_page().map(|p| p.url.clone())),
        Some("https://example.com/doc".to_owned())
    );
}

/// 「新しいブラウザのタブ」で何もないタブが開き、場所の欄に書いて Enter で開く。
/// 読み込み・題名・戻る進むは、ページの知らせで変わる。
#[gpui_kit::test]
fn the_url_bar_opens_what_is_typed(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, _view) = open(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    ui(cx, window, |window, cx| window.click("browser-new", cx));
    let tab = terms(cx, |t| t.active_page().map(|p| p.id)).expect("タブが開く");
    ui(cx, window, |window, cx| {
        window.input("example.com/a", cx);
        window.press("enter", cx);
    });
    let fake = world.fake(tab);
    assert!(fake
        .calls
        .borrow()
        .contains(&"load https://example.com/a".to_owned()));
    // ページが読み込みを始め、題名が付き、戻れるようになる
    fake.history.set((true, false));
    world.send(
        tab,
        Event::Load {
            done: false,
            url: "https://example.com/a".into(),
        },
    );
    ui(cx, window, |_, _| {});
    assert!(terms(cx, |t| t.page(tab).unwrap().loading));
    ui(cx, window, |window, _| {
        assert!(window.find("browser-stop").visible())
    });
    world.send(
        tab,
        Event::Load {
            done: true,
            url: "https://example.com/a".into(),
        },
    );
    world.send(tab, Event::Title("例のページ".into()));
    ui(cx, window, |_, _| {});
    assert_eq!(
        terms(cx, |t| t.page(tab).unwrap().label().to_string()),
        "例のページ"
    );
    ui(cx, window, |window, cx| window.click("browser-back", cx));
    ui(cx, window, |window, cx| window.click("browser-reload", cx));
    let calls = fake.calls.borrow().clone();
    assert!(
        calls.ends_with(&["back".to_owned(), "reload".to_owned()]),
        "{calls:?}"
    );
    // ページが新しいウィンドウを開こうとしたら、新しいタブにする
    world.send(tab, Event::NewTab("https://example.com/b".into()));
    ui(cx, window, |_, _| {});
    assert_eq!(terms(cx, |t| t.pages.len()), 2);
    assert_eq!(
        terms(cx, |t| t.active_page().map(|p| p.url.clone())),
        Some("https://example.com/b".to_owned())
    );
}

/// ページはアプリの画面の上に重なるので、ほかのタブ・ダイアログ・ロックの間は隠す。
/// タブを閉じると、ページも捨てる。
#[gpui_kit::test]
fn the_page_hides_when_something_is_over_it(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, view) = open(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    cx.update_window(window, |_, window, cx| {
        browser::open("https://example.com/", window, cx)
    })
    .unwrap();
    ui(cx, window, |_, _| {});
    let tab = terms(cx, |t| t.active_page().map(|p| p.id)).expect("タブが開く");
    let fake = world.fake(tab);
    assert!(fake.visible.get());
    // AI のタブに切り替えると隠れ、戻すと見える
    ui(cx, window, |window, cx| window.click("term-tab-ai", cx));
    assert!(!fake.visible.get());
    ui(cx, window, |window, cx| {
        window.click(("browser-tab", tab as usize), cx)
    });
    assert!(fake.visible.get());
    // ダイアログの間は隠れる
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.modal = Modal::Notices;
            cx.notify();
        })
    });
    ui(cx, window, |_, _| {});
    assert!(!fake.visible.get());
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.modal = Modal::None;
            cx.notify();
        })
    });
    ui(cx, window, |_, _| {});
    assert!(fake.visible.get());
    // シェルを閉じると隠れる
    ui(cx, window, |window, cx| window.click("ai-shell-close", cx));
    assert!(!fake.visible.get());
    // 同じリンクをもう一度開くと、同じタブがまた見える
    cx.update_window(window, |_, window, cx| {
        browser::open("https://example.com/", window, cx)
    })
    .unwrap();
    ui(cx, window, |_, _| {});
    assert_eq!(terms(cx, |t| t.pages.len()), 1);
    assert!(fake.visible.get());
    // タブを閉じると、ページを捨てる
    ui(cx, window, |window, cx| {
        window.click(("term-close", tab as usize), cx)
    });
    assert!(fake.dropped.get());
    assert!(terms(cx, |t| t.pages.is_empty() && t.active.is_none()));
}

/// 開いていたタブの場所は残り、次に起動したときに並ぶ。ページは見せるときに作る。
#[gpui_kit::test]
fn tabs_come_back_after_a_restart(cx: &mut TestAppContext) {
    let mut store = Store::open_in_memory().unwrap();
    store
        .set_setting(
            "browser.tabs",
            Some(r#"["https://example.com/a","https://example.com/b"]"#),
        )
        .unwrap();
    let world = start(cx, store);
    let (window, _view) = open(cx);
    let tabs = terms(cx, |t| {
        t.pages
            .iter()
            .map(|p| (p.id, p.url.clone()))
            .collect::<Vec<_>>()
    });
    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs[1].1, "https://example.com/b");
    assert!(world.made.borrow().is_empty(), "見せるまでは作らない");
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    ui(cx, window, |window, cx| {
        window.click(("browser-tab", tabs[1].0 as usize), cx)
    });
    assert_eq!(world.made.borrow().len(), 1);
    assert!(world.fake(tabs[1].0).visible.get());
    // 閉じたタブは、残した場所からも消える
    ui(cx, window, |window, cx| {
        window.click(("term-close", tabs[1].0 as usize), cx)
    });
    let saved = cx.update(|cx| {
        malakt_mahoz::db::shared(cx)
            .read(cx)
            .store
            .setting("browser.tabs")
            .unwrap()
    });
    assert_eq!(saved.as_deref(), Some(r#"["https://example.com/a"]"#));
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
    for _ in 0..200 {
        cx.run_until_parked();
        if !cx.update(|cx| ai::shared(cx).read(cx).busy) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    ui(cx, window, |_, _| {});
}

/// AI はアプリ内ブラウザでページを開き、ユーザーのログインのまま中身を読める。
/// 見ているページは、送るたびに AI に伝わる。
#[gpui_kit::test]
fn the_ai_opens_and_reads_a_page(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, view) = open(cx);
    let pane = open_chat(cx, window, &view);
    world.call(
        "browser",
        json!({"action": "open", "url": "https://example.com/doc"}),
        "開きました",
    );
    say(cx, window, &pane, "資料を開いて");
    let (result, failed) = world.last_result();
    assert!(!failed, "{result}");
    let tab = terms(cx, |t| t.active_page().map(|p| p.id)).expect("タブが開く");
    assert!(world.fake(tab).visible.get());
    world.call("read_browser", json!({}), "読みました");
    say(cx, window, &pane, "このページを要約して");
    let (result, failed) = world.last_result();
    assert!(!failed, "{result}");
    let page: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(page["text"], "ページの本文です");
    assert_eq!(page["tab"], tab);
    // 見ているページを、メッセージに添えている
    let sent = world.sent.lock().unwrap();
    let first = sent
        .iter()
        .rev()
        .find(|body| body.to_string().contains("このページを要約して"))
        .expect("送っている")
        .to_string();
    assert!(first.contains("アプリ内ブラウザ"), "{first}");
    drop(sent);
    // 一覧と、閉じる
    world.call("browser", json!({"action": "list"}), "一覧です");
    say(cx, window, &pane, "タブは？");
    let (result, _) = world.last_result();
    assert!(result.contains("https://example.com/doc"), "{result}");
    world.call(
        "browser",
        json!({"action": "close", "tab": tab}),
        "閉じました",
    );
    say(cx, window, &pane, "閉じて");
    assert!(terms(cx, |t| t.pages.is_empty()));
}
