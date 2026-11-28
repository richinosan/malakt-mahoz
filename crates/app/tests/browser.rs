//! アプリ内ブラウザの、画面を通したシナリオテスト。OS のブラウザの部品は偽物に差し替え、
//! タブ・リンク・戻る進む・隠す見せる・AI のツールを、本物の画面で確かめる。

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui_kit::test::{ClickOptions, TestEventExt, TestQueryExt, TestWindowExt};
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
use malakt_mahoz::downloads::{self, Note};
use malakt_mahoz::print_setup::{self, Margins, Pages, PrintSetup};
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
    /// 作っている途中の PDF。テストが終わったことにする。
    pdf: RefCell<Option<browser::PdfDone>>,
    /// PDF を作るときに受け取った設定と、選んだページ。
    pdf_setup: RefCell<Vec<(PrintSetup, Option<Vec<u32>>)>>,
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
    fn save_pdf(
        &self,
        path: &std::path::Path,
        setup: &PrintSetup,
        pages: Option<&[u32]>,
        done: browser::PdfDone,
    ) -> Result<(), String> {
        self.0
            .calls
            .borrow_mut()
            .push(format!("pdf {}", path.display()));
        self.0
            .pdf_setup
            .borrow_mut()
            .push((setup.clone(), pages.map(<[u32]>::to_vec)));
        *self.0.pdf.borrow_mut() = Some(done);
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
        files: std::env::temp_dir().join(format!("malakt-ai-files-{}", std::process::id())),
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

/// 説明の最初の文字（リンク）の上を押す場所。説明は押せる面の内側（左 8・上 4）から始まる。
const ON_LINK: Point<Pixels> = point(px(14.), px(10.));

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
    // リンクを押したときは、説明の編集にならない
    assert!(cx.update(|cx| view.read(cx).editing.is_none()));
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

/// タイトルの下の URL を押すと、アプリ内ブラウザで開く。右クリックではメニューを出す。
#[gpui_kit::test]
fn a_task_url_opens_in_a_browser_tab(cx: &mut TestAppContext) {
    let _world = start(cx, Store::open_in_memory().unwrap());
    let (window, view) = open(cx);
    let id = cx.update(|cx| {
        malakt_mahoz::db::shared(cx).update(cx, |db, _| {
            let draft = TaskDraft {
                title: "調べる".into(),
                urls: vec![
                    "https://example.com/a".into(),
                    "https://example.com/b".into(),
                ],
                ..Default::default()
            };
            db.store.create_task(None, &draft).unwrap().id
        })
    });
    open_task(cx, window, &view, &id);
    ui(cx, window, |window, cx| {
        window.click_with_options(
            ("task-url", 1usize),
            ClickOptions::new().with_button(MouseButton::Right),
            cx,
        )
    });
    assert!(terms(cx, |t| t.pages.is_empty()));
    assert!(cx.update(|cx| browser::menus_open(cx)));
    ui(cx, window, |window, cx| window.press("escape", cx));
    ui(cx, window, |window, cx| {
        window.click(("task-url", 1usize), cx)
    });
    assert_eq!(
        terms(cx, |t| t.active_page().map(|p| p.url.clone())),
        Some("https://example.com/b".to_owned())
    );
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
    // macOS（Safari）は F5 ではなく ⌘⇧R
    press_in_page(cx, &world, window, tab, key("f5", "cmd-shift-r"));
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

/// ほかの場所から URL の欄を押すと、書いてある URL がすべて選ばれ、打つとそのまま置き換わる。
#[gpui_kit::test]
fn a_click_on_the_url_bar_selects_the_whole_url(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, _view) = open(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    let tab = open_page(cx, window, "https://example.com/a");
    let fake = world.fake(tab);
    ui(cx, window, |window, cx| window.click("browser-url", cx));
    ui(cx, window, |window, cx| {
        window.input("example.org", cx);
        window.press("enter", cx);
    });
    assert!(
        fake.calls
            .borrow()
            .contains(&"load https://example.org".to_owned()),
        "{:?}",
        fake.calls.borrow()
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
    // 印刷のダイアログ
    press_in_page(cx, &world, window, tab, key("ctrl-p", "cmd-p"));
    ui(cx, window, |window, _| {
        assert!(window.find("browser-print").visible())
    });
}

/// ページの中で押したアプリのキー（全画面、AI チャット）も、いつもどおりに働く。
#[gpui_kit::test]
fn app_keys_work_inside_the_page(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, _view) = open(cx);
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
    // ダウンロードの一覧（Chrome と同じく Ctrl+J、macOS は ⌥⌘L）
    press_in_page(cx, &world, window, tab, key("ctrl-j", "cmd-alt-l"));
    ui(cx, window, |window, _| {
        assert!(
            window.find("downloads").visible(),
            "ダウンロードの一覧が出る"
        )
    });
    ui(cx, window, |window, cx| window.press("escape", cx));
    ui(cx, window, |window, _| {
        assert!(window.try_find("downloads").is_none())
    });
    // AI チャットのキー（macOS の ⌘J）。AI チャットを閉じるとページも隠れるので、最後に確かめる
    #[cfg(target_os = "macos")]
    press_in_page(cx, &world, window, tab, "cmd-j");
    #[cfg(target_os = "macos")]
    assert!(
        !cx.update(|cx| view.read(cx).ai_open),
        "AI チャットを閉じる"
    );
}

/// 小さなウィンドウのバーを右クリックすると、ほかのウィンドウと同じメニューが出る。
/// メニューの間はページを隠し、「閉じる」でウィンドウとページを閉じる。
#[gpui_kit::test]
fn the_small_window_bar_has_the_window_menu(cx: &mut TestAppContext) {
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
    cx.run_until_parked();
    let small = terms(cx, |t| t.pages.iter().find(|p| p.popup).map(|p| p.id))
        .expect("小さなウィンドウのページがある");
    let popup = cx
        .update(|cx| cx.windows())
        .into_iter()
        .find(|w| *w != window)
        .expect("小さなウィンドウが開く");
    ui(cx, popup, |_, _| {});
    let fake = world.fake(small);
    assert!(fake.visible.get());
    ui(cx, popup, |window, cx| {
        window.click_with_options(
            "popup-title",
            ClickOptions::new().with_button(MouseButton::Right),
            cx,
        )
    });
    assert!(cx.update(|cx| browser::menus_open(cx)));
    assert!(!fake.visible.get(), "メニューの間はページを隠す");
    // いちばん下の「閉じる」
    cx.update_window(popup, |_, window, cx| {
        window.render_frame(cx);
        window.press("up", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(fake.dropped.get());
    assert!(terms(cx, |t| t.page(small).is_none()));
    assert_eq!(cx.update(|cx| cx.windows().len()), 1);
}

/// 小さなウィンドウのページでも、タブと同じキーで読み込み直す・戻る・探す・印刷し、
/// タブを閉じるキーでウィンドウを閉じる。
#[gpui_kit::test]
fn the_small_window_takes_the_browser_keys(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, _view) = open(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    let tab = open_page(cx, window, "https://example.com/");
    world.send(tab, Event::Popup("https://example.com/b".into()));
    cx.run_until_parked();
    let small = terms(cx, |t| t.pages.iter().find(|p| p.popup).map(|p| p.id))
        .expect("小さなウィンドウのページがある");
    let popup = cx
        .update(|cx| cx.windows())
        .into_iter()
        .find(|w| *w != window)
        .expect("小さなウィンドウが開く");
    ui(cx, popup, |_, _| {});
    let fake = world.fake(small);
    let tab_calls = world.fake(tab).calls.borrow().len();
    press_in_page(cx, &world, popup, small, key("ctrl-r", "cmd-r"));
    press_in_page(cx, &world, popup, small, key("alt-left", "cmd-["));
    let calls = fake.calls.borrow().clone();
    let done: Vec<&String> = calls
        .iter()
        .filter(|c| ["reload", "back"].contains(&c.as_str()))
        .collect();
    assert_eq!(done, ["reload", "back"], "{calls:?}");
    // 印刷のダイアログも、このウィンドウの中に出る。プリンターを選ぶと、OS の印刷のダイアログ
    press_in_page(cx, &world, popup, small, key("ctrl-p", "cmd-p"));
    ui(cx, popup, |window, _| {
        assert!(window.find("browser-print").visible())
    });
    assert!(!fake.visible.get(), "ダイアログの間はページを隠す");
    ui(cx, popup, |window, cx| {
        window.click("browser-print-printer", cx)
    });
    ui(cx, popup, |window, cx| window.press("enter", cx));
    assert!(fake.calls.borrow().contains(&"print".to_owned()));
    ui(cx, popup, |window, _| {
        assert!(window.try_find("browser-print").is_none())
    });
    assert!(fake.visible.get(), "閉じたらページを見せる");
    assert_eq!(
        world.fake(tab).calls.borrow().len(),
        tab_calls,
        "タブのページは動かない"
    );
    // ページの中を探す
    press_in_page(cx, &world, popup, small, key("ctrl-f", "cmd-f"));
    ui(cx, popup, |window, _| {
        assert!(window.find("browser-find").visible())
    });
    ui(cx, popup, |window, cx| window.input("例", cx));
    ui(cx, popup, |window, cx| window.press("enter", cx));
    let finds: Vec<String> = fake
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("find"))
        .cloned()
        .collect();
    assert_eq!(finds, ["find 例 from top", "find 例 next"]);
    ui(cx, popup, |window, cx| window.press("escape", cx));
    ui(cx, popup, |window, _| {
        assert!(window.try_find("browser-find").is_none())
    });
    // タブを閉じるキーで、ウィンドウを閉じる
    world.send(small, Event::Focused);
    ui(cx, popup, |_, _| {});
    world.send(small, Event::Key(key("ctrl-w", "cmd-w").into()));
    cx.run_until_parked();
    assert!(fake.dropped.get());
    assert!(terms(cx, |t| t.page(small).is_none()));
    assert_eq!(cx.update(|cx| cx.windows().len()), 1);
}

/// URL の欄に出ている文字。
fn bar(cx: &mut TestAppContext, window: AnyWindowHandle) -> String {
    let mut shown = String::new();
    ui(cx, window, |window, _| {
        shown = window
            .find_by_label("URL")
            .value()
            .unwrap_or_default()
            .to_owned();
    });
    shown
}

/// ページの場所が変わった（OS の部品の場所を変えてから知らせる）。
fn move_page(world: &World, tab: u64, url: &str, event: Event) {
    *world.fake(tab).url.borrow_mut() = url.to_owned();
    world.send(tab, event);
}

/// URL の欄を押してからページに戻ると、そのあとの移動で URL の欄が変わる。
/// リンクで読み込んだとき、リダイレクトしたとき、読み込まずに場所だけ変わったとき（GitHub など）のどれも。
#[gpui_kit::test]
fn the_url_bar_follows_the_page_after_going_back_into_it(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, _view) = open(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    let tab = open_page(cx, window, "https://example.com/a");
    assert_eq!(bar(cx, window), "https://example.com/a");
    // URL の欄を押してから、ページを押す
    ui(cx, window, |window, cx| window.click("browser-url", cx));
    world.send(tab, Event::Focused);
    ui(cx, window, |_, _| {});
    // ページの中のリンク
    let load = |done: bool, url: &str| Event::Load {
        done,
        url: url.into(),
    };
    move_page(
        &world,
        tab,
        "https://example.com/b",
        load(false, "https://example.com/b"),
    );
    ui(cx, window, |_, _| {});
    assert_eq!(bar(cx, window), "https://example.com/b");
    // リダイレクトで、読み込み終わった場所が変わる
    move_page(
        &world,
        tab,
        "https://example.com/c",
        load(true, "https://example.com/c"),
    );
    ui(cx, window, |_, _| {});
    assert_eq!(bar(cx, window), "https://example.com/c");
    // 読み込まずに場所だけ変わる（history.pushState、戻る・進む、# の移動）
    move_page(&world, tab, "https://example.com/c/issues", Event::Moved);
    ui(cx, window, |_, _| {});
    assert_eq!(bar(cx, window), "https://example.com/c/issues");
    assert_eq!(
        terms(cx, |t| t.page(tab).unwrap().url.clone()),
        "https://example.com/c/issues"
    );
}

/// URL の欄を押して選んだだけなら、ページの場所が変われば欄も変わる。
/// 書き換えている間は書き換えず、戻る・進む・読み込み直すを押したら、ページの場所に戻す。
#[gpui_kit::test]
fn the_url_bar_keeps_only_what_is_being_typed(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, _view) = open(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    let tab = open_page(cx, window, "https://example.com/a");
    ui(cx, window, |window, cx| window.click("browser-url", cx));
    move_page(&world, tab, "https://example.com/b", Event::Moved);
    ui(cx, window, |_, _| {});
    assert_eq!(bar(cx, window), "https://example.com/b", "選んだだけ");
    // 書いている途中は、そのまま
    ui(cx, window, |window, cx| window.input("rust gpui", cx));
    move_page(&world, tab, "https://example.com/c", Event::Moved);
    ui(cx, window, |_, _| {});
    assert_eq!(bar(cx, window), "rust gpui", "書いている途中");
    // 戻るを押したら、書きかけを捨ててページの場所を出す
    world.fake(tab).history.set((true, false));
    world.send(tab, Event::Title("C".into()));
    ui(cx, window, |_, _| {});
    ui(cx, window, |window, cx| window.click("browser-back", cx));
    assert!(world.fake(tab).calls.borrow().contains(&"back".to_owned()));
    assert_eq!(bar(cx, window), "https://example.com/c");
    move_page(&world, tab, "https://example.com/b", Event::Moved);
    ui(cx, window, |_, _| {});
    assert_eq!(bar(cx, window), "https://example.com/b");
}

/// 書きかけの URL があっても、タブを切り替えたらそのタブの場所を出す。
#[gpui_kit::test]
fn the_url_bar_shows_the_tab_that_is_switched_to(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, _view) = open(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    let a = open_page(cx, window, "https://example.com/a");
    let b = open_page(cx, window, "https://example.com/b");
    assert_eq!(bar(cx, window), "https://example.com/b");
    ui(cx, window, |window, cx| window.click("browser-url", cx));
    ui(cx, window, |window, cx| window.input("書きかけ", cx));
    cx.update(|cx| term::shared(cx).update(cx, |t, cx| t.activate(Some(a), cx)));
    ui(cx, window, |_, _| {});
    assert_eq!(bar(cx, window), "https://example.com/a");
    // 見ていないタブの場所が変わっても、今のタブの欄はそのまま
    move_page(&world, b, "https://example.com/b2", Event::Moved);
    ui(cx, window, |_, _| {});
    assert_eq!(bar(cx, window), "https://example.com/a");
    cx.update(|cx| term::shared(cx).update(cx, |t, cx| t.activate(Some(b), cx)));
    ui(cx, window, |_, _| {});
    assert_eq!(bar(cx, window), "https://example.com/b2");
}

/// 小さなウィンドウのページも、読み込まずに変わった場所を覚える（「ブラウザで開く」「タブで開く」で使う）。
#[gpui_kit::test]
fn the_small_window_follows_the_page(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, _view) = open(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    let tab = open_page(cx, window, "https://example.com/");
    world.send(tab, Event::Popup("https://example.com/b".into()));
    cx.run_until_parked();
    let small = terms(cx, |t| t.pages.iter().find(|p| p.popup).map(|p| p.id))
        .expect("小さなウィンドウのページがある");
    let popup = cx
        .update(|cx| cx.windows())
        .into_iter()
        .find(|w| *w != window)
        .expect("小さなウィンドウが開く");
    ui(cx, popup, |_, _| {});
    move_page(&world, small, "https://example.com/b#2", Event::Moved);
    ui(cx, popup, |_, _| {});
    assert_eq!(
        terms(cx, |t| t.page(small).unwrap().url.clone()),
        "https://example.com/b#2"
    );
}

/// `pages` ページの PDF を書く。`embedded` でなければ、埋め込んでいない Helvetica を使う。
fn write_pdf(path: &std::path::Path, pages: usize, embedded: bool) {
    use lopdf::{dictionary, Document, Object, Stream};
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let mut font = dictionary! {
        "Type" => "Font",
        "Subtype" => "TrueType",
        "BaseFont" => "Helvetica",
    };
    if embedded {
        let file = doc.add_object(Stream::new(dictionary! {}, b"font".to_vec()));
        let descriptor = doc.add_object(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "Helvetica",
            "FontFile2" => file,
        });
        font.set("FontDescriptor", descriptor);
    }
    let font = doc.add_object(font);
    let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
    let kids: Vec<Object> = (0..pages)
        .map(|i| {
            let content = doc.add_object(Stream::new(
                dictionary! {},
                format!("BT /F1 12 Tf 72 720 Td ({}) Tj ET", i + 1).into_bytes(),
            ));
            doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content,
            })
            .into()
        })
        .collect();
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => pages as i64,
            "Resources" => resources,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        }),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    doc.save(path).unwrap();
}

