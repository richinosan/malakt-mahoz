//! タイマーのシナリオテスト。メインのウィンドウのメニューから開き、長さを決めて測る。

use gpui_kit::test::{TestEventExt, TestQueryExt, TestWindowExt};
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext, Bounds, Entity, Modifiers, MouseButton, Pixels, Point,
    Size, TestAppContext, WindowBounds, WindowOptions,
};
use malakt_core::timer::{Mode, Op, Run};
use malakt_core::Store;
use malakt_mahoz::timer;
use malakt_mahoz::{db, MainView, Modal};

fn start(cx: &mut TestAppContext) {
    let store = Store::open_in_memory().unwrap();
    cx.update(|cx| malakt_mahoz::init(store, cx));
}

fn win(width: f32, height: f32) -> Size<Pixels> {
    let scale = malakt_mahoz::UI_SCALE;
    size(px(width * scale), px(height * scale))
}

fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<MainView>) {
    cx.update(|cx| {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: win(1280., 800.),
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

fn pick(cx: &mut TestAppContext, window: AnyWindowHandle, label: &str) {
    ui(cx, window, |window, cx| {
        let center = window.find_by_label(label).bounds().center();
        window.pointer_move(center, None, cx);
        window.pointer_down(center, MouseButton::Left, cx);
        window.pointer_up(center, MouseButton::Left, cx);
    });
}

fn state(cx: &mut TestAppContext) -> malakt_core::timer::Timer {
    cx.update(|cx| timer::shared(cx).read(cx).timer.clone())
}

fn time(cx: &mut TestAppContext, window: AnyWindowHandle) -> String {
    let mut text = String::new();
    ui(cx, window, |window, _| {
        text = window
            .find("timer-time")
            .label()
            .unwrap_or_default()
            .to_owned()
    });
    text
}

/// 時刻を進める代わりに、動いている時計の始まりを `ms` だけ前にずらす。
fn rewind(cx: &mut TestAppContext, ms: i64) {
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            let mut t = db.store.timer_state().unwrap();
            t.since = t.since.map(|since| since - ms);
            let text = serde_json::to_string(&t).unwrap();
            db.store
                .set_setting(malakt_core::timer::STATE_KEY, Some(&text))
                .unwrap();
            cx.notify();
        })
    });
    cx.run_until_parked();
}

/// 最初の画面にはタイマーは出ない。上のバーの右クリックの「新しいウィンドウ」から開き、
/// もう一度開いても 2 枚にはならない。
#[gpui_kit::test]
fn opens_from_the_new_window_menu(cx: &mut TestAppContext) {
    start(cx);
    let (window, _) = open(cx);
    ui(cx, window, |window, _| {
        assert!(window.try_find("timer-toggle").is_none());
    });
    let mut at = None;
    ui(cx, window, |window, _| {
        let menu = window.find("toggle-sidebar").bounds();
        at = Some(menu.center() + gpui_kit::point(px(300.), px(0.)));
    });
    let at = at.unwrap();
    let mut v = gpui_kit::VisualTestContext::from_window(window, cx);
    v.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
    v.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
    v.run_until_parked();
    pick(cx, window, "新しいウィンドウ");
    pick(cx, window, "タイマー");
    let opened = cx
        .update(|cx| timer::window(cx))
        .expect("タイマーのウィンドウが開く");
    assert_eq!(time(cx, opened), "05:00");
    let count = cx.update(|cx| cx.windows().len());
    cx.update(|cx| timer::open(cx).unwrap());
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.windows().len()), count);
    cx.update(|cx| {
        assert!(db::shared(cx)
            .read(cx)
            .store
            .setting("timer.open")
            .unwrap()
            .is_some())
    });
}

