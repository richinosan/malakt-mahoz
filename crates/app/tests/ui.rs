//! 画面を通したシナリオテスト。実際のウィンドウ（ヘッドレス）を開き、クリックと入力で操作する。

use gpui_kit::component::input::InputState;
use gpui_kit::test::{TestEventExt, TestQueryExt, TestWindowExt};
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext, Bounds, Entity, Modifiers, MouseButton, Pixels, Point,
    Size, TestAppContext, WindowBounds, WindowOptions,
};
use malakt_core::view::Tab;
use malakt_core::{lock, DueRule, Role, Roles, Scope, Status, Store, TaskFilter};
use malakt_mahoz::passkey::{Failure, Passkey};
use malakt_mahoz::{db, CommentMode, MainView, Modal, Nav};

fn start(cx: &mut TestAppContext) {
    let mut store = Store::open_in_memory().unwrap();
    // 日付だけの期限は 0:00 に知らせる。テストを走らせる時刻によらず、当日の通知が出る
    store.set_due_alert_hour(0).unwrap();
    cx.update(|cx| malakt_mahoz::init(store, cx));
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
            cx.new(|cx| {
                let mut view = MainView::new(window, cx);
                // 出入りの動きを待たずに、すぐ最後の位置で確かめる
                view.motion = false;
                view
            })
        })
        .expect("ウィンドウを開く")
    })
}

/// OS の本人確認を、決まった答えを返すものに差し替える。戻り値は、確認を求めた目的の記録。
fn passkey(
    cx: &mut TestAppContext,
    answer: Result<(), Failure>,
) -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = asked.clone();
    cx.update(|cx| {
        cx.set_global(Passkey(std::sync::Arc::new(move |request| {
            log.lock().unwrap().push(request.action);
            answer.clone()
        })))
    });
    asked
}

fn enable_lock(cx: &mut TestAppContext) {
    cx.update(|cx| db::shared(cx).update(cx, |db, _| lock::enable(&mut db.store).unwrap()));
}

/// ウィンドウの大きさ。画面の拡大率に合わせて、同じ見え方になる大きさにする。
fn win(width: f32, height: f32) -> Size<Pixels> {
    let scale = malakt_mahoz::UI_SCALE;
    size(px(width * scale), px(height * scale))
}

fn wide() -> Size<Pixels> {
    win(1280., 800.)
}

/// 画面を操作する。
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

/// データベースを直接読む。
fn store<R>(cx: &mut TestAppContext, f: impl FnOnce(&Store) -> R) -> R {
    cx.update(|cx| f(&db::shared(cx).read(cx).store))
}

fn all_tasks(cx: &mut TestAppContext) -> Vec<malakt_core::Task> {
    store(cx, |s| s.tasks(Scope::All, &TaskFilter::default()).unwrap())
}

/// 何も設定していない状態で開き、そのままタスクを足して進められる。
#[gpui_kit::test]
fn works_right_after_first_launch(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());

    ui(cx, window, |window, cx| {
        window.click("nav-all", cx);
        window.click("quick-add", cx);
        window.input("牛乳を買う", cx);
        window.press("enter", cx);
    });
    let tasks = all_tasks(cx);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].title, "牛乳を買う");
    assert_eq!(tasks[0].workspace_id, None);
    // 入力欄は空に戻り、続けて足せる
    ui(cx, window, |window, cx| {
        assert_eq!(window.find("quick-add").value(), Some(""));
        window.input("卵も買う", cx);
        window.click("quick-add-submit", cx);
    });
    assert_eq!(all_tasks(cx).len(), 2);

    // 未所属の一覧に出て、タイトルを押すと詳細が開く
    ui(cx, window, |window, cx| window.click("nav-unfiled", cx));
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.nav, Nav::Unfiled);
        assert_eq!(view.visible.len(), 2);
        assert_eq!(view.counts, (2, 0));
    });
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    let opened = cx
        .update(|cx| view.read(cx).panel.clone())
        .expect("詳細が開く");

    // 完了にする。ダイアログは出ず、すぐ変わる
    ui(cx, window, |window, cx| window.click("to-completed", cx));
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::None));
    let task = store(cx, |s| s.task(&opened).unwrap());
    assert_eq!(task.status, Status::Completed);
    assert_eq!(task.percent, 100);
    assert!(store(cx, |s| s.comments(&opened).unwrap()).is_empty());
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.modal, Modal::None);
        assert_eq!(view.counts, (1, 1));
        assert_eq!(
            view.visible.len(),
            1,
            "完了したものは未完了のタブから消える"
        );
    });

    // 完了のタブに移る
    ui(cx, window, |window, cx| window.click("tab-closed", cx));
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.query.tab, Tab::Closed);
        assert_eq!(view.visible.len(), 1);
    });
}

/// ワークスペースを入れ子で作り、タスクをあとから入れる。
#[gpui_kit::test]
fn nested_workspaces_and_filing_later(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());

    // 先に、どこにも入れないタスクを作る
    ui(cx, window, |window, cx| {
        window.click("nav-all", cx);
        window.click("quick-add", cx);
        window.input("会場を探す", cx);
        window.press("enter", cx);
    });
    let unfiled = all_tasks(cx).remove(0);

    // ワークスペースを作ると、そこへ移動する
    ui(cx, window, |window, cx| window.click("add-workspace", cx));
    // ダイアログは出ず、「新規ワークスペース」という名前で増えて、その行で名前を書ける
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.modal, Modal::None);
        assert!(view.workspace_naming.as_ref().is_some_and(|n| n.created));
    });
    let parent = store(cx, |s| s.workspaces().unwrap()).remove(0);
    assert_eq!(parent.name, malakt_mahoz::state::NEW_WORKSPACE_NAME);
    // 名前は全部選ばれているので、打つと置き換わる
    ui(cx, window, |window, cx| {
        assert!(window.find("workspace-name-input").visible());
        window.input("冬のイベント", cx);
        window.press("enter", cx);
    });
    let parent = store(cx, |s| s.workspaces().unwrap()).remove(0);
    assert_eq!(parent.name, "冬のイベント");
    cx.update(|cx| assert_eq!(view.read(cx).workspace_naming, None));
    cx.update(|cx| assert_eq!(view.read(cx).nav, Nav::Workspace(parent.id.clone())));

    // その中に子を作る
    ui(cx, window, |window, cx| {
        window.click(("ws-more", 0usize), cx)
    });
    for key in ["down", "down", "down", "enter"] {
        ui(cx, window, |window, cx| window.press(key, cx));
    }
    // ほかの場所を押しても保存される
    ui(cx, window, |window, cx| window.input("会場", cx));
    ui(cx, window, |window, cx| window.click("quick-add", cx));
    cx.update(|cx| assert_eq!(view.read(cx).workspace_naming, None));
    let all = store(cx, |s| s.workspaces().unwrap());
    let child = all
        .iter()
        .find(|w| w.name == "会場")
        .expect("子ができる")
        .clone();
    assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));

    // 子の中で足したタスクは子に入る
    ui(cx, window, |window, cx| {
        window.click("quick-add", cx);
        window.input("下見に行く", cx);
        window.press("enter", cx);
    });
    let inside = store(cx, |s| s.tasks(&child.id, &TaskFilter::default()).unwrap());
    assert_eq!(inside.len(), 1);
    assert_eq!(inside[0].title, "下見に行く");

    // 親を開くと、子は見出しの行で出る。見出しを開くと、子のタスクも見える
    cx.update(|cx| view.update(cx, |v, cx| v.go(Nav::Workspace(parent.id.clone()), cx)));
    cx.update(|cx| {
        assert_eq!(
            (view.read(cx).rows.len(), view.read(cx).visible.len()),
            (1, 0)
        )
    });
    ui(cx, window, |window, cx| {
        window.click(("folder-toggle", 0usize), cx)
    });
    cx.update(|cx| assert_eq!(view.read(cx).visible.len(), 1));

    // 未所属のタスクを、あとから子のワークスペースに入れる
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.go(Nav::Unfiled, cx);
            v.toggle_selected(&unfiled.id, cx);
            v.modal = Modal::MoveTasks(v.selected_ids());
            v.move_tasks_to(Some(child.id.clone()), cx);
        })
    });
    cx.run_until_parked();
    assert_eq!(
        store(cx, |s| s.task(&unfiled.id).unwrap())
            .workspace_id
            .as_deref(),
        Some(child.id.as_str())
    );
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.visible.is_empty(), "未所属からは消える");
        assert!(view.selected.is_empty());
        assert_eq!(view.open_counts.get(&child.id), Some(&2));
    });

    // 何も打たずに Enter を押すと、「新規ワークスペース」のまま残る
    ui(cx, window, |window, cx| window.click("add-workspace", cx));
    ui(cx, window, |window, cx| window.press("enter", cx));
    let all = store(cx, |s| s.workspaces().unwrap());
    assert_eq!(all.len(), 3);
    let added = all
        .iter()
        .find(|w| w.name == malakt_mahoz::state::NEW_WORKSPACE_NAME)
        .expect("新規ワークスペース")
        .id
        .clone();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.workspace_naming, None);
        assert_eq!(view.nav, Nav::Workspace(added.clone()));
    });

    // 行のメニューの「名前を変更」も、その行で書く。空にして Enter を押すと、前の名前のまま
    let rename = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |v, cx| v.rename_workspace(added.clone(), window, cx))
        })
        .unwrap();
        cx.run_until_parked();
    };
    rename(cx);
    cx.update(|cx| {
        let naming = view
            .read(cx)
            .workspace_naming
            .clone()
            .expect("名前を書いている");
        assert!(!naming.created);
    });
    ui(cx, window, |window, cx| {
        window.press("backspace", cx);
        window.press("enter", cx);
    });
    assert_eq!(
        store(cx, |s| s.workspace(&added).unwrap()).name,
        malakt_mahoz::state::NEW_WORKSPACE_NAME
    );
    // Esc は書きかけを捨てる
    rename(cx);
    ui(cx, window, |window, cx| window.input("捨てる名前", cx));
    ui(cx, window, |window, cx| window.press("escape", cx));
    cx.update(|cx| assert_eq!(view.read(cx).workspace_naming, None));
    assert_eq!(
        store(cx, |s| s.workspace(&added).unwrap()).name,
        malakt_mahoz::state::NEW_WORKSPACE_NAME
    );
    // 書いて Enter で変わる
    rename(cx);
    ui(cx, window, |window, cx| {
        window.input("打ち上げ", cx);
        window.press("enter", cx);
    });
    assert_eq!(store(cx, |s| s.workspace(&added).unwrap()).name, "打ち上げ");
}

/// ウィンドウを 2 枚開くと、片方の変更がもう片方にもすぐ出る。
#[gpui_kit::test]
fn windows_share_changes(cx: &mut TestAppContext) {
    start(cx);
    let (first, first_view) = open(cx, wide());
    let (second, second_view) = open(cx, win(900., 700.));
    cx.update(|cx| {
        first_view.update(cx, |v, cx| v.go(Nav::All, cx));
        second_view.update(cx, |v, cx| v.go(Nav::All, cx));
    });

    ui(cx, first, |window, cx| {
        window.click("quick-add", cx);
        window.input("共有されるタスク", cx);
        window.press("enter", cx);
    });
    ui(cx, second, |_, _| {});
    cx.update(|cx| {
        assert_eq!(first_view.read(cx).visible.len(), 1);
        assert_eq!(second_view.read(cx).visible.len(), 1);
        assert_eq!(second_view.read(cx).tasks[0].title, "共有されるタスク");
    });

    // 2 枚目で詳細を開いているタスクを 1 枚目で消すと、2 枚目の詳細は閉じる
    let id = all_tasks(cx).remove(0).id;
    ui(cx, second, |window, cx| window.click(("title", 0usize), cx));
    cx.update(|cx| assert_eq!(second_view.read(cx).panel.as_deref(), Some(id.as_str())));
    cx.update(|cx| {
        first_view.update(cx, |v, cx| {
            v.modal = Modal::DeleteTasks(vec![id.clone()]);
            v.confirm_delete_tasks(cx);
        })
    });
    cx.run_until_parked();
    ui(cx, second, |_, _| {});
    cx.update(|cx| {
        assert_eq!(second_view.read(cx).panel, None);
        assert!(second_view.read(cx).visible.is_empty());
    });
}

/// ロックを掛けると全部のウィンドウが隠れ、パスキーで確認できたときだけ開く。
#[gpui_kit::test]
fn lock_hides_every_window(cx: &mut TestAppContext) {
    start(cx);
    let (first, first_view) = open(cx, wide());
    let (_second, second_view) = open(cx, wide());

    enable_lock(cx);
    // ロックが無効なら「今すぐロック」は何もしないが、有効なら全部を隠す
    cx.update(db::lock_now);
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(!first_view.read(cx).unlocked(cx));
        assert!(!second_view.read(cx).unlocked(cx));
    });

    let asked = passkey(cx, Err(Failure::Denied));
    ui(cx, first, |window, cx| window.click("unlock", cx));
    cx.update(|cx| {
        assert!(!first_view.read(cx).unlocked(cx));
        assert!(!first_view.read(cx).verifying(cx));
        assert_eq!(
            first_view.read(cx).error.as_deref(),
            Some("パスキーで確認できませんでした")
        );
    });
    assert_eq!(*asked.lock().unwrap(), ["ロックを解除"]);

    passkey(cx, Ok(()));
    ui(cx, first, |window, cx| window.click("unlock", cx));
    cx.update(|cx| {
        assert!(first_view.read(cx).unlocked(cx));
        assert!(
            second_view.read(cx).unlocked(cx),
            "解除はどのウィンドウにも効く"
        );
        assert!(first_view.read(cx).error.is_none());
    });
}

/// ロックの有効・無効は、パスキーで確認できたときだけ変わる。
#[gpui_kit::test]
fn lock_changes_only_after_the_passkey(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    let enabled = |cx: &mut TestAppContext| cx.update(|cx| view.read(cx).lock_enabled(cx));

    // パスキーのない端末では、有効にならない（開けなくなるのを防ぐ）
    passkey(cx, Err(Failure::Unavailable));
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.enable_lock(window, cx))
    });
    assert!(!enabled(cx));
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).error.as_deref(),
            Some("この端末にはパスキーがありません")
        )
    });

    let asked = passkey(cx, Ok(()));
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.enable_lock(window, cx))
    });
    assert!(enabled(cx));
    cx.update(|cx| assert!(view.read(cx).unlocked(cx), "有効にしただけでは隠れない"));

    passkey(cx, Err(Failure::Denied));
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.disable_lock(window, cx))
    });
    assert!(enabled(cx));

    let asked_again = passkey(cx, Ok(()));
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.disable_lock(window, cx))
    });
    assert!(!enabled(cx));
    assert_eq!(*asked.lock().unwrap(), ["ロックを有効にする"]);
    assert_eq!(*asked_again.lock().unwrap(), ["ロックを無効にする"]);
}

/// 確認の答えを待っている間は、もう 1 つ確認を出さない。
#[gpui_kit::test]
fn one_passkey_prompt_at_a_time(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    enable_lock(cx);
    cx.update(db::lock_now);
    let asked = passkey(cx, Ok(()));
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |v, cx| {
            v.request_unlock(window, cx);
            v.request_unlock(window, cx);
            assert!(v.verifying(cx));
        })
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(asked.lock().unwrap().len(), 1);
    cx.update(|cx| assert!(view.read(cx).unlocked(cx)));
}

/// ロックが無効なら「今すぐロック」をしても隠れない。
#[gpui_kit::test]
fn lock_now_while_disabled_does_nothing(cx: &mut TestAppContext) {
    start(cx);
    let (_window, view) = open(cx, wide());
    cx.update(db::lock_now);
    cx.run_until_parked();
    cx.update(|cx| assert!(view.read(cx).unlocked(cx)));
}

/// 上のバーを右クリックすると、ウィンドウのメニューが出る。「ロックする」は、ロックが無効なら
/// ロックの設定を開き、有効ならロックする。ロック中は「ロックする」を押せない。
#[gpui_kit::test]
fn header_menu_locks_the_app(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    // バーの、ボタンのない所（今の画面の名前のあたり）を右クリックする
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
    ui(cx, window, |window, _| {
        for label in ["ロックする", "新しいウィンドウ", "設定", "全画面", "閉じる"]
        {
            assert!(window.try_find_by_label(label).is_some(), "{label} がない");
        }
    });
    // ロックが無効なら、ロックを有効にする設定を開く
    pick(cx, window, "ロックする");
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.unlocked(cx));
        assert_eq!(view.modal, Modal::Appearance);
        assert_eq!(
            view.appearance_section,
            malakt_mahoz::state::CustomizeSection::Lock
        );
    });
    ui(cx, window, |window, cx| window.press("escape", cx));

    enable_lock(cx);
    right_click_header(cx);
    pick(cx, window, "ロックする");
    cx.update(|cx| assert!(!view.read(cx).unlocked(cx)));

    // ロック中のメニューは、ウィンドウの操作だけ
    right_click_header(cx);
    ui(cx, window, |window, _| {
        assert!(window.try_find_by_label("閉じる").is_some());
        assert!(window.try_find_by_label("設定").is_none());
    });
    pick(cx, window, "ロックする");
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::None));
}

/// ロックのキー（Ctrl+Shift+L / ⇧⌘L）は、ロックが無効ならロックの設定を開き、有効ならロックする。
/// Ctrl+L だけではロックしない（シェルやターミナルの「画面を消す」）。
#[gpui_kit::test]
fn lock_key_locks_or_opens_the_lock_setting(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    enable_lock(cx);
    ui(cx, window, |window, cx| window.press("ctrl-l", cx));
    cx.update(|cx| assert!(view.read(cx).unlocked(cx)));
    ui(cx, window, |window, cx| {
        window.press("secondary-shift-l", cx)
    });
    cx.update(|cx| assert!(!view.read(cx).unlocked(cx)));
}

/// ロックが無効なときにロックのキーを押すと、ロックの設定を開く。
#[gpui_kit::test]
fn lock_key_without_lock_opens_the_setting(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| {
        window.press("secondary-shift-l", cx)
    });
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.unlocked(cx));
        assert_eq!(view.modal, Modal::Appearance);
        assert_eq!(
            view.appearance_section,
            malakt_mahoz::state::CustomizeSection::Lock
        );
    });
}

/// 書き出したものを、空の別のアプリに取り込める。
#[gpui_kit::test]
fn export_then_import(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("書き出し.zip");
    start(cx);
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| {
        window.click("nav-all", cx);
        window.click("quick-add", cx);
        window.input("持ち出すタスク", cx);
        window.press("enter", cx);
    });
    cx.update(|cx| view.update(cx, |v, cx| v.export_to(&path, cx)));
    assert!(path.exists());
    cx.update(|cx| assert!(view.read(cx).notice.is_some()));

    // 中身を消してから取り込むと、元に戻る
    let id = all_tasks(cx).remove(0).id;
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.modal = Modal::DeleteTasks(vec![id.clone()]);
            v.confirm_delete_tasks(cx);
        })
    });
    assert!(all_tasks(cx).is_empty());
    cx.update(|cx| view.update(cx, |v, cx| v.import_from(&path, cx)));
    cx.run_until_parked();
    let tasks = all_tasks(cx);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, id);

    // 壊れたファイルはエラーを出して、何も変えない
    let broken = dir.path().join("broken.zip");
    std::fs::write(&broken, b"not a zip").unwrap();
    cx.update(|cx| view.update(cx, |v, cx| v.import_from(&broken, cx)));
    cx.update(|cx| assert!(view.read(cx).error.is_some()));
    assert_eq!(all_tasks(cx).len(), 1);
}

/// 狭いウィンドウでも、広いウィンドウでも、どの画面も描ける。
#[gpui_kit::test]
fn every_screen_renders_at_any_size(cx: &mut TestAppContext) {
    start(cx);
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let s = &mut db.store;
            let w = s.create_workspace(None, "W").unwrap();
            s.create_workspace(Some(&w.id), "子").unwrap();
            let me = s.create_person("自分", true).unwrap();
            let mut d = malakt_core::TaskDraft {
                title: "期限つき".into(),
                due: Some(malakt_core::view::today()),
                assignee_ids: vec![me.id],
                categories: vec!["設営".into()],
                priority: 1,
                ..Default::default()
            };
            let t = s.create_task(Some(&w.id), &d).unwrap();
            d.title = "サブ".into();
            d.parent_id = Some(t.id.clone());
            s.create_task(Some(&w.id), &d).unwrap();
            let c = s.add_comment(&t.id, None, "メモ").unwrap();
            s.set_comment_pinned(&c.id, true).unwrap();
            s.set_task_status(&t.id, Status::InProcess).unwrap();
        })
    });
    for window_size in [win(380., 520.), win(900., 600.), win(1600., 1000.)] {
        let (window, view) = open(cx, window_size);
        let (ws, task) = store(cx, |s| {
            (
                s.workspaces().unwrap().remove(0).id,
                s.tasks(Scope::All, &TaskFilter::default())
                    .unwrap()
                    .remove(0)
                    .id,
            )
        });
        ui(cx, window, |_, _| {});
        for nav in [
            Nav::Home,
            Nav::All,
            Nav::Unfiled,
            Nav::Workspace(ws.clone()),
        ] {
            cx.update(|cx| {
                view.update(cx, |v, cx| {
                    v.sidebar_open = false;
                    v.filters_open = true;
                    v.go(nav.clone(), cx)
                })
            });
            ui(cx, window, |_, _| {});
        }
        // 選択中の一括操作のバーと、詳細パネル
        cx.update(|cx| view.update(cx, |v, cx| v.toggle_select_all(cx)));
        ui(cx, window, |_, _| {});
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |v, cx| v.open_panel(&task, window, cx))
        })
        .unwrap();
        ui(cx, window, |_, _| {});
        let modals = [
            Modal::DeleteWorkspace(ws.clone()),
            Modal::DeleteTasks(vec![task.clone()]),
            Modal::MoveTasks(vec![task.clone()]),
            Modal::History,
            Modal::Status(task.clone(), Status::Completed),
            Modal::WorkspaceSettings(ws.clone()),
            Modal::LinkTask(task.clone()),
            Modal::BulkDue,
            Modal::BulkAssignees,
            Modal::BulkCategories,
            Modal::Notices,
            Modal::Ticket(task.clone()),
        ];
        for modal in modals {
            cx.update(|cx| {
                view.update(cx, |v, cx| {
                    v.modal = modal;
                    cx.notify();
                })
            });
            ui(cx, window, |_, _| {});
        }
        // Esc は手前のものから順に閉じる
        cx.update(|cx| {
            view.update(cx, |v, cx| {
                v.dismiss(cx);
                assert_eq!(v.modal, Modal::None);
                assert!(v.panel.is_some());
                v.dismiss(cx);
                assert!(v.panel.is_none());
            })
        });
        ui(cx, window, |_, _| {});
    }
}

/// 狭いウィンドウでは、メニューを開いて行き先を選ぶと、メニューが閉じて中身が出る。
#[gpui_kit::test]
fn narrow_window_menu_opens_and_closes(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, win(520., 800.));

    // 最初はメニューが閉じている。メニューから一覧へ移ると、すぐタスクを足せる
    assert!(!cx.update(|cx| view.read(cx).sidebar_open));
    ui(cx, window, |window, cx| window.click("toggle-sidebar", cx));
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    ui(cx, window, |window, cx| {
        window.click("quick-add", cx);
        window.input("牛乳を買う", cx);
        window.press("enter", cx);
    });
    assert_eq!(all_tasks(cx).len(), 1);

    ui(cx, window, |window, cx| window.click("toggle-sidebar", cx));
    assert!(cx.update(|cx| view.read(cx).sidebar_open));

    // メニューを開いていても「閉じる」は押せる
    ui(cx, window, |window, cx| window.click("toggle-sidebar", cx));
    assert!(!cx.update(|cx| view.read(cx).sidebar_open));

    ui(cx, window, |window, cx| window.click("toggle-sidebar", cx));
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(!view.sidebar_open);
        assert_eq!(view.nav, Nav::All);
    });
    // 中身が出ているので、一覧のタスクを開ける
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    assert!(cx.update(|cx| view.read(cx).panel.is_some()));
}

/// 詳細で担当者を選び、役（RACI）を付け、1 人ずつ完了にしていく。
#[gpui_kit::test]
fn assignee_roles_and_per_person_completion(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    let (me, sato) = cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.write(cx, |s| {
                Ok((
                    s.create_person("自分", true)?.id,
                    s.create_person("佐藤", false)?.id,
                ))
            })
            .unwrap()
        })
    });
    ui(cx, window, |window, cx| {
        window.click("nav-all", cx);
        window.click("quick-add", cx);
        window.input("会場を決める", cx);
        window.press("enter", cx);
    });
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    let id = all_tasks(cx).remove(0).id;
    let load = |cx: &mut TestAppContext| store(cx, |s| s.task(&id).unwrap());

    // 追加行から足したタスクの担当は自分。担当を押すと選ぶところが開く。
    // もう 1 人を足して 2 人を担当にすると、どちらも R から始まる
    assert_eq!(load(cx).assignee_ids, std::slice::from_ref(&me));
    ui(cx, window, |window, cx| window.click("p-assignees", cx));
    ui(cx, window, |window, cx| window.click(("p-as", 1usize), cx));
    let t = load(cx);
    assert_eq!(t.assignee_ids.len(), 2);
    assert!(t.assignee_roles.iter().all(|r| *r == Roles::R));
    let row_of = |t: &malakt_core::Task, person: &str| {
        t.assignee_ids.iter().position(|p| p == person).unwrap()
    };
    let (mine, theirs) = (row_of(&t, &me), row_of(&t, &sato));
    let role_chip = |row: usize, role: Role| ("p-role", row * 4 + role as usize);

    // 自分に A を足し、佐藤を C だけにする
    ui(cx, window, |window, cx| {
        window.click(role_chip(mine, Role::A), cx)
    });
    ui(cx, window, |window, cx| {
        window.click(role_chip(theirs, Role::C), cx)
    });
    ui(cx, window, |window, cx| {
        window.click(role_chip(theirs, Role::R), cx)
    });
    let t = load(cx);
    assert_eq!(t.roles_of(&me).unwrap().to_string(), "R,A");
    assert_eq!(t.roles_of(&sato).unwrap().to_string(), "C");

    // 最後の 1 つの役は外せず、理由が出る
    ui(cx, window, |window, cx| {
        window.click(role_chip(theirs, Role::C), cx)
    });
    assert_eq!(load(cx).roles_of(&sato).unwrap().to_string(), "C");
    cx.update(|cx| assert!(view.read(cx).error.is_some()));

    // A を佐藤に移すと、自分の A は外れる
    ui(cx, window, |window, cx| {
        window.click(role_chip(theirs, Role::A), cx)
    });
    let t = load(cx);
    assert_eq!(t.roles_of(&me).unwrap().to_string(), "R");
    assert_eq!(t.roles_of(&sato).unwrap().to_string(), "A,C");
    cx.update(|cx| assert!(view.read(cx).error.is_none()));

    // 「自分の役」で絞り込める
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.query.my_role = Some(Role::A);
            v.reload(cx);
            assert!(v.visible.is_empty());
            v.query.my_role = Some(Role::R);
            v.reload(cx);
            assert_eq!(v.visible.len(), 1);
        })
    });

    // 1 人ずつ完了にする。全員そろうとタスクが完了になる
    ui(cx, window, |window, cx| window.click(("p-done", mine), cx));
    let t = load(cx);
    assert_eq!((t.status, t.done_count()), (Status::NeedsAction, (1, 2)));
    ui(cx, window, |window, cx| {
        window.click(("p-done", theirs), cx)
    });
    let t = load(cx);
    assert_eq!((t.status, t.done_count()), (Status::Completed, (2, 2)));

    // 1 人の分を取り消すと、タスクは進行中に戻る
    ui(cx, window, |window, cx| {
        window.click(("p-done", theirs), cx)
    });
    let t = load(cx);
    assert_eq!((t.status, t.done_count()), (Status::InProcess, (1, 2)));
    assert!(t.is_done_by(&me));
}

/// コメント欄: 書く、書き直す、メンション、レビューのスレッド、完了報告。
#[gpui_kit::test]
fn comments_edit_mention_review_and_report(cx: &mut TestAppContext) {
    start(cx);
    // コメント欄は詳細の下のほうにあるので、縦に長いウィンドウで開く
    let (window, view) = open(cx, win(1280., 2400.));
    let sato = cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.write(cx, |s| {
                s.create_task(
                    None,
                    &malakt_core::TaskDraft {
                        title: "記事を書く".into(),
                        description: "導入を書く。**本文**を書く。".into(),
                        ..Default::default()
                    },
                )?;
                Ok(s.create_person("佐藤 花子", false)?.id)
            })
            .unwrap()
        })
    });
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    let id = all_tasks(cx).remove(0).id;
    let comments = |cx: &mut TestAppContext| store(cx, |s| s.comments(&id).unwrap());
    let write = |cx: &mut TestAppContext, text: &str| {
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |v, cx| {
                v.inputs
                    .comment
                    .update(cx, |s, cx| s.set_value(text.to_owned(), window, cx))
            })
        })
        .unwrap();
        cx.run_until_parked();
    };

    // `@` を打つと候補が出て、選ぶと名前が入る
    write(cx, "確認お願いします @佐");
    ui(cx, window, |window, cx| {
        window.click(("mention", 0usize), cx)
    });
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).inputs.comment.read(cx).value().as_ref(),
            "確認お願いします @佐藤花子 "
        );
    });
    ui(cx, window, |window, cx| window.click("comment-submit", cx));
    let list = comments(cx);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].mentions, std::slice::from_ref(&sato));
    assert!(!list[0].review);
    cx.update(|cx| assert_eq!(view.read(cx).inputs.comment.read(cx).value().as_ref(), ""));

    // 書き直す
    ui(cx, window, |window, cx| {
        window.click(("edit-comment", 0usize), cx)
    });
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.comment_mode, CommentMode::Edit(list[0].id.clone()));
        assert_eq!(
            view.inputs.comment.read(cx).value().as_ref(),
            "確認お願いします @佐藤花子"
        );
    });
    write(cx, "確認は不要になりました");
    ui(cx, window, |window, cx| window.click("comment-submit", cx));
    let list = comments(cx);
    assert_eq!(list[0].body, "確認は不要になりました");
    assert!(list[0].mentions.is_empty());
    cx.update(|cx| assert_eq!(view.read(cx).comment_mode, CommentMode::New));

    // 書き直しをやめると、欄は空に戻る
    ui(cx, window, |window, cx| {
        window.click(("edit-comment", 0usize), cx)
    });
    ui(cx, window, |window, cx| window.click("comment-cancel", cx));
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.comment_mode, CommentMode::New);
        assert_eq!(view.inputs.comment.read(cx).value().as_ref(), "");
    });

    // 説明の選択範囲に付けるレビューコメント
    let anchor = malakt_mahoz::anchor_at("導入を書く。**本文**を書く。", 18..28);
    assert_eq!(anchor.exact, "**本文**");
    assert_eq!(anchor.prefix, "導入を書く。");
    assert_eq!(anchor.suffix, "を書く。");
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |v, cx| v.set_pending_anchor(anchor.clone(), window, cx))
    })
    .unwrap();
    write(cx, "ここをもっと詳しく");
    ui(cx, window, |window, cx| window.click("comment-submit", cx));
    let list = comments(cx);
    let root = list
        .iter()
        .find(|c| c.review)
        .expect("レビューコメント")
        .clone();
    assert_eq!(root.anchor.as_ref(), Some(&anchor));
    let root_ix = list.iter().position(|c| c.id == root.id).unwrap();
    cx.update(|cx| assert!(view.read(cx).pending_anchor.is_none()));

    // 返信して、解決済みにし、再開する
    ui(cx, window, |window, cx| {
        window.click(("thread-reply", root_ix), cx)
    });
    write(cx, "足しました");
    ui(cx, window, |window, cx| window.click("comment-submit", cx));
    let list = comments(cx);
    let reply = list.iter().find(|c| c.reply_to.is_some()).expect("返信");
    assert_eq!(reply.reply_to.as_deref(), Some(root.id.as_str()));
    ui(cx, window, |window, cx| {
        window.click(("thread-resolve", root_ix), cx)
    });
    assert!(comments(cx)[root_ix].resolved_at.is_some());
    // 解決済みはたためて、開ける
    ui(cx, window, |window, cx| {
        window.click(("thread-toggle", root_ix), cx)
    });
    cx.update(|cx| assert!(view.read(cx).expanded_threads.contains(&root.id)));
    ui(cx, window, |window, cx| {
        window.click(("thread-resolve", root_ix), cx)
    });
    assert!(comments(cx)[root_ix].resolved_at.is_none());

    // 文書全体へのレビューコメント
    write(cx, "全体に見出しがほしい");
    ui(cx, window, |window, cx| window.click("comment-review", cx));
    let list = comments(cx);
    assert_eq!(list.iter().filter(|c| c.is_thread_root()).count(), 2);
    assert!(list.iter().any(|c| c.review && c.anchor.is_none()));

    // 完了にしてコメントすると、完了報告として残る
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |v, cx| {
            v.ask_status(&id, window, cx);
            v.inputs
                .status_comment
                .update(cx, |s, cx| s.set_value("公開しました", window, cx));
            v.submit_status(window, cx);
        })
    })
    .unwrap();
    cx.run_until_parked();
    let list = comments(cx);
    let report = list
        .iter()
        .find(|c| c.is_completion_report())
        .expect("完了報告");
    assert_eq!(report.body, "公開しました");
    assert_eq!(
        store(cx, |s| s.task(&id).unwrap()).status,
        Status::Completed
    );
    ui(cx, window, |_, _| {});
}