/// 最後に PDF に保存した場所。
fn last_pdf(fake: &Fake) -> String {
    fake.calls
        .borrow()
        .iter()
        .rev()
        .find(|c| c.starts_with("pdf "))
        .cloned()
        .unwrap_or_default()
}

/// ページからのダウンロードは、上のバーのアイコンに数と進み具合が出て、押すと一覧が出る。
/// 一覧を開くと数は消え、終わると数がまた出る。一覧から消してもファイルは残る。
#[gpui_kit::test]
fn downloads_show_in_the_header_with_a_count(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, _view) = open(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    let tab = open_page(cx, window, "https://example.com/files");
    ui(cx, window, |window, _| {
        assert!(window.try_find("open-downloads").is_none(), "まだ何もない")
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("資料.zip");
    let url = "https://example.com/資料.zip".to_owned();
    world.send(
        tab,
        Event::Download(Note::Started {
            url: url.clone(),
            path: path.clone(),
        }),
    );
    world.send(
        tab,
        Event::Download(Note::Progress {
            url: url.clone(),
            received: 512,
            total: Some(2048),
        }),
    );
    cx.run_until_parked();
    ui(cx, window, |window, _| {
        assert!(window.find("open-downloads").visible());
        assert!(window.find("open-downloads-count").visible());
    });
    assert_eq!(cx.update(|cx| downloads::shared(cx).read(cx).unseen()), 1);
    let item =
        |cx: &mut TestAppContext| cx.update(|cx| downloads::shared(cx).read(cx).items[0].clone());
    assert_eq!(item(cx).ratio(), Some(0.25));
    // 一覧を開くと、見たことになる
    ui(cx, window, |window, cx| window.click("open-downloads", cx));
    ui(cx, window, |window, _| {
        assert!(window.find(("download", 0usize)).visible());
        assert!(window.try_find("open-downloads-count").is_none());
        // Chrome と同じく、ボタンはブラウザの画面のツールバーの右の端にあり、
        // 一覧はボタンのすぐ下に、右の端をそろえて出る
        let browser = window.find("browser").bounds();
        let button = window.find("open-downloads").bounds();
        let url = window.find("browser-url").bounds();
        assert!(browser.contains(&button.center()), "{browser:?} {button:?}");
        assert!(button.left() > url.right(), "{url:?} {button:?}");
        assert!(
            browser.right() - button.right() < gpui_kit::px(16.),
            "{browser:?} {button:?}"
        );
        let list = window.find("downloads").bounds();
        // 出てくる間は少し上から下りてくる
        assert!(
            list.top() + gpui_kit::px(8.) >= button.bottom(),
            "{list:?} {button:?}"
        );
        assert!(
            list.top() - button.bottom() < gpui_kit::px(16.),
            "{list:?} {button:?}"
        );
        assert!(
            (list.right() - button.right()).abs() < gpui_kit::px(12.),
            "{list:?} {button:?}"
        );
    });
    ui(cx, window, |window, cx| window.press("escape", cx));
    std::fs::write(&path, vec![0u8; 2048]).unwrap();
    world.send(
        tab,
        Event::Download(Note::Finished {
            url: url.clone(),
            path: Some(path.clone()),
            ok: true,
        }),
    );
    cx.run_until_parked();
    ui(cx, window, |window, _| {
        assert!(window.find("open-downloads-count").visible(), "終わった")
    });
    assert_eq!(item(cx).state, downloads::State::Done);
    assert_eq!(item(cx).received, 2048);
    // 一覧は残り、次に起動したときも出る
    let saved = cx.update(|cx| {
        malakt_mahoz::db::shared(cx)
            .read(cx)
            .store
            .setting("browser.downloads")
            .unwrap()
    });
    assert!(saved.is_some_and(|s| s.contains("資料.zip")));
    // 一覧から消しても、ファイルは消さない
    ui(cx, window, |window, cx| window.click("open-downloads", cx));
    ui(cx, window, |window, cx| {
        window.hover(("download", 0usize), cx)
    });
    // 乗せると「フォルダで表示」と「一覧から削除」が同じ大きさのボタンで並ぶ
    ui(cx, window, |window, _| {
        let reveal = window.find(("download-reveal", 0usize));
        let remove = window.find(("download-remove", 0usize));
        assert!(reveal.visible() && remove.visible());
        assert_eq!(reveal.bounds().size, remove.bounds().size);
    });
    ui(cx, window, |window, cx| {
        window.click(("download-remove", 0usize), cx)
    });
    assert!(cx.update(|cx| downloads::shared(cx).read(cx).items.is_empty()));
    assert!(path.exists());
    ui(cx, window, |window, cx| window.press("escape", cx));
    ui(cx, window, |window, _| {
        assert!(window.try_find("open-downloads").is_none())
    });
}

/// 失敗したダウンロードは、失敗と出る。
#[gpui_kit::test]
fn a_failed_download_says_so(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, _view) = open(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    let tab = open_page(cx, window, "https://example.com/files");
    let url = "https://example.com/a.zip".to_owned();
    world.send(
        tab,
        Event::Download(Note::Started {
            url: url.clone(),
            path: "/nowhere/a.zip".into(),
        }),
    );
    world.send(
        tab,
        Event::Download(Note::Finished {
            url,
            path: None,
            ok: false,
        }),
    );
    cx.run_until_parked();
    ui(cx, window, |window, cx| window.click("open-downloads", cx));
    let state = cx.update(|cx| downloads::shared(cx).read(cx).items[0].state.clone());
    assert!(
        matches!(state, downloads::State::Failed { .. }),
        "{state:?}"
    );
    let detail = cx.update(|cx| downloads::shared(cx).read(cx).items[0].detail(0));
    assert_eq!(detail, "ダウンロードできませんでした");
    ui(cx, window, |window, _| {
        assert!(window.find(("download", 0usize)).visible())
    });
}

/// Ctrl+P で印刷のダイアログが出て、送信先は「PDF に保存」。ファイル名はページの題名で、
/// Enter でダウンロードのフォルダに保存し、ダウンロードの一覧に出る。Esc で閉じる。
#[gpui_kit::test]
fn ctrl_p_saves_the_page_as_a_pdf(cx: &mut TestAppContext) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, _view) = open(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    let tab = open_page(cx, window, "https://example.com/report");
    let fake = world.fake(tab);
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| cx.set_global(downloads::DownloadDir(dir.path().to_owned())));
    world.send(tab, Event::Title("週次: 報告".into()));
    cx.run_until_parked();
    // Esc で閉じる
    press_in_page(cx, &world, window, tab, key("ctrl-p", "cmd-p"));
    ui(cx, window, |window, _| {
        assert!(window.find("browser-print").visible())
    });
    assert!(!fake.visible.get(), "ダイアログの間はページを隠す");
    ui(cx, window, |window, cx| window.press("escape", cx));
    ui(cx, window, |window, _| {
        assert!(window.try_find("browser-print").is_none())
    });
    assert!(fake.visible.get());
    // ファイル名を変えて保存する
    press_in_page(cx, &world, window, tab, key("ctrl-p", "cmd-p"));
    ui(cx, window, |window, cx| {
        window.input("議事録", cx);
        window.press("enter", cx);
    });
    let call = last_pdf(&fake);
    let saved = dir.path().join("議事録.pdf");
    assert_eq!(call, format!("pdf {}", saved.display()));
    let item = cx.update(|cx| downloads::shared(cx).read(cx).items[0].clone());
    assert_eq!(item.kind, downloads::Kind::Pdf);
    assert!(item.running());
    ui(cx, window, |window, _| {
        assert!(window.find("open-downloads-count").visible())
    });
    let done = fake.pdf.borrow_mut().take().expect("PDF を作っている");
    write_pdf(&saved, 1, true);
    done(Ok(()));
    cx.run_until_parked();
    let item = cx.update(|cx| downloads::shared(cx).read(cx).items[0].clone());
    assert_eq!(item.state, downloads::State::Done);
    assert_eq!(item.received, std::fs::metadata(&saved).unwrap().len());
    // 同じ名前があれば、番号を付ける
    press_in_page(cx, &world, window, tab, key("ctrl-p", "cmd-p"));
    ui(cx, window, |window, cx| {
        window.input("議事録", cx);
        window.press("enter", cx);
    });
    let call = last_pdf(&fake);
    assert!(call.ends_with("議事録 (1).pdf"), "{call}");
    // 題名がそのままファイル名になる（使えない文字は _）
    press_in_page(cx, &world, window, tab, key("ctrl-p", "cmd-p"));
    ui(cx, window, |window, cx| {
        window.click("browser-print-submit", cx)
    });
    let call = last_pdf(&fake);
    assert!(call.ends_with("週次_ 報告.pdf"), "{call}");
}

