//! タスクのドラッグと、右クリックのコピー・貼り付けのシナリオテスト。
//! 詳細を開いたまま、ほかのペインへ、ほかのウィンドウへ、のどれでも同じように落とせることを確かめる。

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    point, px, size, AnyWindowHandle, AppContext, Bounds, ElementId, Entity, Modifiers,
    MouseButton, Pixels, Point, Size, TestAppContext, VisualTestContext, WindowBounds,
    WindowOptions,
};
use malakt_core::panes::Dir;
use malakt_core::{Scope, Store, Task, TaskFilter};
use malakt_mahoz::state::DropTarget;
use malakt_mahoz::{db, MainView, Nav};

/// 2 つのワークスペースと、そのタスク。
struct Seed {
    festival: String,
    sports: String,
}

fn start(cx: &mut TestAppContext) -> Seed {
    cx.update(|cx| malakt_mahoz::init(Store::open_in_memory().unwrap(), cx));
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let s = &mut db.store;
            let festival = s.create_workspace(None, "文化祭").unwrap().id;
            let sports = s.create_workspace(None, "体育祭").unwrap().id;
            for title in ["会場を決める", "機材を借りる", "受付を決める"] {
                s.add_task_in(Some(&festival), title).unwrap();
            }
            for title in ["競技を決める", "用具をそろえる"] {
                s.add_task_in(Some(&sports), title).unwrap();
            }
            Seed { festival, sports }
        })
    })
}

fn win(width: f32, height: f32) -> Size<Pixels> {
    let scale = malakt_mahoz::UI_SCALE;
    size(px(width * scale), px(height * scale))
}

/// 画面の上の `origin` に、`nav` を開いたウィンドウを出す。
fn open(
    cx: &mut TestAppContext,
    origin: Point<Pixels>,
    window_size: Size<Pixels>,
    nav: Nav,
) -> (AnyWindowHandle, Entity<MainView>) {
    let (window, view) = cx.update(|cx| {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin,
                size: window_size,
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
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.reload(cx);
            v.go(nav, cx)
        })
    });
    ui(cx, window, |_, _| {});
    (window, view)
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

fn all_tasks(cx: &mut TestAppContext) -> Vec<Task> {
    cx.update(|cx| {
        db::shared(cx)
            .read(cx)
            .store
            .tasks(Scope::All, &TaskFilter::default())
            .unwrap()
    })
}

fn task(cx: &mut TestAppContext, title: &str) -> Task {
    all_tasks(cx)
        .into_iter()
        .find(|t| t.title == title)
        .unwrap_or_else(|| panic!("「{title}」がない"))
}

/// 一覧に出ているタイトル。上から順。
fn titles(cx: &mut TestAppContext, view: &Entity<MainView>) -> Vec<String> {
    cx.update(|cx| {
        let v = view.read(cx);
        v.visible
            .iter()
            .map(|&i| v.tasks[i].title.clone())
            .collect()
    })
}

/// ウィンドウの中の、その要素の真ん中。同じ名前の要素がいくつもあるとき（ペイン）は、全部を左から。
fn centers(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    id: impl Into<ElementId>,
) -> Vec<Point<Pixels>> {
    let mut at = Vec::new();
    ui(cx, window, |window, _| {
        at = window
            .find_all(id)
            .iter()
            .map(|e| e.bounds().center())
            .collect()
    });
    at.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap());
    at
}

fn center(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    id: impl Into<ElementId>,
) -> Point<Pixels> {
    centers(cx, window, id)[0]
}

/// 一覧の、行のないところ（右下の「＋」の左）。
fn blank(cx: &mut TestAppContext, window: AnyWindowHandle) -> Point<Pixels> {
    let fab = center(cx, window, "fab-new-task");
    point(fab.x - px(200.), fab.y)
}

/// `window` の中で押して、少しずつ動かす。`drop` なら最後に離す。
/// 押している間のマウスの動きは、ウィンドウの外へ出ても、押し始めたウィンドウに届く。
fn drag(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    from: Point<Pixels>,
    to: Point<Pixels>,
    drop: bool,
) {
    let mut v = VisualTestContext::from_window(window, cx);
    v.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    v.update(|window, cx| window.render_frame(cx));
    for step in 1..=4 {
        let t = step as f32 / 4.;
        let at = point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
        v.simulate_mouse_move(at, MouseButton::Left, Modifiers::default());
        v.update(|window, cx| window.render_frame(cx));
    }
    if drop {
        v.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    }
    v.run_until_parked();
}