/// 説明の本文を押すと編集になり、入力欄は「書く」「プレビュー」で切り替えられる。
#[gpui_kit::test]
fn markdown_preview_and_click_to_edit(cx: &mut TestAppContext) {
    use malakt_mahoz::state::{Compose, Editing};
    start(cx);
    let (window, view) = open(cx, win(1280., 2400.));
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.write(cx, |s| {
                s.create_task(
                    None,
                    &malakt_core::TaskDraft {
                        title: "記事を書く".into(),
                        description: "1 行目\n2 行目".into(),
                        ..Default::default()
                    },
                )
            })
            .unwrap()
        })
    });
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    let id = all_tasks(cx).remove(0).id;

    // 説明の本文を押すと、元の文のまま編集になる
    ui(cx, window, |window, cx| {
        window.click("detail-description", cx)
    });
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.editing, Some(Editing::Description));
        assert_eq!(
            v.inputs.description.read(cx).value().as_ref(),
            "1 行目\n2 行目"
        );
    });

    // プレビューにすると入力欄が消えて本文が描かれ、「書く」で入力欄に戻る
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |v, cx| {
            v.inputs
                .description
                .update(cx, |s, cx| s.set_value("**太字**\n- 項目", window, cx))
        })
    })
    .unwrap();
    ui(cx, window, |window, cx| window.click("desc-preview", cx));
    cx.update(|cx| assert!(view.read(cx).previewing.contains(&Compose::Description)));
    ui(cx, window, |window, _| {
        assert!(window.find("desc-preview-body").visible());
    });
    ui(cx, window, |window, cx| window.click("desc-write", cx));
    cx.update(|cx| assert!(!view.read(cx).previewing.contains(&Compose::Description)));
    ui(cx, window, |window, _| {
        assert!(window.try_find("desc-preview-body").is_none());
    });
    // プレビューのままでも保存でき、次に編集するときは「書く」から始まる
    ui(cx, window, |window, cx| window.click("desc-preview", cx));
    ui(cx, window, |window, cx| window.click("desc-save", cx));
    assert_eq!(
        store(cx, |s| s.task(&id).unwrap()).description,
        "**太字**\n- 項目"
    );
    cx.update(|cx| assert_eq!(view.read(cx).editing, None));
    ui(cx, window, |window, cx| window.click("desc-edit", cx));
    cx.update(|cx| assert!(!view.read(cx).previewing.contains(&Compose::Description)));
    ui(cx, window, |window, cx| window.click("desc-cancel", cx));

    // コメント欄: 空のプレビュー、書いてからのプレビュー、送ると「書く」に戻る
    ui(cx, window, |window, cx| window.click("comment-preview", cx));
    ui(cx, window, |window, _| {
        assert!(window.find("comment-preview-body").visible());
    });
    ui(cx, window, |window, cx| window.click("comment-write", cx));
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |v, cx| {
            v.inputs
                .comment
                .update(cx, |s, cx| s.set_value("見ました\n直します", window, cx))
        })
    })
    .unwrap();
    ui(cx, window, |window, cx| window.click("comment-preview", cx));
    ui(cx, window, |window, cx| window.click("comment-submit", cx));
    let comments = store(cx, |s| s.comments(&id).unwrap());
    assert_eq!(comments[0].body, "見ました\n直します");
    cx.update(|cx| assert!(!view.read(cx).previewing.contains(&Compose::Comment)));
}

/// ファイルを添付し、保存し、消せる。大きすぎるファイルは断る。
#[gpui_kit::test]
fn attachments_add_save_and_delete(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, win(1280., 2400.));
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.write(cx, |s| {
                s.create_task(
                    None,
                    &malakt_core::TaskDraft {
                        title: "見積もりを取る".into(),
                        ..Default::default()
                    },
                )
            })
            .unwrap()
        })
    });
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    let id = all_tasks(cx).remove(0).id;

    let dir = tempfile::tempdir().unwrap();
    let estimate = dir.path().join("見積もり.txt");
    std::fs::write(&estimate, "合計 12,000 円\n".repeat(200)).unwrap();
    let copy = dir.path().join("控え.txt");
    std::fs::copy(&estimate, &copy).unwrap();
    let huge = dir.path().join("huge.bin");
    std::fs::File::create(&huge)
        .unwrap()
        .set_len(malakt_core::MAX_ATTACHMENT_BYTES + 1)
        .unwrap();
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.attach_paths(&[estimate.clone(), copy.clone(), huge.clone()], cx)
        })
    });
    cx.run_until_parked();
    // 大きすぎるものだけ断り、ほかは入る
    let attachments = store(cx, |s| s.attachments(&id).unwrap());
    assert_eq!(
        attachments
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>(),
        ["見積もり.txt", "控え.txt"]
    );
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.error.as_ref().is_some_and(|e| e.contains("huge.bin")));
        assert_eq!(view.detail.as_ref().unwrap().attachments.len(), 2);
        // 同じ中身は 1 つ分、しかも圧縮されている
        let used = view.usage(cx).attachment_bytes;
        assert!(used > 0 && used < attachments[0].size / 2, "{used}");
    });

    // 保存すると、元と同じ中身が出てくる
    let saved = dir.path().join("saved.txt");
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.save_attachment_to(&attachments[0].id, &saved, cx)
        })
    });
    assert_eq!(
        std::fs::read(&saved).unwrap(),
        std::fs::read(&estimate).unwrap()
    );

    // 開くための写しは一時フォルダーに出し、まとめて消せる
    let opened = cx.update(|cx| view.read(cx).opened_copy(&attachments[0].id, cx).unwrap());
    assert_eq!(opened.file_name().unwrap(), "見積もり.txt");
    assert_eq!(
        std::fs::read(&opened).unwrap(),
        std::fs::read(&estimate).unwrap()
    );
    cx.update(|cx| db::shared(cx).read(cx).clear_opened());
    assert!(!opened.exists());

    ui(cx, window, |window, cx| {
        window.click(("att-del", 0usize), cx)
    });
    ui(cx, window, |window, cx| {
        window.click(("att-del", 0usize), cx)
    });
    assert!(store(cx, |s| s.attachments(&id).unwrap()).is_empty());
    cx.update(|cx| assert_eq!(view.read(cx).usage(cx).attachment_bytes, 0));
}

/// ワークスペースの設定で決めた既定値が、そこで足すタスクに入る。
#[gpui_kit::test]
fn workspace_defaults_fill_new_tasks(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, win(1280., 2400.));
    let (ws, sato) = cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.write(cx, |s| {
                Ok((
                    s.create_workspace(None, "冬のイベント")?.id,
                    s.create_person("佐藤 花子", false)?.id,
                ))
            })
            .unwrap()
        })
    });
    let set = |cx: &mut TestAppContext, pick: fn(&MainView) -> &Entity<InputState>, text: &str| {
        cx.update_window(window, |_, window, cx| {
            let input = pick(view.read(cx)).clone();
            input.update(cx, |s, cx| s.set_value(text.to_owned(), window, cx));
        })
        .unwrap();
        cx.run_until_parked();
    };
    cx.update(|cx| view.update(cx, |v, cx| v.go(Nav::Workspace(ws.clone()), cx)));
    cx.run_until_parked();

    // 正しくない日付は保存せず、ダイアログに理由を出す
    ui(cx, window, |window, cx| window.click("ws-settings", cx));
    set(cx, |v| &v.inputs.ws_date, "2026-02-30");
    ui(cx, window, |window, cx| {
        window.click("ws-settings-save", cx)
    });
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.modal, Modal::WorkspaceSettings(ws.clone()));
        assert!(view.error.as_ref().is_some_and(|e| e.contains("基準日")));
    });

    set(cx, |v| &v.inputs.ws_date, "2027-01-03");
    ui(cx, window, |window, cx| {
        window.click("def-due-relative", cx)
    });
    // 全角で入れても読む
    set(cx, |v| &v.inputs.def_days, "－７");
    set(cx, |v| &v.inputs.def_time, "18:00");
    ui(cx, window, |window, cx| {
        window.click(("def-as", 1usize), cx)
    });
    ui(cx, window, |window, cx| {
        window.click(("def-prio", 1usize), cx)
    });
    set(cx, |v| &v.inputs.def_categories, "準備、会場, 準備");
    ui(cx, window, |window, cx| {
        window.click("ws-settings-save", cx)
    });
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.modal, Modal::None);
        assert!(view.error.is_none(), "{:?}", view.error);
    });
    let saved = store(cx, |s| s.task_defaults(&ws).unwrap());
    assert_eq!(saved.due, DueRule::Relative { days: -7 });
    assert_eq!(saved.categories, ["準備", "会場"]);
    assert_eq!(
        store(cx, |s| s.workspace(&ws).unwrap()).date.as_deref(),
        Some("2027-01-03")
    );

    set(cx, |v| &v.inputs.quick_add, "会場を下見する");
    ui(cx, window, |window, cx| {
        window.click("quick-add-submit", cx)
    });
    let task = all_tasks(cx).remove(0);
    assert_eq!(task.due.as_deref(), Some("2026-12-27 18:00"));
    assert_eq!(task.assignee_ids, std::slice::from_ref(&sato));
    assert_eq!(task.priority, 1);
    assert_eq!(task.categories, ["会場", "準備"]);

    // 開き直すと、保存した値が入っている
    ui(cx, window, |window, cx| window.click("ws-settings", cx));
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.inputs.def_days.read(cx).value().as_ref(), "-7");
        assert_eq!(
            view.defaults_draft.assignee_ids,
            std::slice::from_ref(&sato)
        );
    });

    // ワークスペースの外で足すタスクには、既定値は入らない。担当は自分
    ui(cx, window, |window, cx| window.click("modal-cancel", cx));
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    set(cx, |v| &v.inputs.quick_add, "メモ");
    ui(cx, window, |window, cx| {
        window.click("quick-add-submit", cx)
    });
    let memo = all_tasks(cx)
        .into_iter()
        .find(|t| t.title == "メモ")
        .unwrap();
    let me = store(cx, |s| s.people().unwrap())
        .into_iter()
        .find(|p| p.is_self)
        .unwrap()
        .id;
    assert!(memo.due.is_none());
    assert_eq!(memo.assignee_ids, [me]);
}

/// 1 つのタスクをほかのワークスペースにも表示し、片方から外せる。
#[gpui_kit::test]
fn task_shown_in_two_workspaces(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, win(1280., 2400.));
    let (event, press) = cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.write(cx, |s| {
                let event = s.create_workspace(None, "冬のイベント")?.id;
                let press = s.create_workspace(None, "広報")?.id;
                s.add_task_in(Some(&event), "告知を出す")?;
                Ok((event, press))
            })
            .unwrap()
        })
    });
    let go = |cx: &mut TestAppContext, ws: &str| {
        cx.update(|cx| view.update(cx, |v, cx| v.go(Nav::Workspace(ws.to_owned()), cx)));
        cx.run_until_parked();
    };
    let shown = |cx: &mut TestAppContext| cx.update(|cx| view.read(cx).tasks.len());
    let id = all_tasks(cx).remove(0).id;

    go(cx, &event);
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    ui(cx, window, |window, cx| window.click("link-add", cx));
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::LinkTask(id.clone())));
    // 候補は「広報」だけ（いまの場所は出ない）
    ui(cx, window, |window, cx| {
        window.click(("link-to", 1usize), cx)
    });
    let task = store(cx, |s| s.task(&id).unwrap());
    assert_eq!(task.linked_workspace_ids, std::slice::from_ref(&press));
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::None));

    // どちらの一覧にも出て、全体では 1 件
    go(cx, &press);
    assert_eq!(shown(cx), 1);
    assert_eq!(all_tasks(cx).len(), 1);

    // 削除のダイアログから「ここから外すだけ」を選ぶと、タスクは残る
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.modal = Modal::DeleteTasks(vec![id.clone()]);
            cx.notify();
        })
    });
    ui(cx, window, |window, cx| window.click("unlink-here", cx));
    assert_eq!(shown(cx), 0);
    let task = store(cx, |s| s.task(&id).unwrap());
    assert_eq!(task.workspace_id.as_deref(), Some(event.as_str()));
    assert!(task.linked_workspace_ids.is_empty());

    // 詳細の「外す」でも外せる。入っている場所を外すと、表示先に入れ直す
    go(cx, &event);
    cx.update(|cx| view.update(cx, |v, cx| v.write(cx, |s| s.link_task(&id, &press))));
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    ui(cx, window, |window, cx| window.click("unlink-home", cx));
    let task = store(cx, |s| s.task(&id).unwrap());
    assert_eq!(task.workspace_id.as_deref(), Some(press.as_str()));
    assert!(task.linked_workspace_ids.is_empty());
    assert_eq!(shown(cx), 0);
}

/// 選んだタスクの期限・担当者・カテゴリをまとめて変え、項目を入れてタスクを作れる。
#[gpui_kit::test]
fn bulk_changes_and_full_new_task_form(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, win(1280., 2400.));
    let sato = cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.write(cx, |s| {
                for title in ["a", "b", "c", "d"] {
                    s.add_task_in(None, title)?;
                }
                Ok(s.create_person("佐藤 花子", false)?.id)
            })
            .unwrap()
        })
    });
    let set = |cx: &mut TestAppContext, pick: fn(&MainView) -> &Entity<InputState>, text: &str| {
        cx.update_window(window, |_, window, cx| {
            let input = pick(view.read(cx)).clone();
            input.update(cx, |s, cx| s.set_value(text.to_owned(), window, cx));
        })
        .unwrap();
        cx.run_until_parked();
    };
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    let shown: Vec<String> = cx.update(|cx| {
        let v = view.read(cx);
        v.visible.iter().map(|&i| v.tasks[i].id.clone()).collect()
    });

    // 1 行目を選び、Shift つきで 3 行目を押すと、あいだもまとめて選ばれる
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.click_select(&shown[0], false, cx);
            v.click_select(&shown[2], true, cx);
        })
    });
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(view.read(cx).selected_ids(), shown[..3]));

    // 期限: 選ばずに変更すると断り、ダイアログを開いたままにする
    ui(cx, window, |window, cx| window.click("bulk-due", cx));
    ui(cx, window, |window, cx| window.click("bulk-due-submit", cx));
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.modal, Modal::BulkDue);
        assert!(view.error.is_some());
    });
    // ダイアログの中のカレンダーの日付の欄に書いて Enter で変える
    ui(cx, window, |window, cx| {
        window.click("pick-date-input", cx);
        window.input("2026/12/24 午後6時", cx);
        window.press("enter", cx);
    });
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::None));
    let due = |cx: &mut TestAppContext, id: &str| store(cx, |s| s.task(id).unwrap().due);
    assert_eq!(due(cx, &shown[1]).as_deref(), Some("2026-12-24 18:00"));
    assert_eq!(due(cx, &shown[3]), None);

    // 担当者: 追加してから、置き換えで空にする
    ui(cx, window, |window, cx| window.click("bulk-assignees", cx));
    ui(cx, window, |window, cx| {
        window.click(("bulk-person", 1usize), cx)
    });
    ui(cx, window, |window, cx| {
        window.click("bulk-assignees-submit", cx)
    });
    let assignees =
        |cx: &mut TestAppContext, id: &str| store(cx, |s| s.task(id).unwrap().assignee_ids);
    assert_eq!(assignees(cx, &shown[2]), std::slice::from_ref(&sato));
    assert!(assignees(cx, &shown[3]).is_empty());
    ui(cx, window, |window, cx| window.click("bulk-assignees", cx));
    ui(cx, window, |window, cx| {
        window.click(("bulk-mode", 2usize), cx)
    });
    ui(cx, window, |window, cx| {
        window.click("bulk-assignees-submit", cx)
    });
    assert!(assignees(cx, &shown[0]).is_empty());

    // カテゴリ: 足して、片方を外す
    ui(cx, window, |window, cx| window.click("bulk-categories", cx));
    set(cx, |v| &v.inputs.bulk_categories, "準備、会場");
    ui(cx, window, |window, cx| {
        window.click("bulk-categories-submit", cx)
    });
    ui(cx, window, |window, cx| window.click("bulk-categories", cx));
    ui(cx, window, |window, cx| {
        window.click(("bulk-cat-mode", 1usize), cx)
    });
    set(cx, |v| &v.inputs.bulk_categories, "準備");
    ui(cx, window, |window, cx| {
        window.click("bulk-categories-submit", cx)
    });
    assert_eq!(
        store(cx, |s| s.task(&shown[0]).unwrap().categories),
        ["会場"]
    );

    // 期限をなくす
    ui(cx, window, |window, cx| window.click("bulk-due", cx));
    ui(cx, window, |window, cx| window.click("bulk-due-clear", cx));
    assert_eq!(due(cx, &shown[0]), None);

    // 書きかけのタイトルがあれば、ダイアログなしでそのタイトルで作り、詳細を開く
    set(cx, |v| &v.inputs.quick_add, "見積もりを取る");
    ui(cx, window, |window, cx| window.click("new-task-open", cx));
    let made = all_tasks(cx)
        .into_iter()
        .find(|t| t.title == "見積もりを取る")
        .unwrap();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.modal, Modal::None);
        assert_eq!(view.panel.as_deref(), Some(made.id.as_str()));
        assert_eq!(view.editing, None);
        assert_eq!(view.inputs.quick_add.read(cx).value().as_ref(), "");
    });

    // 何も書いていなければ「新規タスク」で作り、詳細でそのままタイトルを書く
    // （詳細は入力欄に重なるので、先に閉じる）
    cx.update(|cx| view.update(cx, |v, cx| v.dismiss(cx)));
    cx.run_until_parked();
    ui(cx, window, |window, cx| window.click("new-task-open", cx));
    let added = all_tasks(cx)
        .into_iter()
        .find(|t| t.title == malakt_mahoz::state::NEW_TASK_TITLE)
        .expect("新規タスク");
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.modal, Modal::None);
        assert_eq!(view.panel.as_deref(), Some(added.id.as_str()));
        assert_eq!(view.editing, Some(malakt_mahoz::state::Editing::Title));
        assert_eq!(view.naming_task.as_deref(), Some(added.id.as_str()));
    });
    // タイトルは全部選ばれているので、打つと置き換わる
    ui(cx, window, |window, cx| {
        window.input("領収書をまとめる", cx);
        window.press("enter", cx);
    });
    cx.run_until_parked();
    assert_eq!(
        store(cx, |s| s.task(&added.id).unwrap()).title,
        "領収書をまとめる"
    );
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.editing, None);
        assert_eq!(view.naming_task, None);
    });
    // 書き終えたあとも Esc を受けられて、詳細が閉じる
    ui(cx, window, |window, cx| window.press("escape", cx));
    cx.update(|cx| assert_eq!(view.read(cx).panel, None));

    // Esc で取り消すと、「新規タスク」のまま残る
    ui(cx, window, |window, cx| window.click("new-task-open", cx));
    ui(cx, window, |window, cx| window.input("捨てるタイトル", cx));
    ui(cx, window, |window, cx| window.press("escape", cx));
    cx.run_until_parked();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.editing, None);
        assert_eq!(view.naming_task, None);
    });
    let titles: Vec<String> = all_tasks(cx).into_iter().map(|t| t.title).collect();
    assert!(titles.contains(&malakt_mahoz::state::NEW_TASK_TITLE.to_owned()));
    assert!(!titles.contains(&"捨てるタイトル".to_owned()));
}

/// 期限が今日のタスクは、通知の一覧と OS の通知に 1 回だけ出る。
#[gpui_kit::test]
fn due_today_reaches_the_notice_list_and_the_os(cx: &mut TestAppContext) {
    start(cx);
    let today = malakt_core::view::today();
    let task = cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let me = db.store.create_person("自分", true).unwrap();
            let draft = malakt_core::TaskDraft {
                title: "請求書を出す".into(),
                due: Some(today.clone()),
                assignee_ids: vec![me.id],
                ..Default::default()
            };
            db.store.create_task(None, &draft).unwrap()
        })
    });
    let (window, view) = open(cx, wide());
    cx.run_until_parked();
    let shown = cx.shown_system_notifications();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].title.as_ref(), "「請求書を出す」の期限は今日です");
    cx.update(|cx| assert_eq!(view.read(cx).unread_notices, 1));

    // ウィンドウを増やしても、読み直しても、同じ通知は出し直さない
    let (_second, second_view) = open(cx, wide());
    cx.update(|cx| view.update(cx, |v, cx| v.write(cx, |s| s.create_person("佐藤", false))));
    cx.run_until_parked();
    assert_eq!(cx.shown_system_notifications().len(), 1);
    cx.update(|cx| assert_eq!(second_view.read(cx).unread_notices, 1));

    // 一覧から開くと、既読になって、そのタスクの詳細が出る
    ui(cx, window, |window, cx| window.click("open-notices", cx));
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::Notices));
    ui(cx, window, |window, cx| {
        window.click(("notice", 0usize), cx)
    });
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.modal, Modal::None);
        assert_eq!(view.panel.as_deref(), Some(task.id.as_str()));
        assert_eq!(view.unread_notices, 0);
        assert_eq!(second_view.read(cx).unread_notices, 0);
    });
}

/// 設定の「通知」で、ほかの人が完了にしたときに知らせるかを、役・担当者・ワークスペースで選ぶ。
/// オフにすると細かい行は隠れる。担当者とワークスペースは重ねた一覧で、押すたびに入れる・外す。
#[gpui_kit::test]
fn done_notices_can_be_chosen(cx: &mut TestAppContext) {
    use malakt_core::done_notice::When;
    use malakt_core::{Role, Roles};
    use malakt_mahoz::state::{CustomizeSection, SubModal};
    start(cx);
    let (sato, event) = cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            db.store.create_person("自分", true).unwrap();
            let sato = db.store.create_person("佐藤", false).unwrap();
            let event = db.store.create_workspace(None, "イベント").unwrap();
            db.store.create_workspace(Some(&event.id), "受付").unwrap();
            (sato, event)
        })
    });
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| {
            v.open_appearance(window, cx);
            v.show_appearance_section(CustomizeSection::Notices, window, cx);
        })
    });
    let notices = |cx: &mut TestAppContext| cx.update(|cx| view.read(cx).done_notices(cx));
    assert!(notices(cx).is_standard());

    // 役は R・A・C・I を押すたびに入れる・外す。最後の 1 つは外れない
    for ix in [0usize, 2, 3] {
        ui(cx, window, |window, cx| window.click(("done-role", ix), cx));
    }
    assert_eq!(notices(cx).roles, Roles::of(&[Role::A]));
    ui(cx, window, |window, cx| {
        window.click(("done-role", 1usize), cx)
    });
    assert_eq!(notices(cx).roles, Roles::of(&[Role::A]));
    ui(cx, window, |window, cx| {
        window.click("done-roles-reset", cx)
    });
    assert_eq!(notices(cx).roles, malakt_core::done_notice::all_roles());

    // 担当者は、自分を除いた一覧。押すと「知らせない」になる
    ui(cx, window, |window, cx| {
        window.click("done-people-open", cx)
    });
    cx.update(|cx| assert_eq!(view.read(cx).sub_modal, Some(SubModal::DonePeople)));
    ui(cx, window, |window, cx| {
        window.click(("done-pick", 0usize), cx)
    });
    assert_eq!(notices(cx).muted_people, std::slice::from_ref(&sato.id));
    ui(cx, window, |window, cx| window.press("escape", cx));
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.sub_modal, None);
        assert_eq!(v.modal, Modal::Appearance);
    });

    // ワークスペースは、親を外すとサブワークスペースも外れて見える
    ui(cx, window, |window, cx| {
        window.click("done-workspaces-open", cx)
    });
    ui(cx, window, |window, cx| {
        window.click(("done-pick", 0usize), cx)
    });
    assert_eq!(
        notices(cx).muted_workspaces,
        std::slice::from_ref(&event.id)
    );
    ui(cx, window, |window, cx| window.press("escape", cx));
    ui(cx, window, |window, cx| {
        window.click("done-workspaces-reset", cx)
    });
    assert!(notices(cx).muted_workspaces.is_empty());

    // 知らせるときを選ぶ
    ui(cx, window, |window, cx| window.click("done-when", cx));
    pick(cx, window, When::Finished.label());
    assert_eq!(notices(cx).when, When::Finished);

    // オフにすると細かい行は隠れ、オンに戻すと前の選び方のまま出る
    ui(cx, window, |window, cx| window.click("done-switch", cx));
    assert!(!notices(cx).enabled);
    ui(cx, window, |window, _| {
        assert!(window.try_find("done-when").is_none());
    });
    ui(cx, window, |window, cx| window.click("done-switch", cx));
    let back = notices(cx);
    assert!(back.enabled);
    assert_eq!(back.when, When::Finished);
    assert_eq!(back.muted_people, std::slice::from_ref(&sato.id));
}

/// 設定の「通知」で、期限をいつ知らせるかを選ぶ。選んだ時刻が来ていれば、すぐ OS の通知が出る。
/// タスクの詳細の「通知」のメニューでは、そのタスクだけを変え、標準に戻せる。
#[gpui_kit::test]
fn due_alert_timing_can_be_chosen(cx: &mut TestAppContext) {
    use malakt_core::alert::TaskAlerts;
    use malakt_mahoz::state::CustomizeSection;
    start(cx);
    // 2 時間後（分は切り捨て）が期限のタスク
    let due = (chrono::Local::now() + chrono::Duration::hours(2))
        .format("%Y-%m-%d %H:%M")
        .to_string();
    let task = cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let me = db.store.create_person("自分", true).unwrap();
            let draft = malakt_core::TaskDraft {
                title: "会議".into(),
                due: Some(due.clone()),
                assignee_ids: vec![me.id],
                ..Default::default()
            };
            db.store.create_task(None, &draft).unwrap()
        })
    });
    let (window, view) = open(cx, wide());
    cx.run_until_parked();
    // 標準は期限ちょうどなので、まだ知らせない
    assert!(cx.shown_system_notifications().is_empty());

    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| {
            v.open_appearance(window, cx);
            v.show_appearance_section(CustomizeSection::Notices, window, cx);
        })
    });
    // 「2 時間前」を足すと、その時刻はもう来ているので、すぐ知らせる
    ui(cx, window, |window, cx| {
        window.click(("alert-timed", 6usize), cx)
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(view.read(cx).due_alert_standard(cx).timed, [0, 120]);
    });
    let shown = cx.shown_system_notifications();
    assert_eq!(shown.len(), 1);
    assert!(
        shown[0].title.starts_with("「会議」の期限は"),
        "{}",
        shown[0].title
    );
    // 日付だけの期限の側と時刻も変えられる
    ui(cx, window, |window, cx| {
        window.click(("alert-date", 1usize), cx)
    });
    ui(cx, window, |window, cx| {
        window.click(("alert-hour", 3usize), cx)
    });
    cx.update(|cx| {
        let standard = view.read(cx).due_alert_standard(cx);
        assert_eq!((standard.date, standard.hour), (vec![0, 1], 8));
    });
    ui(cx, window, |window, cx| {
        window.click("alert-timed-reset", cx)
    });
    cx.update(|cx| assert_eq!(view.read(cx).due_alert_standard(cx).timed, [0]));

    // タスクの詳細では、そのタスクだけを変える。標準から始まる
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| {
            v.modal = Modal::None;
            v.open_panel(&task.id, window, cx);
        })
    });
    ui(cx, window, |window, cx| window.click("task-alerts", cx));
    pick(cx, window, "15 分前");
    let own = |cx: &mut TestAppContext| cx.update(|cx| view.read(cx).task_alerts(&task.id, cx));
    assert_eq!(
        own(cx),
        TaskAlerts {
            task_id: task.id.clone(),
            timed: Some(vec![0, 15]),
            date: None,
        }
    );
    ui(cx, window, |window, cx| window.click("task-alerts", cx));
    pick(cx, window, "期限ちょうど");
    assert_eq!(own(cx).timed, Some(vec![15]));
    ui(cx, window, |window, cx| window.click("task-alerts", cx));
    pick(cx, window, "標準に合わせる（期限ちょうど）");
    assert!(own(cx).is_standard());
}

/// 開いているメニューの中の、`label` の項目を押す。
fn pick(cx: &mut TestAppContext, window: AnyWindowHandle, label: &str) {
    ui(cx, window, |window, cx| {
        let center = window.find_by_label(label).bounds().center();
        window.pointer_move(center, None, cx);
        window.pointer_down(center, MouseButton::Left, cx);
        window.pointer_up(center, MouseButton::Left, cx);
    });
}

/// ロック中も OS の通知は届く。押すと解除を求め、解除したらそのタスクを開く。
#[gpui_kit::test]
fn notices_arrive_while_locked(cx: &mut TestAppContext) {
    start(cx);
    let today = malakt_core::view::today();
    let task = cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let draft = malakt_core::TaskDraft {
                title: "ひみつの用事".into(),
                due: Some(today.clone()),
                ..Default::default()
            };
            let task = db.store.create_task(None, &draft).unwrap();
            lock::enable(&mut db.store).unwrap();
            task
        })
    });
    cx.update(db::lock_now);
    let (window, view) = open(cx, wide());
    cx.run_until_parked();
    cx.update(|cx| assert!(!view.read(cx).unlocked(cx)));
    let shown = cx.shown_system_notifications();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].title.as_ref(), "「ひみつの用事」の期限は今日です");

    // 押すと解除を求め、通ったらそのタスクを開く
    let asked = passkey(cx, Ok(()));
    cx.simulate_system_notification_response(gpui_kit::SystemNotificationResponse {
        tag: shown[0].tag.clone(),
        action_id: None,
    });
    cx.run_until_parked();
    ui(cx, window, |_, _| {});
    assert_eq!(*asked.lock().unwrap(), ["ロックを解除"]);
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.unlocked(cx));
        assert_eq!(view.panel.as_deref(), Some(task.id.as_str()));
        assert_eq!(view.open_after_unlock, None);
    });
    // 解除しても、同じ通知は出し直さない
    assert_eq!(cx.shown_system_notifications().len(), 1);
}

/// ロック中にコメントの通知が来たら、本文は出さない。「タイトルを出さない」にすると、1 行も出さない。
#[gpui_kit::test]
fn locked_notices_hide_what_was_chosen(cx: &mut TestAppContext) {
    start(cx);
    let (_window, view) = open(cx, wide());
    enable_lock(cx);
    let comment = |cx: &mut TestAppContext, title: &str| {
        let title = title.to_owned();
        cx.update(|cx| {
            db::shared(cx).update(cx, |db, cx| {
                let me = db
                    .store
                    .people()
                    .unwrap()
                    .into_iter()
                    .find(|p| p.is_self)
                    .unwrap();
                let sato = db.store.create_person("佐藤", false).unwrap();
                let draft = malakt_core::TaskDraft {
                    title,
                    assignee_ids: vec![me.id.clone(), sato.id.clone()],
                    ..Default::default()
                };
                let task = db.store.create_task(None, &draft).unwrap();
                let ticket = db.store.issue_ticket(&task.id, Some(&sato.id)).unwrap();
                db.store
                    .redeem_ticket(&ticket.secret, "駅前で配り終えました")
                    .unwrap();
                cx.notify();
            })
        });
        cx.run_until_parked();
    };
    cx.update(db::lock_now);
    cx.run_until_parked();
    comment(cx, "チラシを配る");
    let shown = cx.shown_system_notifications();
    assert_eq!(shown.len(), 1);
    assert!(
        shown[0].title.contains("チラシを配る"),
        "{}",
        shown[0].title
    );
    assert_eq!(shown[0].body.as_ref(), "", "ロック中は本文を出さない");

    cx.update(|cx| {
        view.update(cx, |v, cx| {
            assert!(v.set_locked_notices(lock::LockedNotices::Hidden, cx))
        })
    });
    comment(cx, "ポスターを貼る");
    let shown = cx.shown_system_notifications();
    assert_eq!(shown.len(), 2);
    assert_eq!(shown[1].title.as_ref(), "新しい通知があります");
    assert_eq!(shown[1].body.as_ref(), "");
}

/// 「設定」のロックと通知で、自動でロックするまでの時間と、ロック中の通知をメニューから選ぶ。
/// 操作がないまま選んだ時間が過ぎるとロックし、操作すると数え直す。
/// 設定の左に並ぶカテゴリの番号。
fn nav_ix(section: malakt_mahoz::state::CustomizeSection) -> usize {
    malakt_mahoz::state::CustomizeSection::ALL
        .iter()
        .position(|s| *s == section)
        .unwrap()
}

#[gpui_kit::test]
fn auto_lock_after_the_chosen_minutes(cx: &mut TestAppContext) {
    use malakt_mahoz::state::CustomizeSection;
    start(cx);
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.open_lock_settings(window, cx))
    });
    // 「設定」がロックのカテゴリで開く
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.modal, Modal::Appearance);
        assert_eq!(v.appearance_section, CustomizeSection::Lock);
    });
    // ロックが無効なうちは、時間と通知の行は出ない
    ui(cx, window, |window, _| {
        assert!(window.try_find("auto-lock").is_none());
        assert!(window.try_find("locked-notices").is_none());
    });

    // スイッチで有効にする（パスキーで確認する）
    let asked = passkey(cx, Ok(()));
    ui(cx, window, |window, cx| window.click("lock-switch", cx));
    assert_eq!(*asked.lock().unwrap(), ["ロックを有効にする"]);
    cx.update(|cx| {
        assert!(view.read(cx).lock_enabled(cx));
        assert_eq!(
            view.read(cx).auto_lock(cx),
            None,
            "最初は自動でロックしない"
        );
    });

    // 時間はその場のメニューで選ぶ
    ui(cx, window, |window, cx| window.click("auto-lock", cx));
    pick(cx, window, "15 分");
    cx.update(|cx| assert_eq!(view.read(cx).auto_lock(cx), Some(15)));
    // ロック中の通知は「通知」のカテゴリにある
    ui(cx, window, |window, cx| {
        window.click(
            (
                "customize-nav",
                nav_ix(malakt_mahoz::state::CustomizeSection::Notices),
            ),
            cx,
        )
    });
    ui(cx, window, |window, cx| window.click("locked-notices", cx));
    pick(cx, window, "タイトルを出さない");
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).locked_notices(cx),
            lock::LockedNotices::Hidden
        )
    });

    // 15 分たつ前はロックしない。過ぎたらロックする
    let last = cx.update(|cx| db::shared(cx).read(cx).last_input);
    cx.update(|cx| db::lock_if_idle(last + 15 * 60 - 1, cx));
    cx.update(|cx| assert!(view.read(cx).unlocked(cx)));
    cx.update(|cx| db::lock_if_idle(last + 15 * 60, cx));
    cx.run_until_parked();
    cx.update(|cx| assert!(!view.read(cx).unlocked(cx)));

    // 解除したら、そこから数え直す
    passkey(cx, Ok(()));
    ui(cx, window, |window, cx| window.click("unlock", cx));
    let unlocked_at = cx.update(|cx| db::shared(cx).read(cx).last_input);
    assert!(unlocked_at >= last);
    cx.update(|cx| db::lock_if_idle(unlocked_at + 15 * 60 - 1, cx));
    cx.update(|cx| assert!(view.read(cx).unlocked(cx)));

    // マウスを動かすと、数え直す
    cx.update(|cx| db::shared(cx).update(cx, |db, _| db.last_input = 0));
    ui(cx, window, |window, cx| {
        window.pointer_move(Point::new(px(200.), px(200.)), None, cx)
    });
    let moved = cx.update(|cx| db::shared(cx).read(cx).last_input);
    assert!(moved > 0, "マウスの操作を数えていない");
    // キーを押しても、数え直す
    cx.update(|cx| db::shared(cx).update(cx, |db, _| db.last_input = 0));
    cx.simulate_keystrokes(window, "a");
    let typed = cx.update(|cx| db::shared(cx).read(cx).last_input);
    assert!(typed > 0, "キーの操作を数えていない");

    // 「しない」にすると、いくら待ってもロックしない
    ui(cx, window, |window, cx| {
        window.click(
            (
                "customize-nav",
                nav_ix(malakt_mahoz::state::CustomizeSection::Lock),
            ),
            cx,
        )
    });
    ui(cx, window, |window, cx| window.click("auto-lock", cx));
    pick(cx, window, "しない");
    cx.update(|cx| {
        assert_eq!(view.read(cx).auto_lock(cx), None);
        db::lock_if_idle(i64::MAX / 2, cx);
        assert!(view.read(cx).unlocked(cx));
    });
}