/// メニューの項目を押す。
fn pick(cx: &mut TestAppContext, window: AnyWindowHandle, label: &str) {
    ui(cx, window, |window, cx| {
        let center = window.find_by_label(label).bounds().center();
        window.pointer_move(center, None, cx);
        window.pointer_down(center, MouseButton::Left, cx);
        window.pointer_up(center, MouseButton::Left, cx);
    });
}

/// PDF を作る準備: ページを開き、ダウンロードのフォルダを決める。
fn pdf_page(cx: &mut TestAppContext) -> (World, AnyWindowHandle, u64, Rc<Fake>, tempfile::TempDir) {
    let world = start(cx, Store::open_in_memory().unwrap());
    let (window, _view) = open(cx);
    ui(cx, window, |window, cx| window.click("ai-toggle", cx));
    let tab = open_page(cx, window, "https://example.com/report");
    let fake = world.fake(tab);
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| cx.set_global(downloads::DownloadDir(dir.path().to_owned())));
    world.send(tab, Event::Title("報告".into()));
    cx.run_until_parked();
    (world, window, tab, fake, dir)
}

/// 詳細設定で余白「なし」と背景のグラフィック、レイアウト「横」を選ぶと、その設定で PDF を作る。
/// 設定は覚えて、次に開いたときも同じ。
#[gpui_kit::test]
fn print_settings_reach_the_pdf_and_are_remembered(cx: &mut TestAppContext) {
    let (world, window, tab, fake, dir) = pdf_page(cx);
    press_in_page(cx, &world, window, tab, key("ctrl-p", "cmd-p"));
    ui(cx, window, |window, cx| {
        assert!(
            window.try_find("browser-print-margins").is_none(),
            "最初は畳んでいる"
        );
        window.click("browser-print-more", cx);
    });
    ui(cx, window, |window, cx| {
        window.click("browser-print-margins", cx)
    });
    pick(cx, window, "なし");
    ui(cx, window, |window, cx| {
        window.click("browser-print-layout", cx)
    });
    pick(cx, window, "横");
    ui(cx, window, |window, cx| {
        window.click("browser-print-backgrounds", cx);
    });
    ui(cx, window, |window, cx| {
        window.click("browser-print-submit", cx)
    });
    let (setup, pages) = fake.pdf_setup.borrow().last().cloned().unwrap();
    assert_eq!(setup.margins, Margins::None);
    assert!(setup.landscape);
    assert!(setup.backgrounds);
    assert_eq!(pages, None);
    let saved = dir.path().join("報告.pdf");
    let done = fake.pdf.borrow_mut().take().unwrap();
    write_pdf(&saved, 1, true);
    done(Ok(()));
    cx.run_until_parked();
    let item = cx.update(|cx| downloads::shared(cx).read(cx).items[0].clone());
    assert_eq!(item.state, downloads::State::Done);
    // 覚えた設定で開く（詳細設定も開いたまま）
    let stored = cx.update(|cx| {
        malakt_mahoz::db::shared(cx)
            .read(cx)
            .store
            .setting(print_setup::SETTING)
            .unwrap()
    });
    assert!(stored.is_some(), "設定を保存している");
    press_in_page(cx, &world, window, tab, key("ctrl-p", "cmd-p"));
    ui(cx, window, |window, _| {
        assert!(window.find("browser-print-margins").visible());
    });
    ui(cx, window, |window, cx| {
        window.click("browser-print-submit", cx)
    });
    let (again, _) = fake.pdf_setup.borrow().last().cloned().unwrap();
    assert_eq!(again, setup);
}