/// 詳細を開いたままでも、行をドラッグして並べ替えられる。詳細の上でもマウスを追い、
/// 詳細の上で離したら、何も動かさずにドラッグを終える。
#[gpui_kit::test]
fn rows_are_dragged_while_the_detail_is_open(cx: &mut TestAppContext) {
    let seed = start(cx);
    let (window, view) = open(
        cx,
        Point::default(),
        win(1600., 1000.),
        Nav::Workspace(seed.festival.clone()),
    );
    let before = titles(cx, &view);
    assert_eq!(before.len(), 3);
    ui(cx, window, |window, cx| window.click(("title", 0usize), cx));
    let opened = cx.update(|cx| view.read(cx).panel.clone());
    assert!(opened.is_some());

    // 詳細の上へ動かす。マウスに付いたタイトルは詳細の上まで来て、落とし先はなくなる
    let from = center(cx, window, ("title", 0usize));
    let third = center(cx, window, ("title", 2usize));
    let panel = center(cx, window, "detail-panel");
    drag(cx, window, from, third, false);
    cx.update(|cx| {
        let d = view.read(cx).task_drag.clone().unwrap();
        assert_eq!(d.over.map(|(t, _)| t), Some(DropTarget::Row(2)));
    });
    {
        let mut v = VisualTestContext::from_window(window, cx);
        v.simulate_mouse_move(panel, MouseButton::Left, Modifiers::default());
        v.update(|window, cx| window.render_frame(cx));
    }
    cx.update(|cx| {
        let d = view.read(cx).task_drag.clone().unwrap();
        assert_eq!(d.at, panel);
        assert_eq!(d.over, None);
    });
    // 詳細の上で離すと、ドラッグは終わり、並びは変わらない
    {
        let mut v = VisualTestContext::from_window(window, cx);
        v.simulate_mouse_up(panel, MouseButton::Left, Modifiers::default());
        v.run_until_parked();
    }
    cx.update(|cx| assert!(view.read(cx).task_drag.is_none()));
    assert_eq!(titles(cx, &view), before);

    // 詳細を開いたまま、3 行目へ落とすと並びが変わる。詳細は開いていたタスクのまま
    let from = center(cx, window, ("title", 0usize));
    let third = center(cx, window, ("title", 2usize));
    drag(cx, window, from, third, true);
    assert_eq!(
        titles(cx, &view),
        [before[1].clone(), before[2].clone(), before[0].clone()]
    );
    cx.update(|cx| {
        let v = view.read(cx);
        assert!(v.task_drag.is_none());
        assert_eq!(v.panel, opened);
    });

    // 詳細を開いたまま、左のワークスペースへ落とすと、そこへ移る
    let from = center(cx, window, ("title", 0usize));
    let to = center(cx, window, ("ws", 1usize));
    drag(cx, window, from, to, true);
    assert_eq!(
        task(cx, &before[1]).workspace_id.as_deref(),
        Some(seed.sports.as_str())
    );
}

