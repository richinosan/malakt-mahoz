//! 左のメニューのワークスペースをドラッグして、並べ替えたり入れ子にしたりするシナリオテスト。
//! 落とし先の決め方は `ws_drag` のユニットテストで確かめ、ここでは本物のマウスの動きでつながることを見る。

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    point, px, size, AnyWindowHandle, AppContext, Bounds, Entity, Modifiers, MouseButton, Pixels,
    Point, TestAppContext, VisualTestContext, WindowBounds, WindowOptions,
};
use malakt_core::Store;
use malakt_mahoz::ws_drag::{Spot, Zone};
use malakt_mahoz::{db, MainView, Nav};

/// 文化祭（受付）、体育祭、合唱祭
fn start(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<MainView>) {
    cx.update(|cx| malakt_mahoz::init(Store::open_in_memory().unwrap(), cx));
    cx.update(|cx| {
        db::shared(cx).update(cx, |db, _| {
            let s = &mut db.store;
            let festival = s.create_workspace(None, "文化祭").unwrap().id;
            s.create_workspace(Some(&festival), "受付").unwrap();
            s.create_workspace(None, "体育祭").unwrap();
            s.create_workspace(None, "合唱祭").unwrap();
        })
    });
    let scale = malakt_mahoz::UI_SCALE;
    let (window, view) = cx.update(|cx| {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: size(px(1400. * scale), px(900. * scale)),
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
            v.go(Nav::Home, cx)
        })
    });
    frame(cx, window);
    (window, view)
}

fn frame(cx: &mut TestAppContext, window: AnyWindowHandle) {
    cx.update_window(window, |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.run_until_parked();
}

/// 左のメニューに上から並んでいる名前と、入れ子の深さ。
fn sidebar(cx: &mut TestAppContext, view: &Entity<MainView>) -> Vec<(String, usize)> {
    cx.update(|cx| {
        let v = view.read(cx);
        v.tree
            .iter()
            .map(|r| (v.workspaces[r.index].name.clone(), r.depth))
            .collect()
    })
}

/// 左のメニューの `name` の行の位置。
fn row(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    view: &Entity<MainView>,
    name: &str,
) -> Bounds<Pixels> {
    let ix = sidebar(cx, view)
        .iter()
        .position(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("「{name}」がない"));
    let mut bounds = None;
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        bounds = window.find_all(("ws", ix)).first().map(|e| e.bounds());
    })
    .unwrap();
    bounds.unwrap()
}

/// 行の高さの `frac` のところ。
fn at(bounds: Bounds<Pixels>, frac: f32) -> Point<Pixels> {
    point(bounds.center().x, bounds.top() + bounds.size.height * frac)
}

fn press_and_move(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    from: Point<Pixels>,
    to: Point<Pixels>,
) -> VisualTestContext {
    let mut v = VisualTestContext::from_window(window, cx);
    v.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    v.update(|window, cx| window.render_frame(cx));
    for step in 1..=4 {
        let t = step as f32 / 4.;
        let p = point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
        v.simulate_mouse_move(p, MouseButton::Left, Modifiers::default());
        v.update(|window, cx| window.render_frame(cx));
    }
    v
}

fn names(list: &[(&str, usize)]) -> Vec<(String, usize)> {
    list.iter().map(|(n, d)| (n.to_string(), *d)).collect()
}

/// 行の真ん中に落とすとサブワークスペースになり、ほかの行の上の縁に落とすと、その前に並ぶ。
/// どちらも、落とした行へ移るクリックにはならない。
#[gpui_kit::test]
fn workspaces_are_nested_and_reordered_by_dragging(cx: &mut TestAppContext) {
    let (window, view) = start(cx);
    assert_eq!(
        sidebar(cx, &view),
        names(&[("文化祭", 0), ("受付", 1), ("体育祭", 0), ("合唱祭", 0)])
    );

    // 合唱祭を体育祭の真ん中へ。乗せている間は、入れ子になる先を見せる
    let from = row(cx, window, &view, "合唱祭").center();
    let to = row(cx, window, &view, "体育祭").center();
    let mut v = press_and_move(cx, window, from, to);
    let over = cx.update(|cx| view.read(cx).ws_drag.clone().unwrap().over);
    let sports = cx.update(|cx| {
        view.read(cx)
            .workspaces
            .iter()
            .find(|w| w.name == "体育祭")
            .unwrap()
            .id
            .clone()
    });
    assert_eq!(over, Some(Spot::Row(sports.clone(), Zone::Into)));
    v.update(|window, _| {
        assert!(!window.find_all("ws-drag-ghost").is_empty());
    });
    v.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    v.run_until_parked();
    assert_eq!(
        sidebar(cx, &view),
        names(&[("文化祭", 0), ("受付", 1), ("体育祭", 0), ("合唱祭", 1)])
    );
    let nav = cx.update(|cx| view.read(cx).nav.clone());
    assert_eq!(nav, Nav::Home);

    // 受付を文化祭の上の縁へ。最上位の先頭に出る
    let from = row(cx, window, &view, "受付").center();
    let to = at(row(cx, window, &view, "文化祭"), 0.1);
    let mut v = press_and_move(cx, window, from, to);
    v.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    v.run_until_parked();
    assert_eq!(
        sidebar(cx, &view),
        names(&[("受付", 0), ("文化祭", 0), ("体育祭", 0), ("合唱祭", 1)])
    );

    // 元に戻すと、1 回で 1 つ前の並びに戻る
    cx.update(|cx| {
        view.update(cx, |v, cx| {
            v.undo(false, cx);
        })
    });
    frame(cx, window);
    assert_eq!(
        sidebar(cx, &view),
        names(&[("文化祭", 0), ("受付", 1), ("体育祭", 0), ("合唱祭", 1)])
    );
}

/// 自分の中には落とせない。Esc を押すと、何も動かさずにドラッグをやめる。
#[gpui_kit::test]
fn dragging_into_itself_or_cancelled_changes_nothing(cx: &mut TestAppContext) {
    let (window, view) = start(cx);
    let before = sidebar(cx, &view);

    // 文化祭を、自分の子の受付の真ん中へ
    let from = row(cx, window, &view, "文化祭").center();
    let to = row(cx, window, &view, "受付").center();
    let mut v = press_and_move(cx, window, from, to);
    let over = cx.update(|cx| view.read(cx).ws_drag.clone().unwrap().over);
    assert_eq!(over, None);
    v.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    v.run_until_parked();
    assert_eq!(sidebar(cx, &view), before);

    // 体育祭を文化祭の真ん中へ動かしてから Esc
    let from = row(cx, window, &view, "体育祭").center();
    let to = row(cx, window, &view, "文化祭").center();
    let mut v = press_and_move(cx, window, from, to);
    assert!(cx.update(|cx| view.read(cx).ws_drag.clone().unwrap().over.is_some()));
    v.update(|window, cx| window.press("escape", cx));
    v.run_until_parked();
    assert!(cx.update(|cx| view.read(cx).ws_drag.is_none()));
    v.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    v.run_until_parked();
    assert_eq!(sidebar(cx, &view), before);
}