/// 設定の「アクセスキー」で、アクセスキーを発行し、名前を付け、できることを変え、失効させる。
/// キーの文字列は発行した行の下にだけ出て、キーなしの接続の扱いもその場のメニューで選ぶ。
#[gpui_kit::test]
fn access_keys_are_issued_named_limited_and_revoked(cx: &mut TestAppContext) {
    use malakt_core::access::{self, Keyless, NEW_KEY_NAME};
    use malakt_mahoz::state::CustomizeSection;
    start(cx);
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| {
            v.open_appearance(window, cx);
            v.show_appearance_section(CustomizeSection::AccessKeys, window, cx);
        })
    });
    let keys = |cx: &mut TestAppContext| store(cx, |s| s.access_keys().unwrap());

    // キーなしの接続は、最初は今までどおり読み書きできる
    cx.update(|cx| assert_eq!(view.read(cx).keyless(cx), Keyless::Full));
    ui(cx, window, |window, cx| window.click("mcp-keyless", cx));
    pick(cx, window, "読み取りのみ");
    cx.update(|cx| assert_eq!(view.read(cx).keyless(cx), Keyless::ReadOnly));

    // ＋ で「新規アクセスキー」が増え、その行で名前を書ける。キーの文字列はその下に出る
    ui(cx, window, |window, cx| window.click("access-key-add", cx));
    assert_eq!(keys(cx)[0].name, NEW_KEY_NAME);
    let secret = cx.update(|cx| view.read(cx).issued_access_key.clone().unwrap().secret);
    ui(cx, window, |window, cx| {
        assert!(window.find("access-key-input").visible());
        assert!(window.find("access-key-secret").visible());
        window.input("Claude Code", cx);
        window.press("enter", cx);
    });
    assert_eq!(keys(cx)[0].name, "Claude Code");
    // 発行したキーで、外からつなげる
    let check = |cx: &mut TestAppContext, secret: &str| {
        cx.update(|cx| {
            db::shared(cx).update(cx, |db, _| {
                access::check(&mut db.store, Some(secret), 1_000).unwrap()
            })
        })
    };
    let grant = check(cx, &secret).unwrap();
    assert!(!grant.read_only);

    // コピーのボタンは、キーと、Claude Code に登録するコマンド
    ui(cx, window, |window, cx| window.click("access-key-copy", cx));
    let copied = cx.update(|cx| cx.read_from_clipboard().and_then(|i| i.text()));
    assert_eq!(copied.as_deref(), Some(secret.as_str()));
    ui(cx, window, |window, cx| {
        window.click("access-key-copy-claude", cx)
    });
    let copied = cx
        .update(|cx| cx.read_from_clipboard().and_then(|i| i.text()))
        .unwrap();
    assert!(copied.starts_with("claude mcp add --scope user malakt-mahoz -e MALAKT_MAHOZ_KEY="));
    assert!(copied.contains(&secret));
    ui(cx, window, |window, cx| {
        window.click("access-key-copy-config", cx)
    });
    let copied = cx
        .update(|cx| cx.read_from_clipboard().and_then(|i| i.text()))
        .unwrap();
    let config: serde_json::Value = serde_json::from_str(&copied).unwrap();
    assert_eq!(
        config["mcpServers"]["malakt-mahoz"]["env"]["MALAKT_MAHOZ_KEY"],
        secret.as_str()
    );

    // 読み取りのみと、ロック中も使えるを、行のボタンで切り替える
    ui(cx, window, |window, cx| {
        window.click(("access-key-write", 0usize), cx)
    });
    assert!(keys(cx)[0].read_only);
    // ロックが無効なら、パスキーは聞かない
    let asked = passkey(cx, Ok(()));
    ui(cx, window, |window, cx| {
        window.click(("access-key-lock", 0usize), cx)
    });
    assert!(keys(cx)[0].while_locked);
    assert!(asked.lock().unwrap().is_empty());
    ui(cx, window, |window, cx| {
        window.click(("access-key-lock", 0usize), cx)
    });
    assert!(!keys(cx)[0].while_locked);
    // ロックが有効なら、オンにするときだけパスキーで確かめる。断ったら変えない
    enable_lock(cx);
    passkey(cx, Err(Failure::Denied));
    ui(cx, window, |window, cx| {
        window.click(("access-key-lock", 0usize), cx)
    });
    cx.run_until_parked();
    assert!(!keys(cx)[0].while_locked);
    let asked = passkey(cx, Ok(()));
    ui(cx, window, |window, cx| {
        window.click(("access-key-lock", 0usize), cx)
    });
    cx.run_until_parked();
    assert_eq!(
        *asked.lock().unwrap(),
        ["ロック中も使えるアクセスキーにする"]
    );
    assert!(keys(cx)[0].while_locked);

    // 文字列は ✕ で隠すと、もう出ない
    ui(cx, window, |window, cx| window.click("access-key-hide", cx));
    ui(cx, window, |window, _| {
        assert!(window.try_find("access-key-secret").is_none())
    });

    // 失効は、ゴミ箱のメニューから
    ui(cx, window, |window, cx| {
        window.click(("access-key-revoke", 0usize), cx)
    });
    pick(cx, window, "失効させる");
    assert!(keys(cx).is_empty());
    assert_eq!(check(cx, &secret), Err(access::Denied::BadKey));

    // 発行したキーの文字列は、設定を閉じると消える
    ui(cx, window, |window, cx| window.click("access-key-add", cx));
    ui(cx, window, |window, cx| window.press("enter", cx));
    ui(cx, window, |window, cx| window.press("escape", cx));
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.modal, Modal::None);
        assert!(v.issued_access_key.is_none());
    });
}

/// アクセスキーの時計のボタンで、開始・期限・時間帯をその場で決める。
/// 期限は書いても、今からの長さで選んでもよく、今使えないキーには名前の横に印が出る。
#[gpui_kit::test]
fn access_keys_get_a_period_and_hours(cx: &mut TestAppContext) {
    use malakt_core::access::Window as Hours;
    use malakt_mahoz::state::CustomizeSection;
    start(cx);
    let (window, view) = open(cx, wide());
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            db.store
                .issue_access_key("Claude Code", false, false)
                .unwrap();
        })
    });
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| {
            v.open_appearance(window, cx);
            v.show_appearance_section(CustomizeSection::AccessKeys, window, cx);
        })
    });
    let key = |cx: &mut TestAppContext| store(cx, |s| s.access_keys().unwrap().remove(0));
    ui(cx, window, |window, cx| {
        assert!(window.try_find("access-key-expires").is_none());
        window.click(("access-key-limits", 0usize), cx);
    });

    // 期限を書く。過ぎた期限なら、名前の横に印が出る
    ui(cx, window, |window, cx| {
        window.click("access-key-expires", cx)
    });
    ui(cx, window, |window, cx| {
        window.input("2000/1/1 9:00", cx);
        window.press("enter", cx);
    });
    assert_eq!(key(cx).expires.as_deref(), Some("2000-01-01 09:00"));
    // 開き直すと見えている書き方で入っていて、そのまま Enter しても変わらない
    ui(cx, window, |window, cx| {
        window.click("access-key-expires", cx)
    });
    ui(cx, window, |window, cx| window.press("enter", cx));
    assert_eq!(key(cx).expires.as_deref(), Some("2000-01-01 09:00"));
    ui(cx, window, |window, _| {
        assert!(window.find(("access-key-off", 0usize)).visible())
    });
    // 今からの長さで選ぶと、印は消える
    ui(cx, window, |window, cx| {
        window.click("access-key-expiry-menu", cx)
    });
    pick(cx, window, "7 日");
    let expires = key(cx).expires.unwrap();
    assert!(
        expires.as_str() > malakt_core::view::now().as_str(),
        "{expires}"
    );
    ui(cx, window, |window, _| {
        assert!(window.try_find(("access-key-off", 0usize)).is_none())
    });

    // 時間帯を足すと、毎日 9:00〜18:00。曜日と時刻をその場で変える
    ui(cx, window, |window, cx| {
        window.click("access-key-window-add", cx)
    });
    assert_eq!(key(cx).windows, vec![Hours::standard()]);
    // 土曜を外す（7 つずつ並ぶ。5 番目が土）
    ui(cx, window, |window, cx| {
        window.click(("access-key-day", 5usize), cx)
    });
    assert_eq!(key(cx).windows[0].days, vec![0, 1, 2, 3, 4, 6]);
    ui(cx, window, |window, cx| {
        window.click(("access-key-from", 0usize), cx)
    });
    ui(cx, window, |window, cx| {
        window.input("22", cx);
        window.press("enter", cx);
    });
    ui(cx, window, |window, cx| {
        window.click(("access-key-to", 0usize), cx)
    });
    ui(cx, window, |window, cx| {
        window.input("6:00", cx);
        window.press("enter", cx);
    });
    let hours = key(cx).windows[0].clone();
    assert_eq!((hours.from, hours.to), (22 * 60, 6 * 60));

    // 読めない時刻は、何も変えずにエラーを出す。Esc は書きかけを捨てて、設定は開いたまま
    ui(cx, window, |window, cx| {
        window.click(("access-key-to", 0usize), cx)
    });
    ui(cx, window, |window, cx| {
        window.input("あとで", cx);
        window.press("enter", cx);
    });
    cx.update(|cx| {
        let v = view.read(cx);
        assert!(v
            .error
            .as_deref()
            .unwrap_or("")
            .contains("時刻として読めません"));
        assert!(v.key_limit_editing.is_some());
    });
    ui(cx, window, |window, cx| window.press("escape", cx));
    cx.update(|cx| {
        let v = view.read(cx);
        assert!(v.key_limit_editing.is_none());
        assert_eq!(v.modal, Modal::Appearance);
    });
    assert_eq!(key(cx).windows[0], hours);

    // 期限を空にすると、期限なし。時間帯の × で外すと、いつでも
    ui(cx, window, |window, cx| {
        window.click("access-key-expires", cx)
    });
    ui(cx, window, |window, cx| {
        window.press("backspace", cx);
        window.press("enter", cx);
    });
    assert_eq!(key(cx).expires, None);
    ui(cx, window, |window, cx| {
        window.click(("access-key-window-remove", 0usize), cx)
    });
    assert!(!key(cx).limited());
}

/// 歯車を押すと、メニューを挟まずに「設定」が開く。担当者・キー操作・新しいウィンドウ・今すぐロック・版も
/// 「設定」の中にあり、左の目次を押すとその見出しまでスクロールして、目次のその行が光る。
#[gpui_kit::test]
fn the_gear_opens_settings_with_everything_inside(cx: &mut TestAppContext) {
    use malakt_mahoz::state::CustomizeSection as Section;
    start(cx);
    let (window, view) = open(cx, wide());
    let nav = |section: Section| {
        (
            "customize-nav",
            Section::ALL.iter().position(|s| *s == section).unwrap(),
        )
    };
    ui(cx, window, |window, cx| window.click("open-settings", cx));
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::Appearance));
    ui(cx, window, |window, _| {
        assert!(!window.find("person-add").visible())
    });

    // 担当者
    ui(cx, window, |window, cx| {
        window.click(nav(Section::People), cx)
    });
    ui(cx, window, |window, _| {
        assert!(window.find("person-add").visible())
    });
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.appearance_pinned);
        assert_eq!(view.appearance_section, Section::People);
    });
    // キー操作
    ui(cx, window, |window, cx| {
        window.click(nav(Section::Keys), cx)
    });
    ui(cx, window, |window, _| {
        assert!(window.find("app-keys").visible())
    });

    // 新しいウィンドウ
    let before = cx.update(|cx| cx.windows().len());
    ui(cx, window, |window, cx| {
        window.click(nav(Section::Window), cx)
    });
    ui(cx, window, |window, cx| window.click("new-window", cx));
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.windows().len()), before + 1);

    // 今すぐロックは、ロックが有効なときだけ出る。押すと「設定」を閉じてロックする
    ui(cx, window, |window, _| {
        assert!(window.try_find("lock-now").is_none())
    });
    let asked = passkey(cx, Ok(()));
    ui(cx, window, |window, cx| {
        window.click(nav(Section::Lock), cx)
    });
    ui(cx, window, |window, cx| window.click("lock-switch", cx));
    assert_eq!(*asked.lock().unwrap(), ["ロックを有効にする"]);
    ui(cx, window, |window, cx| window.click("lock-now", cx));
    cx.run_until_parked();
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.modal, Modal::None);
        assert!(!view.unlocked(cx));
    });
}

/// 動きを付けているときは、ダイアログは薄い所から浮かび上がり、閉じると薄くなってから消える。
/// 消えるまでの間は上を覆い、閉じたダイアログのボタンは押せない。
#[gpui_kit::test]
fn dialogs_fade_in_and_out(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    cx.update(|cx| view.update(cx, |v, _| v.motion = true));
    // 動いている間は次の描画が続けて来るので、待つ（run_until_parked）前に見る
    let press = |cx: &mut TestAppContext, id: &'static str| {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click(id, cx);
            window.render_frame(cx);
        })
        .unwrap();
    };
    press(cx, "open-settings");
    cx.update(|cx| assert!(view.read(cx).dialog_t.get() < 1.));
    cx.run_until_parked();
    std::thread::sleep(std::time::Duration::from_millis(250));
    ui(cx, window, |_, _| {});
    cx.update(|cx| assert_eq!(view.read(cx).dialog_t.get(), 1.));

    press(cx, "dialog-close");
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::None));
    cx.update_window(window, |_, window, _| {
        assert!(window.try_find("dialog-closing").is_some())
    })
    .unwrap();
    cx.run_until_parked();
    std::thread::sleep(std::time::Duration::from_millis(200));
    ui(cx, window, |_, _| {});
    ui(cx, window, |window, _| {
        assert!(window.try_find("dialog-closing").is_none())
    });
}

/// 動きを付けているときは、タスクの詳細は閉じると右へ抜けながら薄くなって消える。
/// ×・Esc・行き先の切り替えのどれで閉じても同じで、消えるまでの間は閉じた詳細のボタンを
/// 押せない。消えかけのときに別のタスクを開くと、消えかけの詳細はすぐ消える。
#[gpui_kit::test]
fn task_detail_slides_out_when_closed(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    let ids: Vec<String> = cx.update(|cx| {
        view.update(cx, |v, cx| {
            ["チラシを配る", "看板を立てる"]
                .into_iter()
                .map(|title| {
                    let draft = malakt_core::TaskDraft {
                        title: title.into(),
                        ..Default::default()
                    };
                    v.write(cx, |s| Ok(s.create_task(None, &draft)?.id))
                        .unwrap()
                })
                .collect()
        })
    });
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    cx.update(|cx| view.update(cx, |v, _| v.motion = true));
    type Step<'a> = &'a dyn Fn(&mut gpui_kit::Window, &mut gpui_kit::App);
    // 動いている間は次の描画が続けて来るので、待つ（run_until_parked）前に見る
    let act = |cx: &mut TestAppContext, f: Step| {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            f(window, cx);
            window.render_frame(cx);
        })
        .unwrap();
    };
    // 開く動きは画面の時計で、消える動きは本当の時計で進む
    let settle = |cx: &mut TestAppContext| {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(300));
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(300));
        ui(cx, window, |_, _| {});
    };
    let closing = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, _| {
            window.try_find("panel-closing").is_some()
        })
        .unwrap()
    };
    let open_task = |cx: &mut TestAppContext, id: &str| {
        let (id, view) = (id.to_owned(), view.clone());
        act(cx, &move |window, cx| {
            view.update(cx, |v, cx| v.open_panel(&id, window, cx))
        });
    };
    let close_ways: [Step; 3] = [
        &|window, cx| window.click("close-panel", cx),
        &|window, cx| window.press("escape", cx),
        &|_, cx| view.update(cx, |v, cx| v.go(Nav::Home, cx)),
    ];
    for close in close_ways {
        open_task(cx, &ids[0]);
        settle(cx);
        assert!(!closing(cx));
        act(cx, close);
        cx.update(|cx| assert!(view.read(cx).detail.is_none()));
        assert!(closing(cx));
        // 閉じた詳細の「完了チケット」は、消えるまでの間も押せない
        act(cx, &|window, cx| window.click("ticket-open", cx));
        cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::None));
        settle(cx);
        assert!(!closing(cx));
        ui(cx, window, |window, cx| window.click("nav-all", cx));
    }

    // 消えかけのときに別のタスクを開くと、開いたタスクだけが残る
    open_task(cx, &ids[0]);
    settle(cx);
    act(cx, &|window, cx| window.click("close-panel", cx));
    assert!(closing(cx));
    open_task(cx, &ids[1]);
    assert!(!closing(cx));
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(
            v.detail.as_ref().map(|d| d.task.id.as_str()),
            Some(ids[1].as_str())
        );
    });
}

/// 動きを付けているときは、設定の上に重ねたダイアログは、閉じると薄くなってから消える。
/// 消えるまでの間は上を覆い、下の設定は開いたまま。
#[gpui_kit::test]
fn sub_dialogs_fade_out(cx: &mut TestAppContext) {
    use malakt_mahoz::state::SubModal;
    start(cx);
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.open_appearance(window, cx))
    });
    ui(cx, window, |window, cx| {
        window.click(("customize-nav", 3usize), cx)
    });
    ui(cx, window, |window, cx| window.click("font-open", cx));
    cx.update(|cx| assert_eq!(view.read(cx).sub_modal, Some(SubModal::Font)));
    cx.update(|cx| view.update(cx, |v, _| v.motion = true));
    std::thread::sleep(std::time::Duration::from_millis(250));
    ui(cx, window, |_, _| {});

    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(window.try_find("sub-modal-closing").is_some());
    })
    .unwrap();
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.sub_modal, None);
        assert_eq!(v.modal, Modal::Appearance);
    });
    cx.run_until_parked();
    std::thread::sleep(std::time::Duration::from_millis(200));
    ui(cx, window, |_, _| {});
    ui(cx, window, |window, _| {
        assert!(window.try_find("sub-modal-closing").is_none());
        assert!(window.try_find("font-open").is_some());
    });
}

/// 画面で発行した完了チケットをコマンドで引き換えると、開いている画面に届く。
#[gpui_kit::test]
fn completion_ticket_issued_here_and_redeemed_by_command(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.sqlite3");
    cx.update(|cx| malakt_mahoz::init(Store::open(&path).unwrap(), cx));
    let (window, view) = open(cx, win(1280., 2400.));
    let id = cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.write(cx, |s| {
                let me = s.create_person("自分", true)?;
                let sato = s.create_person("佐藤", false)?;
                let draft = malakt_core::TaskDraft {
                    title: "チラシを配る".into(),
                    assignee_ids: vec![me.id, sato.id],
                    ..Default::default()
                };
                Ok(s.create_task(None, &draft)?.id)
            })
            .unwrap()
        })
    });
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    ui(cx, window, |window, cx| window.click("ticket-open", cx));
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::Ticket(id.clone())));
    // 佐藤さんの分として発行する
    ui(cx, window, |window, cx| {
        window.click(("ticket-who", 1usize), cx)
    });
    ui(cx, window, |window, cx| window.click("ticket-issue", cx));
    let secret = cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.active_tickets(&id, cx), 1);
        view.issued_ticket
            .as_ref()
            .expect("発行した")
            .secret
            .clone()
    });
    // チケットの文字列は閉じると二度と出せないので、外側を押しても閉じない
    {
        let mut at = Point::default();
        ui(cx, window, |window, _| {
            at = window.find("dialog").bounds().origin - Point::new(px(4.), px(4.))
        });
        let mut v = gpui_kit::VisualTestContext::from_window(window, cx);
        v.simulate_mouse_down(at, MouseButton::Left, Modifiers::default());
        v.simulate_mouse_up(at, MouseButton::Left, Modifiers::default());
        v.run_until_parked();
    }
    cx.update(|cx| {
        assert!(view.read(cx).issued_ticket.is_some());
        assert_eq!(view.read(cx).modal, Modal::Ticket(id.clone()));
    });
    // 閉じると、チケットの文字列は画面から消える
    ui(cx, window, |window, cx| window.click("dialog-close", cx));
    cx.update(|cx| assert!(view.read(cx).issued_ticket.is_none()));

    // 別のプロセスとして引き換える
    let args: Vec<String> = ["redeem", &secret, "--message", "駅前で配り終えました"]
        .iter()
        .map(|a| (*a).to_owned())
        .collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = malakt_mahoz::cli::run(
        &args,
        None,
        || Store::open(&path),
        &mut std::io::empty(),
        &mut out,
        &mut err,
    );
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));

    // 見張りが気づくのと同じことをする
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            assert!(db.changed_outside());
            cx.notify();
        })
    });
    cx.run_until_parked();
    let task = store(cx, |s| s.task(&id).unwrap());
    assert_eq!(task.done_count(), (1, 2));
    assert_eq!(task.status, Status::NeedsAction);
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.unread_notices, 1);
        assert_eq!(view.active_tickets(&id, cx), 0);
        assert_eq!(view.detail.as_ref().unwrap().comments.len(), 1);
    });
    let shown = cx.shown_system_notifications();
    assert_eq!(shown.len(), 1);
    assert_eq!(
        shown[0].title.as_ref(),
        "佐藤 さんが「チラシを配る」の自分の分を完了にしました（1/2 人）"
    );
    assert_eq!(shown[0].body.as_ref(), "駅前で配り終えました");
}

/// 「自分」は最初からいて、担当者を足さなくても既定の担当に選べる。
/// ワークスペースには、メニューからアイコンと色を付けられる。
#[gpui_kit::test]
fn self_is_always_there_and_workspaces_take_icons(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    let me = store(cx, |s| s.people().unwrap());
    assert_eq!(me.len(), 1);
    assert!(me[0].is_self);

    let ws = cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            cx.notify();
            db.store.create_workspace(None, "引っ越し").unwrap().id
        })
    });
    cx.update(|cx| view.update(cx, |v, cx| v.go(Nav::Workspace(ws.clone()), cx)));
    cx.run_until_parked();

    // 設定の担当者に、自分が出ている
    ui(cx, window, |window, cx| window.click("ws-settings", cx));
    ui(cx, window, |window, cx| {
        window.click(("def-as", 0usize), cx)
    });
    ui(cx, window, |window, cx| {
        window.click("ws-settings-save", cx)
    });
    let saved = store(cx, |s| s.task_defaults(&ws).unwrap());
    assert_eq!(saved.assignee_ids, [me[0].id.clone()]);

    // 行の「…」からアイコンを変える
    ui(cx, window, |window, cx| {
        window.click(("ws-more", 0usize), cx)
    });
    ui(cx, window, |window, cx| window.press("down", cx));
    ui(cx, window, |window, cx| window.press("enter", cx));
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::WorkspaceIcon(ws.clone())));
    ui(cx, window, |window, cx| {
        window.click(("ws-icon", 13usize), cx)
    });
    ui(cx, window, |window, cx| {
        window.click(("ws-color", 2usize), cx)
    });
    let saved = store(cx, |s| s.workspace(&ws).unwrap());
    assert_eq!(saved.icon.as_deref(), Some("rocket"));
    assert_eq!(saved.color.as_deref(), Some("orange"));
    // 色なしに戻しても、アイコンは残る
    ui(cx, window, |window, cx| {
        window.click(("ws-color", 0usize), cx)
    });
    let saved = store(cx, |s| s.workspace(&ws).unwrap());
    assert_eq!(saved.icon.as_deref(), Some("rocket"));
    assert_eq!(saved.color, None);
}

/// 一覧の最後の追加行で、状態と優先度を選んでからタスクを足す。
#[gpui_kit::test]
fn quick_add_row_takes_status_and_priority(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, win(1280., 800.));
    ui(cx, window, |window, cx| window.click("nav-all", cx));

    // 優先度のメニューの 1 つ目は「高」
    ui(cx, window, |window, cx| window.click("quick-priority", cx));
    ui(cx, window, |window, cx| {
        window.press("down", cx);
        window.press("enter", cx);
    });
    // 状態のメニューの 2 つ目は「進行中」
    ui(cx, window, |window, cx| window.click("quick-status", cx));
    ui(cx, window, |window, cx| {
        window.press("down", cx);
        window.press("down", cx);
        window.press("enter", cx);
    });
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.quick.priority, 1);
        assert_eq!(view.quick.status, Status::InProcess);
    });

    ui(cx, window, |window, cx| {
        window.click("quick-add", cx);
        window.input("見積もりを取る", cx);
        window.press("enter", cx);
    });
    let task = all_tasks(cx).remove(0);
    assert_eq!(task.title, "見積もりを取る");
    assert_eq!(task.priority, 1);
    assert_eq!(task.status, Status::InProcess);
    // 足したあとも、選んだ値のまま次の 1 件を足せる
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.quick.priority, 1);
        assert_eq!(view.quick.status, Status::InProcess);
    });
}

/// 追加行の担当は、最初は自分。選び直した値は、足したあとも、ウィンドウを開き直しても残る。
#[gpui_kit::test]
fn quick_add_row_starts_with_me_and_remembers(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, win(1280., 800.));
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    let me = cx.update(|cx| {
        let people = db::shared(cx).read(cx).store.people().unwrap();
        people.into_iter().find(|p| p.is_self).unwrap().id
    });
    cx.update(|cx| assert_eq!(view.read(cx).quick.assignee_ids, vec![me.clone()]));

    ui(cx, window, |window, cx| {
        window.click("quick-add", cx);
        window.input("会場を予約する", cx);
        window.press("enter", cx);
    });
    assert_eq!(all_tasks(cx)[0].assignee_ids, vec![me.clone()]);

    // 担当のメニューの 1 つ目は自分。押すと外れる
    ui(cx, window, |window, cx| window.click("quick-assignee", cx));
    ui(cx, window, |window, cx| {
        window.press("down", cx);
        window.press("enter", cx);
    });
    ui(cx, window, |window, cx| window.click("quick-status", cx));
    ui(cx, window, |window, cx| {
        window.press("down", cx);
        window.press("down", cx);
        window.press("enter", cx);
    });
    ui(cx, window, |window, cx| {
        window.click("quick-add", cx);
        window.input("見積もりを取る", cx);
        window.press("enter", cx);
    });
    let task = all_tasks(cx)
        .into_iter()
        .find(|t| t.title == "見積もりを取る")
        .unwrap();
    assert!(task.assignee_ids.is_empty());
    assert_eq!(task.status, Status::InProcess);

    // 開き直したウィンドウでも、選んだ値で始まる
    let (again, view) = open(cx, win(1280., 800.));
    ui(cx, again, |window, cx| window.click("nav-all", cx));
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.quick.assignee_ids.is_empty());
        assert_eq!(view.quick.status, Status::InProcess);
    });
}

/// 詳細のタイトルは、押すとその場で書き直せる。Enter で保存し、Esc で元に戻る。
#[gpui_kit::test]
fn title_is_edited_in_place(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| {
        window.click("nav-all", cx);
        window.click("quick-add", cx);
        window.input("牛乳を買う", cx);
        window.press("enter", cx);
    });
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));

    ui(cx, window, |window, cx| window.click("detail-title", cx));
    ui(cx, window, |window, cx| {
        assert_eq!(window.find("title-input").value(), Some("牛乳を買う"));
        window.input("と卵", cx);
        window.press("enter", cx);
    });
    assert_eq!(all_tasks(cx)[0].title, "牛乳を買うと卵");
    assert!(cx.update(|cx| view.read(cx).editing.is_none()));

    // Esc は書きかけを捨てて、読むときの表示に戻す
    ui(cx, window, |window, cx| window.click("detail-title", cx));
    ui(cx, window, |window, cx| {
        window.input("も", cx);
        window.press("escape", cx);
    });
    assert_eq!(all_tasks(cx)[0].title, "牛乳を買うと卵");
    assert!(cx.update(|cx| view.read(cx).editing.is_none()));
}

/// 詳細のカテゴリは、名前と × のチップで出る。外すのは × だけで、名前を押しても外れない。
/// 外したものは元に戻せる。
#[gpui_kit::test]
fn categories_are_removed_only_by_their_x(cx: &mut TestAppContext) {
    start(cx);
    let (window, _view) = open(cx, wide());
    ui(cx, window, |window, cx| {
        window.click("nav-all", cx);
        window.click("quick-add", cx);
        window.input("会場を押さえる", cx);
        window.press("enter", cx);
    });
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    let categories = |cx: &mut TestAppContext| all_tasks(cx)[0].categories.clone();
    for name in ["PR", "会場"] {
        ui(cx, window, |window, cx| window.click("category-input", cx));
        ui(cx, window, |window, cx| {
            window.input(name, cx);
            window.press("enter", cx);
        });
    }
    assert_eq!(categories(cx), ["PR", "会場"]);

    // 名前を押しても外れない
    ui(cx, window, |window, cx| window.click(("p-cat", 0usize), cx));
    assert_eq!(categories(cx), ["PR", "会場"]);

    // × で、そのカテゴリだけ外れる
    ui(cx, window, |window, cx| {
        window.click(("p-cat-remove", 0usize), cx)
    });
    assert_eq!(categories(cx), ["会場"]);
    ui(cx, window, |window, _| {
        assert!(window.find(("p-cat", 0usize)).visible());
    });
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| assert!(db.store.undo().unwrap()));
    });
    assert_eq!(categories(cx), ["PR", "会場"]);
}

/// タイトルの下に URL をいくつも付けられる。その場の欄に書いて Enter で足し、✕ で外す。
/// URL でないものは足さずにエラーを出し、Esc で書きかけを捨てる。
#[gpui_kit::test]
fn urls_are_added_and_removed_under_the_title(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| {
        window.click("nav-all", cx);
        window.click("quick-add", cx);
        window.input("会場を押さえる", cx);
        window.press("enter", cx);
    });
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    let urls = |cx: &mut TestAppContext| all_tasks(cx)[0].urls.clone();

    // 貼った文の空白・改行で区切って、まとめて足せる。https:// は省いてよい
    ui(cx, window, |window, cx| window.click("url-add", cx));
    ui(cx, window, |window, cx| {
        window.input("github.com/acme/app/pull/7 https://example.com/map", cx);
        window.press("enter", cx);
    });
    assert_eq!(
        urls(cx),
        [
            "https://github.com/acme/app/pull/7",
            "https://example.com/map"
        ]
    );
    assert!(cx.update(|cx| view.read(cx).editing.is_none()));
    ui(cx, window, |window, _| {
        assert!(window.find(("task-url", 1usize)).visible());
    });

    // URL でないものは足さない。欄は開いたまま
    ui(cx, window, |window, cx| window.click("url-add", cx));
    ui(cx, window, |window, cx| {
        window.input("メモ", cx);
        window.press("enter", cx);
    });
    assert_eq!(urls(cx).len(), 2);
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.error.as_ref().is_some_and(|e| e.contains("「メモ」")));
        assert_eq!(view.editing, Some(malakt_mahoz::state::Editing::Url));
    });
    // Esc は書きかけを捨てる
    ui(cx, window, |window, cx| window.press("escape", cx));
    assert!(cx.update(|cx| view.read(cx).editing.is_none()));
    assert_eq!(urls(cx).len(), 2);

    // ✕ で外す。外したものは元に戻せる
    ui(cx, window, |window, cx| {
        window.click(("task-url-remove", 0usize), cx)
    });
    assert_eq!(urls(cx), ["https://example.com/map"]);
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| assert!(db.store.undo().unwrap()));
    });
    assert_eq!(urls(cx).len(), 2);
}

/// 詳細の開始日と期限は、値を押すとその場に開くカレンダーで決める。日付の欄は「明日」「11/27 14時」の
/// ような書き方も読み、Enter で保存して閉じる。カレンダーの日やチップは押すとすぐ保存し、開いたまま
/// 続けて選べる。選んでいる日をもう 1 度押すと閉じる。書きかけは外を押したときにも保存する。
#[gpui_kit::test]
fn due_picker_takes_days_times_and_spellings(cx: &mut TestAppContext) {
    use chrono::{Datelike, NaiveDate};
    use malakt_core::view::{add_days, today};
    start(cx);
    // 詳細を開いたまま一覧の期限のセルも押すので、詳細を一覧の右のカラムにする「一覧と詳細」で確かめる
    cx.update(|cx| malakt_mahoz::theme::set_overlay(false, cx).unwrap());
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| {
        window.click("nav-all", cx);
        window.click("quick-add", cx);
        window.input("資料を出す", cx);
        window.press("enter", cx);
    });
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    let due = |cx: &mut TestAppContext| all_tasks(cx)[0].due.clone();
    let start = |cx: &mut TestAppContext| all_tasks(cx)[0].start.clone();
    let open_now = |cx: &mut TestAppContext| cx.update(|cx| view.read(cx).date_pick.is_some());
    let shown = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let inputs = &view.read(cx).inputs;
            (
                inputs.pick_date.read(cx).value().to_string(),
                inputs.pick_time.read(cx).value().to_string(),
            )
        })
    };
    let type_in = |cx: &mut TestAppContext, id: &'static str, text: &'static str| {
        ui(cx, window, |window, cx| {
            window.click(id, cx);
            window.press("secondary-a", cx);
            window.press("backspace", cx);
            if !text.is_empty() {
                window.input(text, cx);
            }
            window.press("enter", cx);
        });
    };
    let click = |cx: &mut TestAppContext, id: &'static str| {
        ui(cx, window, |window, cx| window.click(id, cx));
    };
    let chip = |cx: &mut TestAppContext, id: &'static str, ix: usize| {
        ui(cx, window, |window, cx| window.click((id, ix), cx));
    };
    let tomorrow = add_days(&today(), 1);
    let slash = |date: &str| {
        let d = NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap();
        format!("{}/{}/{}", d.year(), d.month(), d.day())
    };

    // 値を押すと開き、日付の欄に「明日」と書いて Enter で保存して閉じる
    click(cx, "due-value");
    assert!(open_now(cx));
    type_in(cx, "pick-date-input", "明日");
    assert_eq!(due(cx), Some(tomorrow.clone()));
    assert!(!open_now(cx));

    // 時刻の欄に書くと、日付はそのままで時刻が付く
    click(cx, "due-value");
    assert_eq!(shown(cx), (slash(&tomorrow), String::new()));
    type_in(cx, "pick-time-input", "14時");
    assert_eq!(due(cx), Some(format!("{tomorrow} 14:00")));

    // 日付の欄に時刻まで書くと、両方が変わる
    click(cx, "due-value");
    type_in(cx, "pick-date-input", "2027/1/5 午後3時半");
    assert_eq!(due(cx).as_deref(), Some("2027-01-05 15:30"));

    // 読めない書き方では保存せず、開いたまま。Esc で書きかけを捨てて閉じる
    click(cx, "due-value");
    type_in(cx, "pick-time-input", "25時");
    assert!(open_now(cx));
    assert_eq!(due(cx).as_deref(), Some("2027-01-05 15:30"));
    ui(cx, window, |window, cx| window.press("escape", cx));
    assert!(!open_now(cx));
    assert_eq!(due(cx).as_deref(), Some("2027-01-05 15:30"));

    // チップは押すとすぐ保存し、開いたまま続けて選べる。日付のチップは時刻を残す
    click(cx, "due-value");
    chip(cx, "time-chip", 4);
    assert_eq!(due(cx).as_deref(), Some("2027-01-05"));
    chip(cx, "time-chip", 0);
    assert_eq!(due(cx).as_deref(), Some("2027-01-05 09:00"));
    chip(cx, "date-chip", 1);
    assert_eq!(due(cx), Some(format!("{tomorrow} 09:00")));
    assert!(open_now(cx));
    assert_eq!(shown(cx), (slash(&tomorrow), "09:00".to_owned()));
    // 選んでいる日をもう 1 度押すと閉じる
    chip(cx, "date-chip", 1);
    assert!(!open_now(cx));

    // 次の月を出して、カレンダーの日を押す
    click(cx, "due-value");
    click(cx, "date-next");
    let (year, month) = cx.update(|cx| view.read(cx).date_pick.as_ref().unwrap().month);
    let first = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
    ui(cx, window, |window, cx| {
        window.click(("date-day", first.num_days_from_ce() as usize), cx)
    });
    assert_eq!(due(cx), Some(format!("{} 09:00", first.format("%Y-%m-%d"))));

    // キーでも日を選べる（→ で次の日、Enter で選ぶ）
    ui(cx, window, |window, cx| {
        window.press("right", cx);
        window.press("enter", cx);
    });
    let second = first.succ_opt().unwrap();
    assert_eq!(
        due(cx),
        Some(format!("{} 09:00", second.format("%Y-%m-%d")))
    );

    // 「クリア」で期限をなくして閉じる
    click(cx, "date-clear");
    assert_eq!(due(cx), None);
    assert!(!open_now(cx));

    // 開始日も同じ書き方を読む
    click(cx, "start-value");
    type_in(cx, "pick-date-input", "20270104 10時");
    assert_eq!(start(cx).as_deref(), Some("2027-01-04 10:00"));

    // 書きかけのまま外（一覧の行）を押すと、書いたものを保存して閉じる
    click(cx, "start-value");
    chip(cx, "date-chip", 1);
    assert_eq!(start(cx), Some(format!("{tomorrow} 10:00")));
    ui(cx, window, |window, cx| {
        window.click("pick-time-input", cx);
        window.press("secondary-a", cx);
        window.press("backspace", cx);
        window.input("17時", cx);
    });
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    assert_eq!(start(cx), Some(format!("{tomorrow} 17:00")));
    assert!(!open_now(cx));

    // 開始と期限が同じ日なら、一覧では時間帯で出る
    click(cx, "due-value");
    type_in(cx, "pick-date-input", "明日 18時");
    let day = malakt_core::view::format_date(&tomorrow, &today());
    cx.update(|cx| {
        let task = &view.read(cx).tasks[0];
        assert_eq!(
            malakt_core::view::format_span(
                task.start.as_deref(),
                task.due.as_deref().unwrap(),
                &today()
            ),
            format!("{day} 17:00–18:00")
        );
    });

    // 一覧の期限のセルからも開き、選ぶとすぐ保存する
    ui(cx, window, |window, cx| window.click(("due", 0usize), cx));
    assert!(open_now(cx));
    chip(cx, "date-chip", 0);
    assert_eq!(due(cx), Some(format!("{} 18:00", today())));
}