/// ほかのウィンドウへドラッグすると、そのウィンドウの行・一覧・左のワークスペースに落とせる。
#[gpui_kit::test]
fn tasks_are_dragged_to_another_window(cx: &mut TestAppContext) {
    let seed = start(cx);
    let size = win(1200., 800.);
    let (left, from_view) = open(
        cx,
        Point::default(),
        size,
        Nav::Workspace(seed.festival.clone()),
    );
    // 右隣に、少し下げて置く
    let origin = point(size.width + px(40.), px(60.));
    let (right, to_view) = open(cx, origin, size, Nav::Workspace(seed.sports.clone()));
    // 右のウィンドウの中の位置を、左のウィンドウから見た位置にする
    let across = |at: Point<Pixels>| at + origin;

    // 動かしている間、右のウィンドウに落とし先が出て、マウスにタイトルが付く
    let from = center(cx, left, ("title", 0usize));
    let row = center(cx, right, ("title", 0usize));
    drag(cx, left, from, across(row), false);
    cx.update(|cx| {
        let d = to_view.read(cx).task_drop.clone().unwrap();
        assert_eq!(d.title.as_ref(), "会場を決める");
        assert_eq!(d.at, Some(row));
        assert_eq!(d.over, Some(DropTarget::Row(0)));
        assert!(from_view
            .read(cx)
            .task_drag
            .as_ref()
            .unwrap()
            .over_view
            .is_some());
    });
    // 離すと、その行の上に入り、その行のワークスペースへ移る
    {
        let mut v = VisualTestContext::from_window(left, cx);
        v.simulate_mouse_up(across(row), MouseButton::Left, Modifiers::default());
        v.run_until_parked();
    }
    assert_eq!(
        task(cx, "会場を決める").workspace_id.as_deref(),
        Some(seed.sports.as_str())
    );
    assert_eq!(
        titles(cx, &to_view),
        ["会場を決める", "競技を決める", "用具をそろえる"]
    );
    assert_eq!(titles(cx, &from_view), ["機材を借りる", "受付を決める"]);
    cx.update(|cx| {
        assert!(from_view.read(cx).task_drag.is_none());
        let to = to_view.read(cx);
        assert!(to.task_drop.is_none());
        assert!(to.notice.is_some());
        // 落としたことが、落とした先の行のクリックにならない
        assert!(to.panel.is_none());
    });

    // 行のないところに落とすと、右のウィンドウが開いているワークスペースへ移る
    let from = center(cx, left, ("title", 0usize));
    let blank = blank(cx, right);
    drag(cx, left, from, across(blank), true);
    assert_eq!(
        task(cx, "機材を借りる").workspace_id.as_deref(),
        Some(seed.sports.as_str())
    );
    assert_eq!(titles(cx, &from_view), ["受付を決める"]);

    // 右のウィンドウの、左のメニューのワークスペースにも落とせる
    let from = center(cx, right, ("title", 0usize));
    let moving = titles(cx, &to_view)[0].clone();
    let back = center(cx, left, ("ws", 0usize));
    drag(cx, right, from, back - origin, true);
    assert_eq!(
        task(cx, &moving).workspace_id.as_deref(),
        Some(seed.festival.as_str())
    );

    // どのウィンドウでもないところで離すと、何も動かない
    let tasks = all_tasks(cx);
    let from = center(cx, left, ("title", 0usize));
    drag(cx, left, from, point(px(-500.), px(-500.)), true);
    let after = all_tasks(cx);
    assert_eq!(
        tasks
            .iter()
            .map(|t| (&t.id, &t.workspace_id))
            .collect::<Vec<_>>(),
        after
            .iter()
            .map(|t| (&t.id, &t.workspace_id))
            .collect::<Vec<_>>()
    );
    cx.update(|cx| {
        assert!(from_view.read(cx).task_drag.is_none());
        assert!(to_view.read(cx).task_drop.is_none());
    });

    // 右のウィンドウで詳細を開いていると、詳細の下に隠れた場所には落ちない
    ui(cx, right, |window, cx| window.click(("title", 0usize), cx));
    let panel = center(cx, right, "detail-panel");
    let moving = titles(cx, &from_view)[0].clone();
    let from = center(cx, left, ("title", 0usize));
    drag(cx, left, from, across(panel), true);
    assert_eq!(
        task(cx, &moving).workspace_id.as_deref(),
        Some(seed.festival.as_str())
    );
    // 詳細を開いたままの行には落とせる
    let from = center(cx, left, ("title", 0usize));
    let row = center(cx, right, ("title", 0usize));
    drag(cx, left, from, across(row), true);
    assert_eq!(
        task(cx, &moving).workspace_id.as_deref(),
        Some(seed.sports.as_str())
    );
}

