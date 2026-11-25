//! 画面を通したシナリオテスト。実際のウィンドウ（ヘッドレス）を開き、クリックと入力で操作する。

use gpui_kit::component::input::InputState;
use gpui_kit::test::{TestQueryExt, TestWindowExt};
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext, Bounds, Entity, Pixels, Point, Size, TestAppContext,
    WindowBounds, WindowOptions,
};
use malakt_core::view::Tab;
use malakt_core::{lock, DueRule, Role, Roles, Scope, Status, Store, TaskFilter};
use malakt_mahoz::passkey::{Failure, Passkey};
use malakt_mahoz::{db, CommentMode, MainView, Modal, Nav};

fn start(cx: &mut TestAppContext) {
    cx.update(|cx| malakt_mahoz::init(Store::open_in_memory().unwrap(), cx));
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

    // 完了にする（コメントは書かない）
    ui(cx, window, |window, cx| window.click("to-completed", cx));
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).modal,
            Modal::Status(opened.clone(), Status::Completed)
        );
    });
    ui(cx, window, |window, cx| window.click("status-submit", cx));
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

    // 親を開くと、子のタスクも見える
    cx.update(|cx| view.update(cx, |v, cx| v.go(Nav::Workspace(parent.id.clone()), cx)));
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
            Modal::People,
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

    // 担当を押すと選ぶところが開く。2 人を担当にすると、どちらも R から始まる
    ui(cx, window, |window, cx| window.click("p-assignees", cx));
    ui(cx, window, |window, cx| window.click(("p-as", 0usize), cx));
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

    // ワークスペースの外で足すタスクには、何も入らない
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
    assert!(memo.due.is_none() && memo.assignee_ids.is_empty());
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

    // 期限: 正しくない値は断り、ダイアログを開いたままにする
    ui(cx, window, |window, cx| window.click("bulk-due", cx));
    set(cx, |v| &v.inputs.bulk_due, "そのうち");
    ui(cx, window, |window, cx| window.click("bulk-due-submit", cx));
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.modal, Modal::BulkDue);
        assert!(view.error.is_some());
    });
    set(cx, |v| &v.inputs.bulk_due, "2026/12/24 午後6時");
    ui(cx, window, |window, cx| window.click("bulk-due-submit", cx));
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

/// ロック中は、タスクのタイトルを OS の通知に出さない。解除したら出す。
#[gpui_kit::test]
fn notices_wait_until_unlocked(cx: &mut TestAppContext) {
    start(cx);
    let today = malakt_core::view::today();
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let draft = malakt_core::TaskDraft {
                title: "ひみつの用事".into(),
                due: Some(today.clone()),
                ..Default::default()
            };
            db.store.create_task(None, &draft).unwrap();
            lock::enable(&mut db.store).unwrap();
        })
    });
    cx.update(db::lock_now);
    let (window, view) = open(cx, wide());
    cx.run_until_parked();
    cx.update(|cx| assert!(!view.read(cx).unlocked(cx)));
    assert!(cx.shown_system_notifications().is_empty());

    passkey(cx, Ok(()));
    ui(cx, window, |window, cx| window.click("unlock", cx));
    cx.update(|cx| assert!(view.read(cx).unlocked(cx)));
    assert_eq!(cx.shown_system_notifications().len(), 1);
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
    // 閉じると、チケットの文字列は画面から消える
    ui(cx, window, |window, cx| window.click("dialog-close", cx));
    cx.update(|cx| assert!(view.read(cx).issued_ticket.is_none()));

    // 別のプロセスとして引き換える
    let args: Vec<String> = ["redeem", &secret, "--message", "駅前で配り終えました"]
        .iter()
        .map(|a| (*a).to_owned())
        .collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = malakt_mahoz::cli::run(&args, || Store::open(&path), &mut out, &mut err);
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));

    // 1 分ごとの見張りが気づくのと同じことをする
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
    // 足したあとは、次の 1 件のために最初の値へ戻る
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.quick.priority, 0);
        assert_eq!(view.quick.status, Status::NeedsAction);
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