/// 奇数ページだけを選ぶと、保存したあとで偶数のページを除く。
#[gpui_kit::test]
fn odd_pages_are_kept_after_saving(cx: &mut TestAppContext) {
    let (world, window, tab, fake, dir) = pdf_page(cx);
    press_in_page(cx, &world, window, tab, key("ctrl-p", "cmd-p"));
    ui(cx, window, |window, cx| {
        window.click("browser-print-pages", cx)
    });
    pick(cx, window, "奇数ページのみ");
    ui(cx, window, |window, cx| {
        window.click("browser-print-submit", cx)
    });
    let (setup, pages) = fake.pdf_setup.borrow().last().cloned().unwrap();
    assert_eq!(setup.pages, Pages::Odd);
    assert_eq!(pages, None, "1 回で作り、あとで選ぶ");
    let saved = dir.path().join("報告.pdf");
    write_pdf(&saved, 5, true);
    fake.pdf.borrow_mut().take().unwrap()(Ok(()));
    cx.run_until_parked();
    let item = cx.update(|cx| downloads::shared(cx).read(cx).items[0].clone());
    assert_eq!(item.state, downloads::State::Done);
    assert_eq!(print_setup::page_count(&saved).unwrap(), 3);
}

/// ページを選んで 1 枚に 2 ページ並べるときは、先にページの数を数えてから、選んだページで作り直す。
#[cfg(not(target_os = "macos"))]
#[gpui_kit::test]
fn chosen_pages_side_by_side_are_counted_first(cx: &mut TestAppContext) {
    let (world, window, tab, fake, dir) = pdf_page(cx);
    press_in_page(cx, &world, window, tab, key("ctrl-p", "cmd-p"));
    ui(cx, window, |window, cx| {
        window.click("browser-print-pages", cx)
    });
    pick(cx, window, "カスタム");
    ui(cx, window, |window, cx| {
        window.click("browser-print-ranges", cx);
    });
    ui(cx, window, |window, cx| window.input("2-", cx));
    ui(cx, window, |window, cx| {
        window.click("browser-print-more", cx)
    });
    ui(cx, window, |window, cx| {
        window.click("browser-print-per-sheet", cx)
    });
    pick(cx, window, "2");
    ui(cx, window, |window, cx| {
        window.click("browser-print-submit", cx)
    });
    let saved = dir.path().join("報告.pdf");
    let (first, pages) = fake.pdf_setup.borrow().last().cloned().unwrap();
    assert_eq!(first.per_sheet, 1, "数えるときは並べない");
    assert_eq!(pages, None);
    write_pdf(&saved, 4, true);
    fake.pdf.borrow_mut().take().unwrap()(Ok(()));
    cx.run_until_parked();
    let (second, pages) = fake.pdf_setup.borrow().last().cloned().unwrap();
    assert_eq!(second.per_sheet, 2);
    assert_eq!(pages, Some(vec![2, 3, 4]));
    write_pdf(&saved, 2, true);
    fake.pdf.borrow_mut().take().unwrap()(Ok(()));
    cx.run_until_parked();
    let item = cx.update(|cx| downloads::shared(cx).read(cx).items[0].clone());
    assert_eq!(item.state, downloads::State::Done);
    assert_eq!(
        print_setup::page_count(&saved).unwrap(),
        2,
        "並べたあとは選び直さない"
    );
}