/// 上のバーの配色のボタンで、画面の色がその場で変わり、選択が保存される。
#[gpui_kit::test]
fn theme_buttons_switch_the_colors(cx: &mut TestAppContext) {
    use gpui_kit::component::{ActiveTheme, Theme};
    start(cx);
    let (window, _view) = open(cx, wide());
    let look = |cx: &mut TestAppContext| {
        cx.update(|cx| (Theme::global(cx).mode.is_dark(), cx.theme().background))
    };
    let saved = |cx: &mut TestAppContext| store(cx, |s| s.setting("theme").unwrap());

    ui(cx, window, |window, cx| window.click(("theme", 1usize), cx));
    let (dark, dark_bg) = look(cx);
    assert!(dark);
    assert_eq!(saved(cx).as_deref(), Some("dark"));

    ui(cx, window, |window, cx| window.click(("theme", 0usize), cx));
    let (dark, light_bg) = look(cx);
    assert!(!dark);
    assert_ne!(dark_bg, light_bg);
    assert_eq!(saved(cx).as_deref(), Some("light"));

    // 行き来しても毎回変わる
    ui(cx, window, |window, cx| window.click(("theme", 1usize), cx));
    assert_eq!(look(cx), (true, dark_bg));
    ui(cx, window, |window, cx| window.click(("theme", 2usize), cx));
    assert_eq!(saved(cx).as_deref(), Some("system"));
}

/// 設定の「フォントとカラー」で、書体と色がその場で変わり、保存される。開き直しても残り、標準に戻せる。
#[gpui_kit::test]
fn font_and_colors_are_chosen_in_settings(cx: &mut TestAppContext) {
    use gpui_kit::component::{ActiveTheme, Theme};
    use malakt_mahoz::state::SubModal;
    use malakt_mahoz::theme::{self, Color, Slot};
    start(cx);
    let (window, view) = open(cx, wide());
    let saved = |cx: &mut TestAppContext, key: &str| store(cx, |s| s.setting(key).unwrap());
    let font = |cx: &mut TestAppContext| cx.update(|cx| cx.theme().font_family.to_string());
    let stock = cx.update(|cx| (cx.theme().primary, cx.theme().background));
    assert_eq!(font(cx), "LINE Seed JP");

    // 設定のダイアログの上に、書体の一覧が重なって開く
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.open_appearance(window, cx))
    });
    ui(cx, window, |window, cx| {
        window.click(("customize-nav", 3usize), cx)
    });
    ui(cx, window, |window, cx| window.click("font-open", cx));
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            assert_eq!(v.sub_modal, Some(SubModal::Font));
            assert_eq!(v.fonts[0], "LINE Seed JP");
            // この PC に入っている書体の代わり
            v.fonts = ["LINE Seed JP", "Noto Sans", "Noto Serif"]
                .map(String::from)
                .to_vec();
            cx.notify();
        })
    });
    // 検索で絞って選ぶ
    ui(cx, window, |window, cx| window.input("serif", cx));
    ui(cx, window, |window, cx| {
        assert!(window.try_find(("font", 1usize)).is_none());
        window.click(("font", 0usize), cx)
    });
    assert_eq!(font(cx), "Noto Serif");
    assert_eq!(saved(cx, "font.family").as_deref(), Some("Noto Serif"));

    // Esc は重ねたほうだけを閉じ、もう一度で設定を閉じる
    ui(cx, window, |window, cx| window.press("escape", cx));
    assert!(cx.update(|cx| {
        let v = view.read(cx);
        v.sub_modal.is_none() && v.modal == Modal::Appearance
    }));
    ui(cx, window, |window, cx| window.press("escape", cx));
    assert!(cx.update(|cx| view.read(cx).modal == Modal::None));

    // 色を選ぶと、ライトとダークの両方に使われる
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.open_appearance(window, cx))
    });
    ui(cx, window, |window, cx| {
        window.click(("customize-nav", 3usize), cx)
    });
    let pick = |cx: &mut TestAppContext, slot: usize, hex: &str| {
        let color = Color::parse(hex).unwrap();
        // 色の見本を押すと、色を選ぶところが開く
        ui(cx, window, |window, cx| {
            window.click(("color-open", slot), cx)
        });
        assert!(cx.update(|cx| view.read(cx).color_pickers[slot].read(cx).is_open()));
        ui(cx, window, |window, cx| {
            let picker = view.read(cx).color_pickers[slot].clone();
            picker.update(cx, |p, cx| p.select_color(color.into(), window, cx));
        });
    };
    pick(cx, 0, "#1d4ed8");
    pick(cx, 1, "#fdf6e3");
    pick(cx, 2, "#002b36");
    assert_eq!(saved(cx, "color.accent").as_deref(), Some("#1d4ed8"));
    assert_eq!(
        saved(cx, "color.background.light").as_deref(),
        Some("#fdf6e3")
    );
    assert_eq!(
        saved(cx, "color.background.dark").as_deref(),
        Some("#002b36")
    );
    let hex = |color: gpui_kit::Hsla| Color::from(color).hex();
    ui(cx, window, |window, cx| {
        theme::set(theme::Choice::Light, window, cx).unwrap()
    });
    let light = cx.update(|cx| (cx.theme().primary, cx.theme().background));
    assert_eq!(hex(light.0), "#1d4ed8");
    assert_eq!(hex(light.1), "#fdf6e3");
    ui(cx, window, |window, cx| {
        theme::set(theme::Choice::Dark, window, cx).unwrap()
    });
    let dark = cx.update(|cx| (cx.theme().primary, cx.theme().background));
    assert_eq!(hex(dark.1), "#002b36");
    // 暗い地では、同じ青を見える明るさまで上げて使う
    assert_ne!(dark.0, light.0);

    // 起動し直しても残る
    cx.update(|cx| {
        Theme::global_mut(cx).font_family = "x".into();
        theme::init(cx);
    });
    assert_eq!(font(cx), "Noto Serif");
    assert_eq!(hex(cx.update(|cx| cx.theme().background)), "#002b36");

    // 標準に戻すボタンで、同梱の書体と色に戻る
    for id in ["font-reset", "accent-reset", "light-reset", "dark-reset"] {
        ui(cx, window, |window, cx| window.click(id, cx));
    }
    for key in [
        "font.family",
        "color.accent",
        "color.background.light",
        "color.background.dark",
    ] {
        assert_eq!(saved(cx, key), None, "{key}");
    }
    assert_eq!(font(cx), "LINE Seed JP");
    ui(cx, window, |window, cx| {
        theme::set(theme::Choice::Light, window, cx).unwrap()
    });
    assert_eq!(
        cx.update(|cx| (cx.theme().primary, cx.theme().background)),
        stock
    );
    assert_eq!(
        cx.update(|cx| theme::shown(Slot::Accent, cx).hex()),
        "#86600f"
    );
}

/// 設定の「カスタマイズ」で、背景の画像・透明度・ぼかし・カード表示を変えられ、開き直しても残る。
#[gpui_kit::test]
fn backdrop_and_cards_are_chosen_in_settings(cx: &mut TestAppContext) {
    use gpui_kit::component::ActiveTheme;
    use malakt_mahoz::state::SubModal;
    use malakt_mahoz::theme::{self, Color, Scene};
    start(cx);
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            db.store.add_task_in(None, "切符を買う").unwrap();
            cx.notify();
        })
    });
    let (window, view) = open(cx, wide());
    cx.update(|cx| view.update(cx, |v, cx| v.go(Nav::All, cx)));
    let saved = |cx: &mut TestAppContext, key: &str| store(cx, |s| s.setting(key).unwrap());
    let primary = |cx: &mut TestAppContext| cx.update(|cx| Color::from(cx.theme().primary).hex());
    ui(cx, window, |window, cx| {
        theme::set(theme::Choice::Light, window, cx).unwrap()
    });
    assert!(cx.update(|cx| theme::backdrop(cx).is_none()));

    // 背景の画像の一覧は、設定の上に重なって開く。選ぶと、その画像に合う色になる
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.open_appearance(window, cx))
    });
    ui(cx, window, |window, cx| {
        window.click(("customize-nav", 2usize), cx)
    });
    ui(cx, window, |window, cx| {
        // 画像を出していないときは、透明度とぼかしは出ない
        assert!(window.try_find("clear-slider").is_none());
        window.click("scene-open", cx)
    });
    assert_eq!(
        cx.update(|cx| view.read(cx).sub_modal),
        Some(SubModal::Scene)
    );
    ui(cx, window, |window, cx| window.click(("scene", 1usize), cx));
    assert_eq!(saved(cx, "scene").as_deref(), Some("sky"));
    assert!(cx.update(|cx| theme::backdrop(cx).is_some()));
    assert_eq!(primary(cx), "#1f6fd0");
    ui(cx, window, |window, cx| window.press("escape", cx));
    assert!(cx.update(|cx| {
        let v = view.read(cx);
        v.sub_modal.is_none() && v.modal == Modal::Appearance
    }));

    // 透明度とぼかしは、押したところの値になる
    let alpha = |cx: &mut TestAppContext| cx.update(|cx| theme::backdrop(cx).unwrap().alpha);
    let before = alpha(cx);
    ui(cx, window, |window, cx| {
        let slider = window.find("clear-slider").bounds();
        let right = gpui_kit::point(slider.size.width * 0.9, slider.size.height / 2.);
        window.click_at("clear-slider", right, cx)
    });
    assert!(alpha(cx) < before, "透明度を上げると、面が薄くなる");
    assert_eq!(
        saved(cx, "scene.clear").and_then(|v| v.parse::<u8>().ok()),
        Some(cx.update(|cx| theme::clear(cx)))
    );
    assert!(cx.update(|cx| theme::clear(cx)) > theme::DEFAULT_CLEAR);
    ui(cx, window, |window, cx| window.click("blur-slider", cx));
    assert_eq!(saved(cx, "scene.blur").as_deref(), Some("50"));

    // 自分で選んだ色は、画像に合う色より優先する
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| {
            v.set_color(theme::Slot::Accent, Color::parse("#b3344f"), window, cx)
        })
    });
    assert_eq!(primary(cx), "#b3344f");
    ui(cx, window, |window, cx| {
        window.click(("customize-nav", 3usize), cx)
    });
    ui(cx, window, |window, cx| window.click("accent-reset", cx));
    assert_eq!(primary(cx), "#1f6fd0");

    // カードとパネルの行は「表示」にある
    ui(cx, window, |window, cx| {
        window.click(("customize-nav", 4usize), cx)
    });
    // カードで表示にすると、広い画面でも表の見出しがなくなる
    ui(cx, window, |window, cx| {
        assert!(window.try_find(("col-head", 0usize)).is_some());
        window.click("cards-switch", cx)
    });
    assert_eq!(saved(cx, "list.cards").as_deref(), Some("1"));
    ui(cx, window, |window, _| {
        assert!(window.try_find(("col-head", 0usize)).is_none())
    });

    // パネルを浮かせても、設定のスイッチと一覧の場所は変わらない（カードは中身の後ろに敷くだけ）
    let place = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, _| {
            ["cards-switch", "float-switch", "nav-home"].map(|id| window.find(id).bounds())
        })
        .unwrap()
    };
    // 上のバーのボタンは、どれも同じ高さに並ぶ（縦の中央にそろう）
    let bar_centers = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, _| {
            [
                window.find("toggle-sidebar").bounds(),
                window.find(("theme", 0usize)).bounds(),
                window.find("pane-split").bounds(),
                window.find("ai-toggle").bounds(),
                window.find("open-notices").bounds(),
                window.find("open-settings").bounds(),
            ]
            .map(|bounds| bounds.center().y)
        })
        .unwrap()
    };
    let flat = place(cx);
    let centers = bar_centers(cx);
    assert!(centers.iter().all(|y| *y == centers[0]), "{centers:?}");
    ui(cx, window, |window, cx| window.click("float-switch", cx));
    assert_eq!(saved(cx, "panels.float").as_deref(), Some("1"));
    assert!(cx.update(|cx| theme::floating(cx)));
    assert_eq!(place(cx), flat);
    // 浮かせると、バーの中身はカードの内側に収まる。そこでも同じ高さに並ぶ
    let centers = bar_centers(cx);
    assert!(centers.iter().all(|y| *y == centers[0]), "{centers:?}");
    ui(cx, window, |window, cx| window.click("float-switch", cx));
    assert_eq!(saved(cx, "panels.float"), None);
    ui(cx, window, |window, cx| window.click("float-switch", cx));

    // 起動し直しても残る。ダークでは夜の絵に変わる
    let day = cx.update(|cx| theme::backdrop(cx).unwrap().image.id);
    cx.update(theme::init);
    assert!(cx.update(|cx| theme::backdrop(cx).is_some()));
    ui(cx, window, |window, cx| {
        theme::set(theme::Choice::Dark, window, cx).unwrap()
    });
    assert_ne!(cx.update(|cx| theme::backdrop(cx).unwrap().image.id), day);

    // 自分の画像。読めない画像は、前のままにして理由を出す
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("ない.png");
    let broken = dir.path().join("壊れた.png");
    std::fs::write(&broken, b"png?").unwrap();
    for bad in [missing, broken] {
        ui(cx, window, |window, cx| {
            view.update(cx, |v, cx| {
                v.set_scene(Scene::File(bad.clone()), window, cx)
            })
        });
        let error = cx.update(|cx| view.read(cx).error.clone()).unwrap();
        assert!(error.contains(&bad.display().to_string()), "{error}");
        assert_eq!(saved(cx, "scene").as_deref(), Some("sky"));
    }
    let own = dir.path().join("海.png");
    image::RgbaImage::from_pixel(8, 8, image::Rgba([20, 80, 160, 255]))
        .save(&own)
        .unwrap();
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| {
            v.set_scene(Scene::File(own.clone()), window, cx)
        })
    });
    assert!(cx.update(|cx| view.read(cx).error.is_none()));
    assert_eq!(cx.update(|cx| theme::scene(cx)), Scene::File(own.clone()));
    assert_eq!(cx.update(|cx| theme::scene(cx).label()), "海.png");
    // 画像がなくなっていたら、次の起動では画像なしで始めて、理由を知らせる
    std::fs::remove_file(&own).unwrap();
    cx.update(theme::init);
    assert!(cx.update(|cx| theme::backdrop(cx).is_none()));
    assert!(cx.update(theme::take_failure).unwrap().contains("海.png"));

    // 標準に戻すと、画像も、画像に合わせた色もなくなる
    ui(cx, window, |window, cx| {
        window.click(("customize-nav", 2usize), cx)
    });
    ui(cx, window, |window, cx| window.click("scene-reset", cx));
    assert_eq!(saved(cx, "scene"), None);
    assert!(cx.update(|cx| theme::backdrop(cx).is_none()));
    ui(cx, window, |window, cx| {
        theme::set(theme::Choice::Light, window, cx).unwrap()
    });
    assert_eq!(primary(cx), "#86600f");
}

/// 「カスタマイズ」のいちばん上でテーマを選ぶと、画像・色・カードがまとめて変わる。
/// 今の見た目は自分のテーマとして保存でき、名前を変えられ、消せる。
#[gpui_kit::test]
fn themes_are_chosen_and_saved_in_settings(cx: &mut TestAppContext) {
    use gpui_kit::component::ActiveTheme;
    use malakt_mahoz::theme::{self, Choice, Color, Preset, Scene};
    start(cx);
    let (window, view) = open(cx, wide());
    let saved = |cx: &mut TestAppContext, key: &str| store(cx, |s| s.setting(key).unwrap());
    let hex = |color: gpui_kit::Hsla| Color::from(color).hex();
    let stock: Vec<String> = Preset::recommended()
        .iter()
        .map(|p| p.name.clone())
        .collect();
    assert_eq!(stock[..3], ["標準", "ガラスと青い空", "ダークフォレスト"]);

    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.open_appearance(window, cx))
    });
    // 開くとテーマの見出しが上に見えている。下の見出しは、左で押すとそこまでスクロールする
    ui(cx, window, |window, cx| {
        assert!(window.find(("preset", 0usize)).visible());
        assert!(!window.find("font-open").visible());
        window.click(("customize-nav", 3usize), cx)
    });
    ui(cx, window, |window, cx| {
        assert!(window.find("font-open").visible());
        assert!(!window.find(("preset", 0usize)).visible());
        window.click(("customize-nav", 0usize), cx)
    });

    // ダークフォレスト: 森の画像、ダーク、浮かせたパネル
    ui(cx, window, |window, cx| {
        window.click(("preset", 2usize), cx)
    });
    assert_eq!(saved(cx, "scene").as_deref(), Some("forest"));
    assert_eq!(saved(cx, "theme").as_deref(), Some("dark"));
    assert_eq!(saved(cx, "panels.float").as_deref(), Some("1"));
    assert_eq!(saved(cx, "scene.clear").as_deref(), Some("45"));
    assert!(cx.update(|cx| cx.theme().is_dark() && theme::backdrop(cx).is_some()));
    assert!(cx.update(|cx| (theme::backdrop(cx).unwrap().alpha - 0.55).abs() < 0.001));
    assert!(cx.update(|cx| Preset::recommended()[2].same_look(&theme::current("", cx))));

    // 色だけのテーマ（さくら）は、画像を外して色を変える
    let sakura = stock.iter().position(|n| n == "さくら").unwrap();
    ui(cx, window, |window, cx| {
        window.click(("preset", sakura), cx)
    });
    assert_eq!(saved(cx, "scene"), None);
    assert_eq!(saved(cx, "theme").as_deref(), Some("light"));
    assert_eq!(saved(cx, "panels.float"), None);
    assert!(cx.update(|cx| theme::backdrop(cx).is_none()));
    assert_eq!(hex(cx.update(|cx| cx.theme().background)), "#fdf1f5");
    assert_eq!(hex(cx.update(|cx| cx.theme().primary)), "#c2477a");

    // 少し変えてから保存すると、「マイテーマ 1」ができて、そのまま名前を書ける
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.set_cards(true, cx));
        window.click("preset-save", cx)
    });
    assert_eq!(
        cx.update(|cx| view.read(cx).preset_naming),
        Some(0),
        "保存したテーマの名前を書いている"
    );
    ui(cx, window, |window, cx| window.input("桜とカード", cx));
    ui(cx, window, |window, cx| window.press("enter", cx));
    let own = cx.update(|cx| theme::own_presets(cx).unwrap());
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].name, "桜とカード");
    assert!(own[0].cards && own[0].mode == Some(Choice::Light));
    assert_eq!(own[0].look.accent, Color::parse("#c2477a"));

    // 標準に戻してから自分のテーマを選ぶと、保存したときの見た目に戻る
    ui(cx, window, |window, cx| {
        window.click(("preset", 0usize), cx)
    });
    assert_eq!(saved(cx, "color.accent"), None);
    assert_eq!(saved(cx, "list.cards"), None);
    assert_eq!(hex(cx.update(|cx| cx.theme().primary)), "#86600f");
    ui(cx, window, |window, cx| {
        window.click(("own-preset", 0usize), cx)
    });
    assert_eq!(saved(cx, "list.cards").as_deref(), Some("1"));
    assert_eq!(hex(cx.update(|cx| cx.theme().background)), "#fdf1f5");

    // 名前を押すと書き直せる。Esc は書きかけだけを捨てて、ダイアログは開いたまま
    ui(cx, window, |window, cx| {
        window.click(("own-preset-name", 0usize), cx)
    });
    ui(cx, window, |window, cx| window.input("x", cx));
    ui(cx, window, |window, cx| window.press("escape", cx));
    assert!(cx.update(|cx| {
        let v = view.read(cx);
        v.preset_naming.is_none() && v.modal == Modal::Appearance
    }));
    assert_eq!(
        cx.update(|cx| theme::own_presets(cx).unwrap()[0].name.clone()),
        "桜とカード"
    );

    // 開き直しても残る
    ui(cx, window, |window, cx| window.press("escape", cx));
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.open_appearance(window, cx))
    });
    assert_eq!(cx.update(|cx| view.read(cx).own_presets.len()), 1);

    // 画像がなくなったテーマは使えない。前の見た目のまま、理由が出る
    let gone = Preset {
        name: "海".into(),
        scene: Scene::File("/ない/海.png".into()),
        ..own[0].clone()
    };
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.apply_preset(&gone, window, cx))
    });
    let error = cx.update(|cx| view.read(cx).error.clone()).unwrap();
    assert!(
        error.contains("海.png") && error.contains("テーマ「海」"),
        "{error}"
    );
    assert_eq!(saved(cx, "scene"), None);

    // 消しても、今の見た目は変わらない
    ui(cx, window, |window, cx| {
        window.click(("own-preset-delete", 0usize), cx)
    });
    assert_eq!(saved(cx, "presets"), None);
    assert!(cx.update(|cx| view.read(cx).own_presets.is_empty()));
    assert_eq!(hex(cx.update(|cx| cx.theme().background)), "#fdf1f5");
}

/// 「カスタマイズ」でレイアウトを選ぶと、詳細の出し方・一覧の形・パネルの形・AI チャットの並びが変わる。
#[gpui_kit::test]
fn layouts_are_chosen_in_settings(cx: &mut TestAppContext) {
    use malakt_mahoz::theme::{self, Layout};
    start(cx);
    let task = cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            let task = db.store.add_task_in(None, "切符を買う").unwrap();
            cx.notify();
            task.id
        })
    });
    let (window, view) = open(cx, wide());
    let saved = |cx: &mut TestAppContext, key: &str| store(cx, |s| s.setting(key).unwrap());
    let list = |cx: &mut TestAppContext| {
        ui(cx, window, |_, _| ());
        cx.update(|cx| view.read(cx).list_width)
    };
    cx.update(|cx| view.update(cx, |v, cx| v.go(Nav::All, cx)));
    let full = list(cx);
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.open_panel(&task, window, cx))
    });
    // 最初は、詳細が一覧の上に重なり、一覧の幅は変わらない
    assert_eq!(list(cx), full);
    assert!(cx.update(|cx| Layout::PATTERNS[0].shown(false, cx)));
    assert_eq!(Layout::PATTERNS[0].name, "重ねる詳細");

    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.open_appearance(window, cx))
    });
    // テーマとレイアウトは、左のカテゴリで行き来する
    let section = |cx: &mut TestAppContext, ix: usize| {
        ui(cx, window, |window, cx| {
            window.click(("customize-nav", ix), cx)
        });
    };
    let choose = |cx: &mut TestAppContext, name: &str| {
        let ix = Layout::PATTERNS
            .iter()
            .position(|l| l.name == name)
            .unwrap();
        section(cx, 1);
        ui(cx, window, |window, cx| window.click(("layout", ix), cx));
        let chat = cx.update(|cx| view.read(cx).ai_docked(cx));
        assert!(
            cx.update(|cx| Layout::PATTERNS[ix].shown(chat, cx)),
            "{name}"
        );
    };

    // 一覧と詳細: 詳細が一覧の右のカラムで、一覧はその分せまくなる
    choose(cx, "一覧と詳細");
    assert_eq!(saved(cx, "panel.overlay").as_deref(), Some("0"));
    let beside = list(cx);
    assert!(beside < full);
    // テーマを選んでも、詳細の出し方は変わらない
    section(cx, 0);
    ui(cx, window, |window, cx| {
        window.click(("preset", 1usize), cx)
    });
    assert_eq!(saved(cx, "panel.overlay").as_deref(), Some("0"));
    ui(cx, window, |window, cx| {
        window.click(("preset", 0usize), cx)
    });

    // 重ねる詳細: 一覧は全幅のままで、詳細がその上に乗る
    choose(cx, "重ねる詳細");
    assert_eq!(saved(cx, "panel.overlay"), None);
    assert_eq!(list(cx), full);

    // カードと浮いたパネルも、詳細は重ねて一覧の幅を変えない
    choose(cx, "カード");
    assert_eq!(saved(cx, "panel.overlay"), None);
    assert_eq!(saved(cx, "list.cards").as_deref(), Some("1"));
    assert_eq!(list(cx), full);

    choose(cx, "浮いたパネル");
    assert_eq!(saved(cx, "list.cards"), None);
    assert_eq!(saved(cx, "panels.float").as_deref(), Some("1"));
    assert_eq!(list(cx), full);

    // 3 カラムは AI チャットも右に並べ、「一覧と詳細」はチャットを閉じる
    choose(cx, "3 カラム");
    assert_eq!(saved(cx, "panels.float"), None);
    assert_eq!(saved(cx, "panel.overlay").as_deref(), Some("0"));
    assert!(cx.update(|cx| view.read(cx).ai_docked(cx)));
    // チャットを開いても、Esc は「カスタマイズ」を閉じる
    assert!(cx.update(|cx| view.read(cx).modal == Modal::Appearance));
    choose(cx, "一覧と詳細");
    assert!(!cx.update(|cx| view.read(cx).ai_docked(cx)));
    ui(cx, window, |window, cx| window.press("escape", cx));
    assert!(cx.update(|cx| view.read(cx).modal == Modal::None));

    // 起動し直しても残る
    cx.update(|cx| theme::apply_layout(&Layout::PATTERNS[1], cx).unwrap());
    cx.update(theme::init);
    assert_eq!(saved(cx, "panel.overlay").as_deref(), Some("0"));
    assert!(!cx.update(|cx| theme::overlay(cx)));
    // 前の版で「重ねる詳細」を選んで保存した値も、重ねるとして読む
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            db.store.set_setting("panel.overlay", Some("1")).unwrap()
        })
    });
    assert!(cx.update(|cx| theme::overlay(cx)));
}

/// 「カスタマイズ」は開くとすぐ設定を検索でき、合う設定だけがカテゴリごとに出る。
/// 左のカテゴリを押すと検索をやめてそのカテゴリを出し、Esc で閉じる。
#[gpui_kit::test]
fn customize_settings_are_searched(cx: &mut TestAppContext) {
    use malakt_mahoz::state::CustomizeSection;
    start(cx);
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.open_appearance(window, cx))
    });
    let nav =
        |window: &gpui_kit::Window, ix: usize| window.try_find(("customize-nav", ix)).is_some();
    ui(cx, window, |window, _| {
        // 開いたときはテーマ。左にはカテゴリがすべて並ぶ
        assert!(window.try_find(("preset", 0usize)).is_some());
        // この OS にない連携（通知・アップデート）のほかは、カテゴリがすべて並ぶ
        assert!((0..=8).all(|ix| nav(window, ix)));
    });

    // 書いた言葉に合う設定だけが出る。補足の言葉でも当たる
    ui(cx, window, |window, cx| window.input("行の間隔", cx));
    ui(cx, window, |window, _| {
        assert!(window.try_find(("density", 0usize)).is_some());
        assert!(window.try_find(("feel", 0usize)).is_none());
        assert!(window.try_find(("preset", 0usize)).is_none());
        assert!(nav(window, 4) && !nav(window, 0));
    });

    // いくつものカテゴリに合うときは、カテゴリごとに並ぶ
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| {
            v.inputs
                .customize_search
                .update(cx, |s, cx| s.set_value("カード", window, cx))
        })
    });
    ui(cx, window, |window, _| {
        assert!(window.try_find("cards-switch").is_some());
        assert!(window.try_find("float-switch").is_some());
        // 「カード」というレイアウトがある
        assert!(window.try_find(("layout", 0usize)).is_some());
        assert!(window.try_find("font-open").is_none());
    });

    // 何にも合わなければ、そう出る
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| {
            v.inputs
                .customize_search
                .update(cx, |s, cx| s.set_value("存在しない設定", window, cx))
        })
    });
    ui(cx, window, |window, _| {
        assert!((0..CustomizeSection::ALL.len()).all(|ix| !nav(window, ix)));
        assert!(window.try_find("cards-switch").is_none());
    });

    // カテゴリを押すと、検索をやめてそのカテゴリだけを出す
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| {
            v.inputs
                .customize_search
                .update(cx, |s, cx| s.set_value("シェル", window, cx))
        })
    });
    ui(cx, window, |window, cx| {
        assert!(!nav(window, 0));
        window.click(("customize-nav", 6usize), cx)
    });
    assert!(cx.update(|cx| {
        let v = view.read(cx);
        v.appearance_section == CustomizeSection::AiBehavior
            && v.inputs.customize_search.read(cx).value().is_empty()
    }));
    ui(cx, window, |window, _| {
        assert!(window.try_find("shell-choice").is_some());
        // この OS にない連携（通知・アップデート）のほかは、カテゴリがすべて並ぶ
        assert!((0..=8).all(|ix| nav(window, ix)));
    });

    // 検索欄に書いている途中でも、Esc で閉じる
    ui(cx, window, |window, cx| window.input("色", cx));
    ui(cx, window, |window, cx| window.press("escape", cx));
    assert!(cx.update(|cx| view.read(cx).modal == Modal::None));
}

/// 1920 幅の画面でも、3 カラムにすると一覧・詳細・AI チャットの 3 つが並ぶ。
#[gpui_kit::test]
fn three_columns_fit_a_1920_wide_screen(cx: &mut TestAppContext) {
    use malakt_mahoz::state::PANEL_MIN;
    use malakt_mahoz::theme::Layout;
    start(cx);
    let task = cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            let task = db.store.add_task_in(None, "切符を買う").unwrap();
            cx.notify();
            task.id
        })
    });
    let scale = malakt_mahoz::UI_SCALE;
    // 実際の画面の 1920×1080
    let (window, view) = open(cx, size(px(1920.), px(1080.)));
    cx.update(|cx| view.update(cx, |v, cx| v.go(Nav::All, cx)));
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| {
            v.open_panel(&task, window, cx);
            let three = Layout::PATTERNS
                .iter()
                .find(|l| l.name == "3 カラム")
                .unwrap();
            v.apply_layout(three, window, cx);
        })
    });
    ui(cx, window, |_, _| ());
    assert!(cx.update(|cx| view.read(cx).ai_docked(cx)));
    ui(cx, window, |window, _| {
        let list = window.find(("title", 0usize)).bounds();
        let detail = window.find("detail-panel").bounds();
        let chat = window.find("ai-input").bounds();
        // 左から、一覧・詳細・チャットの順に、重ならずに並ぶ
        assert!(list.right() <= detail.left(), "{list:?} {detail:?}");
        assert!(detail.right() <= chat.left(), "{detail:?} {chat:?}");
        assert!(chat.right() <= px(1920.));
        assert!(detail.size.width + px(1.) >= px(PANEL_MIN * scale));
    });
    // 一覧には、タイトルを読める幅が残る
    assert!(cx.update(|cx| view.read(cx).list_width) >= 400.);
}

