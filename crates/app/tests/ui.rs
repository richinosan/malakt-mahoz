//! 画面を通したシナリオテスト。実際のウィンドウ（ヘッドレス）を開き、クリックと入力で操作する。

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    px, size, AnyWindowHandle, AppContext, Bounds, Entity, Pixels, Point, Size, TestAppContext,
    WindowBounds, WindowOptions,
};
use malakt_core::view::Tab;
use malakt_core::{lock, Role, Roles, Scope, Status, Store, TaskFilter};
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
            cx.new(|cx| MainView::new(window, cx))
        })
        .expect("ウィンドウを開く")
    })
}

fn wide() -> Size<Pixels> {
    size(px(1280.), px(800.))
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
    ui(cx, window, |window, cx| window.click("ws-add-child", cx));
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
    let (second, second_view) = open(cx, size(px(900.), px(700.)));
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

/// ロックを掛けると全部のウィンドウが隠れ、正しいパスフレーズでだけ開く。
#[gpui_kit::test]
fn lock_hides_every_window(cx: &mut TestAppContext) {
    start(cx);
    let (first, first_view) = open(cx, wide());
    let (_second, second_view) = open(cx, wide());

    cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            lock::set_passphrase(&mut db.store, None, "ひらけごま").unwrap()
        })
    });
    // ロックが無効なら「今すぐロック」は何もしないが、有効なら全部を隠す
    cx.update(db::lock_now);
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(!first_view.read(cx).unlocked(cx));
        assert!(!second_view.read(cx).unlocked(cx));
    });

    ui(cx, first, |window, cx| {
        window.click("unlock-input", cx);
        window.input("ちがう", cx);
        window.press("enter", cx);
    });
    cx.update(|cx| {
        assert!(!first_view.read(cx).unlocked(cx));
        assert!(first_view.read(cx).error.is_some());
    });
    ui(cx, first, |window, cx| {
        window.click("unlock-input", cx);
        window.input("ひらけごま", cx);
        window.click("unlock", cx);
    });
    cx.update(|cx| {
        assert!(first_view.read(cx).unlocked(cx));
        assert!(
            second_view.read(cx).unlocked(cx),
            "解除はどのウィンドウにも効く"
        );
        assert!(first_view.read(cx).error.is_none());
    });
}

/// ロックが無効なら「今すぐロック」をしても隠れない。
#[gpui_kit::test]
fn lock_now_without_passphrase_does_nothing(cx: &mut TestAppContext) {
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
    for window_size in [
        size(px(380.), px(520.)),
        size(px(900.), px(600.)),
        size(px(1600.), px(1000.)),
    ] {
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
            Modal::Lock,
            Modal::Usage,
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
    let (window, view) = open(cx, size(px(520.), px(800.)));

    // 最初はメニューが閉じていて、すぐタスクを足せる
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
        window.click("quick-add", cx);
        window.input("会場を決める", cx);
        window.press("enter", cx);
    });
    ui(cx, window, |window, cx| window.click("nav-all", cx));
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    let id = all_tasks(cx).remove(0).id;
    let load = |cx: &mut TestAppContext| store(cx, |s| s.task(&id).unwrap());

    // 2 人を担当にすると、どちらも R から始まる
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
    let (window, view) = open(cx, size(px(1280.), px(2400.)));
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

/// 使用量を見て、その場で整理できる。
#[gpui_kit::test]
fn usage_dialog_shows_size_and_compacts(cx: &mut TestAppContext) {
    start(cx);
    let (window, view) = open(cx, wide());
    ui(cx, window, |window, cx| window.click("open-usage", cx));
    cx.update(|cx| {
        let view = view.read(cx);
        assert_eq!(view.modal, Modal::Usage);
        assert!(view.usage(cx).database_bytes > 0);
    });
    ui(cx, window, |window, cx| window.click("usage-compact", cx));
    cx.update(|cx| {
        let view = view.read(cx);
        assert!(view.error.is_none());
        assert!(view
            .notice
            .as_ref()
            .is_some_and(|n| n.contains("空けました")));
        assert_eq!(view.usage(cx).reclaimable_bytes, 0);
    });
}
