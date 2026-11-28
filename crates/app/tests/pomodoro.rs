//! ポモドーロタイマーのシナリオテスト。メインのウィンドウのメニューから開き、タスクを選んで測る。

use std::sync::{Arc, Mutex};

use gpui_kit::test::{TestEventExt, TestQueryExt, TestWindowExt};
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext, Bounds, Entity, Modifiers, MouseButton, Pixels, Point,
    Size, TestAppContext, WindowBounds, WindowOptions,
};
use malakt_core::pomodoro::{Media, Op, Phase, Run, Settings};
use malakt_core::{Store, TaskDraft};
use malakt_mahoz::now_playing::{self, NowPlayingReader};
use malakt_mahoz::pomodoro::{self, MediaSender};
use malakt_mahoz::{db, MainView};
use malakt_media::{Command, Failure, NowPlaying};

fn start(cx: &mut TestAppContext) -> Arc<Mutex<Vec<Command>>> {
    let store = Store::open_in_memory().unwrap();
    // 音楽の操作は、送ったものを覚えるだけにする
    let sent = Arc::new(Mutex::new(Vec::new()));
    let log = sent.clone();
    cx.update(|cx| {
        cx.set_global(MediaSender(Arc::new(move |command| {
            log.lock().unwrap().push(command);
            Ok(())
        })));
        // OS の再生中の曲は読まない（何も開いていないことにする）
        cx.set_global(NowPlayingReader(Arc::new(|| Ok(None))));
        malakt_mahoz::init(store, cx)
    });
    sent
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

fn add_task(cx: &mut TestAppContext, title: &str) -> String {
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            let draft = TaskDraft {
                title: title.into(),
                ..TaskDraft::default()
            };
            let task = db.store.create_task(None, &draft).unwrap();
            cx.notify();
            task.id
        })
    })
}

fn timer(cx: &mut TestAppContext) -> malakt_core::pomodoro::Timer {
    cx.update(|cx| pomodoro::shared(cx).read(cx).timer.clone())
}

/// 時刻を進める代わりに、動いている段の始まりを `ms` だけ前にずらす。
fn rewind(cx: &mut TestAppContext, ms: i64) {
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            let mut t = db.store.pomodoro_timer().unwrap();
            t.since = t.since.map(|since| since - ms);
            t.from = t.from.map(|from| from - ms);
            let text = serde_json::to_string(&t).unwrap();
            db.store
                .set_setting(malakt_core::pomodoro::TIMER_KEY, Some(&text))
                .unwrap();
            cx.notify();
        })
    });
    cx.run_until_parked();
}

/// 最初の画面にはポモドーロは出ない。上のバーの右クリックの「新しいウィンドウ」から開き、
/// もう一度開いても 2 枚にはならない。
#[gpui_kit::test]
fn opens_from_the_new_window_menu(cx: &mut TestAppContext) {
    start(cx);
    let (window, _) = open(cx);
    ui(cx, window, |window, _| {
        assert!(window.try_find("pomodoro-toggle").is_none());
    });
    let mut at = None;
    ui(cx, window, |window, _| {
        let menu = window.find("toggle-sidebar").bounds();
        at = Some(menu.center() + gpui_kit::point(px(300.), px(0.)));
    });
    let at = at.unwrap();
    let right_click_header = |cx: &mut TestAppContext| {
        let mut v = gpui_kit::VisualTestContext::from_window(window, cx);
        v.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
        v.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
        v.run_until_parked();
    };
    right_click_header(cx);
    pick(cx, window, "新しいウィンドウ");
    ui(cx, window, |window, _| {
        assert!(window.try_find_by_label("タスク").is_some());
    });
    pick(cx, window, "ポモドーロ");
    let opened = cx
        .update(|cx| pomodoro::window(cx))
        .expect("ポモドーロのウィンドウが開く");
    let count = cx.update(|cx| cx.windows().len());
    ui(cx, opened, |window, _| {
        assert_eq!(window.find("pomodoro-time").label(), Some("25:00"));
    });
    // 2 回目は、開いているウィンドウを手前に出す
    cx.update(|cx| pomodoro::open(cx).unwrap());
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.windows().len()), count);
    // 設定の「ポモドーロ」からも開ける
    cx.update(|cx| {
        assert!(db::shared(cx)
            .read(cx)
            .store
            .setting("pomodoro.open")
            .unwrap()
            .is_some())
    });
}