/// 同じウィンドウの中の、ほかのペインへもドラッグできる。
#[gpui_kit::test]
fn tasks_are_dragged_to_another_pane(cx: &mut TestAppContext) {
    let seed = start(cx);
    let (window, view) = open(
        cx,
        Point::default(),
        win(1900., 1000.),
        Nav::Workspace(seed.festival.clone()),
    );
    // 右に分けて、右のペインで体育祭を開く
    let pane = cx.update(|cx| {
        window
            .update(cx, |_, window, cx| {
                view.update(cx, |v, cx| {
                    let id = v.split_pane(Dir::Right, window, cx);
                    v.children[&id].clone()
                })
            })
            .unwrap()
    });
    cx.update(|cx| pane.update(cx, |v, cx| v.go(Nav::Workspace(seed.sports.clone()), cx)));
    ui(cx, window, |_, _| {});
    assert_eq!(titles(cx, &pane), ["競技を決める", "用具をそろえる"]);

    let rows = centers(cx, window, ("title", 1usize));
    assert_eq!(rows.len(), 2);
    let from = centers(cx, window, ("title", 0usize))[0];
    // 左のペインから、右のペインの 2 行目へ
    drag(cx, window, from, rows[1], false);
    cx.update(|cx| {
        let d = pane.read(cx).task_drop.clone().unwrap();
        assert_eq!(d.over, Some(DropTarget::Row(1)));
    });
    {
        let mut v = VisualTestContext::from_window(window, cx);
        v.simulate_mouse_up(rows[1], MouseButton::Left, Modifiers::default());
        v.run_until_parked();
    }
    assert_eq!(
        titles(cx, &pane),
        ["競技を決める", "会場を決める", "用具をそろえる"]
    );
    assert_eq!(
        task(cx, "会場を決める").workspace_id.as_deref(),
        Some(seed.sports.as_str())
    );
    assert_eq!(titles(cx, &view), ["機材を借りる", "受付を決める"]);

    // 右のペインから、左のペインの 1 行目へ戻す
    let from = centers(cx, window, ("title", 1usize))[1];
    let to = centers(cx, window, ("title", 0usize))[0];
    drag(cx, window, from, to, true);
    assert_eq!(
        titles(cx, &view),
        ["会場を決める", "機材を借りる", "受付を決める"]
    );
    cx.update(|cx| {
        assert!(pane.read(cx).task_drag.is_none());
        assert!(view.read(cx).task_drop.is_none());
    });
}

/// 行の右クリックの「コピー」でコピーし、ほかのワークスペースやウィンドウの右クリックの「貼り付け」で写しを作る。
#[gpui_kit::test]
fn the_row_menu_copies_and_pastes(cx: &mut TestAppContext) {
    let seed = start(cx);
    let size = win(1200., 800.);
    let (left, from_view) = open(
        cx,
        Point::default(),
        size,
        Nav::Workspace(seed.festival.clone()),
    );
    let (right, to_view) = open(
        cx,
        point(size.width, px(0.)),
        size,
        Nav::Workspace(seed.sports.clone()),
    );
    // メニューを開いて、上から `nth` 番目（0 から）の項目を押す
    let pick = |cx: &mut TestAppContext, window: AnyWindowHandle, at: Point<Pixels>, nth: usize| {
        {
            let mut v = VisualTestContext::from_window(window, cx);
            v.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
            v.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
            v.run_until_parked();
        }
        for _ in 0..=nth {
            ui(cx, window, |window, cx| window.press("down", cx));
        }
        ui(cx, window, |window, cx| window.press("enter", cx));
    };
    let clipboard = |cx: &mut TestAppContext| {
        cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
    };

    // クリップボードが空の間は、「貼り付け」を押しても何も起きない
    let empty = blank(cx, right);
    pick(cx, right, empty, 0);
    assert_eq!(all_tasks(cx).len(), 5);

    // 開く、状態を変更、コピー、貼り付け、削除 の順
    let second = center(cx, left, ("title", 1usize));
    pick(cx, left, second, 2);
    let copied = task(cx, "機材を借りる");
    assert_eq!(clipboard(cx), Some(malakt_core::tasklink::url(&copied.id)));
    cx.update(|cx| assert!(from_view.read(cx).notice.is_some()));

    // ほかのウィンドウの、行のないところで貼り付けると、そこのワークスペースに写しができる
    pick(cx, right, empty, 0);
    let made: Vec<Task> = all_tasks(cx)
        .into_iter()
        .filter(|t| t.title == "機材を借りる")
        .collect();
    assert_eq!(made.len(), 2);
    assert!(made
        .iter()
        .any(|t| t.workspace_id.as_deref() == Some(seed.sports.as_str())));
    assert!(made
        .iter()
        .any(|t| t.workspace_id.as_deref() == Some(seed.festival.as_str())));
    assert_eq!(titles(cx, &to_view).len(), 3);

    // 行の上の右クリックからも貼り付けられる
    let first = center(cx, left, ("title", 0usize));
    pick(cx, left, first, 3);
    assert_eq!(titles(cx, &from_view).len(), 4);

    // 選んでいる行の上でコピーすると、選んでいる全部がコピーされる
    cx.update(|cx| {
        from_view.update(cx, |v, cx| {
            let ids: Vec<_> = v.visible.iter().map(|&i| v.tasks[i].id.clone()).collect();
            v.selected.extend(ids.into_iter().take(2));
            cx.notify();
        })
    });
    let first = center(cx, left, ("title", 0usize));
    pick(cx, left, first, 2);
    assert_eq!(clipboard(cx).unwrap().lines().count(), 2);
}