/// 密度を変えると一覧の行の高さが変わる。エディタ風のテーマは行を詰め、
/// この PC に等幅の書体があればそれを使い、なければ標準の書体のままにする。
#[gpui_kit::test]
fn density_and_editor_themes(cx: &mut TestAppContext) {
    use malakt_mahoz::theme::{self, Density, Preset};
    start(cx);
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            db.store.add_task_in(None, "切符を買う").unwrap();
            cx.notify();
        })
    });
    let (window, view) = open(cx, wide());
    let saved = |cx: &mut TestAppContext, key: &str| store(cx, |s| s.setting(key).unwrap());
    cx.update(|cx| view.update(cx, |v, cx| v.go(Nav::All, cx)));
    // 表の 1 行の高さ
    let row = |cx: &mut TestAppContext| {
        ui(cx, window, |_, _| ());
        cx.update(|cx| view.read(cx).row_sizes[0].1)
    };
    let standard = row(cx);

    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.open_appearance(window, cx))
    });
    ui(cx, window, |window, cx| {
        window.click(("customize-nav", 4usize), cx)
    });
    ui(cx, window, |window, cx| {
        window.click(("density", 2usize), cx)
    });
    assert_eq!(saved(cx, "density").as_deref(), Some("compact"));
    let compact = row(cx);
    assert!(compact < standard, "{compact:?} {standard:?}");
    ui(cx, window, |window, cx| {
        window.click(("density", 0usize), cx)
    });
    assert_eq!(saved(cx, "density").as_deref(), Some("roomy"));
    assert!(row(cx) > standard);
    ui(cx, window, |window, cx| {
        window.click(("density", 1usize), cx)
    });
    assert_eq!(saved(cx, "density"), None);
    assert_eq!(row(cx), standard);
    // 起動し直しても残る
    cx.update(|cx| theme::set_density(Density::Compact, cx).unwrap());
    assert_eq!(cx.update(|cx| theme::density(cx)), Density::Compact);
    assert_eq!(row(cx), compact);
    cx.update(|cx| theme::set_density(Density::Standard, cx).unwrap());

    // エディタ風: 等幅の書体が入っていれば、それを使って行を詰める
    let editor = Preset::recommended()
        .iter()
        .position(|p| p.name == "エディタ風")
        .unwrap();
    let pick = |cx: &mut TestAppContext, fonts: &[&str]| {
        let fonts: Vec<String> = fonts.iter().map(|f| f.to_string()).collect();
        ui(cx, window, |window, cx| {
            view.update(cx, |v, cx| {
                v.open_appearance(window, cx);
                // この PC に入っている書体の代わり
                v.fonts = fonts;
                cx.notify();
            })
        });
        ui(cx, window, |window, cx| {
            window.click(("preset", editor), cx)
        });
    };
    pick(cx, &["LINE Seed JP", "Menlo", "Consolas"]);
    assert_eq!(saved(cx, "density").as_deref(), Some("compact"));
    // 候補の並びで先にあるものを使う
    assert_eq!(saved(cx, "font.family").as_deref(), Some("Consolas"));
    assert_eq!(saved(cx, "theme").as_deref(), Some("dark"));
    assert_eq!(row(cx), compact);

    // 等幅の書体がなければ、標準の書体のまま
    pick(cx, &["LINE Seed JP", "Noto Serif"]);
    assert_eq!(saved(cx, "font.family"), None);
    assert_eq!(saved(cx, "density").as_deref(), Some("compact"));

    // 標準のテーマに戻すと、密度も書体も戻る
    ui(cx, window, |window, cx| {
        window.click(("preset", 0usize), cx)
    });
    assert_eq!(saved(cx, "density"), None);
    assert_eq!(row(cx), standard);
}

/// 乗せたとき・押したときの描き方（ホバー）は「カスタマイズ」の詳細で選べ、テーマにも含まれる。
#[gpui_kit::test]
fn hover_feel_follows_the_theme(cx: &mut TestAppContext) {
    use malakt_mahoz::theme::{self, Feel, Preset};
    start(cx);
    let (window, view) = open(cx, wide());
    let saved = |cx: &mut TestAppContext| store(cx, |s| s.setting("feel").unwrap());
    let feel = |cx: &mut TestAppContext| cx.update(|cx| theme::feel(cx));
    assert_eq!(feel(cx), Feel::Standard);

    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.open_appearance(window, cx))
    });
    // テーマを選ぶと、そのテーマの描き方になる
    let presets = Preset::recommended();
    let pick = |cx: &mut TestAppContext, name: &str| {
        let ix = presets.iter().position(|p| p.name == name).unwrap();
        ui(cx, window, |window, cx| window.click(("preset", ix), cx));
    };
    pick(cx, "ガラスと青い空");
    assert_eq!(feel(cx), Feel::Glass);
    assert_eq!(saved(cx).as_deref(), Some("glass"));
    pick(cx, "エディタ風");
    assert_eq!(feel(cx), Feel::Crisp);
    pick(cx, "さくら");
    assert_eq!(feel(cx), Feel::Tint);
    pick(cx, "標準");
    assert_eq!(feel(cx), Feel::Standard);
    assert_eq!(saved(cx), None);

    // 「表示」の「ホバー」でも選べる
    ui(cx, window, |window, cx| {
        window.click(("customize-nav", 4usize), cx)
    });
    for (ix, key) in [(1usize, "glass"), (2, "crisp"), (3, "tint")] {
        ui(cx, window, |window, cx| window.click(("feel", ix), cx));
        assert_eq!(saved(cx).as_deref(), Some(key));
        assert_eq!(feel(cx), Feel::ALL[ix]);
    }
    ui(cx, window, |window, cx| window.click(("feel", 0usize), cx));
    assert_eq!(saved(cx), None);
    assert_eq!(feel(cx), Feel::Standard);

    // 自分のテーマにも入る
    cx.update(|cx| theme::set_feel(Feel::Glass, cx).unwrap());
    assert_eq!(cx.update(|cx| theme::current("x", cx)).feel, Feel::Glass);
    cx.update(|cx| theme::set_feel(Feel::Standard, cx).unwrap());
}

/// アイコンは検索して選べる。日本語でも探せ、見つからなければその旨が出る。
#[gpui_kit::test]
fn workspace_icon_is_found_by_search(cx: &mut TestAppContext) {
    start(cx);
    let ws = cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            let ws = db.store.create_workspace(None, "旅行").unwrap();
            cx.notify();
            ws.id
        })
    });
    let (window, view) = open(cx, wide());
    // 開いているワークスペースの行には「…」が出ている
    cx.update(|cx| view.update(cx, |v, cx| v.go(Nav::Workspace(ws.clone()), cx)));
    ui(cx, window, |window, cx| {
        window.click(("ws-more", 0usize), cx)
    });
    ui(cx, window, |window, cx| window.press("down", cx));
    ui(cx, window, |window, cx| window.press("enter", cx));
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::WorkspaceIcon(ws.clone())));

    // 同梱の 48 個にない「飛行機の離陸」を名前で探して選ぶ
    ui(cx, window, |window, cx| {
        window.click("icon-search", cx);
        window.input("plane-takeoff", cx);
    });
    ui(cx, window, |window, cx| {
        window.click(("ws-icon", 0usize), cx)
    });
    let saved = store(cx, |s| s.workspace(&ws).unwrap());
    assert_eq!(saved.icon.as_deref(), Some("plane-takeoff"));

    // 日本語でも探せる
    ui(cx, window, |window, cx| {
        window.click("icon-search", cx);
        window.press("secondary-a", cx);
        window.input("猫", cx);
    });
    ui(cx, window, |window, cx| {
        window.click(("ws-icon", 0usize), cx)
    });
    let saved = store(cx, |s| s.workspace(&ws).unwrap());
    assert_eq!(saved.icon.as_deref(), Some("cat"));

    // カテゴリで絞る。「動物・自然」の中で、ひらがなで探して絵文字のペンギンを選ぶ
    ui(cx, window, |window, cx| {
        window.click(("icon-category", 0usize), cx)
    });
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).icon_category,
            Some(malakt_mahoz::icons::Category::Nature)
        )
    });
    ui(cx, window, |window, cx| {
        window.click("icon-search", cx);
        window.press("secondary-a", cx);
        window.input("ぺんぎん", cx);
    });
    ui(cx, window, |window, cx| {
        window.click(("ws-icon", 0usize), cx)
    });
    let saved = store(cx, |s| s.workspace(&ws).unwrap());
    assert_eq!(saved.icon.as_deref(), Some("emoji-penguin"));
    // ほかのカテゴリに切り替える
    ui(cx, window, |window, cx| {
        window.click(("icon-category", 1usize), cx)
    });
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).icon_category,
            Some(malakt_mahoz::icons::Category::Food)
        )
    });
    // もう一度押すとカテゴリが外れる
    ui(cx, window, |window, cx| {
        window.click(("icon-category", 1usize), cx)
    });
    cx.update(|cx| assert_eq!(view.read(cx).icon_category, None));
    // どのカテゴリのボタンも押せて、その中の先頭のアイコンを選べる
    ui(cx, window, |window, cx| {
        window.click("icon-search", cx);
        window.press("secondary-a", cx);
        window.press("backspace", cx);
    });
    for (ix, category) in malakt_mahoz::icons::Category::ALL.into_iter().enumerate() {
        ui(cx, window, |window, cx| {
            window.click(("icon-category", ix), cx)
        });
        cx.update(|cx| assert_eq!(view.read(cx).icon_category, Some(category)));
        ui(cx, window, |window, cx| {
            window.click(("ws-icon", 0usize), cx)
        });
        let saved = store(cx, |s| s.workspace(&ws).unwrap());
        let first = malakt_mahoz::icons::search("", Some(category), &[], 1);
        assert_eq!(saved.icon.as_deref(), first.first().copied());
    }
}

/// 未読の数の丸は、1 桁なら正円、2 桁以上は高さのまま横に伸びる。100 以上は「99+」。
/// 上のバーのベル、左のメニューの「通知」、畳んだ列のベルのどれも同じ。
#[gpui_kit::test]
fn unread_bubble_is_round(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    let set = |cx: &mut TestAppContext, n: u32| {
        cx.update(|cx| {
            view.update(cx, |v, cx| {
                v.unread_notices = n;
                cx.notify();
            })
        });
        cx.run_until_parked();
    };
    let ids = ["open-notices-count", "nav-notices-count"];
    set(cx, 0);
    ui(cx, window, |window, _| {
        for id in ids {
            assert!(window.try_find(id).is_none(), "{id}");
        }
    });
    let mut widths = std::collections::HashMap::new();
    for n in [1, 12, 150] {
        set(cx, n);
        ui(cx, window, |window, _| {
            for id in ids {
                let size = window.find(id).bounds().size;
                assert!(size.height > px(0.), "{id}");
                if n < 10 {
                    assert_eq!(size.width, size.height, "{id} {n}");
                } else {
                    assert!(size.width > size.height, "{id} {n}");
                    // 横に伸びても高さは同じ
                    assert_eq!(size.height, widths[&(id, 1)], "{id} {n}");
                }
                widths.insert((id, n), size.width);
            }
        });
    }
    for id in ids {
        assert!(widths[&(id, 150)] > widths[&(id, 12)], "{id}");
    }
    // 畳んだ列のベルも同じ丸
    set(cx, 3);
    ui(cx, window, |window, cx| window.click("toggle-sidebar", cx));
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(400));
    ui(cx, window, |window, _| {
        let size = window.find("rail-notices-count").bounds().size;
        assert_eq!(size.width, size.height);
        assert_eq!(size, window.find("open-notices-count").bounds().size);
    });
}

/// 上のバーのメニューのボタンで左のメニューをアイコンだけの列に畳んで、また広げる。
/// 境界で変えた幅は保存される。
#[gpui_kit::test]
fn sidebar_toggles_and_keeps_its_width(cx: &mut TestAppContext) {
    use malakt_mahoz::state::{RAIL_WIDTH, SIDEBAR_MAX, SIDEBAR_WIDTH};
    start(cx);
    let ws = cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            db.store.create_workspace(None, "文化祭").unwrap().id
        })
    });
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| {
        assert!(window.try_find("nav-all").is_some());
        assert!(window.try_find("rail").is_none());
        window.click("toggle-sidebar", cx);
    });
    // 動きが終わるまで進める
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(400));
    let scale = malakt_mahoz::UI_SCALE;
    ui(cx, window, |window, _| {
        assert!(window.try_find("nav-all").is_none());
        assert!(window.try_find("sidebar-resize").is_none());
        // 畳んだ列は細く、アイコンだけが並ぶ
        let rail = window.find("rail").bounds();
        assert_eq!(rail.size.width, px(RAIL_WIDTH * scale));
        for id in ["rail-home", "rail-notices", "rail-all", "rail-unfiled"] {
            assert!(window.try_find(id).is_some(), "{id}");
        }
        assert_eq!(window.find("rail-all").label(), Some("すべてのタスク"));
        assert_eq!(window.find(("rail-ws", 0usize)).label(), Some("文化祭"));
    });
    // アイコンを押すと、その画面に移る。畳んだまま
    ui(cx, window, |window, cx| {
        window.click(("rail-ws", 0usize), cx)
    });
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.nav, Nav::Workspace(ws.clone()));
        assert!(v.sidebar_hidden);
    });
    ui(cx, window, |window, cx| window.click("rail-all", cx));
    cx.update(|cx| assert_eq!(view.read(cx).nav, Nav::All));

    ui(cx, window, |window, cx| window.click("toggle-sidebar", cx));
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(400));
    ui(cx, window, |window, _| {
        assert!(window.try_find("rail").is_none());
        assert!(window.try_find("nav-all").is_some());
        assert!(window.try_find("sidebar-resize").is_some());
    });

    // 畳んだ列から名前を変えるときは、名前を書く行が見えるように広げる
    ui(cx, window, |window, cx| window.click("toggle-sidebar", cx));
    cx.update(|cx| assert!(view.read(cx).sidebar_hidden));
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |v, cx| v.rename_workspace(ws.clone(), window, cx))
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert!(!view.read(cx).sidebar_hidden));
    ui(cx, window, |window, _| {
        assert!(window.try_find("workspace-name-input").is_some())
    });

    // 境界をドラッグした幅は、範囲の中に収めて保存する
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            assert_eq!(v.sidebar_width, SIDEBAR_WIDTH);
            v.sidebar_resizing = true;
            v.drag_sidebar(320., cx);
            v.drag_sidebar(9999., cx);
            v.finish_sidebar_drag(cx);
            assert_eq!(v.sidebar_width, SIDEBAR_MAX);
            assert!(!v.sidebar_resizing);
        })
    });
    let saved = store(cx, |s| s.setting("sidebar.width").unwrap());
    assert_eq!(saved.as_deref(), Some(SIDEBAR_MAX.to_string().as_str()));
    // 新しく開いたウィンドウも同じ幅で始まる
    let (_second, second_view) = open(cx, wide());
    cx.update(|cx| assert_eq!(second_view.read(cx).sidebar_width, SIDEBAR_MAX));
}

/// 表の担当の欄は、人が多くて折り返しても、頭文字の丸を上下で切らない。行に入る段だけ出す。
#[gpui_kit::test]
fn assignee_cell_wraps_without_cutting(cx: &mut TestAppContext) {
    start(cx);
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let s = &mut db.store;
            let ids = ["田中 一郎", "Alice Johnson", "佐藤 花子"]
                .map(|name| s.create_person(name, false).unwrap().id)
                .to_vec();
            let draft = malakt_core::TaskDraft {
                title: "受付".into(),
                assignee_ids: ids,
                ..Default::default()
            };
            s.create_task(None, &draft).unwrap();
        })
    });
    let (window, view) = open(cx, wide());
    cx.update(|cx| view.update(cx, |v, cx| v.go(Nav::All, cx)));
    let rem = cx.update(|cx| {
        use gpui_kit::component::ActiveTheme;
        cx.theme().font_size
    });
    // 頭文字の丸 1 段（size_6）と、段の間（gap_y_1）
    let (line, gap) = (rem * 1.5, rem * 0.25);
    // 担当のボタンの高さと、表の 1 行の高さ
    let heights = |cx: &mut TestAppContext| {
        let mut cell = px(0.);
        ui(cx, window, |window, _| {
            cell = window.find(("assignee", 0usize)).bounds().size.height;
        });
        let density = cx.update(|cx| view.read(cx).row_density);
        (cell, px(60. * malakt_mahoz::UI_SCALE * density.rows()))
    };
    // 標準の詰め方では 2 段に折り返し、ボタンは 2 段の高さまで伸びて、行の中に収まる
    let (cell, row) = heights(cx);
    assert!(cell >= line * 2. + gap, "{cell:?}");
    assert!(cell <= row, "{cell:?} > {row:?}");
    // 詰めると 2 段は入らないので、1 段だけ出す
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.set_density(malakt_mahoz::theme::Density::Compact, cx)
        })
    });
    let (cell, row) = heights(cx);
    assert!(cell >= line, "{cell:?}");
    assert!(cell < line * 2. + gap, "{cell:?}");
    assert!(cell <= row, "{cell:?} > {row:?}");
}

/// 表の値（状態・優先度・担当・期限）は、セルの左から並ぶ。列を狭めても、ボタンの左右の外へははみ出さない。
#[gpui_kit::test]
fn table_values_start_at_the_left_of_the_cell(cx: &mut TestAppContext) {
    use malakt_mahoz::state::{Column, COLUMN_MIN};
    start(cx);
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| {
        window.click("nav-all", cx);
        window.click("quick-add", cx);
        window.input("会場を決める", cx);
        window.press("enter", cx);
    });
    let scale = malakt_mahoz::UI_SCALE;
    let check = |cx: &mut TestAppContext| {
        ui(cx, window, |window, _| {
            for id in ["status", "prio", "assignee", "due"] {
                let button = window.find((id, 0usize)).bounds();
                let value = window.within((id, 0usize)).find("value").bounds();
                // ボタンの左の余白（px_2）だけ空けて、左から始まる
                assert!(value.left() >= button.left(), "{id}: {value:?} {button:?}");
                assert!(
                    value.left() - button.left() <= px(12. * scale),
                    "{id}: {value:?} {button:?}"
                );
                assert!(
                    value.right() <= button.right(),
                    "{id}: {value:?} {button:?}"
                );
            }
        });
    };
    check(cx);
    // 担当の列をいちばん狭くしても、「未割り当て」は真ん中から両側へはみ出さない
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.start_column_resize(Column::Assignee, 0., cx);
            v.drag_column(9999., cx);
            v.finish_column_resize(cx);
            let width = v.columns.iter().find(|c| c.column == Column::Assignee);
            assert_eq!(width.unwrap().width, COLUMN_MIN);
        })
    });
    check(cx);
}

/// 左のメニューの矢印は、小さくても押しやすい大きさで押せる。
#[gpui_kit::test]
fn tree_caret_has_room_to_press(cx: &mut TestAppContext) {
    start(cx);
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let ws = db.store.create_workspace(None, "文化祭").unwrap();
            db.store.create_workspace(Some(&ws.id), "設営").unwrap();
        })
    });
    let scale = malakt_mahoz::UI_SCALE;
    let (window, _view) = open(cx, wide());
    ui(cx, window, |window, cx| {
        let caret = window.find(("caret", 0usize)).bounds();
        assert!(caret.size.width >= px(22. * scale), "{caret:?}");
        assert!(caret.size.height >= px(28. * scale), "{caret:?}");
        // 「＋」と「…」も、押しやすい大きさがある
        for id in [
            gpui_kit::ElementId::from("add-workspace"),
            ("ws-more", 0usize).into(),
        ] {
            let size = window.find(id.clone()).bounds().size;
            assert!(
                size.width.min(size.height) >= px(20. * scale),
                "{id:?}: {size:?}"
            );
        }
        // 行の名前とは重ならず、押すとたたむ
        let name = window.find(("ws", 0usize)).bounds();
        assert!(caret.right() <= name.left());
        assert!(window.try_find(("ws", 1usize)).is_some());
        window.click(("caret", 0usize), cx);
    });
    ui(cx, window, |window, _| {
        assert!(window.try_find(("ws", 1usize)).is_none());
    });
}

/// 「一覧と詳細」のタスクの詳細は、左の境界をドラッグして幅を変えられる。幅は保存され、一覧より広くはならない。
#[gpui_kit::test]
fn detail_panel_resizes_and_keeps_its_width(cx: &mut TestAppContext) {
    use malakt_mahoz::state::{PANEL_MAX, PANEL_MIN, PANEL_WIDTH};
    start(cx);
    // 詳細を一覧の右のカラムにする「一覧と詳細」のレイアウトで確かめる
    cx.update(|cx| malakt_mahoz::theme::set_overlay(false, cx).unwrap());
    let scale = malakt_mahoz::UI_SCALE;
    let (window, view) = open(cx, win(1600., 800.));
    ui(cx, window, |window, cx| {
        window.click("nav-all", cx);
        window.click("quick-add", cx);
        window.input("会場を決める", cx);
        window.press("enter", cx);
    });
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    let width = |cx: &mut TestAppContext| {
        let mut width = px(0.);
        ui(cx, window, |window, _| {
            // 左の枠の 1px を足すと、カラムの幅になる
            width = window.find("detail-panel").bounds().size.width + px(1.);
        });
        width
    };
    assert_eq!(width(cx), px(PANEL_WIDTH * scale));
    // 詳細は一覧の右に並ぶ。一覧の行は詳細の下に隠れない
    let beside = |cx: &mut TestAppContext| {
        ui(cx, window, |window, _| {
            let panel = window.find("detail-panel").bounds();
            let row = window.find(("title", 0usize)).bounds();
            assert!(row.right() <= panel.left(), "一覧の行が詳細に隠れる");
            assert!(row.size.width >= px(120. * scale), "一覧の行が狭すぎる");
            let add = window.find("new-task-open").bounds();
            assert!(add.right() <= panel.left(), "一覧のボタンが詳細に隠れる");
        });
    };
    beside(cx);

    // 境界を左へ動かすと広がり、右へ動かすと狭まる。範囲の外には出ない
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.start_panel_resize(1000., cx);
            v.drag_panel(1000. - 100. * scale, cx);
            assert_eq!(v.panel_width, PANEL_WIDTH + 100.);
            v.drag_panel(99999., cx);
            assert_eq!(v.panel_width, PANEL_MIN);
            v.drag_panel(1000. - 200. * scale, cx);
            v.finish_panel_resize(cx);
            assert_eq!(v.panel_width, PANEL_WIDTH + 200.);
            assert!(v.panel_resize.is_none());
        })
    });
    assert_eq!(width(cx), px((PANEL_WIDTH + 200.) * scale));
    let saved = store(cx, |s| s.setting("panel.width").unwrap());
    assert_eq!(
        saved.as_deref(),
        Some((PANEL_WIDTH + 200.).to_string().as_str())
    );

    // いちばん広くしても、一覧の幅に収まる
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.start_panel_resize(1000., cx);
            v.drag_panel(-99999., cx);
            v.finish_panel_resize(cx);
            assert_eq!(v.panel_width, PANEL_MAX);
        })
    });
    beside(cx);

    // 一覧を並べる幅がないウィンドウでは、詳細が全面を使う。境界は出ない
    let (narrow, narrow_view) = open(cx, win(900., 800.));
    ui(cx, narrow, |window, cx| {
        narrow_view.update(cx, |v, cx| v.go(Nav::All, cx));
        window.render_frame(cx);
        window.click(("title", 0usize), cx);
    });
    ui(cx, narrow, |window, _| {
        let panel = window.find("detail-panel").bounds();
        let sidebar = malakt_mahoz::state::SIDEBAR_WIDTH;
        assert_eq!(panel.size.width, px((900. - sidebar) * scale));
        assert!(window.try_find("panel-resize").is_none());
    });
    cx.update_window(narrow, |_, window, _| window.remove_window())
        .unwrap();

    // 新しく開いたウィンドウも同じ幅で始まる。境界のダブルクリックで最初の幅に戻る
    let (_second, second_view) = open(cx, wide());
    cx.update(|cx| {
        assert_eq!(second_view.read(cx).panel_width, PANEL_MAX);
        view.update(cx, |v, cx| v.reset_panel_width(cx));
    });
    assert_eq!(width(cx), px(PANEL_WIDTH * scale));
    let saved = store(cx, |s| s.setting("panel.width").unwrap());
    assert_eq!(saved.as_deref(), Some(PANEL_WIDTH.to_string().as_str()));
}

/// 表の列は、幅を変え、並べ替え、隠せる。見出しのメニューで並び順を変えられる。どれも保存される。
#[gpui_kit::test]
fn table_columns_resize_reorder_hide_and_sort(cx: &mut TestAppContext) {
    use malakt_core::view::Sort;
    use malakt_mahoz::state::{Column, COLUMN_MAX, COLUMN_MIN};
    start(cx);
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| {
        window.click("nav-all", cx);
        window.click("quick-add", cx);
        window.input("会場を決める", cx);
        window.press("enter", cx);
    });
    let head = |column: Column| ("col-head", column as usize);
    ui(cx, window, |window, _| {
        for column in Column::ALL {
            assert!(window.try_find(head(column)).is_some());
            assert!(window.try_find(("col-resize", column as usize)).is_some());
        }
        assert!(window.try_find("col-title").is_some());
    });

    // 左の境界を左へ動かすと広がり、右へ動かすと狭まる。範囲の外には出ない
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            let width =
                |v: &MainView, column| v.columns.iter().find(|c| c.column == column).unwrap().width;
            let scale = malakt_mahoz::UI_SCALE;
            v.start_column_resize(Column::Due, 500., cx);
            v.drag_column(500. - 40. * scale, cx);
            assert_eq!(width(v, Column::Due), Column::Due.default_width() + 40.);
            v.drag_column(-9999., cx);
            assert_eq!(width(v, Column::Due), COLUMN_MAX);
            v.drag_column(9999., cx);
            v.finish_column_resize(cx);
            assert_eq!(width(v, Column::Due), COLUMN_MIN);
            assert!(v.column_resize.is_none());
            // ダブルクリックで最初の幅に戻す
            v.reset_column_width(Column::Due, cx);
            assert_eq!(width(v, Column::Due), Column::Due.default_width());
            v.start_column_resize(Column::Status, 0., cx);
            v.drag_column(-20. * scale, cx);
            v.finish_column_resize(cx);

            // 見出しをほかの列の上まで動かして離すと、そこへ入る
            v.column_drag = Some(malakt_mahoz::state::ColumnDrag {
                column: Column::Updated,
                start_x: 0.,
                moved: true,
                over: Some(Column::Status),
            });
            v.finish_column_drag(cx);
            assert_eq!(v.columns[0].column, Column::Updated);
            // 動かさずに離したら（クリック）、並びは変わらない
            v.column_drag = Some(malakt_mahoz::state::ColumnDrag {
                column: Column::Due,
                start_x: 0.,
                moved: false,
                over: Some(Column::Status),
            });
            v.finish_column_drag(cx);
            assert_eq!(v.columns[4].column, Column::Due);

            v.toggle_column(Column::Assignee, cx);
        })
    });
    ui(cx, window, |window, _| {
        assert!(window.try_find(head(Column::Assignee)).is_none());
        assert!(window.try_find(head(Column::Due)).is_some());
        // 追加のボタンは、どの列を隠しても残る
        assert!(window.try_find("quick-add-submit").is_some());
    });
    let saved = store(cx, |s| s.setting("table.columns").unwrap());
    assert_eq!(
        saved.as_deref(),
        Some("updated:84,status:116,priority:84,-assignee:160,due:150")
    );

    // 新しく開いたウィンドウも同じ列で始まる
    let (second, second_view) = open(cx, wide());
    cx.update(|cx| {
        let v = second_view.read(cx);
        assert_eq!(v.columns[0].column, Column::Updated);
        assert!(v.columns[3].hidden);
    });
    ui(cx, second, |window, cx| window.click("nav-all", cx));
    ui(cx, second, |window, _| {
        assert!(window.try_find(head(Column::Assignee)).is_none())
    });

    // 見出しのメニューの並べ替え。同じものをもう一度選ぶと、最初の並び順に戻る
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.sort_by(Sort::Title, true, cx);
            assert_eq!((v.query.sort, v.query.descending), (Sort::Title, true));
            v.sort_by(Sort::Title, true, cx);
            assert_eq!((v.query.sort, v.query.descending), (Sort::Due, false));
        })
    });
    // 見出しを押すと、その列で並べ替える。押すたびに昇順と降順が入れ替わる
    let sorted = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let v = view.read(cx);
            (v.query.sort, v.query.descending)
        })
    };
    ui(cx, window, |window, cx| window.click("col-title", cx));
    assert_eq!(sorted(cx), (Sort::Title, false));
    cx.update(|cx| assert_eq!(view.read(cx).column_sorted(None), Some(false)));
    ui(cx, window, |window, cx| window.click("col-title", cx));
    assert_eq!(sorted(cx), (Sort::Title, true));
    cx.update(|cx| assert_eq!(view.read(cx).column_sorted(None), Some(true)));
    ui(cx, window, |window, cx| window.click("col-title", cx));
    assert_eq!(sorted(cx), (Sort::Title, false));
    // 更新の昇順は古い順（もとの「新しい順」の逆）
    let updated = ("col-menu", Column::Updated as usize);
    ui(cx, window, |window, cx| window.click(updated, cx));
    assert_eq!(sorted(cx), (Sort::Updated, true));
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).column_sorted(Some(Column::Updated)),
            Some(false)
        )
    });
    ui(cx, window, |window, cx| window.click(updated, cx));
    assert_eq!(sorted(cx), (Sort::Updated, false));
    // 担当の列も並べ替えられる（この表では隠しているので、見出しを押したときの処理を呼ぶ）
    cx.update(|cx| view.update(cx, |v, cx| v.sort_by_column(Some(Column::Assignee), cx)));
    assert_eq!(sorted(cx), (Sort::Assignee, false));

    // 右クリックでメニューが開き、もう一度右クリックすると閉じる
    let right_click = |cx: &mut TestAppContext, id: gpui_kit::ElementId| {
        let mut at = None;
        ui(cx, window, |window, _| {
            at = Some(window.try_find(id).unwrap().bounds().center());
        });
        let mut v = gpui_kit::VisualTestContext::from_window(window, cx);
        v.simulate_mouse_down(at.unwrap(), MouseButton::Right, Modifiers::default());
        v.simulate_mouse_up(at.unwrap(), MouseButton::Right, Modifiers::default());
        v.run_until_parked();
    };
    let menu_of = |cx: &mut TestAppContext| {
        cx.update(|cx| view.read(cx).column_menu.as_ref().map(|m| m.column))
    };
    right_click(cx, "col-title".into());
    assert_eq!(menu_of(cx), Some(None));
    // 右クリックでは並べ替えない
    assert_eq!(sorted(cx), (Sort::Assignee, false));
    right_click(cx, "col-title".into());
    assert_eq!(menu_of(cx), None);
    right_click(cx, ("col-menu", Column::Due as usize).into());
    assert_eq!(menu_of(cx), Some(Some(Column::Due)));
    // メニューを開いたまま見出しを押すと、メニューを閉じて並べ替える
    ui(cx, window, |window, cx| {
        window.click(("col-menu", Column::Due as usize), cx)
    });
    assert_eq!(menu_of(cx), None);
    assert_eq!(sorted(cx), (Sort::Due, false));
}

/// タスクのリンクをコピーできる。コメントに貼ったリンクはカードになり、押すとそのタスクが開く。
#[gpui_kit::test]
fn task_link_is_copied_and_opens_from_a_comment_card(cx: &mut TestAppContext) {
    start(cx);
    // コメントまで見える高さにする
    let (window, view) = open(cx, win(1600., 2400.));
    let (first, second) = cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let s = &mut db.store;
            let first = s.add_task_in(None, "会場を決める").unwrap();
            let second = s.add_task_in(None, "機材を借りる").unwrap();
            (first, second)
        })
    });

    // 詳細の上のボタンで、リンクがクリップボードに入る
    cx.update(|cx| view.update(cx, |v, cx| v.reload(cx)));
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    cx.update(|cx| {
        window
            .update(cx, |_, window, cx| {
                view.update(cx, |v, cx| v.open_panel(&first.id, window, cx))
            })
            .unwrap()
    });
    ui(cx, window, |window, cx| window.click("copy-link", cx));
    let url = malakt_core::tasklink::url(&first.id);
    let copied = cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some(url.as_str()));
    cx.update(|cx| assert!(view.read(cx).notice.is_some()));

    // もう 1 つのタスクのコメントに貼ると、カードになる。消したタスクへのリンクもカードで知らせる
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let body = format!("先にこれを {url}");
            db.store.add_comment(&second.id, None, &body).unwrap();
            let gone = malakt_core::tasklink::url("01NOSUCHTASK");
            db.store.add_comment(&second.id, None, &gone).unwrap();
        });
        window
            .update(cx, |_, window, cx| {
                view.update(cx, |v, cx| v.open_panel(&second.id, window, cx))
            })
            .unwrap()
    });
    cx.update(|cx| {
        let v = view.read(cx);
        let linked = &v.detail.as_ref().unwrap().linked;
        assert_eq!(linked.len(), 1);
        assert_eq!(linked[0].id, first.id);
    });
    ui(cx, window, |window, cx| {
        assert!(window.try_find(("task-card", 16usize)).is_some());
        window.click(("task-card", 0usize), cx)
    });
    cx.update(|cx| assert_eq!(view.read(cx).panel.as_deref(), Some(first.id.as_str())));
}

/// タスクの行をドラッグすると並べ替えられ、左のワークスペースに落とすと移る。
#[gpui_kit::test]
fn task_rows_are_dragged_to_reorder_and_to_another_workspace(cx: &mut TestAppContext) {
    use gpui_kit::VisualTestContext;
    start(cx);
    let (window, view) = open(cx, win(1600., 1000.));
    let (ws, _ids) = cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let s = &mut db.store;
            let ws = s.create_workspace(None, "文化祭").unwrap();
            let ids: Vec<_> = ["会場を決める", "機材を借りる", "受付を決める"]
                .iter()
                .map(|title| s.add_task_in(None, title).unwrap().id)
                .collect();
            (ws, ids)
        })
    });
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.reload(cx);
            v.go(Nav::All, cx)
        })
    });
    let order = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let v = view.read(cx);
            v.visible
                .iter()
                .map(|&i| v.tasks[i].id.clone())
                .collect::<Vec<_>>()
        })
    };
    let center = |cx: &mut TestAppContext, id: gpui_kit::ElementId| {
        let mut at = Point::default();
        ui(cx, window, |window, _| {
            at = window.find(id).bounds().center()
        });
        at
    };
    // 押して、少しずつ動かして、離す
    let drag = |cx: &mut TestAppContext, from: Point<Pixels>, to: Point<Pixels>, drop: bool| {
        let mut v = VisualTestContext::from_window(window, cx);
        v.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        for step in 1..=4 {
            let t = step as f32 / 4.;
            let at = Point::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
            v.simulate_mouse_move(at, MouseButton::Left, Modifiers::default());
            v.update(|window, cx| window.render_frame(cx));
        }
        if drop {
            v.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
        }
        v.run_until_parked();
    };
    let before = order(cx);
    assert_eq!(before.len(), 3);

    // 少し動かしただけでは、ドラッグにならない
    let first = center(cx, ("title", 0usize).into());
    drag(cx, first, Point::new(first.x + px(3.), first.y), true);
    assert_eq!(order(cx), before);

    // 動かしている間は、マウスに付いてタイトルが出て、落とし先が決まる
    cx.update(|cx| view.update(cx, |v, cx| v.close_panel(cx)));
    let (first, third) = (
        center(cx, ("title", 0usize).into()),
        center(cx, ("title", 2usize).into()),
    );
    drag(cx, first, third, false);
    cx.update(|cx| {
        let d = view.read(cx).task_drag.clone().unwrap();
        assert!(d.moved);
        assert_eq!(
            d.over.map(|(t, _)| t),
            Some(malakt_mahoz::state::DropTarget::Row(2))
        );
    });
    // 離すと、その行の位置に入り、並び順が手動になる
    let mut v = VisualTestContext::from_window(window, cx);
    v.simulate_mouse_up(third, MouseButton::Left, Modifiers::default());
    v.run_until_parked();
    assert_eq!(
        order(cx),
        [before[1].clone(), before[2].clone(), before[0].clone()]
    );
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.query.sort, malakt_core::view::Sort::Manual);
        assert!(v.task_drag.is_none());
        assert!(v.panel.is_none());
    });

    // 左のワークスペースに落とすと、そこへ移る
    let from = center(cx, ("title", 0usize).into());
    let to = center(cx, ("ws", 0usize).into());
    drag(cx, from, to, true);
    let moved = store(cx, |s| s.task(&before[1]).unwrap());
    assert_eq!(moved.workspace_id.as_deref(), Some(ws.id.as_str()));
    cx.update(|cx| assert!(view.read(cx).notice.is_some()));
    // 「未所属」に落とすと戻る
    let ix = order(cx).iter().position(|id| *id == before[1]).unwrap();
    let from = center(cx, ("title", ix).into());
    let to = center(cx, "nav-unfiled".into());
    drag(cx, from, to, true);
    let back = store(cx, |s| s.task(&before[1]).unwrap());
    assert_eq!(back.workspace_id, None);
}