/// タスクを選んで始め、時刻が来ると作業時間がタスクに残り、休憩が止まったまま待つ。
/// 一時停止・停止もボタンでできる。
#[gpui_kit::test]
fn measures_work_on_the_chosen_task(cx: &mut TestAppContext) {
    let sent = start(cx);
    let (main, view) = open(cx);
    let task = add_task(cx, "資料を作る");
    let window = cx.update(|cx| pomodoro::open(cx).unwrap());
    cx.run_until_parked();

    ui(cx, window, |window, cx| window.click("pomodoro-task", cx));
    ui(cx, window, |window, _| {
        assert!(window.try_find("pomodoro-search").is_some());
        assert!(window.try_find_by_label("資料を作る").is_some());
    });
    ui(cx, window, |window, cx| {
        window.click(("pomodoro-pick", 0usize), cx)
    });
    assert_eq!(timer(cx).task_id.as_deref(), Some(task.as_str()));
    ui(cx, window, |window, _| {
        assert!(window.try_find_by_label("資料を作る").is_some());
    });

    ui(cx, window, |window, cx| window.click("pomodoro-toggle", cx));
    assert_eq!(timer(cx).run(), Run::Running);
    // 一時停止して、続きから
    ui(cx, window, |window, cx| window.click("pomodoro-toggle", cx));
    assert_eq!(timer(cx).run(), Run::Paused);
    ui(cx, window, |window, cx| window.click("pomodoro-toggle", cx));
    assert_eq!(timer(cx).run(), Run::Running);

    // 25 分たった
    rewind(cx, 25 * 60_000);
    let t = timer(cx);
    assert_eq!(t.phase, Phase::ShortBreak);
    assert_eq!(t.run(), Run::Ready);
    assert_eq!(t.done, 1);
    let total = cx.update(|cx| db::shared(cx).read(cx).store.work_total(&task).unwrap());
    assert_eq!(total.0, 1);
    assert!(total.1 >= 25 * 60 - 1);
    ui(cx, window, |window, _| {
        assert_eq!(window.find("pomodoro-phase").label(), Some("休憩"));
        assert_eq!(window.find("pomodoro-time").label(), Some("05:00"));
    });
    // 音楽は選んでいないので、何も送らない
    assert!(sent.lock().unwrap().is_empty());
    // 期限の通知と同じく、アプリの通知（ベル）と OS の通知に 1 回だけ出る
    let shown = cx.shown_system_notifications();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].title.as_ref(), "作業が終わりました");
    cx.update(|cx| assert_eq!(view.read(cx).unread_notices, 1));
    rewind(cx, 0);
    assert_eq!(cx.shown_system_notifications().len(), 1);

    // タスクの詳細に作業時間が出る
    cx.update_window(main, |_, window, cx| {
        view.update(cx, |v, cx| v.open_panel(&task, window, cx))
    })
    .unwrap();
    ui(cx, main, |window, _| {
        assert_eq!(window.find("work-total").label(), Some("25 分（1 回）"));
    });

    // 停止すると、最初の作業に戻る
    ui(cx, window, |window, cx| window.click("pomodoro-stop", cx));
    let t = timer(cx);
    assert_eq!((t.phase, t.run(), t.done), (Phase::Work, Run::Ready, 0));
}