/// 読めないページの範囲では保存できず、何がおかしいかを出す。
#[gpui_kit::test]
fn wrong_page_ranges_cannot_be_saved(cx: &mut TestAppContext) {
    let (world, window, tab, fake, _dir) = pdf_page(cx);
    press_in_page(cx, &world, window, tab, key("ctrl-p", "cmd-p"));
    ui(cx, window, |window, cx| {
        window.click("browser-print-pages", cx)
    });
    pick(cx, window, "カスタム");
    ui(cx, window, |window, cx| {
        window.click("browser-print-ranges", cx)
    });
    ui(cx, window, |window, cx| window.input("3-1", cx));
    ui(cx, window, |window, cx| {
        assert!(window.find("browser-print-problem").visible());
        assert_eq!(window.find("browser-print-submit").disabled(), Some(true));
        window.press("enter", cx);
    });
    assert!(fake.pdf_setup.borrow().is_empty(), "保存しない");
    ui(cx, window, |window, _| {
        assert!(window.find("browser-print").visible())
    });
}

/// フォントを埋め込んでいない PDF は失敗にして、ファイルを残さない。
#[gpui_kit::test]
fn a_pdf_without_embedded_fonts_fails(cx: &mut TestAppContext) {
    let (world, window, tab, fake, dir) = pdf_page(cx);
    press_in_page(cx, &world, window, tab, key("ctrl-p", "cmd-p"));
    ui(cx, window, |window, cx| {
        window.click("browser-print-submit", cx)
    });
    let saved = dir.path().join("報告.pdf");
    write_pdf(&saved, 1, false);
    fake.pdf.borrow_mut().take().unwrap()(Ok(()));
    cx.run_until_parked();
    let item = cx.update(|cx| downloads::shared(cx).read(cx).items[0].clone());
    let downloads::State::Failed { reason } = item.state else {
        panic!("失敗にする: {:?}", item.state);
    };
    assert!(
        reason.contains("フォントを埋め込めませんでした: Helvetica"),
        "{reason}"
    );
    assert!(!saved.exists());
}