/// 長さを選んで始め、0 になると止まって、アプリの通知（ベル）と OS の通知に 1 回だけ出る。
/// 通知を押すと既読になる。
#[gpui_kit::test]
fn the_countdown_ends_with_a_notice(cx: &mut TestAppContext) {
    start(cx);
    let (main, view) = open(cx);
    let window = cx.update(|cx| timer::open(cx).unwrap());
    cx.run_until_parked();

    // 「1 分」を選ぶ
    ui(cx, window, |window, cx| {
        window.click(("timer-preset", 0usize), cx)
    });
    assert_eq!(time(cx, window), "01:00");
    ui(cx, window, |window, cx| window.click("timer-toggle", cx));
    assert_eq!(state(cx).run(), Run::Running);
    // 一時停止して、続きから
    ui(cx, window, |window, cx| window.press("space", cx));
    assert_eq!(state(cx).run(), Run::Paused);
    ui(cx, window, |window, cx| window.press("space", cx));
    assert_eq!(state(cx).run(), Run::Running);
    assert!(cx.shown_system_notifications().is_empty());

    rewind(cx, 60_000);
    assert_eq!(state(cx).run(), Run::Done);
    assert_eq!(time(cx, window), "00:00");
    ui(cx, window, |window, _| {
        assert_eq!(window.find("timer-status").label(), Some("終わりました"));
    });
    let shown = cx.shown_system_notifications();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].title.as_ref(), "タイマーが終わりました");
    cx.update(|cx| assert_eq!(view.read(cx).unread_notices, 1));
    // 読み直しても、出し直さない
    rewind(cx, 0);
    assert_eq!(cx.shown_system_notifications().len(), 1);

    // ベルの一覧から押すと既読になる
    ui(cx, main, |window, cx| window.click("open-notices", cx));
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::Notices));
    cx.update(|cx| {
        let notice = &view.read(cx).notices[0];
        assert_eq!(notice.headline(), "タイマーが終わりました");
        assert_eq!(notice.excerpt, "1 分");
    });
    ui(cx, main, |window, cx| window.click(("notice", 0usize), cx));
    cx.update(|cx| {
        assert_eq!(view.read(cx).unread_notices, 0);
        assert_eq!(view.read(cx).modal, Modal::None);
    });

    // もう一度始めると、最初から
    ui(cx, window, |window, cx| window.click("timer-toggle", cx));
    assert_eq!(state(cx).run(), Run::Running);
    // リセットで止めて最初に戻す
    ui(cx, window, |window, cx| window.click("timer-reset", cx));
    assert_eq!(
        (state(cx).run(), time(cx, window)),
        (Run::Ready, "01:00".into())
    );
}

/// 止まっている間は、時間を押すとその場で書き直せる。Enter で決まり、Esc で取り消し、
/// 読めない書き方はエラーになる。
#[gpui_kit::test]
fn the_length_is_typed_in_place(cx: &mut TestAppContext) {
    start(cx);
    open(cx);
    let window = cx.update(|cx| timer::open(cx).unwrap());
    cx.run_until_parked();

    ui(cx, window, |window, cx| window.click("timer-time", cx));
    ui(cx, window, |window, cx| {
        assert!(window.try_find("timer-input").is_some());
        window.input("1:30", cx);
        window.press("enter", cx);
    });
    assert_eq!(state(cx).length, 90);
    assert_eq!(time(cx, window), "01:30");

    // Esc で取り消す
    ui(cx, window, |window, cx| window.click("timer-time", cx));
    ui(cx, window, |window, cx| {
        window.input("10", cx);
        window.press("escape", cx);
    });
    ui(cx, window, |window, _| {
        assert!(window.try_find("timer-input").is_none());
    });
    assert_eq!(state(cx).length, 90);

    // 日本語の書き方も読む
    ui(cx, window, |window, cx| window.click("timer-time", cx));
    ui(cx, window, |window, cx| {
        window.input("2分30秒", cx);
        window.press("enter", cx);
    });
    assert_eq!(state(cx).length, 150);

    // 読めなければエラー
    ui(cx, window, |window, cx| window.click("timer-time", cx));
    ui(cx, window, |window, cx| {
        window.input("あとで", cx);
        window.press("enter", cx);
    });
    ui(cx, window, |window, _| {
        let text = window
            .find("timer-error")
            .label()
            .unwrap_or_default()
            .to_owned();
        assert!(text.contains("時間を読めませんでした"), "{text}");
    });
    assert_eq!(state(cx).length, 150);
    ui(cx, window, |window, cx| window.press("escape", cx));
    ui(cx, window, |window, _| {
        assert!(window.try_find("timer-error").is_none());
    });

    // ＋ で 1 分足す
    ui(cx, window, |window, cx| window.click("timer-add", cx));
    assert_eq!(state(cx).length, 210);
}