/// 見ていた場所・絞り込み・開いていた詳細・ウィンドウの位置は設定に残り、次に開いたウィンドウに戻る。
#[gpui_kit::test]
fn the_last_screen_comes_back_after_a_restart(cx: &mut TestAppContext) {
    use malakt_mahoz::session;
    start(cx);
    let (ws, sub, task) = cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let s = &mut db.store;
            let ws = s.create_workspace(None, "文化祭").unwrap();
            let sub = s.create_workspace(Some(&ws.id), "会場").unwrap();
            let task = s.add_task_in(Some(&ws.id), "機材を借りる").unwrap();
            (ws, sub, task)
        })
    });
    let (window, view) = open(cx, win(1600., 1000.));
    cx.update(|cx| {
        window
            .update(cx, |_, window, cx| {
                view.update(cx, |v, cx| {
                    v.track_session(window, cx);
                    v.reload(cx);
                    v.go(Nav::Workspace(ws.id.clone()), cx);
                    v.query.tab = Tab::Open;
                    v.query.q = "機材".into();
                    v.query.status = Some(Status::NeedsAction);
                    v.query.sort = malakt_core::view::Sort::Title;
                    v.query.descending = true;
                    v.toggle_collapsed(&ws.id, cx);
                    v.sidebar_hidden = true;
                    v.reload(cx);
                    v.open_panel(&task.id, window, cx);
                    cx.notify();
                })
            })
            .unwrap()
    });
    cx.run_until_parked();
    let before = cx.update(|cx| view.read(cx).session_state(cx));
    assert_eq!(before.nav, format!("ws:{}", ws.id));
    assert_eq!(before.panel.as_deref(), Some(task.id.as_str()));
    assert!(before.bounds.is_some());

    // 設定に残っている
    let saved = cx.update(|cx| session::saved(cx));
    assert_eq!(saved, std::slice::from_ref(&before));

    // 新しく開いたウィンドウに戻すと、同じ画面になる
    let (window, again) = open(cx, win(1600., 1000.));
    cx.update(|cx| {
        window
            .update(cx, |_, window, cx| {
                again.update(cx, |v, cx| v.restore_session(&saved[0], window, cx))
            })
            .unwrap()
    });
    cx.update(|cx| {
        let v = again.read(cx);
        assert_eq!(v.nav, Nav::Workspace(ws.id.clone()));
        assert_eq!(v.query.q, "機材");
        assert_eq!(v.query.status, Some(Status::NeedsAction));
        assert!(v.query.descending && v.sidebar_hidden);
        assert!(v.collapsed.contains(&ws.id));
        assert_eq!(v.panel.as_deref(), Some(task.id.as_str()));
        assert_eq!(v.inputs.search.read(cx).value().as_ref(), "機材");
        let mut state = v.session_state(cx);
        state.bounds = before.bounds;
        state.maximized = before.maximized;
        assert_eq!(state, before);
    });

    // なくなったワークスペースやタスクは開かず、ホームに戻る
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            db.store.delete_task(&task.id).unwrap();
            db.store.delete_workspace(&sub.id).unwrap();
            db.store.delete_workspace(&ws.id).unwrap();
        });
        window
            .update(cx, |_, window, cx| {
                again.update(cx, |v, cx| {
                    v.reload(cx);
                    v.restore_session(&saved[0], window, cx)
                })
            })
            .unwrap()
    });
    cx.update(|cx| {
        let v = again.read(cx);
        assert_eq!(v.nav, Nav::Home);
        assert_eq!(v.panel, None);
    });

    // 画面の外に出た位置や、壊れた設定は使わない
    let mut off = before.clone();
    off.bounds = Some([-90000., -90000., 800., 600.]);
    cx.update(|cx| {
        assert!(off.window_bounds(cx).is_none());
        db::shared(cx).update(cx, |db, _| {
            db.store
                .set_setting("session.windows", Some("{broken"))
                .unwrap()
        });
        assert!(session::saved(cx).is_empty());
    });
}

/// 画面はペインに分けられる。Ctrl+B のキーで分割・移動・入れ替え・拡大・リサイズ・閉じるができ、
/// どのペインも別々の場所を持ち、配置は再起動しても戻る。
#[gpui_kit::test]
fn pane_presets_are_saved_applied_and_deleted(cx: &mut TestAppContext) {
    use malakt_core::panes::{Action, Dir};
    start(cx);
    let (window, view) = open(cx, win(1600., 1000.));
    let act = |cx: &mut TestAppContext, action: Action| {
        cx.update(|cx| {
            window
                .update(cx, |_, window, cx| {
                    view.update(cx, |v, cx| v.pane_action(action, window, cx))
                })
                .unwrap()
        });
        ui(cx, window, |_, _| {});
    };
    // 3 つに分けて、プリセットの一覧から保存する
    act(cx, Action::Split(Dir::Right));
    act(cx, Action::Split(Dir::Down));
    let saved = cx.update(|cx| view.read(cx).layout.encode());
    act(cx, Action::Presets);
    ui(cx, window, |window, cx| {
        window.click("pane-preset-save", cx)
    });
    // 同じ配置は 2 つにならない
    ui(cx, window, |window, cx| {
        window.click("pane-preset-save", cx)
    });
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).pane_presets,
            vec![("配置 1".to_string(), saved.clone())]
        )
    });

    // 1 画面に戻してから、保存した配置を選ぶと 3 つに戻る
    ui(cx, window, |window, cx| {
        window.click(("pane-preset", 0usize), cx)
    });
    cx.update(|cx| assert_eq!(view.read(cx).layout.panes().len(), 1));
    act(cx, Action::Presets);
    ui(cx, window, |window, cx| {
        window.click(("pane-preset-own", 0usize), cx)
    });
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.layout.panes().len(), 3);
        assert_eq!(v.layout.encode().split('@').next(), saved.split('@').next());
    });

    // 開き直したウィンドウにも出る。消すと一覧からなくなる
    let (window, view) = open(cx, win(1600., 1000.));
    cx.update(|cx| assert_eq!(view.read(cx).pane_presets.len(), 1));
    cx.update(|cx| {
        window
            .update(cx, |_, window, cx| {
                view.update(cx, |v, cx| v.pane_action(Action::Presets, window, cx))
            })
            .unwrap()
    });
    ui(cx, window, |window, cx| {
        window.click(("pane-preset-delete", 0usize), cx)
    });
    ui(cx, window, |window, _| {
        assert!(window.try_find(("pane-preset-own", 0usize)).is_none());
    });
    cx.update(|cx| assert!(view.read(cx).pane_presets.is_empty()));
}

#[gpui_kit::test]
fn panes_split_move_resize_close_and_come_back(cx: &mut TestAppContext) {
    use gpui_kit::Keystroke;
    use malakt_core::panes::{Action, Dir, Keys};
    use malakt_mahoz::panes::PaneOverlay;
    start(cx);
    let ws = cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let ws = db.store.create_workspace(None, "文化祭").unwrap();
            db.store.add_task_in(Some(&ws.id), "機材を借りる").unwrap();
            ws
        })
    });
    let (window, view) = open(cx, win(1600., 1000.));
    // Ctrl+B のあとにキーを押す
    let keys = |cx: &mut TestAppContext, strokes: &[&'static str]| {
        for stroke in strokes {
            let stroke = Keystroke::parse(stroke).unwrap();
            cx.update(|cx| {
                window
                    .update(cx, |_, window, cx| {
                        view.update(cx, |v, cx| {
                            if !v.pane_resize {
                                v.pane_prefix = true;
                            }
                            assert!(v.pane_key(&stroke, window, cx));
                        })
                    })
                    .unwrap()
            });
        }
        ui(cx, window, |_, _| {});
    };
    let panes = |cx: &mut TestAppContext| cx.update(|cx| view.read(cx).layout.panes());
    let focus = |cx: &mut TestAppContext| cx.update(|cx| view.read(cx).layout.focus());
    cx.update(|cx| view.update(cx, |v, cx| v.reload(cx)));

    // 分けていないときは、ペインの枠を出さない
    ui(cx, window, |window, _| {
        assert!(window.try_find(("pane", 0usize)).is_none());
    });

    // v で右に、- で下に分ける。新しいペインが今のペインになり、ホームから始まる
    keys(cx, &["v", "-"]);
    assert_eq!((panes(cx), focus(cx)), (vec![0, 1, 2], 2));
    ui(cx, window, |window, _| {
        for id in 0..3usize {
            assert!(window.try_find(("pane", id)).is_some());
        }
        assert!(window.try_find(("pane-border", 1usize)).is_some());
    });

    // 左のメニューは、今のペインだけを動かす
    cx.update(|cx| view.update(cx, |v, cx| v.go(Nav::Workspace(ws.id.clone()), cx)));
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.nav, Nav::Home);
        assert_eq!(v.shown_nav(cx), Nav::Workspace(ws.id.clone()));
        assert_eq!(v.children[&2].read(cx).nav, Nav::Workspace(ws.id.clone()));
        assert_eq!(v.children[&2].read(cx).visible.len(), 1);
        assert_eq!(v.children[&1].read(cx).nav, Nav::Home);
    });

    // 向きと順番で移る
    keys(cx, &["k"]);
    assert_eq!(focus(cx), 1);
    keys(cx, &["left"]);
    assert_eq!(focus(cx), 0);
    keys(cx, &["tab"]);
    assert_eq!(focus(cx), 1);
    keys(cx, &["shift-tab"]);
    assert_eq!(focus(cx), 0);
    // 押したペインが今のペインになる
    ui(cx, window, |window, cx| {
        window.click(("pane-bar", 2usize), cx)
    });
    assert_eq!(focus(cx), 2);

    // Shift+K で上のペインと入れ替える
    keys(cx, &["shift-k"]);
    assert_eq!((panes(cx), focus(cx)), (vec![0, 2, 1], 2));

    // z で拡大し、Esc で戻る
    keys(cx, &["z"]);
    cx.update(|cx| assert!(view.read(cx).layout.zoomed()));
    ui(cx, window, |window, _| {
        assert!(window.try_find(("pane", 0usize)).is_none())
    });
    cx.update(|cx| view.update(cx, |v, cx| v.dismiss(cx)));
    cx.update(|cx| assert!(!view.read(cx).layout.zoomed()));

    // r でリサイズモード。キーを押すたびに境界が動き、Esc で終わる
    let width = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let v = view.read(cx);
            v.layout
                .rects()
                .iter()
                .find(|(id, _)| *id == 2)
                .unwrap()
                .1
                .w
        })
    };
    let before = width(cx);
    keys(cx, &["r", "l", "l"]);
    cx.update(|cx| assert!(view.read(cx).pane_resize));
    assert!(width(cx) > before);
    cx.update(|cx| view.update(cx, |v, cx| v.dismiss(cx)));
    cx.update(|cx| assert!(!view.read(cx).pane_resize));

    // ? でキーの一覧、b でプリセット。Esc で閉じる
    keys(cx, &["shift-/"]);
    cx.update(|cx| assert_eq!(view.read(cx).pane_overlay, Some(PaneOverlay::Help)));
    ui(cx, window, |window, cx| {
        window.click(("pane-keys", 1usize), cx)
    });
    cx.update(|cx| assert_eq!(view.read(cx).pane_keys, Keys::Tmux));
    assert_eq!(
        store(cx, |s| s.setting("panes.keys").unwrap()).as_deref(),
        Some("tmux")
    );
    cx.update(|cx| view.update(cx, |v, cx| v.set_pane_keys(Keys::Herdr, cx)));
    cx.update(|cx| view.update(cx, |v, cx| v.dismiss(cx)));
    keys(cx, &["b"]);
    cx.update(|cx| assert_eq!(view.read(cx).pane_overlay, Some(PaneOverlay::Presets)));
    cx.update(|cx| view.update(cx, |v, cx| v.dismiss(cx)));
    cx.update(|cx| assert_eq!(view.read(cx).pane_overlay, None));

    // d で同じ中身のペインを右に足す
    keys(cx, &["d"]);
    assert_eq!(panes(cx).len(), 4);
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.children[&3].read(cx).nav, Nav::Workspace(ws.id.clone()));
    });

    // 配置とそれぞれのペインの中身は、保存した形から戻る
    let saved = cx.update(|cx| view.read(cx).session_state(cx));
    assert!(saved.layout.is_some());
    assert_eq!(saved.panes.len(), 3);
    let (other_window, other) = open(cx, win(1600., 1000.));
    cx.update(|cx| {
        other_window
            .update(cx, |_, window, cx| {
                other.update(cx, |v, cx| v.restore_session(&saved, window, cx))
            })
            .unwrap()
    });
    cx.update(|cx| {
        let v = other.read(cx);
        assert_eq!(v.layout.encode(), view.read(cx).layout.encode());
        assert_eq!(v.children[&2].read(cx).nav, Nav::Workspace(ws.id.clone()));
        assert_eq!(v.session_state(cx).panes, saved.panes);
    });
    // ペインの数が合わない保存は、配置を戻さない
    let mut broken = saved.clone();
    broken.panes.pop();
    cx.update(|cx| {
        other_window
            .update(cx, |_, window, cx| {
                other.update(cx, |v, cx| {
                    v.apply_pane_layout("0@0", window, cx);
                    v.restore_session(&broken, window, cx);
                    assert_eq!(v.layout.panes(), [0]);
                })
            })
            .unwrap()
    });

    // x で閉じる。ウィンドウが兼ねているペインを閉じると、ほかのペインの中身を引き取る
    keys(cx, &["x"]);
    assert_eq!(panes(cx).len(), 3);
    cx.update(|cx| {
        window
            .update(cx, |_, window, cx| {
                view.update(cx, |v, cx| {
                    v.focus_pane(0, cx);
                    v.pane_action(Action::Close, window, cx);
                })
            })
            .unwrap()
    });
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.layout.panes(), [2, 1]);
        assert_eq!(v.primary, 2);
        assert_eq!(v.nav, Nav::Workspace(ws.id.clone()));
        assert_eq!(v.children.len(), 1);
    });
    // プリセットで 4 つに分け、1 画面に戻す。最後の 1 つは閉じない
    cx.update(|cx| {
        window
            .update(cx, |_, window, cx| {
                view.update(cx, |v, cx| {
                    v.apply_pane_layout(malakt_core::panes::PRESETS[4].1, window, cx);
                    assert_eq!(v.layout.panes().len(), 4);
                    assert_eq!(v.nav, Nav::Workspace(ws.id.clone()));
                    v.apply_pane_layout("0@0", window, cx);
                    assert_eq!(v.layout.panes(), [0]);
                    assert!(v.children.is_empty());
                    v.pane_action(Action::Close, window, cx);
                    assert_eq!(v.layout.panes(), [0]);
                    v.pane_action(Action::Split(Dir::Down), window, cx);
                    assert_eq!(v.layout.panes().len(), 2);
                })
            })
            .unwrap()
    });
    ui(cx, window, |_, _| {});
}

/// OS のリマインダーを、メモリの中のものに差し替える。戻り値は、その中身。
fn reminders(
    cx: &mut TestAppContext,
    refuse: Option<&'static str>,
) -> std::sync::Arc<std::sync::Mutex<malakt_core::remind::Memory>> {
    use malakt_mahoz::reminders::{Reminders, Target};
    let os = std::sync::Arc::new(std::sync::Mutex::new(malakt_core::remind::Memory::default()));
    let inside = os.clone();
    cx.update(|cx| {
        cx.set_global(Reminders {
            target: Some(Target::Reminders),
            run: std::sync::Arc::new(move |snapshot, links| match refuse {
                Some(reason) => Err(reason.into()),
                None => Ok(malakt_core::remind::sync(
                    &mut *inside.lock().unwrap(),
                    &snapshot,
                    &links,
                )),
            }),
        })
    });
    os
}

/// 歯車で「設定」を開いてそのカテゴリまでスクロールし、スイッチを押して閉じる。
fn press_in_settings(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    view: &Entity<MainView>,
    section: malakt_mahoz::state::CustomizeSection,
    id: &'static str,
) {
    ui(cx, window, |window, cx| window.click("open-settings", cx));
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.show_appearance_section(section, window, cx))
    });
    ui(cx, window, |window, cx| window.click(id, cx));
    ui(cx, window, |window, cx| window.click("dialog-close", cx));
}

/// リマインダーとの連携は、選ぶまで動かない。選ぶと、タスクが期限つきでリストに入り、
/// 完了は両方向に伝わり、解除すると作ったものが消える。
#[gpui_kit::test]
fn reminders_follow_tasks_once_chosen(cx: &mut TestAppContext) {
    start(cx);
    let os = reminders(cx, None);
    let (desk, milk) = cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let event = db.store.create_workspace(None, "冬のイベント").unwrap();
            let venue = db.store.create_workspace(Some(&event.id), "会場").unwrap();
            let draft = |title: &str, due: &str| malakt_core::TaskDraft {
                title: title.into(),
                due: Some(due.into()),
                ..Default::default()
            };
            let desk = db
                .store
                .create_task(Some(&venue.id), &draft("机を借りる", "2099-12-20 18:30"))
                .unwrap();
            let milk = db
                .store
                .create_task(None, &draft("牛乳を買う", "2099-12-01"))
                .unwrap();
            (desk, milk)
        })
    });
    let (window, view) = open(cx, wide());
    cx.run_until_parked();
    // 標準では何も作らない
    assert!(os.lock().unwrap().items.is_empty());

    press_in_settings(
        cx,
        window,
        &view,
        malakt_mahoz::state::CustomizeSection::Notices,
        "reminders-switch",
    );
    cx.run_until_parked();
    {
        let os = os.lock().unwrap();
        assert_eq!(os.titles_in("冬のイベント / 会場"), ["机を借りる"]);
        assert_eq!(os.titles_in("Malakt Mahoz"), ["牛乳を買う"]);
        assert_eq!(
            os.find("机を借りる").unwrap().due_parts(),
            Some((2099, 12, 20, Some((18, 30))))
        );
    }

    // アプリで完了にすると、リマインダーも完了になる
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.write(cx, |s| s.set_task_status(&desk.id, Status::Completed))
        })
    });
    cx.run_until_parked();
    assert!(os.lock().unwrap().find("机を借りる").unwrap().done);

    // リマインダーで完了にすると、1 分以内にタスクも完了になる
    os.lock().unwrap().set_done("牛乳を買う", true);
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(61));
    cx.run_until_parked();
    assert_eq!(
        store(cx, |s| s.task(&milk.id).unwrap().status),
        Status::Completed
    );

    // 落ち着いたら、もう書き直さない
    let writes = os.lock().unwrap().writes;
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(61));
    cx.run_until_parked();
    assert_eq!(os.lock().unwrap().writes, writes);

    // 解除すると、作ったリマインダーとリストが消える。タスクは残る
    press_in_settings(
        cx,
        window,
        &view,
        malakt_mahoz::state::CustomizeSection::Notices,
        "reminders-switch",
    );
    cx.run_until_parked();
    {
        let os = os.lock().unwrap();
        assert!(os.items.is_empty() && os.lists.is_empty());
    }
    assert_eq!(all_tasks(cx).len(), 2);
    assert!(store(cx, |s| s.reminder_links().unwrap()).is_empty());
    cx.update(|cx| assert!(view.read(cx).error.is_none()));
}

/// アクセスが許可されなかったら、設定はオフに戻り、理由が画面に出る。
#[gpui_kit::test]
fn reminders_refused_by_the_os_turn_back_off(cx: &mut TestAppContext) {
    start(cx);
    reminders(cx, Some("リマインダーへのアクセスが許可されていません"));
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            db.store
                .create_task(
                    None,
                    &malakt_core::TaskDraft {
                        title: "牛乳を買う".into(),
                        ..Default::default()
                    },
                )
                .unwrap();
        })
    });
    let (window, view) = open(cx, wide());
    cx.run_until_parked();
    press_in_settings(
        cx,
        window,
        &view,
        malakt_mahoz::state::CustomizeSection::Notices,
        "reminders-switch",
    );
    cx.run_until_parked();
    assert!(!store(cx, |s| s.reminders_enabled().unwrap()));
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).error.as_deref(),
            Some("リマインダーへのアクセスが許可されていません")
        )
    });
}

/// 自動アップデートの外とのやり取りを差し替える。`check` は探した結果、`install` は入れ替えの結果。
/// 戻り値は、探したときに渡された「見送っている版」と、入れ替えた版の記録。
#[allow(clippy::type_complexity)]
fn updater(
    cx: &mut TestAppContext,
    check: Result<Option<malakt_mahoz::update::Staged>, malakt_mahoz::update::Error>,
    install: Result<(), &'static str>,
) -> (
    std::sync::Arc<std::sync::Mutex<Vec<Option<String>>>>,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) {
    use malakt_mahoz::update::Updater;
    let checked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let installed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (check_log, install_log) = (checked.clone(), installed.clone());
    cx.update(|cx| {
        cx.set_global(Updater {
            available: true,
            check: std::sync::Arc::new(move |skipped| {
                check_log.lock().unwrap().push(skipped);
                check.clone()
            }),
            install: std::sync::Arc::new(move |staged| {
                install_log.lock().unwrap().push(staged.version.clone());
                install.map_err(str::to_owned)
            }),
        });
        malakt_mahoz::update::check_now(cx);
    });
    cx.run_until_parked();
    (checked, installed)
}

/// 取ってきて置いてある v0.2.0。
fn staged_update(root: &std::path::Path) -> malakt_mahoz::update::Staged {
    let dir = root.join("update");
    std::fs::create_dir_all(dir.join("payload")).unwrap();
    std::fs::write(dir.join("payload/malakt-mahoz"), b"new").unwrap();
    malakt_mahoz::update::Staged {
        version: "0.2.0".into(),
        payload: dir.join("payload/malakt-mahoz"),
        dir,
    }
}

fn has(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) -> bool {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.try_find(id).is_some()
    })
    .unwrap()
}

/// 新しい版を取ってくると上のバーに印が出て、アプリを閉じるときに 1 回だけ入れ替わる。
#[gpui_kit::test]
fn a_new_version_is_installed_when_the_app_closes(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    // つないでいなければ（開発用のビルドなど）、印もメニューの項目も出ない
    assert!(!has(cx, window, "update-now"));
    cx.update(|cx| assert_eq!(malakt_mahoz::update::menu_label(cx), None));

    let root = tempfile::tempdir().unwrap();
    let (checked, installed) = updater(cx, Ok(Some(staged_update(root.path()))), Ok(()));
    assert_eq!(*checked.lock().unwrap(), [None]);
    assert!(has(cx, window, "update-now"));
    // 取ってきただけでは入れ替えない
    assert!(installed.lock().unwrap().is_empty());
    // もう置いてあるので、探し直さない
    cx.update(malakt_mahoz::update::check_now);
    cx.run_until_parked();
    assert_eq!(checked.lock().unwrap().len(), 1);

    cx.update(malakt_mahoz::update::install_on_quit);
    assert_eq!(*installed.lock().unwrap(), ["0.2.0"]);
    // 終了の途中でもう一度呼ばれても、入れ替えは 1 回
    cx.update(malakt_mahoz::update::install_on_quit);
    assert_eq!(installed.lock().unwrap().len(), 1);
    assert!(!has(cx, window, "update-now"));
    cx.update(|cx| assert!(view.read(cx).error.is_none()));
}

/// 設定のメニューで無効にすると、取ってきた版を消し、閉じても入れ替えない。
/// 有効に戻すと、もう一度探す。
#[gpui_kit::test]
fn auto_update_can_be_turned_off(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    let root = tempfile::tempdir().unwrap();
    let staged = staged_update(root.path());
    let (checked, installed) = updater(cx, Ok(Some(staged.clone())), Ok(()));
    assert!(has(cx, window, "update-now"));

    press_in_settings(
        cx,
        window,
        &view,
        malakt_mahoz::state::CustomizeSection::Update,
        "auto-update-switch",
    );
    assert!(!has(cx, window, "update-now"));
    assert!(!staged.dir.exists());
    cx.update(malakt_mahoz::update::install_on_quit);
    assert!(installed.lock().unwrap().is_empty());
    // 無効の間は、探しに行かない
    cx.update(malakt_mahoz::update::check_now);
    cx.run_until_parked();
    assert_eq!(checked.lock().unwrap().len(), 1);

    press_in_settings(
        cx,
        window,
        &view,
        malakt_mahoz::state::CustomizeSection::Update,
        "auto-update-switch",
    );
    cx.run_until_parked();
    assert_eq!(checked.lock().unwrap().len(), 2);
    assert!(has(cx, window, "update-now"));
}

/// 閉じるときに入れ替えられなかったら、次に開いたウィンドウに理由が 1 回だけ出て、
/// その版はもう取ってこない。
#[gpui_kit::test]
fn a_failed_install_is_shown_on_the_next_launch(cx: &mut TestAppContext) {
    start(cx);
    let root = tempfile::tempdir().unwrap();
    let staged = staged_update(root.path());
    let (checked, installed) = updater(
        cx,
        Ok(Some(staged.clone())),
        Err("v0.2.0 に更新できませんでした。/Applications に書き込めません"),
    );
    cx.update(malakt_mahoz::update::install_on_quit);
    assert_eq!(*installed.lock().unwrap(), ["0.2.0"]);
    assert!(!staged.dir.exists());

    let (_, view) = open(cx, wide());
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).error.as_deref(),
            Some("v0.2.0 に更新できませんでした。/Applications に書き込めません")
        )
    });
    let (_, second) = open(cx, wide());
    cx.update(|cx| assert!(second.read(cx).error.is_none()));
    // 次に探すときは、失敗した版を見送る
    cx.update(malakt_mahoz::update::check_now);
    cx.run_until_parked();
    assert_eq!(*checked.lock().unwrap(), [None, Some("0.2.0".to_owned())]);
}

/// 取ってきたものが使えなかったら画面に出す。つながらないだけなら何も出さない。
#[gpui_kit::test]
fn a_bad_download_is_reported_but_being_offline_is_not(cx: &mut TestAppContext) {
    use malakt_mahoz::update::Error;
    start(cx);
    let (window, view) = open(cx, wide());
    updater(cx, Err(Error::Offline("つながりません".into())), Ok(()));
    cx.update(|cx| assert!(view.read(cx).error.is_none()));
    assert!(!has(cx, window, "update-now"));

    updater(
        cx,
        Err(Error::Failed(
            "v0.2.0 の a.zip が SHA256SUMS と合いません。".into(),
        )),
        Ok(()),
    );
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).error.as_deref(),
            Some("v0.2.0 の a.zip が SHA256SUMS と合いません。")
        )
    });
    assert!(!has(cx, window, "update-now"));
}

/// どの幅でも、一覧の上のボタンは重ならず、タイトルは読める幅で残る。サブワークスペースの見出しも出る。
/// AI チャットを開いても、列を広げても同じ。
#[gpui_kit::test]
fn narrow_widths_keep_the_title_and_never_overlap(cx: &mut TestAppContext) {
    use malakt_mahoz::state::{Column, COLUMN_MAX, TITLE_MIN};
    start(cx);
    let ws = cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let s = &mut db.store;
            let ws = s.create_workspace(None, "文化祭").unwrap();
            s.create_workspace(Some(&ws.id), "設営").unwrap();
            s.set_workspace_date(&ws.id, Some(&malakt_core::view::today()))
                .unwrap();
            let d = malakt_core::TaskDraft {
                title: "機材を借りる".into(),
                due: Some(malakt_core::view::today()),
                ..Default::default()
            };
            s.create_task(Some(&ws.id), &d).unwrap();
            ws
        })
    });
    let scale = malakt_mahoz::UI_SCALE;
    let header = [
        "ws-settings",
        "ws-export",
        "ws-import",
        "ws-delete",
        "new-task-open",
    ];
    let mut width = 380.;
    while width <= 1600. {
        for (ai, widened) in [(false, false), (true, false), (false, true), (true, true)] {
            let (window, view) = open(cx, win(width, 800.));
            cx.update_window(window, |_, window, cx| {
                view.update(cx, |v, cx| {
                    for c in &mut v.columns {
                        c.width = if widened {
                            COLUMN_MAX
                        } else {
                            c.column.default_width()
                        };
                    }
                    v.go(Nav::Workspace(ws.id.clone()), cx);
                    if ai {
                        v.toggle_ai(window, cx);
                    }
                })
            })
            .unwrap();
            let case = format!("幅 {width}、AI チャット {ai}、列を広げた {widened}");
            ui(cx, window, |window, _| {
                // 右のボタンは、たがいに重ならない
                let mut right = px(0.);
                for id in header {
                    let button = window.find(id).bounds();
                    assert!(button.left() >= px(0.), "{case}: {id} が左にはみ出す");
                    if button.left() >= right {
                        right = button.right();
                    } else {
                        // 次の行へ送ったボタンは、左から並び直す
                        assert!(
                            button.left() < px(200.) || button.left() >= right,
                            "{case}: {id} が重なる"
                        );
                        right = button.right();
                    }
                }
                // AI チャットがウィンドウを覆う幅では、一覧は見えない
                let chat = window.try_find("ai-resize").map(|e| e.bounds().left());
                if chat.is_some_and(|left| left < px(200.)) {
                    return;
                }
                // サブワークスペースの見出しは、表でもカードでもタスクの上に出る
                let folder = window
                    .try_find(("folder", 0usize))
                    .unwrap_or_else(|| panic!("{case}: サブワークスペースの見出しが出ていない"))
                    .bounds();
                // タイトルは、表でもカードでも読める幅がある。重ねた AI チャットにも隠れない
                let title = window
                    .try_find(("title", 0usize))
                    .unwrap_or_else(|| panic!("{case}: タイトルが出ていない"))
                    .bounds();
                assert!(folder.bottom() <= title.top(), "{case}");
                let shown = chat.map_or(title.right(), |left| left.min(title.right()));
                assert!(
                    shown - title.left() >= px(TITLE_MIN * scale * 0.6),
                    "{case}: タイトルの幅が {:?}",
                    shown - title.left()
                );
                // 表では、見出しと行の列がそろう
                if let Some(head) = window.try_find(("col-head", Column::Priority as usize)) {
                    let cell = window.find(("prio", 0usize)).bounds();
                    assert_eq!(head.bounds().left(), cell.left(), "{case}");
                }
            });
            cx.update_window(window, |_, window, _| window.remove_window())
                .unwrap();
        }
        width += 20.;
    }

    // 列を広げて入らなくなったら、右の列から出さなくなる。追加のボタンは右の端に残る
    let (window, view) = open(cx, win(1280., 800.));
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            for c in &mut v.columns {
                c.width = COLUMN_MAX;
            }
            v.go(Nav::Workspace(ws.id.clone()), cx);
        })
    });
    ui(cx, window, |window, _| {
        assert!(window.try_find("col-title").is_some());
        assert!(window
            .try_find(("col-head", Column::Status as usize))
            .is_some());
        assert!(window
            .try_find(("col-head", Column::Updated as usize))
            .is_none());
        assert!(window.try_find("quick-add-submit").is_some());
        let title = window.find(("title", 0usize)).bounds();
        assert!(title.size.width >= px(TITLE_MIN * scale));
    });

    // ホームの「期限が近いタスク」も、タイトルが残る
    for width in [900., 1150., 1280.] {
        let (window, view) = open(cx, win(width, 800.));
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |v, cx| {
                v.go(Nav::Home, cx);
                v.toggle_ai(window, cx);
            })
        })
        .unwrap();
        ui(cx, window, |window, _| {
            let row = window.find(("due", 0usize)).bounds();
            let chat = window.find("ai-resize").bounds().left();
            assert!(
                chat.min(row.right()) - row.left() >= px(300.),
                "幅 {width}: ホームの行が {:?}",
                chat.min(row.right()) - row.left()
            );
        });
    }
}

/// 名前の書きかけを Esc で捨てたあと、もう一度 Esc を押すとダイアログが閉じる。
#[gpui_kit::test]
fn escape_after_cancelling_a_name_closes_what_is_open(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |v, cx| v.open_people(window, cx))
    })
    .unwrap();
    ui(cx, window, |window, cx| window.click("person-add", cx));
    ui(cx, window, |window, cx| window.press("enter", cx));
    let row = cx.update(|cx| {
        let people = &view.read(cx).people;
        people
            .iter()
            .position(|p| p.name == malakt_mahoz::state::NEW_PERSON_NAME)
            .unwrap()
    });
    ui(cx, window, |window, cx| window.click(("person", row), cx));
    ui(cx, window, |window, cx| window.input("捨てる名前", cx));
    ui(cx, window, |window, cx| window.press("escape", cx));
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.person_naming, None);
        assert_eq!(view.modal, Modal::Appearance);
    });
    ui(cx, window, |window, cx| window.press("escape", cx));
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::None));

    // ワークスペースの名前を捨てたあとも、キーが画面に届く
    ui(cx, window, |window, cx| window.click("add-workspace", cx));
    cx.run_until_parked();
    cx.update(|cx| assert!(view.read(cx).workspace_naming.is_some()));
    ui(cx, window, |window, cx| window.press("escape", cx));
    cx.update(|cx| assert!(view.read(cx).workspace_naming.is_none()));
    cx.update_window(window, |_, window, cx| {
        assert!(view.read(cx).focus.is_focused(window));
    })
    .unwrap();
}