/// 選んだときだけ、作業を始めたら再生、休憩に入ったら一時停止を OS の音楽に送る。
#[gpui_kit::test]
fn controls_the_music_only_when_chosen(cx: &mut TestAppContext) {
    let sent = start(cx);
    open(cx);
    let window = cx.update(|cx| pomodoro::open(cx).unwrap());
    cx.run_until_parked();
    ui(cx, window, |window, cx| {
        window.click("pomodoro-settings", cx)
    });
    ui(cx, window, |window, cx| {
        window.click(("pomodoro-work-media", Media::Play as usize), cx)
    });
    ui(cx, window, |window, cx| {
        window.click(("pomodoro-break-media", Media::Pause as usize), cx)
    });
    // 作業の長さを 25 分から 20 分に
    ui(cx, window, |window, cx| {
        window.click("pomodoro-work-down", cx)
    });
    let settings = cx.update(|cx| pomodoro::shared(cx).read(cx).settings);
    assert_eq!(
        (settings.work, settings.work_media, settings.break_media),
        (20, Media::Play, Media::Pause)
    );
    // Esc で設定を閉じる
    ui(cx, window, |window, cx| window.press("escape", cx));
    ui(cx, window, |window, _| {
        assert_eq!(window.find("pomodoro-time").label(), Some("20:00"));
    });

    ui(cx, window, |window, cx| window.click("pomodoro-toggle", cx));
    assert_eq!(*sent.lock().unwrap(), [Command::Play]);
    rewind(cx, 20 * 60_000);
    assert_eq!(timer(cx).phase, Phase::ShortBreak);
    assert_eq!(*sent.lock().unwrap(), [Command::Play, Command::Pause]);
    // 手で送るボタン
    ui(cx, window, |window, cx| window.click("media-next", cx));
    assert_eq!(sent.lock().unwrap().last(), Some(&Command::Next));
}

/// 音楽の操作が失敗したら、ウィンドウにエラーを出す。Esc で閉じる。
#[gpui_kit::test]
fn shows_when_the_music_fails(cx: &mut TestAppContext) {
    start(cx);
    cx.update(|cx| {
        cx.set_global(MediaSender(Arc::new(|_| {
            Err(malakt_media::Failure::NoPlayer)
        })))
    });
    open(cx);
    let window = cx.update(|cx| pomodoro::open(cx).unwrap());
    cx.run_until_parked();
    ui(cx, window, |window, cx| window.click("media-toggle", cx));
    ui(cx, window, |window, _| {
        let text = window
            .find("pomodoro-error")
            .label()
            .unwrap_or_default()
            .to_owned();
        assert!(
            text.contains("操作できる再生中のメディアがありません"),
            "{text}"
        );
    });
    ui(cx, window, |window, cx| window.press("escape", cx));
    ui(cx, window, |window, _| {
        assert!(window.try_find("pomodoro-error").is_none())
    });
}

/// AI や MCP（データベースのツール）がタイマーを動かすと、画面もそのまま追いかける。
#[gpui_kit::test]
fn follows_the_ai_tools(cx: &mut TestAppContext) {
    start(cx);
    open(cx);
    let task = add_task(cx, "レビュー");
    let window = cx.update(|cx| pomodoro::open(cx).unwrap());
    cx.run_until_parked();
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            malakt_ai::tools::execute(
                &mut db.store,
                None,
                "pomodoro",
                &serde_json::json!({"action": "set_settings", "work_minutes": 50}),
            )
            .unwrap();
            malakt_ai::tools::execute(
                &mut db.store,
                None,
                "pomodoro",
                &serde_json::json!({"action": "start", "task_id": task}),
            )
            .unwrap();
            cx.notify();
        })
    });
    cx.run_until_parked();
    let t = timer(cx);
    assert_eq!((t.run(), t.length), (Run::Running, 50 * 60));
    assert_eq!(t.task_id.as_deref(), Some(task.as_str()));
    ui(cx, window, |window, _| {
        assert!(window.try_find_by_label("レビュー").is_some());
        assert_eq!(window.find("pomodoro-toggle").label(), Some("一時停止"));
    });
    // Space で止める
    ui(cx, window, |window, cx| window.press("space", cx));
    assert_eq!(timer(cx).run(), Run::Paused);
    let got = cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            malakt_ai::tools::execute(&mut db.store, None, "get_pomodoro", &serde_json::json!({}))
                .unwrap()
        })
    });
    let got: serde_json::Value = serde_json::from_str(&got).unwrap();
    assert_eq!(got["run"], "paused");
    assert_eq!(got["task"]["title"], "レビュー");
    // 止まったタイマーへの操作
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            db.store.pomodoro(Op::Stop, malakt_core::now_ms()).unwrap();
            cx.notify();
        })
    });
    cx.run_until_parked();
    assert_eq!(timer(cx).run(), Run::Ready);
    let _ = Settings::default();
}