/// 詳細の期限は、日付の欄と時刻の欄で決める。「明日」「11/27 14時」のような書き方も読み、
/// 保存すると欄は保存した形に直る。一覧のメニューで日付を変えても時刻は残る。
#[gpui_kit::test]
fn due_takes_a_time_and_many_spellings(cx: &mut TestAppContext) {
    use malakt_core::view::{add_days, today};
    start(cx);
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| {
        window.click("nav-all", cx);
        window.click("quick-add", cx);
        window.input("資料を出す", cx);
        window.press("enter", cx);
    });
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    let due = |cx: &mut TestAppContext| all_tasks(cx)[0].due.clone();
    let shown = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let inputs = &view.read(cx).inputs;
            (
                inputs.due.read(cx).value().to_string(),
                inputs.due_time.read(cx).value().to_string(),
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

    // 時刻だけを入れると、今日のその時刻になる
    type_in(cx, "due-time-input", "14時");
    assert_eq!(due(cx), Some(format!("{} 14:00", today())));
    assert_eq!(shown(cx), (today(), "14:00".to_owned()));

    // 日付だけを書き換えても、時刻は残る
    type_in(cx, "due-input", "明日");
    let tomorrow = add_days(&today(), 1);
    assert_eq!(due(cx), Some(format!("{tomorrow} 14:00")));
    assert_eq!(shown(cx), (tomorrow.clone(), "14:00".to_owned()));

    // 日付の欄に時刻まで書くと、両方が変わる
    type_in(cx, "due-input", "2027/1/5 午後3時半");
    assert_eq!(due(cx).as_deref(), Some("2027-01-05 15:30"));
    assert_eq!(shown(cx), ("2027-01-05".to_owned(), "15:30".to_owned()));
    type_in(cx, "due-time-input", "9:05");
    assert_eq!(due(cx).as_deref(), Some("2027-01-05 09:05"));

    // 読めない書き方は断り、書いたものと元の期限を残す
    type_in(cx, "due-time-input", "25時");
    assert!(cx.update(|cx| view.read(cx).error.is_some()));
    assert_eq!(due(cx).as_deref(), Some("2027-01-05 09:05"));
    assert_eq!(shown(cx).1, "25時");

    // 時刻を空にすると、日付だけの期限に戻る
    type_in(cx, "due-time-input", "");
    assert_eq!(due(cx).as_deref(), Some("2027-01-05"));
    assert!(cx.update(|cx| view.read(cx).error.is_none()));

    // ボタンで日付を変えても時刻は残り、クリアで両方の欄が空になる
    type_in(cx, "due-time-input", "2pm");
    ui(cx, window, |window, cx| window.click(("p-due", 1usize), cx));
    assert_eq!(due(cx), Some(format!("{tomorrow} 14:00")));
    assert_eq!(shown(cx), (tomorrow.clone(), "14:00".to_owned()));

    // 書き換えてからほかの場所を押しても保存する
    cx.update_window(window, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    ui(cx, window, |window, cx| {
        window.click("due-time-input", cx);
        window.press("secondary-a", cx);
        window.press("backspace", cx);
        window.input("18:00", cx);
    });
    ui(cx, window, |window, cx| window.click("due-input", cx));
    assert_eq!(due(cx), Some(format!("{tomorrow} 18:00")));

    ui(cx, window, |window, cx| window.click(("p-due", 3usize), cx));
    assert_eq!(due(cx), None);
    assert_eq!(shown(cx), (String::new(), String::new()));

    // 開始日も同じ書き方を読む
    type_in(cx, "start-input", "20270104 10時");
    assert_eq!(all_tasks(cx)[0].start.as_deref(), Some("2027-01-04 10:00"));
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
        window.press("ctrl-a", cx);
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
        window.press("ctrl-a", cx);
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
        window.press("ctrl-a", cx);
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

/// 上のバーのメニューのボタンで左のメニューを閉じて開ける。境界で変えた幅は保存される。
#[gpui_kit::test]
fn sidebar_toggles_and_keeps_its_width(cx: &mut TestAppContext) {
    use malakt_mahoz::state::{SIDEBAR_MAX, SIDEBAR_WIDTH};
    start(cx);
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| {
        assert!(window.try_find("nav-all").is_some());
        window.click("toggle-sidebar", cx);
    });
    ui(cx, window, |window, _| {
        assert!(window.try_find("nav-all").is_none())
    });
    ui(cx, window, |window, cx| window.click("toggle-sidebar", cx));
    ui(cx, window, |window, _| {
        assert!(window.try_find("nav-all").is_some());
        assert!(window.try_find("sidebar-resize").is_some());
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
    // 見出しを押すとメニューが開き、もう一度押すと閉じる
    ui(cx, window, |window, cx| window.click("col-title", cx));
    cx.update(|cx| assert!(view.read(cx).column_menu.is_some()));
    ui(cx, window, |window, cx| window.click("col-title", cx));
    cx.update(|cx| assert!(view.read(cx).column_menu.is_none()));
    ui(cx, window, |window, cx| {
        window.click(("col-menu", Column::Due as usize), cx)
    });
    cx.update(|cx| {
        let open = view.read(cx).column_menu.as_ref().map(|m| m.column);
        assert_eq!(open, Some(Some(Column::Due)));
    });
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
    use gpui_kit::{Modifiers, MouseButton, VisualTestContext};
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

/// 設定のメニューを開いて、その文の項目を押す。
fn choose_in_menu(cx: &mut TestAppContext, window: AnyWindowHandle, label: &str) {
    ui(cx, window, |window, cx| window.click("open-menu", cx));
    let id = cx
        .update_window(window, |_, window, _| {
            window
                .elements()
                .iter()
                .find(|e| e.label() == Some(label))
                .and_then(|e| e.path().last().cloned())
        })
        .unwrap()
        .unwrap_or_else(|| panic!("メニューに「{label}」がありません"));
    ui(cx, window, |window, cx| window.click(id, cx));
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

    choose_in_menu(cx, window, "リマインダーと連携する");
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
    choose_in_menu(cx, window, "リマインダーとの連携を解除する");
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
    choose_in_menu(cx, window, "リマインダーと連携する");
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
    let (window, _) = open(cx, wide());
    let root = tempfile::tempdir().unwrap();
    let staged = staged_update(root.path());
    let (checked, installed) = updater(cx, Ok(Some(staged.clone())), Ok(()));
    assert!(has(cx, window, "update-now"));

    choose_in_menu(cx, window, "自動アップデートを無効にする");
    assert!(!has(cx, window, "update-now"));
    assert!(!staged.dir.exists());
    cx.update(malakt_mahoz::update::install_on_quit);
    assert!(installed.lock().unwrap().is_empty());
    // 無効の間は、探しに行かない
    cx.update(malakt_mahoz::update::check_now);
    cx.run_until_parked();
    assert_eq!(checked.lock().unwrap().len(), 1);

    choose_in_menu(cx, window, "自動アップデートを有効にする");
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

/// どの幅でも、一覧の上のチェックボックスとボタンは重ならず、タイトルは読める幅で残る。
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
                // チェックボックスと右のボタンは重ならない
                let check = window.find("include-descendants").bounds();
                for id in header {
                    let button = window.find(id).bounds();
                    assert!(
                        !check.intersects(&button),
                        "{case}: 「サブワークスペースも含める」と {id} が重なる"
                    );
                    assert!(button.left() >= px(0.), "{case}: {id} が左にはみ出す");
                }
                // AI チャットがウィンドウを覆う幅では、一覧は見えない
                let chat = window.try_find("ai-resize").map(|e| e.bounds().left());
                if chat.is_some_and(|left| left < px(200.)) {
                    return;
                }
                // タイトルは、表でもカードでも読める幅がある。重ねた AI チャットにも隠れない
                let title = window
                    .try_find(("title", 0usize))
                    .unwrap_or_else(|| panic!("{case}: タイトルが出ていない"))
                    .bounds();
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