/// タスクの「担当者を登録」はメニューを開く。いちばん下の「新規担当として登録」は、ダイアログを
/// 出さずに「新規担当者」を足して担当にし、その行で名前を書ける。足した人は親のワークスペースと
/// そのサブワークスペースで選べる。ほかのワークスペースの人は、メニューから選ぶとここでも選べる。
#[gpui_kit::test]
fn assignees_are_registered_from_a_menu(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    let (a, sub, b, tanaka, id) = cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            cx.notify();
            let s = &mut db.store;
            let a = s.create_workspace(None, "Lorem").unwrap().id;
            let sub = s.create_workspace(Some(&a), "Ipsum").unwrap().id;
            let b = s.create_workspace(None, "Dolor").unwrap().id;
            let tanaka = s.create_local_person("田中", &b).unwrap().id;
            let draft = malakt_core::TaskDraft {
                title: "Lorem ipsum".into(),
                ..Default::default()
            };
            let id = s.create_task(Some(&sub), &draft).unwrap().id;
            (a, sub, b, tanaka, id)
        })
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |v, cx| {
            v.reload(cx);
            v.open_panel(&id, window, cx)
        })
    })
    .unwrap();
    cx.run_until_parked();
    let assignees = |cx: &mut TestAppContext| store(cx, |s| s.task(&id).unwrap()).assignee_ids;
    let person = |cx: &mut TestAppContext, name: &str| {
        store(cx, |s| s.people().unwrap())
            .into_iter()
            .find(|p| p.name == name)
    };
    // ほかのワークスペースの人は、担当を選ぶところに並ばない
    cx.update(|cx| {
        let view = view.read(cx);
        let here = view.people_in(Some(&sub));
        assert!(here.iter().all(|p| p.id != tanaka));
        assert!(view.people_in(Some(&b)).iter().any(|p| p.id == tanaka));
        assert!(view.people_in(None).iter().all(|p| p.id != tanaka));
    });

    // メニューのいちばん下が「新規担当として登録」。押すとすぐ増えて担当に入り、名前を書ける
    ui(cx, window, |window, cx| window.click("p-assignees", cx));
    ui(cx, window, |window, cx| window.click("p-add-person", cx));
    ui(cx, window, |window, cx| {
        window.press("up", cx);
        window.press("enter", cx);
    });
    cx.run_until_parked();
    let new = person(cx, malakt_mahoz::state::NEW_PERSON_NAME).unwrap();
    assert_eq!(new.workspace_ids, std::slice::from_ref(&a));
    assert_eq!(assignees(cx), std::slice::from_ref(&new.id));
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.modal, Modal::None);
        assert_eq!(view.person_naming.as_ref(), Some(&new.id));
        assert_eq!(
            view.inputs.name.read(cx).value().as_ref(),
            malakt_mahoz::state::NEW_PERSON_NAME
        );
    });
    ui(cx, window, |window, cx| {
        assert!(window.find("person-input").visible());
        window.input("佐藤", cx);
        window.press("enter", cx);
    });
    assert_eq!(person(cx, "佐藤").unwrap().id, new.id);
    // 名前を書いても、担当のまま
    assert_eq!(assignees(cx), std::slice::from_ref(&new.id));
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.person_naming, None);
        // 親のワークスペースでもサブワークスペースでも選べて、ほかのワークスペースでは選べない
        assert!(view.people_in(Some(&a)).iter().any(|p| p.id == new.id));
        assert!(view.people_in(Some(&sub)).iter().any(|p| p.id == new.id));
        assert!(view.people_in(Some(&b)).iter().all(|p| p.id != new.id));
    });

    // メニューの上には、ほかのワークスペースの人が並ぶ。選ぶと、ここでも選べるようになって担当に入る
    ui(cx, window, |window, cx| window.click("p-add-person", cx));
    ui(cx, window, |window, cx| {
        window.press("down", cx);
        window.press("enter", cx);
    });
    cx.run_until_parked();
    let mut both = vec![a.clone(), b.clone()];
    both.sort();
    assert_eq!(person(cx, "田中").unwrap().workspace_ids, both);
    let mut two = vec![new.id.clone(), tanaka.clone()];
    two.sort();
    assert_eq!(assignees(cx), two);
    cx.update(|cx| {
        assert!(view
            .read(cx)
            .people_in(Some(&sub))
            .iter()
            .any(|p| p.id == tanaka));
    });

    // 元に戻すと、担当からも、このワークスペースからも外れる
    cx.update(|cx| view.update(cx, |v, cx| v.undo(false, cx)));
    assert_eq!(
        person(cx, "田中").unwrap().workspace_ids,
        std::slice::from_ref(&b)
    );
    assert_eq!(assignees(cx), std::slice::from_ref(&new.id));

    // 担当者のダイアログで、すべてのワークスペースで使う担当者にできる
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |v, cx| v.open_people(window, cx))
    })
    .unwrap();
    let row = cx.update(|cx| {
        let view = view.read(cx);
        view.people.iter().position(|p| p.id == new.id).unwrap()
    });
    ui(cx, window, |window, cx| {
        window.click(("person-scope", row), cx)
    });
    ui(cx, window, |window, cx| {
        window.press("down", cx);
        window.press("enter", cx);
    });
    assert!(person(cx, "佐藤").unwrap().is_global());
    cx.update(|cx| {
        assert!(view
            .read(cx)
            .people_in(Some(&b))
            .iter()
            .any(|p| p.id == new.id));
    });
    // 3 つ目は 2 つ目のワークスペース。選ぶと、そこでだけ使う
    let row = cx.update(|cx| {
        let view = view.read(cx);
        view.people.iter().position(|p| p.id == new.id).unwrap()
    });
    ui(cx, window, |window, cx| {
        window.click(("person-scope", row), cx)
    });
    ui(cx, window, |window, cx| {
        window.press("down", cx);
        window.press("down", cx);
        window.press("down", cx);
        window.press("enter", cx);
    });
    assert_eq!(person(cx, "佐藤").unwrap().workspace_ids, [b]);
}

/// 担当者は、ダイアログの中で「新規担当者」として増やし、その行で名前を書く。
#[gpui_kit::test]
fn people_are_added_and_named_in_the_list(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |v, cx| v.open_people(window, cx))
    })
    .unwrap();
    let names = |cx: &mut TestAppContext| -> Vec<String> {
        store(cx, |s| s.people().unwrap())
            .into_iter()
            .filter(|p| !p.is_self)
            .map(|p| p.name)
            .collect()
    };

    // 押すとすぐ増えて、名前が全部選ばれた入力欄になる
    ui(cx, window, |window, cx| window.click("person-add", cx));
    assert_eq!(names(cx), [malakt_mahoz::state::NEW_PERSON_NAME]);
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.person_naming.is_some());
        assert_eq!(
            view.inputs.name.read(cx).value().as_ref(),
            malakt_mahoz::state::NEW_PERSON_NAME
        );
    });
    ui(cx, window, |window, cx| {
        assert!(window.find("person-input").visible());
        window.input("佐藤", cx);
        window.press("enter", cx);
    });
    assert_eq!(names(cx), ["佐藤"]);
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.person_naming, None);
        assert_eq!(view.modal, Modal::Appearance);
    });

    // 名前を押すと書き直せる。Esc は書きかけを捨てて、ダイアログは開いたまま
    let row = cx.update(|cx| {
        let view = view.read(cx);
        view.people.iter().position(|p| p.name == "佐藤").unwrap()
    });
    ui(cx, window, |window, cx| window.click(("person", row), cx));
    ui(cx, window, |window, cx| window.input("捨てる名前", cx));
    ui(cx, window, |window, cx| window.press("escape", cx));
    assert_eq!(names(cx), ["佐藤"]);
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.person_naming, None);
        assert_eq!(view.modal, Modal::Appearance);
    });

    // 書き直して、ほかの場所（追加のボタン）を押すと保存され、次の人が増える
    ui(cx, window, |window, cx| window.click(("person", row), cx));
    ui(cx, window, |window, cx| window.input("佐藤 花子", cx));
    ui(cx, window, |window, cx| window.click("person-add", cx));
    cx.run_until_parked();
    let mut all = names(cx);
    all.sort();
    let mut expected = ["佐藤 花子", malakt_mahoz::state::NEW_PERSON_NAME].map(String::from);
    expected.sort();
    assert_eq!(all, expected);

    // 空にして Enter を押すと、「新規担当者」のまま
    ui(cx, window, |window, cx| {
        window.press("backspace", cx);
        window.press("enter", cx);
    });
    assert_eq!(names(cx).len(), 2);
    assert!(names(cx).contains(&malakt_mahoz::state::NEW_PERSON_NAME.to_owned()));
    // もう一度 Esc でダイアログが閉じる
    ui(cx, window, |window, cx| window.press("escape", cx));
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::None));
}

/// ふつうのキーで操作する。↑↓ で選び、Enter で開き、F2 で名前を変え、Delete で確認を出して消す。
/// 入力欄の中では、同じキーは文字の編集のまま。
#[gpui_kit::test]
fn standard_keys_work_on_the_list(cx: &mut TestAppContext) {
    start(cx);
    let ws = cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let ws = db.store.create_workspace(None, "仕事").unwrap();
            for title in ["一", "二", "三"] {
                let draft = malakt_core::TaskDraft {
                    title: title.into(),
                    ..Default::default()
                };
                db.store.create_task(Some(&ws.id), &draft).unwrap();
            }
            ws.id
        })
    });
    let (window, view) = open(cx, wide());
    cx.update(|cx| view.update(cx, |v, cx| v.go(Nav::Workspace(ws.clone()), cx)));
    let key =
        |cx: &mut TestAppContext, key: &str| ui(cx, window, |window, cx| window.press(key, cx));
    let id_at = |cx: &mut TestAppContext, ix: usize| {
        cx.update(|cx| {
            let v = view.read(cx);
            v.tasks[v.visible[ix]].id.clone()
        })
    };
    let (first, second, third) = (id_at(cx, 0), id_at(cx, 1), id_at(cx, 2));
    ui(cx, window, |window, cx| {
        view.read(cx).focus.clone().focus(window, cx)
    });

    // 何も選んでいないときの F2 は、見ているワークスペースの名前の変更
    key(cx, "f2");
    cx.update(|cx| {
        let naming = view.read(cx).workspace_naming.clone();
        assert_eq!(naming.map(|n| n.id), Some(ws.clone()));
    });
    // 名前の入力欄の中では、↓ や Delete は一覧に届かない
    key(cx, "down");
    key(cx, "delete");
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.cursor, None);
        assert_eq!(v.modal, Modal::None);
    });
    // ↓ は名前の末尾へ動く（選んでいた名前は選ばれていない）。選び直して打ち替える
    ui(cx, window, |window, cx| {
        window.press("secondary-a", cx);
        window.input("しごと", cx);
        window.press("enter", cx);
    });
    assert_eq!(store(cx, |s| s.workspace(&ws).unwrap()).name, "しごと");

    // ↓ で 1 行目、もう一度で 2 行目。End と Home で端へ
    key(cx, "down");
    cx.update(|cx| assert_eq!(view.read(cx).cursor, Some(first.clone())));
    ui(cx, window, |window, _| {
        assert!(window.try_find(("row-cursor", 0usize)).is_some())
    });
    key(cx, "down");
    cx.update(|cx| assert_eq!(view.read(cx).cursor, Some(second.clone())));
    key(cx, "end");
    cx.update(|cx| assert_eq!(view.read(cx).cursor, Some(third.clone())));
    key(cx, "home");
    cx.update(|cx| assert_eq!(view.read(cx).cursor, Some(first.clone())));

    // Shift+↓ で続けてチェックが付き、Space で 1 行ずつ変わる
    key(cx, "shift-down");
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).selected_ids(),
            [first.clone(), second.clone()]
        )
    });
    key(cx, "space");
    cx.update(|cx| assert_eq!(view.read(cx).selected_ids(), std::slice::from_ref(&first)));
    key(cx, "secondary-a");
    cx.update(|cx| assert_eq!(view.read(cx).selected_ids().len(), 3));
    key(cx, "escape");
    cx.update(|cx| assert!(view.read(cx).selected.is_empty()));

    // Ctrl+C でリンクがクリップボードに入る
    key(cx, "secondary-c");
    let copied = cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied, Some(malakt_core::tasklink::url(&second)));

    // Enter で開き、Esc で閉じる
    key(cx, "enter");
    cx.update(|cx| assert_eq!(view.read(cx).panel, Some(second.clone())));
    key(cx, "escape");
    cx.update(|cx| assert_eq!(view.read(cx).panel, None));

    // F2 でタイトルをその場で変える
    key(cx, "f2");
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.panel, Some(second.clone()));
        assert_eq!(v.editing, Some(malakt_mahoz::state::Editing::Title));
    });
    ui(cx, window, |window, cx| {
        window.input("二の次", cx);
        window.press("enter", cx);
    });
    assert_eq!(store(cx, |s| s.task(&second).unwrap()).title, "二の次");
    key(cx, "escape");

    // Ctrl+D で複製。できたタスクが選ばれる
    key(cx, "secondary-d");
    let tasks = all_tasks(cx);
    assert_eq!(tasks.len(), 4);
    let copy = tasks.iter().find(|t| t.title == "二の次 のコピー").unwrap();
    assert_eq!(copy.workspace_id.as_deref(), Some(ws.as_str()));
    cx.update(|cx| assert_eq!(view.read(cx).cursor, Some(copy.id.clone())));

    // Delete は確認を出す。Esc でやめると消えない
    key(cx, "delete");
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).modal,
            Modal::DeleteTasks(vec![copy.id.clone()])
        )
    });
    key(cx, "escape");
    assert_eq!(all_tasks(cx).len(), 4);
    key(cx, "delete");
    ui(cx, window, |window, cx| {
        window.click("confirm-delete-tasks", cx)
    });
    assert_eq!(all_tasks(cx).len(), 3);

    // Ctrl+F で検索欄へ。その中の Delete や ↓ は文字の編集で、一覧は動かない
    key(cx, "home");
    key(cx, "secondary-f");
    ui(cx, window, |window, cx| window.input("三", cx));
    cx.update(|cx| assert_eq!(view.read(cx).visible.len(), 1));
    key(cx, "delete");
    key(cx, "down");
    key(cx, "f2");
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.modal, Modal::None);
        assert_eq!(v.editing, None);
        assert_eq!(v.cursor, Some(first.clone()));
    });

    // Esc で検索欄を出て、もう一度で行の選択も外れる。そのあとの F2 はワークスペースの名前
    key(cx, "escape");
    key(cx, "escape");
    cx.update(|cx| assert_eq!(view.read(cx).cursor, None));

    // Ctrl+N で新しいタスクができて、そのままタイトルを書ける
    key(cx, "secondary-n");
    ui(cx, window, |window, cx| {
        window.input("四", cx);
        window.press("enter", cx);
    });
    let tasks = all_tasks(cx);
    assert_eq!(tasks.len(), 4);
    assert!(tasks.iter().any(|t| t.title == "四"));
    key(cx, "escape");

    // F1 でキーの一覧、Ctrl+, でフォントとカラー
    key(cx, "f1");
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).pane_overlay,
            Some(malakt_mahoz::panes::PaneOverlay::Help)
        )
    });
    ui(cx, window, |window, _| {
        assert!(window.try_find("app-keys").is_some())
    });
    key(cx, "f1");
    cx.update(|cx| assert_eq!(view.read(cx).pane_overlay, None));
    key(cx, "secondary-,");
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::Appearance));
}

/// Ctrl+Z で元に戻し、Ctrl+Y でやり直す。編集、削除、画面の外からの変更（AI など）、貼り付けのどれも戻せる。
#[gpui_kit::test]
fn undo_redo_and_paste_work_with_keys(cx: &mut TestAppContext) {
    start(cx);
    let ws = cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let ws = db.store.create_workspace(None, "仕事").unwrap();
            for title in ["一", "二"] {
                let draft = malakt_core::TaskDraft {
                    title: title.into(),
                    ..Default::default()
                };
                db.store.create_task(Some(&ws.id), &draft).unwrap();
            }
            ws.id
        })
    });
    let (window, view) = open(cx, wide());
    cx.update(|cx| view.update(cx, |v, cx| v.go(Nav::Workspace(ws.clone()), cx)));
    let key =
        |cx: &mut TestAppContext, key: &str| ui(cx, window, |window, cx| window.press(key, cx));
    let titles = |cx: &mut TestAppContext| {
        let mut titles: Vec<String> = all_tasks(cx).into_iter().map(|t| t.title).collect();
        titles.sort();
        titles
    };
    ui(cx, window, |window, cx| {
        view.read(cx).focus.clone().focus(window, cx)
    });
    let first = cx.update(|cx| {
        let v = view.read(cx);
        v.tasks[v.visible[0]].id.clone()
    });

    // タイトルを変えて、戻して、やり直す
    key(cx, "down");
    key(cx, "f2");
    ui(cx, window, |window, cx| {
        window.input("一番", cx);
        window.press("enter", cx);
    });
    key(cx, "escape");
    assert_eq!(store(cx, |s| s.task(&first).unwrap()).title, "一番");
    key(cx, "secondary-z");
    assert_eq!(store(cx, |s| s.task(&first).unwrap()).title, "一");
    key(cx, "secondary-shift-z");
    assert_eq!(store(cx, |s| s.task(&first).unwrap()).title, "一番");

    // 削除を戻す
    key(cx, "delete");
    ui(cx, window, |window, cx| {
        window.click("confirm-delete-tasks", cx)
    });
    assert_eq!(titles(cx), ["二"]);
    key(cx, "secondary-z");
    assert_eq!(titles(cx), ["一番", "二"]);

    // 画面の操作を通らない変更（AI が変えた分）も、1 つ前の操作として戻せる
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            let mut draft = malakt_core::TaskDraft::from(&db.store.task(&first).unwrap());
            draft.priority = 1;
            db.store.update_task(&first, &draft).unwrap();
            cx.notify();
        })
    });
    cx.run_until_parked();
    key(cx, "secondary-z");
    let task = store(cx, |s| s.task(&first).unwrap());
    assert_eq!((task.title.as_str(), task.priority), ("一番", 0));

    // 文字を貼り付けると、1 行が 1 つのタスクになる。戻すと、まとめて消える
    cx.update(|cx| {
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
            "牛乳を買う\n\nhttps://example.com/a\n".into(),
        ))
    });
    key(cx, "secondary-v");
    assert_eq!(
        titles(cx),
        ["https://example.com/a", "一番", "二", "牛乳を買う"]
    );
    assert!(all_tasks(cx)
        .iter()
        .all(|t| t.workspace_id.as_deref() == Some(ws.as_str())));
    key(cx, "secondary-z");
    assert_eq!(titles(cx), ["一番", "二"]);

    // コピーしたタスクを貼り付けると、写しができる
    key(cx, "home");
    key(cx, "secondary-c");
    key(cx, "secondary-v");
    assert_eq!(titles(cx), ["一番", "一番", "二"]);

    // 入力欄の中の Ctrl+Z は文字の編集で、タスクは戻らない
    key(cx, "secondary-f");
    ui(cx, window, |window, cx| window.input("一", cx));
    key(cx, "secondary-z");
    assert_eq!(all_tasks(cx).len(), 3);
}

/// ワークスペースの一覧に、サブワークスペースが見出しの行として入る。見出しは開く・閉じるができ、
/// 開くと中のタスクと、さらに下のサブワークスペースが出る。
#[gpui_kit::test]
fn sub_workspaces_fold_inside_the_task_list(cx: &mut TestAppContext) {
    use gpui_kit::VisualTestContext;
    use malakt_core::view::ListRow;
    start(cx);
    let (ws, hall, setup, press) = cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let s = &mut db.store;
            let ws = s.create_workspace(None, "文化祭").unwrap();
            let hall = s.create_workspace(Some(&ws.id), "会場").unwrap();
            let setup = s.create_workspace(Some(&hall.id), "設営").unwrap();
            let press = s.create_workspace(Some(&ws.id), "広報").unwrap();
            s.add_task_in(Some(&ws.id), "予算を決める").unwrap();
            s.add_task_in(Some(&hall.id), "下見に行く").unwrap();
            s.add_task_in(Some(&setup.id), "机を並べる").unwrap();
            (ws, hall, setup, press)
        })
    });
    let (window, view) = open(cx, win(1600., 1000.));
    cx.update(|cx| {
        window
            .update(cx, |_, window, cx| {
                view.update(cx, |v, cx| {
                    v.track_session(window, cx);
                    v.go(Nav::Workspace(ws.id.clone()), cx)
                })
            })
            .unwrap()
    });
    cx.run_until_parked();
    // 行を「名前 数」（見出し）と「タイトル」（タスク）で書き出す。見出しは、開いていれば v、閉じていれば > を付ける
    let rows = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let v = view.read(cx);
            v.rows
                .iter()
                .map(|row| match *row {
                    ListRow::Task { index, depth } => {
                        format!("{depth}:{}", v.tasks[v.visible[index]].title)
                    }
                    ListRow::Folder {
                        workspace,
                        depth,
                        count,
                        open,
                    } => format!(
                        "{depth}:{}{} {count}",
                        if open { 'v' } else { '>' },
                        v.workspaces[workspace].name
                    ),
                })
                .collect::<Vec<_>>()
        })
    };
    let click = |cx: &mut TestAppContext, id: gpui_kit::ElementId| {
        ui(cx, window, |window, cx| window.click(id, cx))
    };
    let left = |cx: &mut TestAppContext, id: gpui_kit::ElementId| {
        let mut at = px(0.);
        ui(cx, window, |window, _| at = window.find(id).bounds().left());
        at
    };

    // 最初は、見出しだけが閉じて並ぶ。数は、さらに下のサブワークスペースの分も含む
    assert_eq!(rows(cx), ["0:>会場 2", "0:>広報 0", "0:予算を決める"]);
    assert!(!has(cx, window, "include-descendants"));
    cx.update(|cx| assert_eq!(view.read(cx).counts, (3, 0)));

    // 矢印を押すと開き、サブのサブも同じように開ける。中の行は 1 段ずつ下がる
    click(cx, ("folder-toggle", 0usize).into());
    assert_eq!(
        rows(cx),
        [
            "0:v会場 2",
            "1:>設営 1",
            "1:下見に行く",
            "0:>広報 0",
            "0:予算を決める"
        ]
    );
    click(cx, ("folder-toggle", 1usize).into());
    assert_eq!(
        rows(cx),
        [
            "0:v会場 2",
            "1:v設営 1",
            "2:机を並べる",
            "1:下見に行く",
            "0:>広報 0",
            "0:予算を決める"
        ]
    );
    let (deep, mid, top) = (
        left(cx, ("title", 0usize).into()),
        left(cx, ("title", 1usize).into()),
        left(cx, ("title", 2usize).into()),
    );
    assert!(deep > mid && mid > top, "{deep:?} {mid:?} {top:?}");
    assert!(
        left(cx, ("folder-toggle", 1usize).into()) > left(cx, ("folder-toggle", 0usize).into())
    );

    // 開いた見出しは設定に残る
    let saved = cx.update(|cx| view.read(cx).session_state(cx));
    let mut open_ids = vec![hall.id.clone(), setup.id.clone()];
    open_ids.sort();
    assert_eq!(saved.list_open, open_ids);

    // 絞り込むと、条件に合うタスクがないサブワークスペースは出ない
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.query.q = "机".into();
            v.refilter();
            cx.notify();
        })
    });
    assert_eq!(rows(cx), ["0:v会場 1", "1:v設営 1", "2:机を並べる"]);
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.query.clear_filters();
            v.refilter();
            cx.notify();
        })
    });

    // 完了のタブでも、見出しは残る
    click(cx, "tab-closed".into());
    assert_eq!(rows(cx), ["0:v会場 0", "1:v設営 0", "0:>広報 0"]);
    click(cx, "tab-open".into());

    // ↑↓ は見出しにも止まり、Space で開く・閉じる
    cx.update_window(window, |_, window, cx| {
        view.read(cx).focus.clone().focus(window, cx)
    })
    .unwrap();
    let key =
        |cx: &mut TestAppContext, key: &str| ui(cx, window, |window, cx| window.press(key, cx));
    key(cx, "down");
    cx.update(|cx| assert_eq!(view.read(cx).cursor.as_deref(), Some(hall.id.as_str())));
    assert!(has_id(cx, window, ("folder-cursor", 0usize)));
    key(cx, "down");
    cx.update(|cx| assert_eq!(view.read(cx).cursor.as_deref(), Some(setup.id.as_str())));
    key(cx, "space");
    assert_eq!(
        rows(cx),
        [
            "0:v会場 2",
            "1:>設営 1",
            "1:下見に行く",
            "0:>広報 0",
            "0:予算を決める"
        ]
    );
    // 見出しから Shift+↓ を押すと、すぐ下のタスクまでを選ぶ。いちばん上のタスクへは飛ばない
    key(cx, "shift-down");
    cx.update(|cx| {
        let v = view.read(cx);
        let picked: Vec<_> = v
            .tasks
            .iter()
            .filter(|t| v.selected.contains(&t.id))
            .map(|t| t.title.clone())
            .collect();
        assert_eq!(picked, ["下見に行く"]);
    });
    // 見出しへ戻って Shift+↑ を押しても、上にタスクがないので何も変わらない
    key(cx, "up");
    cx.update(|cx| assert_eq!(view.read(cx).cursor.as_deref(), Some(setup.id.as_str())));
    key(cx, "shift-up");
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.cursor.as_deref(), Some(setup.id.as_str()));
        assert_eq!(v.selected.len(), 1);
    });
    key(cx, "escape");
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.selected.clear();
            cx.notify();
        })
    });
    key(cx, "delete");
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::None));
    // 上の見出しを閉じると、中の見出しを選んでいたのは外れる
    click(cx, ("folder-toggle", 0usize).into());
    assert_eq!(rows(cx), ["0:>会場 2", "0:>広報 0", "0:予算を決める"]);
    cx.update(|cx| assert_eq!(view.read(cx).cursor, None));
    // Enter は、選んでいる見出しのワークスペースへ移る
    key(cx, "down");
    key(cx, "down");
    key(cx, "enter");
    cx.update(|cx| assert_eq!(view.read(cx).nav, Nav::Workspace(press.id.clone())));
    cx.update(|cx| view.update(cx, |v, cx| v.go(Nav::Workspace(ws.id.clone()), cx)));

    // タスクを見出しへドラッグすると、そのサブワークスペースへ移る
    let center = |cx: &mut TestAppContext, id: gpui_kit::ElementId| {
        let mut at = Point::default();
        ui(cx, window, |window, _| {
            at = window.find(id).bounds().center()
        });
        at
    };
    let drag = |cx: &mut TestAppContext, from: Point<Pixels>, to: Point<Pixels>| {
        let mut v = VisualTestContext::from_window(window, cx);
        v.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        for step in 1..=4 {
            let t = step as f32 / 4.;
            let at = Point::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
            v.simulate_mouse_move(at, MouseButton::Left, Modifiers::default());
            v.update(|window, cx| window.render_frame(cx));
        }
        v.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
        v.run_until_parked();
    };
    let (from, to) = (
        center(cx, ("title", 0usize).into()),
        center(cx, ("folder", 1usize).into()),
    );
    drag(cx, from, to);
    assert_eq!(rows(cx), ["0:>会場 2", "0:>広報 1"]);
    cx.update(|cx| assert_eq!(view.read(cx).nav, Nav::Workspace(ws.id.clone())));

    // 開いたサブワークスペースのタスクの行へ落とすと、そのサブワークスペースのその位置に入る
    click(cx, ("folder-toggle", 0usize).into());
    click(cx, ("folder-toggle", 3usize).into());
    assert_eq!(
        rows(cx),
        [
            "0:v会場 2",
            "1:>設営 1",
            "1:下見に行く",
            "0:v広報 1",
            "1:予算を決める"
        ]
    );
    let (from, to) = (
        center(cx, ("title", 0usize).into()),
        center(cx, ("title", 1usize).into()),
    );
    drag(cx, from, to);
    assert_eq!(
        rows(cx),
        [
            "0:v会場 1",
            "1:>設営 1",
            "0:v広報 2",
            "1:予算を決める",
            "1:下見に行く"
        ]
    );
    let moved = all_tasks(cx)
        .into_iter()
        .find(|t| t.title == "下見に行く")
        .unwrap();
    assert_eq!(moved.workspace_id.as_deref(), Some(press.id.as_str()));

    // 見出しの行を押すと、そのワークスペースへ移る
    click(cx, ("folder", 0usize).into());
    cx.update(|cx| assert_eq!(view.read(cx).nav, Nav::Workspace(hall.id.clone())));
    assert_eq!(rows(cx), ["0:>設営 1"]);

    // 狭い幅のカード表示でも、同じ行が出て同じように開ける
    let (narrow, small) = open(cx, win(420., 800.));
    cx.update(|cx| small.update(cx, |v, cx| v.go(Nav::Workspace(hall.id.clone()), cx)));
    ui(cx, narrow, |window, cx| {
        window.click(("folder-toggle", 0usize), cx)
    });
    ui(cx, narrow, |window, _| {
        let folder = window.find(("folder", 0usize)).bounds();
        let title = window.find(("title", 0usize)).bounds();
        assert!(folder.bottom() <= title.top());
        assert!(folder.right() <= window.viewport_size().width);
    });
    cx.update(|cx| {
        let v = small.read(cx);
        assert_eq!(v.visible.len(), 1);
        assert_eq!(v.tasks[v.visible[0]].title, "机を並べる");
    });
}

/// サブワークスペースの一覧の上に、最上位からのパンくずが出る。押すとその段のワークスペースへ移る。
/// 深いときは間を「…」にまとめ、そこから選べる。最上位では出さない。
#[gpui_kit::test]
fn breadcrumbs_lead_to_upper_workspaces(cx: &mut TestAppContext) {
    start(cx);
    let (top, hall, setup, desk, chair) = cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let s = &mut db.store;
            let top = s.create_workspace(None, "文化祭").unwrap();
            let hall = s.create_workspace(Some(&top.id), "会場").unwrap();
            let setup = s.create_workspace(Some(&hall.id), "設営").unwrap();
            let desk = s.create_workspace(Some(&setup.id), "机").unwrap();
            let chair = s.create_workspace(Some(&desk.id), "椅子").unwrap();
            (top.id, hall.id, setup.id, desk.id, chair.id)
        })
    });
    let (window, view) = open(cx, wide());
    let go = |cx: &mut TestAppContext, id: &str| {
        let id = id.to_owned();
        cx.update(|cx| view.update(cx, |v, cx| v.go(Nav::Workspace(id), cx)));
        cx.run_until_parked();
    };
    let crumb = |cx: &mut TestAppContext, id: &str| {
        cx.update(|cx| {
            let v = view.read(cx);
            (
                "crumb",
                v.workspaces.iter().position(|w| w.id == id).unwrap(),
            )
        })
    };

    // 最上位では出さない
    go(cx, &top);
    assert!(!has(cx, window, "crumbs"));

    // 3 段目では、上の 2 段と今いる所が並ぶ
    go(cx, &setup);
    assert!(has(cx, window, "crumbs"));
    for id in [&top, &hall, &setup] {
        let at = crumb(cx, id);
        assert!(has_id(cx, window, at), "{id}");
    }
    assert!(!has(cx, window, "crumb-more"));

    // 段を押すと、そのワークスペースへ移る
    let at = crumb(cx, &hall);
    ui(cx, window, |window, cx| window.click(at, cx));
    cx.update(|cx| assert_eq!(view.read(cx).nav, Nav::Workspace(hall.clone())));

    // 今いる所は押しても移らない
    go(cx, &setup);
    let at = crumb(cx, &setup);
    ui(cx, window, |window, cx| window.click(at, cx));
    cx.update(|cx| assert_eq!(view.read(cx).nav, Nav::Workspace(setup.clone())));

    // 5 段目では、最上位・「…」・上の 1 段・今いる所の 4 つにまとめる
    go(cx, &chair);
    for id in [&top, &desk, &chair] {
        let at = crumb(cx, id);
        assert!(has_id(cx, window, at), "{id}");
    }
    for id in [&hall, &setup] {
        let at = crumb(cx, id);
        assert!(!has_id(cx, window, at), "{id}");
    }
    assert!(has(cx, window, "crumb-more"));
}

fn has_id(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    id: impl Into<gpui_kit::ElementId>,
) -> bool {
    let mut found = false;
    let id = id.into();
    ui(cx, window, |window, _| {
        found = window.try_find(id).is_some()
    });
    found
}

/// 自分の画像を足すと、小さくして保存され、ワークスペースのアイコンになる。足した画像はあとからも選べる。
#[gpui_kit::test]
fn an_image_becomes_a_workspace_icon(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    let ws = cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            cx.notify();
            let ws = db.store.create_workspace(None, "Lorem").unwrap();
            db.store
                .set_workspace_icon(&ws.id, Some("rocket"), Some("orange"))
                .unwrap();
            ws.id
        })
    });
    cx.run_until_parked();
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.modal = Modal::WorkspaceIcon(ws.clone());
            cx.notify();
        })
    });
    ui(cx, window, |window, _| {
        assert!(window.find("icon-add-image").visible());
        assert!(window.try_find(("ws-custom-icon", 0usize)).is_none());
    });

    let dir = tempfile::tempdir().unwrap();
    // 開けないファイルは、名前を出して断る。アイコンはそのまま
    let text = dir.path().join("notes.png");
    std::fs::write(&text, "lorem ipsum").unwrap();
    cx.update(|cx| view.update(cx, |v, cx| v.add_icon_file(&ws, &text, cx)));
    cx.update(|cx| {
        let error = view.read(cx).error.clone().unwrap();
        assert!(error.contains("notes.png"), "{error}");
    });
    assert_eq!(
        store(cx, |s| s.workspace(&ws).unwrap()).icon.as_deref(),
        Some("rocket")
    );
    assert!(store(cx, |s| s.custom_icons().unwrap()).is_empty());

    // SVG は色のまま小さな PNG になる
    let svg = dir.path().join("logo.svg");
    std::fs::write(
        &svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 400 400"><circle cx="200" cy="200" r="180" fill="#e5484d"/></svg>"##,
    )
    .unwrap();
    cx.update(|cx| view.update(cx, |v, cx| v.add_icon_file(&ws, &svg, cx)));
    cx.run_until_parked();
    let saved = store(cx, |s| s.workspace(&ws).unwrap());
    let custom = saved.icon.clone().unwrap();
    assert!(malakt_core::icon::is_custom(&custom), "{custom}");
    assert_eq!(saved.color.as_deref(), Some("orange"));
    let png = store(cx, |s| s.custom_icon(&custom).unwrap()).unwrap();
    assert!(png.len() < 16 * 1024, "{} バイト", png.len());
    cx.update(|cx| assert_eq!(view.read(cx).error, None));
    cx.update(|cx| {
        assert!(malakt_mahoz::icons::custom_image(&custom, cx).is_some());
    });

    // ほかのアイコンにしても、足した画像は一覧の先頭に残っていて、押すと戻せる
    ui(cx, window, |window, cx| {
        assert!(window.find(("ws-custom-icon", 0usize)).visible());
        window.click(("ws-icon", 13usize), cx)
    });
    assert_eq!(
        store(cx, |s| s.workspace(&ws).unwrap()).icon.as_deref(),
        Some("rocket")
    );
    ui(cx, window, |window, cx| {
        window.click(("ws-custom-icon", 0usize), cx)
    });
    assert_eq!(
        store(cx, |s| s.workspace(&ws).unwrap()).icon,
        Some(custom.clone())
    );

    // 同じ画像をもう一度足しても増えない
    cx.update(|cx| view.update(cx, |v, cx| v.add_icon_file(&ws, &svg, cx)));
    assert_eq!(store(cx, |s| s.custom_icons().unwrap()), [custom]);

    // 絞り込んでいる間は、足した画像を出さない
    ui(cx, window, |window, cx| {
        window.click(("icon-category", 0usize), cx)
    });
    ui(cx, window, |window, _| {
        assert!(window.try_find(("ws-custom-icon", 0usize)).is_none());
    });
}