/// ストップウォッチに切り替えると 0 から数え、AI や MCP（データベースのツール）が動かしても
/// 画面がそのまま追いかける。
#[gpui_kit::test]
fn the_stopwatch_and_the_ai_tools(cx: &mut TestAppContext) {
    start(cx);
    open(cx);
    let window = cx.update(|cx| timer::open(cx).unwrap());
    cx.run_until_parked();

    ui(cx, window, |window, cx| window.click("timer-stopwatch", cx));
    assert_eq!(state(cx).mode, Mode::Stopwatch);
    assert_eq!(time(cx, window), "00:00");
    ui(cx, window, |window, _| {
        // ストップウォッチには「足す」がない
        assert!(window.try_find("timer-add").is_none());
        assert!(window.try_find("timer-presets").is_none());
    });
    ui(cx, window, |window, cx| window.click("timer-toggle", cx));
    rewind(cx, 65_000);
    assert_eq!(time(cx, window), "01:05");
    ui(cx, window, |window, cx| window.click("timer-countdown", cx));
    assert_eq!(
        (state(cx).mode, state(cx).run()),
        (Mode::Countdown, Run::Ready)
    );

    let run = |cx: &mut TestAppContext, name: &str, args: serde_json::Value| -> serde_json::Value {
        let text = cx.update(|cx| {
            db::shared(cx).update(cx, |db, cx| {
                let out = malakt_ai::tools::execute(&mut db.store, None, name, &args).unwrap();
                cx.notify();
                out
            })
        });
        cx.run_until_parked();
        serde_json::from_str(&text).unwrap()
    };
    let got = run(
        cx,
        "timer",
        serde_json::json!({"action": "start", "text": "2分"}),
    );
    assert_eq!(got["run"], "running");
    assert_eq!(got["length_seconds"], 120);
    assert_eq!(state(cx).run(), Run::Running);
    ui(cx, window, |window, _| {
        assert_eq!(window.find("timer-toggle").label(), Some("一時停止"));
    });
    run(
        cx,
        "timer",
        serde_json::json!({"action": "add", "seconds": 60}),
    );
    assert_eq!(state(cx).length, 180);
    run(cx, "timer", serde_json::json!({"action": "pause"}));
    assert_eq!(state(cx).run(), Run::Paused);
    let got = run(cx, "get_timer", serde_json::json!({}));
    assert_eq!(got["run"], "paused");
    assert_eq!(got["mode"], "countdown");
    let got = run(
        cx,
        "timer",
        serde_json::json!({"action": "set", "mode": "stopwatch"}),
    );
    assert_eq!(got["mode"], "stopwatch");
    assert_eq!(got["shown_text"], "00:00");
    // ストップウォッチのときに長さを渡して始めると、その長さのカウントダウンになる
    let got = run(
        cx,
        "timer",
        serde_json::json!({"action": "start", "seconds": 90}),
    );
    assert_eq!(
        (
            got["mode"].as_str(),
            got["run"].as_str(),
            got["length_seconds"].as_i64()
        ),
        (Some("countdown"), Some("running"), Some(90))
    );
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            db.store.timer(Op::Reset, malakt_core::now_ms()).unwrap();
            cx.notify();
        })
    });
    cx.run_until_parked();
    assert_eq!(state(cx).run(), Run::Ready);
}

/// タイマーのウィンドウでも Ctrl+?（⌘?）でキー操作の一覧が開き、タイマーのキーが先頭に出る。
/// 開いている間の Space は検索の欄の文字で、Esc で一覧を閉じると、Space はまたタイマーを動かす。
#[gpui_kit::test]
fn keys_help_opens_in_the_timer_window(cx: &mut TestAppContext) {
    start(cx);
    let _ = open(cx);
    let window = cx.update(|cx| timer::open(cx).unwrap());
    cx.run_until_parked();
    let titles = |cx: &mut TestAppContext| -> Vec<&'static str> {
        cx.update_window(window, |_, window, cx| {
            malakt_mahoz::keys_help::opened(window, cx)
                .map(|help| help.read(cx).shown(cx).iter().map(|g| g.title).collect())
                .unwrap_or_default()
        })
        .unwrap()
    };
    ui(cx, window, |window, cx| window.click("timer-time", cx));
    ui(cx, window, |window, cx| window.press("escape", cx));
    let before = state(cx).run();
    ui(cx, window, |window, cx| {
        window.press("secondary-shift-/", cx)
    });
    assert_eq!(&titles(cx)[..2], ["タイマー", "どのウィンドウでも"]);
    ui(cx, window, |window, cx| {
        assert!(window.try_find("keys-search").is_some());
        window.press("space", cx);
    });
    assert_eq!(state(cx).run(), before);
    ui(cx, window, |window, cx| window.press("escape", cx));
    assert!(titles(cx).is_empty());
    ui(cx, window, |window, _| {
        assert!(window.try_find("keys-search").is_none())
    });
    ui(cx, window, |window, cx| window.press("space", cx));
    assert_ne!(state(cx).run(), before);
}