/// 1×1 の PNG。
const PNG: [u8; 69] = [
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0,
    0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 56, 224, 160, 0, 0, 2, 228,
    1, 33, 129, 37, 200, 200, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// OS で再生中の曲を、ジャケット・曲名・アーティストと一緒に出す。曲が変わったら追いかけ、
/// 読めなくなったら何が失敗したかを出し、何も開いていなければ出さない。
#[gpui_kit::test]
fn shows_the_song_playing_now(cx: &mut TestAppContext) {
    let sent = start(cx);
    let now: Arc<Mutex<Result<Option<NowPlaying>, Failure>>> =
        Arc::new(Mutex::new(Ok(Some(NowPlaying {
            title: "Blue in Green".into(),
            artist: "Miles Davis".into(),
            album: "Kind of Blue".into(),
            playing: true,
            artwork: Some(PNG.to_vec()),
        }))));
    let read = now.clone();
    cx.update(|cx| {
        cx.set_global(NowPlayingReader(Arc::new(move || {
            read.lock().unwrap().clone()
        })))
    });
    open(cx);
    let window = cx.update(|cx| pomodoro::open(cx).unwrap());
    cx.run_until_parked();
    let label = |cx: &mut TestAppContext, id: &'static str| {
        let mut text = None;
        ui(cx, window, |window, _| {
            text = window
                .try_find(id)
                .map(|e| e.label().unwrap_or_default().to_owned())
        });
        text
    };
    assert_eq!(
        label(cx, "now-playing-title").as_deref(),
        Some("Blue in Green")
    );
    assert_eq!(
        label(cx, "now-playing-artist").as_deref(),
        Some("Miles Davis · Kind of Blue")
    );
    assert_eq!(
        label(cx, "media-toggle").as_deref(),
        Some("再生中の音楽を一時停止")
    );
    assert!(cx.update(|cx| now_playing::shared(cx)
        .read(cx)
        .track
        .as_ref()
        .unwrap()
        .artwork
        .is_some()));

    // 止めると、送ったあとで読み直して「再生」のボタンになる
    now.lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .as_mut()
        .unwrap()
        .playing = false;
    ui(cx, window, |window, cx| window.click("media-toggle", cx));
    assert_eq!(*sent.lock().unwrap(), [Command::Toggle]);
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(500));
    cx.run_until_parked();
    assert_eq!(
        label(cx, "media-toggle").as_deref(),
        Some("再生中の音楽を再生")
    );

    // 曲が変わると、2 秒ごとに読み直して追いかける
    *now.lock().unwrap() = Ok(Some(NowPlaying {
        title: "So What".into(),
        ..Default::default()
    }));
    cx.executor().advance_clock(now_playing::EVERY);
    cx.run_until_parked();
    assert_eq!(label(cx, "now-playing-title").as_deref(), Some("So What"));
    assert_eq!(label(cx, "now-playing-artist"), None);

    // 読めなくなったら、失敗を出す
    *now.lock().unwrap() = Err(Failure::Os("D-Bus が切れました".into()));
    cx.executor().advance_clock(now_playing::EVERY);
    cx.run_until_parked();
    assert_eq!(label(cx, "now-playing"), None);
    let error = label(cx, "now-playing-error").unwrap();
    assert!(
        error.contains(
            "再生中の曲を読めませんでした: OS がメディアの操作を断りました: D-Bus が切れました"
        ),
        "{error}"
    );

    // 何も開いていなければ、曲も失敗も出さない
    *now.lock().unwrap() = Ok(None);
    cx.executor().advance_clock(now_playing::EVERY);
    cx.run_until_parked();
    assert_eq!(label(cx, "now-playing"), None);
    assert_eq!(label(cx, "now-playing-error"), None);
}
