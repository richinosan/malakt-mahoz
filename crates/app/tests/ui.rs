//! 画面を通したシナリオテスト。実際のウィンドウ（ヘッドレス）を開き、クリックと入力で操作する。

use gpui_kit::component::input::InputState;
use gpui_kit::test::TestWindowExt;
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
    ui(cx, window, |window, cx| {
        window.click("name-input", cx);
        window.input("冬のイベント", cx);
        window.press("enter", cx);
    });
    let parent = store(cx, |s| s.workspaces().unwrap()).remove(0);
    assert_eq!(parent.name, "冬のイベント");
    cx.update(|cx| assert_eq!(view.read(cx).nav, Nav::Workspace(parent.id.clone())));

    // その中に子を作る
    ui(cx, window, |window, cx| {
        window.click(("ws-more", 0usize), cx)
    });
    for key in ["down", "down", "down", "enter"] {
        ui(cx, window, |window, cx| window.press(key, cx));
    }
    ui(cx, window, |window, cx| {
        window.click("name-input", cx);
        window.input("会場", cx);
        window.click("name-submit", cx);
    });
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

    // 空の名前では作れず、ダイアログは開いたまま
    ui(cx, window, |window, cx| window.click("add-workspace", cx));
    ui(cx, window, |window, cx| window.click("name-submit", cx));
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(matches!(view.modal, Modal::WorkspaceName { .. }));
        assert!(view.error.is_some());
    });
    assert_eq!(store(cx, |s| s.workspaces().unwrap()).len(), 2);
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
            Modal::WorkspaceName {
                parent: None,
                rename: Some(ws.clone()),
            },
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
            Modal::NewTask,
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
    set(cx, |v| &v.inputs.bulk_due, "来週");
    ui(cx, window, |window, cx| window.click("bulk-due-submit", cx));
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.modal, Modal::BulkDue);
        assert!(view.error.is_some());
    });
    set(cx, |v| &v.inputs.bulk_due, "2026-12-24 18:00");
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

    // 書きかけのタイトルを引き継いで、項目を入れて作る
    set(cx, |v| &v.inputs.quick_add, "見積もりを取る");
    ui(cx, window, |window, cx| window.click("new-task-open", cx));
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.modal, Modal::NewTask);
        assert_eq!(
            view.inputs.nt_title.read(cx).value().as_ref(),
            "見積もりを取る"
        );
    });
    set(cx, |v| &v.inputs.nt_due, "2026-12-01");
    set(cx, |v| &v.inputs.nt_start, "2026-11-30 09:00");
    set(cx, |v| &v.inputs.nt_categories, "経理");
    ui(cx, window, |window, cx| {
        window.click(("nt-prio", 1usize), cx)
    });
    ui(cx, window, |window, cx| window.click(("nt-as", 1usize), cx));
    ui(cx, window, |window, cx| window.click("new-task-submit", cx));
    let made = all_tasks(cx)
        .into_iter()
        .find(|t| t.title == "見積もりを取る")
        .unwrap();
    assert_eq!(made.due.as_deref(), Some("2026-12-01"));
    assert_eq!(made.start.as_deref(), Some("2026-11-30 09:00"));
    assert_eq!(made.categories, ["経理"]);
    assert_eq!(made.assignee_ids, std::slice::from_ref(&sato));
    assert_eq!(made.priority, 1);
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.modal, Modal::None);
        // 作ったタスクの詳細が開く
        assert_eq!(view.panel.as_deref(), Some(made.id.as_str()));
    });

    // タイトルが空なら作らない（詳細は入力欄に重なるので、先に閉じる）
    cx.update(|cx| view.update(cx, |v, cx| v.dismiss(cx)));
    cx.run_until_parked();
    ui(cx, window, |window, cx| window.click("new-task-open", cx));
    ui(cx, window, |window, cx| window.click("new-task-submit", cx));
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.modal, Modal::NewTask);
        assert!(view.error.is_some());
    });
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