/// 状態は押して選ぶとすぐ変わる。コメントを付けるときだけダイアログが開く。
#[gpui_kit::test]
fn status_changes_from_a_menu_without_a_dialog(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    let id = cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            cx.notify();
            let draft = malakt_core::TaskDraft {
                title: "Lorem ipsum".into(),
                ..Default::default()
            };
            db.store.create_task(None, &draft).unwrap().id
        })
    });
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    cx.run_until_parked();
    let status = |cx: &mut TestAppContext| store(cx, |s| s.task(&id).unwrap()).status;

    // 一覧の状態を押すとメニューが開く。2 つ目は「進行中」
    ui(cx, window, |window, cx| {
        window.click(("status", 0usize), cx)
    });
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::None));
    ui(cx, window, |window, cx| {
        window.press("down", cx);
        window.press("down", cx);
        window.press("enter", cx);
    });
    assert_eq!(status(cx), Status::InProcess);
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::None));

    // 最後の項目は「コメントを付けて変更…」。ここからだけダイアログが開く
    ui(cx, window, |window, cx| {
        window.click(("status", 0usize), cx)
    });
    ui(cx, window, |window, cx| {
        window.press("up", cx);
        window.press("enter", cx);
    });
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).modal,
            Modal::Status(id.clone(), Status::Completed)
        );
    });
    ui(cx, window, |window, cx| {
        window.input("dolor sit amet", cx);
        window.click("status-submit", cx);
    });
    assert_eq!(status(cx), Status::Completed);
    let comments = store(cx, |s| s.comments(&id).unwrap());
    assert!(comments.iter().any(|c| c.is_completion_report()));

    // 詳細のボタンも、押すとすぐ変わる
    ui(cx, window, |window, cx| window.click("tab-closed", cx));
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |v, cx| v.open_panel(&id, window, cx))
    })
    .unwrap();
    cx.run_until_parked();
    ui(cx, window, |window, cx| window.click("to-reopen", cx));
    assert_eq!(status(cx), Status::NeedsAction);
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::None));
    ui(cx, window, |window, cx| window.click("to-cancelled", cx));
    assert_eq!(status(cx), Status::Cancelled);

    // 知らせの「元に戻す」で、すぐ戻せる
    ui(cx, window, |window, cx| window.click("notice-undo", cx));
    assert_eq!(status(cx), Status::NeedsAction);
    ui(cx, window, |window, _| {
        assert!(window.try_find("notice-undo").is_none());
    });
    // 知らせは一覧の右下の「＋」に重なる。知らせを押しても、下のボタンは押されない
    cx.update(|cx| assert_eq!(view.read(cx).detail.as_ref().unwrap().task.id, id));
    ui(cx, window, |window, cx| window.click("to-cancelled", cx));

    // 詳細の状態からも選べる。1 つ目は「未着手」
    ui(cx, window, |window, cx| window.click("p-status", cx));
    ui(cx, window, |window, cx| {
        window.press("down", cx);
        window.press("enter", cx);
    });
    assert_eq!(status(cx), Status::NeedsAction);

    // コメントを付けるボタンでダイアログが開く
    ui(cx, window, |window, cx| {
        window.click("status-with-comment", cx)
    });
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).modal,
            Modal::Status(id.clone(), Status::Completed)
        );
    });
}

/// ダイアログの外側の暗い所を押すと、いちばん上の 1 枚だけが閉じる。
/// 中で押し始めて外で離したときは閉じない。書きかけのコメントは残る。
#[gpui_kit::test]
fn clicking_outside_closes_the_top_dialog(cx: &mut TestAppContext) {
    use malakt_mahoz::state::SubModal;
    start(cx);
    let (window, view) = open(cx, wide());
    let id = cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.write(cx, |s| {
                let draft = malakt_core::TaskDraft {
                    title: "チラシを配る".into(),
                    ..Default::default()
                };
                Ok(s.create_task(None, &draft)?.id)
            })
            .unwrap()
        })
    });
    let spot = |cx: &mut TestAppContext, name: &'static str, inside: bool| {
        let mut at = Point::default();
        ui(cx, window, |window, _| {
            let bounds = window.find(name).bounds();
            at = if inside {
                bounds.center()
            } else {
                bounds.origin - Point::new(px(4.), px(4.))
            };
        });
        at
    };
    let press = |cx: &mut TestAppContext, from: Point<Pixels>, to: Point<Pixels>| {
        let mut v = gpui_kit::VisualTestContext::from_window(window, cx);
        v.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        v.simulate_mouse_move(to, MouseButton::Left, Modifiers::default());
        v.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
        v.run_until_parked();
    };

    // 設定の上に書体の一覧を重ねる。外側を押すと一覧だけが閉じ、もう一度押すと設定が閉じる
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.open_appearance(window, cx))
    });
    ui(cx, window, |window, cx| {
        window.click(("customize-nav", 3usize), cx)
    });
    ui(cx, window, |window, cx| window.click("font-open", cx));
    cx.update(|cx| assert_eq!(view.read(cx).sub_modal, Some(SubModal::Font)));
    let outside = spot(cx, "dialog", false);
    press(cx, outside, outside);
    cx.update(|cx| {
        assert_eq!(view.read(cx).sub_modal, None);
        assert_eq!(view.read(cx).modal, Modal::Appearance);
    });

    // 中で押し始めて外で離しても、外で押し始めて中で離しても、閉じない
    let inside = spot(cx, "dialog", true);
    let outside = spot(cx, "dialog", false);
    press(cx, inside, outside);
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::Appearance));
    press(cx, outside, inside);
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::Appearance));
    press(cx, outside, outside);
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::None));

    // 状態の変更に書きかけたコメントは、外側を押して閉じても、同じタスクで開き直すと残っている
    let ask = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |v, cx| v.ask_status(&id, window, cx))
        })
        .unwrap();
        cx.run_until_parked();
    };
    let comment = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            view.read(cx)
                .inputs
                .status_comment
                .read(cx)
                .value()
                .to_string()
        })
    };
    ask(cx);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |v, cx| {
            v.inputs
                .status_comment
                .update(cx, |s, cx| s.set_value("配り終えた", window, cx))
        })
    })
    .unwrap();
    let outside = spot(cx, "dialog", false);
    press(cx, outside, outside);
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::None));
    ask(cx);
    assert_eq!(comment(cx), "配り終えた");
    // キャンセルで閉じたときは残さない
    ui(cx, window, |window, cx| window.click("modal-cancel", cx));
    ask(cx);
    assert_eq!(comment(cx), "");
}

/// メインのウィンドウを閉じると、追加のウィンドウと AI チャットのウィンドウも閉じる。
/// 閉じたメインのウィンドウは、次の起動で追加のウィンドウと一緒に戻る。
#[gpui_kit::test]
fn closing_main_window_closes_the_others(cx: &mut TestAppContext) {
    start(cx);
    cx.update(malakt_mahoz::quit_when_main_closes);
    let (main, _) = cx.update(|cx| malakt_mahoz::open_main_window(cx).unwrap());
    let (second, second_view) = cx.update(|cx| malakt_mahoz::open_main_window(cx).unwrap());
    let (third, _) = cx.update(|cx| malakt_mahoz::open_main_window(cx).unwrap());
    cx.update(|cx| {
        malakt_mahoz::open_ai_window(malakt_mahoz::PaneMode::ChatWindow, cx).unwrap();
        second_view.update(cx, |v, cx| v.go(Nav::All, cx));
    });
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.windows().len()), 4);

    // 追加のウィンドウを閉じても、ほかのウィンドウは残る
    cx.update_window(third, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.windows().len()), 3);
    assert_eq!(cx.update(|cx| malakt_mahoz::session::saved(cx)).len(), 2);
    assert!(cx.update_window(second, |_, _, _| ()).is_ok());

    // メインを閉じると、全部閉じる
    cx.update_window(main, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.windows().len()), 0);

    // 次の起動では、メインと追加のウィンドウが戻る
    let saved = cx.update(|cx| malakt_mahoz::session::saved(cx));
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[0].nav, "home");
    assert_eq!(saved[1].nav, "all");
}

/// メインと一緒に最小化するウィンドウを覚えて、メインを戻すときに、開いているものだけを返す。
#[gpui_kit::test]
fn windows_minimized_with_the_main_window_come_back_together(cx: &mut TestAppContext) {
    use malakt_mahoz::session;
    start(cx);
    let (main, _) = cx.update(|cx| malakt_mahoz::open_main_window(cx).unwrap());
    let (second, _) = cx.update(|cx| malakt_mahoz::open_main_window(cx).unwrap());
    let (third, _) = cx.update(|cx| malakt_mahoz::open_main_window(cx).unwrap());
    cx.run_until_parked();
    assert_eq!(cx.update(session::main_window), Some(main));

    // メインを最小化したときに開いていた 2 枚を覚える。もう 1 度呼んでも、覚えは変わらない
    let tucked = cx.update(|cx| session::tuck(vec![second, third], cx));
    assert_eq!(tucked, vec![second, third]);
    assert!(cx.update(|cx| session::tuck(vec![second], cx)).is_empty());

    // 最小化している間に閉じたウィンドウは、戻すものに入らない
    cx.update_window(third, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert_eq!(cx.update(session::untuck), vec![second]);

    // 戻したあとは、覚えが空になる
    assert!(cx.update(session::untuck).is_empty());
}

/// 画面の下の知らせは 3 分で閉じる。マウスが乗っている間は閉じず、失敗は閉じるまで出したまま。
#[gpui_kit::test]
fn notice_closes_after_three_minutes(cx: &mut TestAppContext) {
    use malakt_mahoz::{NOTICE_FADE, NOTICE_LIFE};
    let second = std::time::Duration::from_secs(1);
    start(cx);
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| {
        window.click("nav-all", cx);
        window.click("quick-add", cx);
        window.input("知らせを出すタスク", cx);
        window.press("enter", cx);
    });
    let id = all_tasks(cx).remove(0).id;
    let notice = |cx: &mut TestAppContext| cx.update(|cx| view.read(cx).notice.clone());
    let set_status = |cx: &mut TestAppContext, status| {
        cx.update(|cx| view.update(cx, |v, cx| v.set_status(&id, status, cx)));
        ui(cx, window, |_, _| {});
    };

    set_status(cx, Status::Completed);
    assert!(notice(cx).is_some());
    cx.executor().advance_clock(NOTICE_LIFE - second);
    cx.run_until_parked();
    assert!(notice(cx).is_some());
    cx.executor().advance_clock(second);
    ui(cx, window, |window, _| {
        assert!(window.try_find("notice").is_none());
        assert!(window.try_find("notice-undo").is_none());
    });
    assert!(notice(cx).is_none());

    // 出し直した知らせは、そこから 3 分
    set_status(cx, Status::NeedsAction);
    cx.executor().advance_clock(NOTICE_LIFE - second);
    cx.run_until_parked();
    set_status(cx, Status::Completed);
    cx.executor().advance_clock(NOTICE_LIFE - second);
    cx.run_until_parked();
    assert!(notice(cx).is_some());
    cx.executor().advance_clock(second);
    cx.run_until_parked();
    assert!(notice(cx).is_none());

    // マウスが乗っている間は閉じず、離れたら閉じる
    set_status(cx, Status::NeedsAction);
    ui(cx, window, |window, cx| window.hover("notice", cx));
    cx.executor().advance_clock(NOTICE_LIFE * 2);
    cx.run_until_parked();
    assert!(notice(cx).is_some());
    ui(cx, window, |window, cx| window.hover("nav-all", cx));
    assert!(notice(cx).is_none());

    // 動きを付けているときは、薄くなってから消える。消えかけでも、乗れば戻る
    cx.update(|cx| view.update(cx, |v, _| v.motion = true));
    set_status(cx, Status::Completed);
    cx.executor().advance_clock(NOTICE_LIFE);
    cx.run_until_parked();
    cx.update(|cx| assert!(view.read(cx).notice_clock.closing));
    ui(cx, window, |window, cx| window.hover("notice", cx));
    cx.executor().advance_clock(NOTICE_FADE * 2);
    cx.run_until_parked();
    cx.update(|cx| assert!(!view.read(cx).notice_clock.closing));
    assert!(notice(cx).is_some());
    ui(cx, window, |window, cx| window.hover("nav-all", cx));
    cx.update(|cx| assert!(view.read(cx).notice_clock.closing));
    cx.executor().advance_clock(NOTICE_FADE);
    cx.run_until_parked();
    assert!(notice(cx).is_none());

    // 失敗は時間では閉じない
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.error = Some("保存できませんでした。".into());
            cx.notify();
        })
    });
    ui(cx, window, |_, _| {});
    cx.executor().advance_clock(NOTICE_LIFE * 2);
    cx.run_until_parked();
    ui(cx, window, |window, _| {
        assert!(window.try_find("notice").is_some())
    });
    cx.update(|cx| assert!(view.read(cx).error.is_some()));
}

/// 設定の「動作」に「AI のタブのシェル」があり、押すとシェルを選ぶメニューが開く。
#[gpui_kit::test]
fn the_ai_shell_is_chosen_in_customize(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    cx.update(|cx| {
        malakt_mahoz::term::shared(cx).update(cx, |terms, cx| {
            terms.set_profiles(
                vec![malakt_term::Profile {
                    id: "dash".into(),
                    name: "dash".into(),
                    kind: malakt_term::profile::Kind::Shell,
                    program: "/bin/sh".into(),
                    args: Vec::new(),
                }],
                cx,
            )
        })
    });
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.open_appearance(window, cx))
    });
    ui(cx, window, |window, cx| {
        window.click(("customize-nav", 6usize), cx)
    });
    ui(cx, window, |window, cx| {
        assert!(window.find("shell-choice").visible());
        assert!(window.try_find("shell-choice-reset").is_some());
        window.click("shell-choice", cx)
    });
    ui(cx, window, |window, _| {
        assert!(window.try_find("popup-menu").is_some());
    });
}

/// 絞り込みの右の × は、いつも同じ場所にあり、効いているときだけ押せる。押すとその絞り込みだけ外れる。
#[gpui_kit::test]
fn each_filter_is_cleared_by_its_own_x(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let s = &mut db.store;
            s.add_task_in(None, "会場を決める").unwrap();
            s.add_task_in(None, "機材を借りる").unwrap();
        })
    });
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    let ids = [
        "task-search-clear",
        "f-status-clear",
        "f-assignee-clear",
        "f-role-clear",
        "f-priority-clear",
        "f-category-clear",
    ];
    let disabled = |cx: &mut TestAppContext, id: &'static str| {
        let mut out = None;
        ui(cx, window, |window, _| out = window.find(id).disabled());
        out == Some(true)
    };
    let place = |cx: &mut TestAppContext, id: &'static str| {
        let mut out = None;
        ui(cx, window, |window, _| out = Some(window.find(id).bounds()));
        out.unwrap()
    };
    // 何も絞り込んでいないときも置いてあり、押せない
    let before: Vec<_> = ids.iter().map(|id| place(cx, id)).collect();
    for id in ids {
        assert!(disabled(cx, id), "{id} は押せない");
    }

    // キーワードを打つと、検索の × だけが押せるようになる
    ui(cx, window, |window, cx| {
        window.click("task-search", cx);
        window.input("会場", cx);
    });
    cx.update(|cx| assert_eq!(view.read(cx).visible.len(), 1));
    assert!(!disabled(cx, "task-search-clear"));
    assert!(disabled(cx, "f-status-clear"));
    // × が押せるようになっても、欄の場所と大きさは変わらない
    assert_eq!(place(cx, "task-search-clear"), before[0]);
    ui(cx, window, |window, cx| {
        window.click("task-search-clear", cx)
    });
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.query.q, "");
        assert_eq!(v.inputs.search.read(cx).value().as_ref(), "");
        assert_eq!(v.visible.len(), 2);
    });
    assert!(disabled(cx, "task-search-clear"));

    // 状態と優先度を絞り込む。状態の × は状態だけを外す
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.query.status = Some(Status::InProcess);
            v.query.priority = Some(malakt_core::view::Priority::High);
            v.query.my_role = Some(Role::A);
            v.query.assignee = malakt_core::view::AssigneeFilter::Unassigned;
            v.query.category = Some("設営".into());
            v.refilter();
            cx.notify();
        })
    });
    for id in &ids[1..] {
        assert!(!disabled(cx, id), "{id} は押せる");
    }
    ui(cx, window, |window, cx| window.click("f-status-clear", cx));
    cx.update(|cx| {
        let q = &view.read(cx).query;
        assert_eq!(q.status, None);
        assert_eq!(q.priority, Some(malakt_core::view::Priority::High));
    });
    assert!(disabled(cx, "f-status-clear"));
    assert!(!disabled(cx, "f-priority-clear"));
    for id in [
        "f-assignee-clear",
        "f-role-clear",
        "f-priority-clear",
        "f-category-clear",
    ] {
        ui(cx, window, |window, cx| window.click(id, cx));
    }
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(v.query.active_count(), 0);
        assert_eq!(v.visible.len(), 2);
    });
    let after: Vec<_> = ids.iter().map(|id| place(cx, id)).collect();
    assert_eq!(before, after);
    for id in ids {
        assert!(disabled(cx, id), "{id} は押せない");
    }
}

/// 入力欄を 2 つだけ置いた画面。↑↓ の動きを確かめる。
struct Fields {
    line: Entity<InputState>,
    area: Entity<gpui_kit::component::input::TextareaState>,
}

impl gpui_kit::Render for Fields {
    fn render(
        &mut self,
        _: &mut gpui_kit::Window,
        _: &mut gpui_kit::Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        use gpui_kit::component::input::{Input, Textarea};
        use gpui_kit::{div, ParentElement, Styled};
        div().size_full().child(Input::new(&self.line)).child(
            div()
                .w(px(160.))
                .child(Textarea::new(&self.area).h(px(200.))),
        )
    }
}

fn open_fields(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Fields>) {
    start(cx);
    cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| Fields {
                line: cx.new(|cx| InputState::new(window, cx)),
                area: cx.new(|cx| gpui_kit::component::input::TextareaState::new(window, cx)),
            })
        })
        .expect("ウィンドウを開く")
    })
}

/// 1 行の欄では、↑ で文の先頭へ、↓ で文の末尾へ動く。Shift を押せば、そこまで選ぶ。
#[gpui_kit::test]
fn up_and_down_go_to_the_ends_in_a_line(cx: &mut TestAppContext) {
    let (window, view) = open_fields(cx);
    let line = cx.update(|cx| view.read(cx).line.clone());
    let at = |cx: &mut TestAppContext, range: std::ops::Range<usize>| {
        cx.update_window(window, |_, window, cx| {
            line.update(cx, |s, cx| {
                s.focus(window, cx);
                s.set_selected_range(range, cx);
            })
        })
        .unwrap();
    };
    let selected = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let s = line.read(cx);
            (s.selected_range(), s.is_selection_reversed())
        })
    };
    ui(cx, window, |window, cx| {
        line.update(cx, |s, cx| s.focus(window, cx));
        window.input("牛乳を買う", cx);
    });
    let len = "牛乳を買う".len();

    at(cx, 6..6);
    ui(cx, window, |window, cx| window.press("up", cx));
    assert_eq!(selected(cx), (0..0, false), "↑ で先頭へ");
    ui(cx, window, |window, cx| window.press("down", cx));
    assert_eq!(selected(cx), (len..len, false), "↓ で末尾へ");
    // 押し続けても、そこにとどまる
    ui(cx, window, |window, cx| window.press("down", cx));
    assert_eq!(selected(cx), (len..len, false));

    at(cx, 6..6);
    ui(cx, window, |window, cx| window.press("shift-up", cx));
    assert_eq!(selected(cx), (0..6, true), "Shift+↑ で先頭まで選ぶ");
    ui(cx, window, |window, cx| window.press("shift-down", cx));
    assert_eq!(
        selected(cx),
        (6..len, false),
        "Shift+↓ で、選び始めた所から末尾まで選ぶ"
    );

    // 選んでいるときの ↑ も、選ぶのをやめて先頭へ
    at(cx, 3..9);
    ui(cx, window, |window, cx| window.press("up", cx));
    assert_eq!(selected(cx), (0..0, false));
}

/// 複数行の欄では、↑↓ はまず行を動き、いちばん上の行の ↑ で文の先頭へ、いちばん下の行の ↓ で
/// 文の末尾へ動く。折り返した行も、見た目の行で数える。
#[gpui_kit::test]
fn up_and_down_go_to_the_ends_in_a_textarea(cx: &mut TestAppContext) {
    let (window, view) = open_fields(cx);
    let area = cx.update(|cx| view.read(cx).area.clone());
    let first = "一行目のメモ";
    let text = format!("{first}\n二行目\n三行目のメモ");
    ui(cx, window, |window, cx| {
        area.update(cx, |s, cx| {
            s.set_value(text.clone(), window, cx);
            s.focus(window, cx);
        });
    });
    let cursor = |cx: &mut TestAppContext| cx.update(|cx| area.read(cx).selected_range());
    let second = first.len() + 1;
    cx.update(|cx| area.update(cx, |s, cx| s.set_selected_range(second + 3..second + 3, cx)));

    ui(cx, window, |window, cx| window.press("up", cx));
    let range = cursor(cx);
    assert!(
        range.is_empty() && range.start > 0 && range.start < first.len(),
        "↑ でまず 1 行目へ: {range:?}"
    );
    ui(cx, window, |window, cx| window.press("up", cx));
    assert_eq!(cursor(cx), 0..0, "1 行目の ↑ で先頭へ");

    ui(cx, window, |window, cx| {
        window.press("down", cx);
        window.press("down", cx);
    });
    let range = cursor(cx);
    assert!(
        range.start > second && range.start < text.len(),
        "↓ で 3 行目へ: {range:?}"
    );
    ui(cx, window, |window, cx| window.press("down", cx));
    assert_eq!(cursor(cx), text.len()..text.len(), "最後の行の ↓ で末尾へ");
}

/// 1 行の文が折り返していれば、上の見た目の行の ↑ で先頭へ動く前に、まず見た目の行を上がる。
#[gpui_kit::test]
fn up_moves_by_wrapped_rows_first(cx: &mut TestAppContext) {
    let (window, view) = open_fields(cx);
    let area = cx.update(|cx| view.read(cx).area.clone());
    let text = "とても長い一行の文で、欄の幅に入りきらないので、いくつもの見た目の行に折り返す。";
    ui(cx, window, |window, cx| {
        area.update(cx, |s, cx| {
            s.set_value(text, window, cx);
            s.focus(window, cx);
        });
    });
    cx.update(|cx| area.update(cx, |s, cx| s.set_selected_range(text.len()..text.len(), cx)));
    ui(cx, window, |window, cx| window.press("up", cx));
    let range = cx.update(|cx| area.read(cx).selected_range());
    assert!(
        range.start > 0 && range.start < text.len(),
        "折り返した行では、まず 1 つ上の見た目の行へ: {range:?}"
    );
}

/// アプリの入力欄（タスクを追加する欄）でも、↑ で先頭へ、↓ で末尾へ動く。
#[gpui_kit::test]
fn up_goes_to_the_start_in_quick_add(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| {
        window.click("nav-all", cx);
        window.click("quick-add", cx);
        window.input("牛乳を買う", cx);
    });
    let quick_add = cx.update(|cx| view.read(cx).inputs.quick_add.clone());
    let cursor = |cx: &mut TestAppContext| cx.update(|cx| quick_add.read(cx).selected_range());
    let len = "牛乳を買う".len();
    assert_eq!(cursor(cx), len..len);
    ui(cx, window, |window, cx| window.press("up", cx));
    assert_eq!(cursor(cx), 0..0, "↑ で先頭へ");
    ui(cx, window, |window, cx| window.press("down", cx));
    assert_eq!(cursor(cx), len..len, "↓ で末尾へ");
}

/// 期限は開始日からの範囲で出る。見せ方（日付・相対）は、期限のセルと見出しの右クリックと、
/// 設定の「表示」で選べ、どのウィンドウの一覧にも同じように出る。
#[gpui_kit::test]
fn due_format_is_picked_from_the_due_column_and_settings(cx: &mut TestAppContext) {
    use malakt_core::view::{self, DueFormat};
    use malakt_mahoz::state::Column;
    start(cx);
    let today = view::today();
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let draft = malakt_core::TaskDraft {
                title: "会場を押さえる".into(),
                start: Some(view::add_days(&today, -2)),
                due: Some(view::add_days(&today, 3)),
                ..Default::default()
            };
            db.store.create_task(None, &draft).unwrap()
        })
    });
    let (window, view) = open(cx, win(1600., 900.));
    let (_second, _) = open(cx, wide());
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    let format = |cx: &mut TestAppContext| cx.update(|cx| malakt_mahoz::theme::due_format(cx));
    let saved = |cx: &mut TestAppContext| store(cx, |s| s.setting("due.format").unwrap());
    assert_eq!(format(cx), DueFormat::Date);
    let right_click = |cx: &mut TestAppContext, id: gpui_kit::ElementId| {
        let mut at = None;
        ui(cx, window, |window, _| {
            at = Some(window.try_find(id).unwrap().bounds().center());
        });
        let mut v = gpui_kit::VisualTestContext::from_window(window, cx);
        v.simulate_mouse_down(at.unwrap(), MouseButton::Right, Modifiers::default());
        v.simulate_mouse_up(at.unwrap(), MouseButton::Right, Modifiers::default());
        v.run_until_parked();
    };

    // 期限のセルを右クリックすると、見せ方のメニューが開く（行のメニューは開かない）
    right_click(cx, ("due-cell", 0usize).into());
    ui(cx, window, |window, _| {
        assert!(window.try_find_by_label("日付で表示").is_some());
        assert!(window.try_find_by_label("削除").is_none());
    });
    pick(cx, window, "相対で表示");
    assert_eq!(format(cx), DueFormat::Relative);
    assert_eq!(saved(cx).as_deref(), Some("relative"));

    // 見出しの右クリックのメニューにも同じ項目がある。日付に戻すと、保存した値は消える
    right_click(cx, ("col-menu", Column::Due as usize).into());
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).column_menu.as_ref().map(|m| m.column),
            Some(Some(Column::Due))
        )
    });
    pick(cx, window, "日付で表示");
    assert_eq!(format(cx), DueFormat::Date);
    assert_eq!(saved(cx), None);
    // ほかの見出しのメニューには出さない
    right_click(cx, ("col-menu", Column::Status as usize).into());
    ui(cx, window, |window, _| {
        assert!(window.try_find_by_label("昇順で並べ替え").is_some());
        assert!(window.try_find_by_label("相対で表示").is_none());
    });
    right_click(cx, ("col-menu", Column::Status as usize).into());

    // 設定の「表示」にも同じ項目がある
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.open_appearance(window, cx))
    });
    ui(cx, window, |window, cx| {
        window.click(("customize-nav", 4usize), cx)
    });
    ui(cx, window, |window, cx| {
        window.click(("due-format", 1usize), cx)
    });
    assert_eq!(format(cx), DueFormat::Relative);
    ui(cx, window, |window, cx| {
        window.click(("due-format", 0usize), cx)
    });
    assert_eq!(format(cx), DueFormat::Date);
    // 設定の検索でも見つかる
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| {
            v.inputs
                .customize_search
                .update(cx, |s, cx| s.set_value("相対", window, cx))
        })
    });
    ui(cx, window, |window, _| {
        assert!(window.try_find(("due-format", 1usize)).is_some())
    });
}

/// 誰がいつ作り、何をどう変えたかが、詳細の上・変更履歴・コメント欄の出来事に出る。
/// 自分・AI（自動の実行）・外のエージェント・リマインダーを分けて残す。
#[gpui_kit::test]
fn history_shows_who_changed_what(cx: &mut TestAppContext) {
    use malakt_core::history;
    use malakt_core::{Actor, TaskDraft};
    start(cx);
    let (window, view) = open(cx, win(1280., 2400.));
    let id = cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.write(cx, |s| {
                let draft = TaskDraft {
                    title: "会場の下見".into(),
                    ..Default::default()
                };
                Ok(s.create_task(None, &draft)?.id)
            })
            .unwrap()
        })
    });
    let watch = Actor::ai("Claude", Some("GitHub の定期チェック"));
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            let s = &mut db.store;
            let mut d = TaskDraft::from(&s.task(&id).unwrap());
            d.due = Some("2099-01-05".into());
            s.acting(watch.clone(), |s| s.update_task(&id, &d)).unwrap();
            d.description = "下見の段取り".into();
            s.acting(Actor::agent("claude-code"), |s| s.update_task(&id, &d))
                .unwrap();
            s.acting(Actor::Reminders, |s| {
                s.set_task_status(&id, Status::Completed)
            })
            .unwrap();
            cx.notify();
        })
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |v, cx| v.open_panel(&id, window, cx))
    })
    .unwrap();
    ui(cx, window, |_, _| {});
    let task = store(cx, |s| s.task(&id).unwrap());
    assert_eq!(task.created_by, Actor::Me);
    assert_eq!(task.updated_by, Actor::Reminders);

    cx.update(|cx| {
        let v = view.read(cx);
        let detail = v.detail.as_ref().unwrap();
        let lines: Vec<String> = v
            .timeline_events(detail, cx)
            .iter()
            .map(|e| {
                format!(
                    "{}|{}|{}",
                    v.actor_name(&e.actor).unwrap_or_default(),
                    e.actor.badge().unwrap_or_default(),
                    e.text
                )
            })
            .collect();
        assert_eq!(
            lines,
            [
                "自分||このタスクを作成しました",
                "AI（Claude）|GitHub の定期チェック|期限を 2099/1/5 に設定しました",
                "claude-code|外部エージェント|説明を追加しました",
                "自分|リマインダー|完了にしました",
            ]
        );
        // 変更履歴は 1 回の変更ごとにまとまり、作成から全部記録されている
        let entries = history::entries(&detail.changes);
        assert_eq!(entries.len(), 4);
        assert!(!history::misses_older(&entries));
    });

    // 変更履歴を開く。説明の出来事からも開ける
    ui(cx, window, |window, cx| window.click("history", cx));
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::History));
    cx.update(|cx| view.update(cx, |v, cx| v.dismiss(cx)));
    ui(cx, window, |window, cx| {
        window.click(("event-history", 2usize), cx)
    });
    cx.update(|cx| assert_eq!(view.read(cx).modal, Modal::History));

    // 差分の文字は、ドラッグで選んで Ctrl+C（macOS は ⌘C）でコピーできる
    let ids: Vec<String> = cx
        .update_window(window, |_, window, _| {
            ["history-1-0-after", "history-1-0-0-after"]
                .into_iter()
                .filter(|id| window.try_find(*id).is_some())
                .map(String::from)
                .collect()
        })
        .unwrap();
    let added = ids.first().expect("説明を足した差分の枠がある").clone();
    let mut bounds = None;
    ui(cx, window, |window, _| {
        bounds = Some(
            window
                .find(gpui_kit::SharedString::from(added.clone()))
                .bounds(),
        )
    });
    let b = bounds.unwrap();
    // ダイアログを開いている間も、フォーカスはタスクの画面にある
    cx.update_window(window, |_, window, cx| {
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx)
    })
    .unwrap();
    {
        use gpui_kit::{point, Modifiers, MouseButton, VisualTestContext};
        // 枠の余白（左右 0.625rem）の内側から、右の端まで
        let (from, to) = (
            point(b.left() + px(13.5), b.center().y),
            point(b.right() - px(2.), b.center().y),
        );
        let mut v = VisualTestContext::from_window(window, cx);
        v.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        v.update(|window, cx| window.render_frame(cx));
        v.simulate_mouse_move(to, MouseButton::Left, Modifiers::default());
        v.update(|window, cx| window.render_frame(cx));
        v.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
        v.run_until_parked();
    }
    ui(cx, window, |window, cx| window.press("secondary-c", cx));
    let copied = cx.update(|cx| cx.read_from_clipboard().and_then(|i| i.text()));
    assert_eq!(copied.as_deref(), Some("下見の段取り"));
}

/// 詳細の「繰り返し」をその場のメニューで選び、時刻と AI への指示を付ける。
/// 時刻が来ると次の回が一覧に出て、繰り返しは新しい回へ移る。Ctrl+Z で設定を戻せる。
#[gpui_kit::test]
fn repeat_is_chosen_in_place_and_makes_the_next_copy(cx: &mut TestAppContext) {
    start(cx);
    // 2099-11-30 は月曜
    let task = cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let draft = malakt_core::TaskDraft {
                title: "週次定例の報告".into(),
                due: Some("2099-11-30 10:00".into()),
                ..Default::default()
            };
            db.store.create_task(None, &draft).unwrap()
        })
    });
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    let repeat = |cx: &mut TestAppContext| store(cx, |s| s.repeat(&task.id).unwrap());
    assert!(repeat(cx).is_none());

    ui(cx, window, |window, cx| window.click("task-repeat", cx));
    pick(cx, window, "毎週 月");
    let r = repeat(cx).unwrap();
    assert_eq!(r.rule.label(), "毎週 月");
    assert_eq!(r.next_at, "2099-12-07 00:00");
    ui(cx, window, |window, _| {
        assert!(window.find_by_label("繰り返し: 毎週 月").visible());
    });

    // 時刻は、メニューの「時刻」から
    ui(cx, window, |window, cx| window.click("task-repeat", cx));
    pick(cx, window, "時刻");
    pick(cx, window, "9:00");
    assert_eq!(repeat(cx).unwrap().rule.label(), "毎週 月 9:00");
    assert_eq!(repeat(cx).unwrap().next_at, "2099-12-07 09:00");

    // AI への指示は、その場で書いて Enter
    ui(cx, window, |window, cx| window.click("repeat-ai-add", cx));
    ui(cx, window, |window, cx| {
        window.input("先週のまとめを下書きする", cx);
        // Shift+Enter で改行できる。長い指示は折り返して全部見える
        window.press("shift-enter", cx);
        window.input("終わったら担当に知らせる", cx);
        window.press("enter", cx);
    });
    assert_eq!(
        repeat(cx).unwrap().ai,
        "先週のまとめを下書きする\n終わったら担当に知らせる"
    );
    ui(cx, window, |window, _| {
        assert!(window.find("repeat-ai-text").visible());
    });

    // Ctrl+Z で、指示を書く前に戻る
    ui(cx, window, |window, cx| window.press("secondary-z", cx));
    assert_eq!(repeat(cx).unwrap().ai, "");

    // 時刻が来ると次の回ができ、繰り返しはそちらへ移る
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, cx| {
            assert!(db.sweep_repeats("2099-12-07 09:00"));
            cx.notify();
        })
    });
    cx.run_until_parked();
    let tasks = all_tasks(cx);
    assert_eq!(tasks.len(), 2);
    let copy = tasks.iter().find(|t| t.id != task.id).unwrap();
    assert_eq!(copy.due.as_deref(), Some("2099-12-07 10:00"));
    assert_eq!(copy.created_by, malakt_core::Actor::Repeat);
    assert!(repeat(cx).is_none());
    // 開いている前の回は「なし」に戻る
    ui(cx, window, |window, _| {
        assert!(window.find_by_label("繰り返し: なし").visible());
    });
    // 外す
    ui(cx, window, |window, cx| {
        view.update(cx, |v, cx| v.open_panel(&copy.id, window, cx))
    });
    ui(cx, window, |window, cx| window.click("task-repeat", cx));
    pick(cx, window, "なし");
    assert!(store(cx, |s| s.repeat(&copy.id).unwrap()).is_none());
}
