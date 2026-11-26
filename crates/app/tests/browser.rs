//! アプリ内ブラウザの、画面を通したシナリオテスト。OS のブラウザの部品は偽物に差し替え、
//! タブ・リンク・戻る進む・隠す見せる・AI のツールを、本物の画面で確かめる。

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui_kit::test::{ClickOptions, TestWindowExt};
use gpui_kit::{
    point, px, size, AnyWindowHandle, AppContext, Bounds, Entity, MouseButton, Pixels, Point,
    RenderImage, TestAppContext, WindowBounds, WindowOptions,
};
use malakt_ai::keys::{KeyStore, MemoryKeys};
use malakt_ai::shell::Spec;
use malakt_ai::Provider;
use malakt_core::{Store, TaskDraft};
use malakt_mahoz::ai::{self, ShellPlace};
use malakt_mahoz::browser::{self, Engine, Event, Factory, Setup, Shot, Zoom};
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
    /// 画像を撮った回数。
    shots: Cell<usize>,
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
        self.0.calls.borrow_mut().push("focus".into());
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
    fn zoom(&self, factor: f64) -> Result<(), String> {
        self.0.calls.borrow_mut().push(format!("zoom {factor}"));
        Ok(())
    }
    fn print(&self) -> Result<(), String> {
        self.0.calls.borrow_mut().push("print".into());
        Ok(())
    }
    fn find(
        &self,
        query: &str,
        backwards: bool,
        again: bool,
        done: Box<dyn Fn(bool) + Send + 'static>,
    ) -> Result<(), String> {
        let how = match (backwards, again) {
            (_, false) => "from top",
            (false, true) => "next",
            (true, true) => "previous",
        };
        self.0
            .calls
            .borrow_mut()
            .push(format!("find {query} {how}"));
        done(!query.contains("ない"));
        Ok(())
    }
    fn snapshot(&self, done: Shot) -> Result<(), String> {
        // 本物と同じく、見えていないページは撮らない
        assert!(self.0.visible.get(), "隠したページを撮ろうとした");
        self.0.shots.set(self.0.shots.get() + 1);
        let frame = image::Frame::new(image::RgbaImage::new(4, 3));
        done(Ok(Arc::new(RenderImage::new(vec![frame]))));
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

/// シェルを選ぶメニューを開いても、後ろのページは消えない。ページは上に重なるので隠すが、
/// 隠す前に撮った画像を同じ場所に出す。閉じるとページに戻る。
#[gpui_kit::test]
fn the_page_stays_on_screen_behind_a_menu(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, _view) = open(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    cx.update_window(window, |_, window, cx| {
        browser::open("https://example.com/", window, cx)
    })
    .unwrap();
    ui(cx, window, |_, _| {});
    let tab = terms(cx, |t| t.active_page().map(|p| p.id)).expect("タブが開く");
    let fake = world.fake(tab);
    assert!(fake.visible.get());
    // メニューを開くと、ページの画像を撮ってから隠す
    ui(cx, window, |window, cx| window.click("term-shells", cx));
    assert!(cx.update(|cx| browser::menus_open(cx)));
    assert_eq!(fake.shots.get(), 1);
    assert!(!fake.visible.get());
    assert!(terms(cx, |t| t.page(tab).is_some_and(|p| p.shot.is_some())));
    // 閉じると、ページに戻る
    ui(cx, window, |window, cx| window.press("escape", cx));
    assert!(!cx.update(|cx| browser::menus_open(cx)));
    assert!(fake.visible.get());
    // ページが変わっていなければ、次は撮った画像ですぐに隠す
    ui(cx, window, |window, cx| window.click("term-shells", cx));
    assert_eq!(fake.shots.get(), 1);
    assert!(!fake.visible.get());
    ui(cx, window, |window, cx| window.press("escape", cx));
    assert!(fake.visible.get());
    // マウスがページに入ると画像を捨て、出たときに撮り直す
    world.send(tab, Event::Pointer(true));
    cx.run_until_parked();
    assert!(terms(cx, |t| t.page(tab).is_some_and(|p| p.shot.is_none())));
    world.send(tab, Event::Pointer(false));
    cx.run_until_parked();
    assert_eq!(fake.shots.get(), 2);
    assert!(terms(cx, |t| t.page(tab).is_some_and(|p| p.shot.is_some())));
    // 読み込むと、画像を捨てる
    world.send(
        tab,
        Event::Load {
            done: true,
            url: "https://example.com/next".into(),
        },
    );
    cx.run_until_parked();
    assert!(terms(cx, |t| t.page(tab).is_some_and(|p| p.shot.is_none())));
    // メニューの間に読み込みが終わっても、出している画像は閉じるまで残す
    ui(cx, window, |window, cx| window.click("term-shells", cx));
    assert_eq!(fake.shots.get(), 3);
    assert!(!fake.visible.get());
    world.send(
        tab,
        Event::Load {
            done: true,
            url: "https://example.com/last".into(),
        },
    );
    ui(cx, window, |_, _| {});
    assert!(!fake.visible.get());
    assert!(terms(cx, |t| t.page(tab).is_some_and(|p| p.shot.is_some())));
    // 閉じてページを見せたら、古い画像を捨てる
    ui(cx, window, |window, cx| window.press("escape", cx));
    assert!(fake.visible.get());
    assert!(terms(cx, |t| t.page(tab).is_some_and(|p| p.shot.is_none())));
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

/// ページの上で Ctrl+ホイールや Ctrl++ / Ctrl+- を押すと、拡大率が 1 段ずつ変わり、
/// 場所の欄の右に今の拡大率が出る。押すと 100% に戻り、少したつと消える。
#[gpui_kit::test]
fn zooming_the_page_shows_the_zoom_level(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, _view) = open(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    cx.update_window(window, |_, window, cx| {
        browser::open("https://example.com/", window, cx)
    })
    .unwrap();
    ui(cx, window, |_, _| {});
    let tab = terms(cx, |t| t.active_page().map(|p| p.id)).expect("タブが開く");
    let fake = world.fake(tab);
    // 100% のあいだは出さない
    ui(cx, window, |window, _| {
        assert!(window.try_find("browser-zoom").is_none())
    });
    world.send(tab, Event::Zoom(Zoom::In));
    world.send(tab, Event::Zoom(Zoom::In));
    ui(cx, window, |_, _| {});
    assert_eq!(terms(cx, |t| t.page(tab).unwrap().zoom), 1.25);
    assert!(fake
        .calls
        .borrow()
        .ends_with(&["zoom 1.1".to_owned(), "zoom 1.25".to_owned()]));
    ui(cx, window, |window, _| {
        assert!(window.find("browser-zoom").visible())
    });
    assert_eq!(browser::zoom_label(1.25), "125%");
    // 時間がたっても、100% でなければ出したまま
    cx.executor()
        .advance_clock(browser::ZOOM_SHOWN + std::time::Duration::from_secs(1));
    ui(cx, window, |window, _| {
        assert!(window.find("browser-zoom").visible())
    });
    assert!(!terms(cx, |t| t.page(tab).unwrap().zoom_recent));
    // 押すと 100% に戻り、100% を少し出してから消える
    ui(cx, window, |window, cx| window.click("browser-zoom", cx));
    assert_eq!(terms(cx, |t| t.page(tab).unwrap().zoom), 1.0);
    assert_eq!(fake.calls.borrow().last().unwrap(), "zoom 1");
    ui(cx, window, |window, _| {
        assert!(window.find("browser-zoom").visible())
    });
    cx.executor()
        .advance_clock(browser::ZOOM_SHOWN + std::time::Duration::from_secs(1));
    ui(cx, window, |window, _| {
        assert!(window.try_find("browser-zoom").is_none())
    });
    // 縮めるときも 1 段ずつ。端より先へは動かない
    world.send(tab, Event::Zoom(Zoom::Out));
    ui(cx, window, |_, _| {});
    assert_eq!(terms(cx, |t| t.page(tab).unwrap().zoom), 0.9);
    for _ in 0..20 {
        world.send(tab, Event::Zoom(Zoom::Out));
    }
    ui(cx, window, |_, _| {});
    assert_eq!(terms(cx, |t| t.page(tab).unwrap().zoom), 0.25);
    world.send(tab, Event::Zoom(Zoom::Reset));
    ui(cx, window, |_, _| {});
    assert_eq!(terms(cx, |t| t.page(tab).unwrap().zoom), 1.0);
}

/// AI もページの拡大率を変えられる。
#[gpui_kit::test]
fn the_ai_zooms_the_page(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, view) = open(cx);
    let pane = open_chat(cx, window, &view);
    world.call(
        "browser",
        json!({"action": "open", "url": "https://example.com/doc"}),
        "開きました",
    );
    say(cx, window, &pane, "資料を開いて");
    let tab = terms(cx, |t| t.active_page().map(|p| p.id)).expect("タブが開く");
    world.call("browser", json!({"action": "zoom_in"}), "大きくしました");
    say(cx, window, &pane, "文字を大きくして");
    let (result, failed) = world.last_result();
    assert!(!failed, "{result}");
    assert!(result.contains("110%"), "{result}");
    assert_eq!(terms(cx, |t| t.page(tab).unwrap().zoom), 1.1);
    world.call("browser", json!({"action": "zoom_reset"}), "戻しました");
    say(cx, window, &pane, "元の大きさに");
    assert_eq!(terms(cx, |t| t.page(tab).unwrap().zoom), 1.0);
}

/// ページの中のリンクをホイールで押すと、小さなウィンドウで開く。タブの並びには出ない。
/// 「タブで開く」で、ブラウザのタブに移ってウィンドウは閉じる。ウィンドウを閉じると、ページも閉じる。
#[gpui_kit::test]
fn a_middle_click_opens_the_link_in_a_small_window(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, _view) = open(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    cx.update_window(window, |_, window, cx| {
        browser::open("https://example.com/", window, cx)
    })
    .unwrap();
    ui(cx, window, |_, _| {});
    let tab = terms(cx, |t| t.active_page().map(|p| p.id)).expect("タブが開く");
    world.send(tab, Event::Popup("https://example.com/b".into()));
    // ホイールの押下を止められない部品から届く「新しいタブで開く」は、捨てる
    world.send(tab, Event::NewTab("https://example.com/b".into()));
    cx.run_until_parked();
    let small = terms(cx, |t| t.pages.iter().find(|p| p.popup).map(|p| p.id))
        .expect("小さなウィンドウのページがある");
    assert_eq!(terms(cx, |t| t.pages.len()), 2);
    assert_eq!(terms(cx, |t| t.order()), vec![tab]);
    assert_eq!(terms(cx, |t| t.active), Some(tab));
    let popup = cx
        .update(|cx| cx.windows())
        .into_iter()
        .find(|w| *w != window)
        .expect("小さなウィンドウが開く");
    ui(cx, popup, |window, _| {
        assert!(window.find("popup-dock").visible());
        assert!(window.find("popup-title").visible());
        assert!(window.find("browser-page").visible());
    });
    let fake = world.fake(small);
    assert!(fake.visible.get(), "小さなウィンドウにページが見えている");
    assert!(
        world.fake(tab).visible.get(),
        "タブのページもそのまま見えている"
    );
    // 小さなウィンドウの中でも、戻る・拡大縮小ができる
    fake.history.set((true, false));
    *fake.url.borrow_mut() = "https://example.com/b2".into();
    world.send(
        small,
        Event::Load {
            done: true,
            url: "https://example.com/b2".into(),
        },
    );
    world.send(small, Event::Zoom(Zoom::In));
    cx.run_until_parked();
    ui(cx, popup, |window, cx| window.click("popup-back", cx));
    assert!(fake.calls.borrow().contains(&"back".to_owned()));
    ui(cx, popup, |window, _| {
        assert!(window.find("browser-zoom").visible())
    });
    // タブで開くと、ブラウザのタブに移り、ウィンドウは閉じる
    cx.update_window(popup, |_, window, cx| window.click("popup-dock", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(
        fake.dropped.get(),
        "閉じる前に小さなウィンドウの部品を捨てる"
    );
    assert_eq!(cx.update(|cx| cx.windows().len()), 1);
    assert_eq!(terms(cx, |t| t.order()), vec![tab, small]);
    assert_eq!(terms(cx, |t| t.active), Some(small));
    ui(cx, window, |window, _| {
        assert!(window.find(("browser-tab", small as usize)).visible())
    });
    let docked = world.fake(small);
    assert!(docked.visible.get(), "タブで同じページが見える");
    assert_eq!(docked.url.borrow().as_str(), "https://example.com/b2");
    assert_eq!(terms(cx, |t| t.page(small).unwrap().zoom), 1.1);
    // 小さなウィンドウは、場所を残さない。タブに移したものは残す
    world.send(tab, Event::Popup("https://example.com/c".into()));
    cx.run_until_parked();
    let other =
        terms(cx, |t| t.pages.iter().find(|p| p.popup).map(|p| p.id)).expect("もう一度開ける");
    let saved = cx.update(|cx| {
        malakt_mahoz::db::shared(cx)
            .read(cx)
            .store
            .setting("browser.tabs")
            .unwrap()
    });
    assert_eq!(
        saved.as_deref(),
        Some(r#"["https://example.com/","https://example.com/b2"]"#)
    );
    // ウィンドウを閉じると、ページも閉じる
    let popup = cx
        .update(|cx| cx.windows())
        .into_iter()
        .find(|w| *w != window)
        .expect("小さなウィンドウが開く");
    ui(cx, popup, |_, _| {});
    let closing = world.fake(other);
    cx.update_window(popup, |_, window, cx| {
        browser::window_closing(window, cx);
        window.remove_window();
    })
    .unwrap();
    cx.run_until_parked();
    assert!(closing.dropped.get());
    assert!(terms(cx, |t| t.page(other).is_none()));
    assert_eq!(terms(cx, |t| t.pages.len()), 2);
}

/// AI は小さなウィンドウのページも一覧で見られ、閉じるとウィンドウも閉じる。
#[gpui_kit::test]
fn the_ai_sees_and_closes_a_small_window(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, view) = open(cx);
    let pane = open_chat(cx, window, &view);
    cx.update_window(window, |_, window, cx| {
        browser::open("https://example.com/", window, cx)
    })
    .unwrap();
    ui(cx, window, |_, _| {});
    let tab = terms(cx, |t| t.active_page().map(|p| p.id)).expect("タブが開く");
    world.send(tab, Event::Popup("https://example.com/b".into()));
    cx.run_until_parked();
    let small = terms(cx, |t| t.pages.iter().find(|p| p.popup).map(|p| p.id))
        .expect("小さなウィンドウのページがある");
    world.call("browser", json!({"action": "list"}), "一覧です");
    say(cx, window, &pane, "タブは？");
    let (result, _) = world.last_result();
    let list: Value = serde_json::from_str(&result).unwrap();
    let found = list["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["tab"] == small)
        .cloned()
        .expect("一覧にある");
    assert_eq!(found["window"], true);
    world.call(
        "browser",
        json!({"action": "close", "tab": small}),
        "閉じました",
    );
    say(cx, window, &pane, "閉じて");
    assert!(terms(cx, |t| t.page(small).is_none()));
    assert_eq!(cx.update(|cx| cx.windows().len()), 1);
}

/// macOS では ⌘、ほかでは Ctrl などのキー。
fn key(others: &'static str, mac: &'static str) -> &'static str {
    if cfg!(target_os = "macos") {
        mac
    } else {
        others
    }
}

/// ブラウザのタブを 1 つ開いて、ページの部品を作る。
fn open_page(cx: &mut TestAppContext, window: AnyWindowHandle, url: &str) -> u64 {
    cx.update_window(window, |_, window, cx| browser::open(url, window, cx))
        .unwrap();
    ui(cx, window, |_, _| {});
    terms(cx, |t| t.active_page().map(|p| p.id)).expect("タブが開く")
}

/// ページの中でキーを押した（ページのスクリプトが知らせた）。
fn press_in_page(
    cx: &mut TestAppContext,
    world: &World,
    window: AnyWindowHandle,
    tab: u64,
    keys: &str,
) {
    world.send(tab, Event::Focused);
    ui(cx, window, |_, _| {});
    world.send(tab, Event::Key(keys.into()));
    ui(cx, window, |_, _| {});
}

/// ページの中にキーの入力先があっても、Chrome と同じキーで読み込み直す・戻る・進む・
/// 新しいタブ・タブを閉じる・閉じたタブを開き直す・タブを切り替える。
#[gpui_kit::test]
fn browser_keys_work_inside_the_page(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, _view) = open(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    let tab = open_page(cx, window, "https://example.com/a");
    let fake = world.fake(tab);
    press_in_page(cx, &world, window, tab, key("ctrl-r", "cmd-r"));
    press_in_page(cx, &world, window, tab, "f5");
    press_in_page(cx, &world, window, tab, key("alt-left", "cmd-["));
    press_in_page(cx, &world, window, tab, key("alt-right", "cmd-]"));
    let calls = fake.calls.borrow().clone();
    let steps: Vec<&String> = calls
        .iter()
        .filter(|c| ["reload", "back", "forward"].contains(&c.as_str()))
        .collect();
    assert_eq!(steps, ["reload", "reload", "back", "forward"], "{calls:?}");
    // 知らせると決めていないキーは受けない
    press_in_page(cx, &world, window, tab, "ctrl-shift-alt-x");
    assert_eq!(fake.calls.borrow().len(), calls.len());

    // 新しいタブ。何もないタブは、すぐに URL を書ける
    press_in_page(cx, &world, window, tab, key("ctrl-t", "cmd-t"));
    let blank = terms(cx, |t| t.active_page().map(|p| (p.id, p.url.clone())));
    let (blank, url) = blank.expect("新しいタブを見ている");
    assert_ne!(blank, tab);
    assert_eq!(url, browser::BLANK);
    // タブを切り替える。AI のタブ → a → 新しいタブ の順で回る
    press_in_page(cx, &world, window, tab, "ctrl-tab");
    assert_eq!(terms(cx, |t| t.active), None, "端からは AI のタブへ回る");
    // AI のタブでは、入力行で続けて押せる
    ui(cx, window, |window, cx| {
        window.press(key("ctrl-2", "cmd-2"), cx)
    });
    assert_eq!(terms(cx, |t| t.active), Some(tab));
    // 見せたページには、続けてキーを使えるように入力先を移す
    assert_eq!(
        fake.calls.borrow().last().map(String::as_str),
        Some("focus")
    );
    press_in_page(cx, &world, window, tab, "ctrl-shift-tab");
    assert_eq!(terms(cx, |t| t.active), None);
    ui(cx, window, |window, cx| {
        window.press(key("ctrl-9", "cmd-9"), cx)
    });
    assert_eq!(terms(cx, |t| t.active), Some(blank), "9 は最後のタブ");

    // 閉じて、開き直す
    cx.update(|cx| term::shared(cx).update(cx, |t, cx| t.activate(Some(tab), cx)));
    ui(cx, window, |_, _| {});
    press_in_page(cx, &world, window, tab, key("ctrl-w", "cmd-w"));
    assert!(terms(cx, |t| t.page(tab).is_none()), "タブを閉じる");
    assert_eq!(terms(cx, |t| t.active), Some(blank));
    press_in_page(
        cx,
        &world,
        window,
        blank,
        key("ctrl-shift-t", "cmd-shift-t"),
    );
    assert_eq!(
        terms(cx, |t| t.active_page().map(|p| p.url.clone())),
        Some("https://example.com/a".to_owned())
    );
}

/// ページの中の Ctrl+L（⌘L）は URL の欄に移って、書いてある場所をすべて選ぶ。
/// URL の欄にいるときの Ctrl+R は、アプリではなくページを読み込み直す。
#[gpui_kit::test]
fn ctrl_l_moves_to_the_url_bar_and_ctrl_r_reloads_the_page(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, _view) = open(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    // ロックを使える状態でも、Ctrl+L はロックしない
    cx.update(|cx| {
        malakt_mahoz::db::shared(cx).update(cx, |db, _| {
            malakt_core::lock::enable(&mut db.store).unwrap()
        })
    });
    let tab = open_page(cx, window, "https://example.com/a");
    let fake = world.fake(tab);
    press_in_page(cx, &world, window, tab, key("ctrl-l", "cmd-l"));
    assert!(
        cx.update(|cx| malakt_mahoz::db::shared(cx).read(cx).unlocked),
        "ロックしない"
    );
    assert!(fake.calls.borrow().contains(&"focus_parent".to_owned()));
    // 選んだ場所を書き換えて開く
    ui(cx, window, |window, cx| {
        window.input("example.org", cx);
        window.press("enter", cx);
    });
    assert!(fake
        .calls
        .borrow()
        .contains(&"load https://example.org".to_owned()));
    let before = fake.calls.borrow().len();
    ui(cx, window, |window, cx| window.click("browser-url", cx));
    ui(cx, window, |window, cx| {
        window.press(key("ctrl-r", "cmd-r"), cx)
    });
    // 開いたあとはページにいる。URL の欄を押すと、キーの入力先がアプリに戻る
    assert_eq!(
        fake.calls.borrow()[before..],
        ["focus_parent".to_owned(), "reload".to_owned()]
    );
}

/// Ctrl+F（⌘F）でページの中を探す欄が出る。書くと頭から探し、Enter で次、Shift+Enter で前、
/// 見つからなければ「見つかりません」。Esc で閉じてページに戻る。
#[gpui_kit::test]
fn ctrl_f_finds_in_the_page(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, _view) = open(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    let tab = open_page(cx, window, "https://example.com/a");
    let fake = world.fake(tab);
    press_in_page(cx, &world, window, tab, key("ctrl-f", "cmd-f"));
    ui(cx, window, |window, _| {
        assert!(window.find("browser-find").visible())
    });
    ui(cx, window, |window, cx| window.input("例", cx));
    ui(cx, window, |window, cx| window.press("enter", cx));
    ui(cx, window, |window, cx| window.press("shift-enter", cx));
    let calls = fake.calls.borrow().clone();
    let finds: Vec<&String> = calls.iter().filter(|c| c.starts_with("find")).collect();
    assert_eq!(
        finds,
        ["find 例 from top", "find 例 next", "find 例 previous"],
        "{calls:?}"
    );
    ui(cx, window, |window, _| {
        assert!(window.try_find("browser-find-missing").is_none())
    });
    ui(cx, window, |window, cx| window.input("ない", cx));
    ui(cx, window, |_, _| {});
    ui(cx, window, |window, _| {
        assert!(window.find("browser-find-missing").visible())
    });
    // F3（⌘G）でも次を探す
    press_in_page(cx, &world, window, tab, key("f3", "cmd-g"));
    assert!(fake
        .calls
        .borrow()
        .last()
        .is_some_and(|c| c.ends_with("next")));
    // Esc で閉じて、ページに戻る
    ui(cx, window, |window, cx| {
        window.click("browser-find-input", cx)
    });
    ui(cx, window, |window, cx| window.press("escape", cx));
    ui(cx, window, |window, _| {
        assert!(window.try_find("browser-find").is_none())
    });
    assert_eq!(
        fake.calls.borrow().last().map(String::as_str),
        Some("focus")
    );
    // 印刷
    press_in_page(cx, &world, window, tab, key("ctrl-p", "cmd-p"));
    assert_eq!(
        fake.calls.borrow().last().map(String::as_str),
        Some("print")
    );
}

/// ページの中で押したアプリのキー（全画面、AI チャット）も、いつもどおりに働く。
#[gpui_kit::test]
fn app_keys_work_inside_the_page(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, view) = open(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    let tab = open_page(cx, window, "https://example.com/a");
    let fullscreen = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, _| window.is_fullscreen())
            .unwrap()
    };
    assert!(!fullscreen(cx));
    cx.update_window(window, |_, window, _| window.activate_window())
        .unwrap();
    press_in_page(cx, &world, window, tab, key("f11", "ctrl-cmd-f"));
    assert!(fullscreen(cx), "全画面になる");
    press_in_page(cx, &world, window, tab, key("ctrl-j", "cmd-j"));
    assert!(
        !cx.update(|cx| view.read(cx).ai_open),
        "AI チャットを閉じる"
    );
}
