//! ドラッグしたタスクを落とせる場所の台帳。
//!
//! マウスのボタンを押している間、マウスの動きは押し始めたウィンドウにだけ届く。
//! ほかのウィンドウやほかのペイン、詳細の上では、落とし先の要素にマウスの動きが届かないので、
//! 落とせる場所は描いたときの位置で覚えておき、マウスの画面上の位置から探す。

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use gpui_kit::{
    canvas, AnyWindowHandle, App, Bounds, Global, IntoElement, Pixels, Point, Styled, WeakEntity,
    Window,
};

use crate::state::{DropTarget, MainView};

/// 重なったときに、どの面を先に見るか。
pub const PANE: u8 = 0;
pub const ROOT: u8 = 1;
pub const CHAT: u8 = 2;

struct Zone {
    bounds: Bounds<Pixels>,
    /// 落としたものを受け取る画面。
    owner: WeakEntity<MainView>,
    /// `None` は、下にある場所を隠すだけの面（詳細、ダイアログ）。
    target: Option<DropTarget>,
    /// ほかの画面から来たドラッグだけを受ける。
    others_only: bool,
}

/// 1 つの画面（ウィンドウのタスクの画面、ペイン、AI bot のウィンドウ）が描いた、落とせる場所。
#[derive(Default)]
pub struct Surface {
    zones: RefCell<Vec<Zone>>,
    layer: Cell<u8>,
}

impl Surface {
    /// 描き始め。前に描いた場所を忘れる。
    pub fn begin(&self, layer: u8) {
        self.zones.borrow_mut().clear();
        self.layer.set(layer);
    }

    /// もう描かれていない画面の場所を忘れる。
    pub fn clear(&self) {
        self.zones.borrow_mut().clear();
    }

    /// 親の要素いっぱいに重ねて、その位置を落とせる場所として覚える。親は `relative` か `absolute` にしておく。
    pub fn mark(
        self: &Rc<Self>,
        owner: WeakEntity<MainView>,
        target: Option<DropTarget>,
        others_only: bool,
    ) -> impl IntoElement {
        let surface = self.clone();
        canvas(
            move |bounds, window, _| {
                // スクロールで隠れている分は、落とせる場所にしない
                let bounds = bounds.intersect(&window.content_mask().bounds);
                surface.zones.borrow_mut().push(Zone {
                    bounds,
                    owner,
                    target,
                    others_only,
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    /// この画面の中で、いちばん上に描かれた場所。
    fn at(&self, at: Point<Pixels>) -> Option<Found> {
        let zones = self.zones.borrow();
        let zone = zones.iter().rev().find(|z| z.bounds.contains(&at))?;
        Some(Found {
            owner: zone.owner.clone(),
            target: zone.target.clone(),
            others_only: zone.others_only,
        })
    }
}

struct Found {
    owner: WeakEntity<MainView>,
    target: Option<DropTarget>,
    others_only: bool,
}

#[derive(Clone)]
struct Entry {
    window: AnyWindowHandle,
    surface: Weak<Surface>,
    /// ウィンドウのタスクの画面かペインなら、その画面。
    view: Option<WeakEntity<MainView>>,
}

#[derive(Default)]
struct Registry(Vec<Entry>);

impl Global for Registry {}

/// 画面を作ったときに、その画面が描く場所を台帳に載せる。
pub fn register(
    surface: &Rc<Surface>,
    window: AnyWindowHandle,
    view: Option<WeakEntity<MainView>>,
    cx: &mut App,
) {
    let registry = cx.default_global::<Registry>();
    registry.0.retain(|e| e.surface.strong_count() > 0);
    registry.0.push(Entry {
        window,
        surface: Rc::downgrade(surface),
        view,
    });
}

/// マウスの下にあるもの。
pub struct Hit {
    /// マウスの下のウィンドウが、ドラッグを始めたウィンドウではない。
    pub other_window: bool,
    /// そのウィンドウのタスクの画面と、その中でのマウスの位置。
    pub root: Option<WeakEntity<MainView>>,
    pub at: Point<Pixels>,
    /// 落とせる場所と、受け取る画面。
    pub zone: Option<(WeakEntity<MainView>, DropTarget)>,
}

/// `window`（ドラッグを始めたウィンドウ）の中の位置 `at` から、マウスの下の落とせる場所を探す。
/// `from` はドラッグを始めた画面。
pub fn hit(
    from: &WeakEntity<MainView>,
    at: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) -> Option<Hit> {
    let entries: Vec<Entry> = cx
        .try_global::<Registry>()?
        .0
        .iter()
        .filter(|e| e.surface.strong_count() > 0)
        .cloned()
        .collect();
    let own = window.window_handle();
    let own_bounds = window.bounds();
    let screen = at + own_bounds.origin;
    // 押しているウィンドウはいちばん手前にあるので、先に見る
    let mut windows = vec![(own, own_bounds)];
    for entry in &entries {
        if windows.iter().any(|(w, _)| *w == entry.window) {
            continue;
        }
        // 閉じている途中のウィンドウには落とせない
        if let Ok(bounds) = entry.window.update(cx, |_, window, _| window.bounds()) {
            windows.push((entry.window, bounds));
        }
    }
    // OS が重なりの順を教えてくれるなら、手前のウィンドウから見る
    if let Some(stack) = cx.window_stack() {
        let place = |w: &AnyWindowHandle| stack.iter().position(|s| s == w).unwrap_or(usize::MAX);
        windows.sort_by_key(|(w, _)| place(w));
    }
    let (under, bounds) = windows.into_iter().find(|(_, b)| b.contains(&screen))?;
    let at = screen - bounds.origin;
    let mut surfaces: Vec<(Rc<Surface>, Option<WeakEntity<MainView>>)> = entries
        .into_iter()
        .filter(|e| e.window == under)
        .filter_map(|e| Some((e.surface.upgrade()?, e.view)))
        .collect();
    surfaces.sort_by_key(|(s, _)| std::cmp::Reverse(s.layer.get()));
    let root = surfaces
        .iter()
        .find(|(s, _)| s.layer.get() == ROOT)
        .and_then(|(_, view)| view.clone());
    let zone = surfaces
        .iter()
        .find_map(|(s, _)| s.at(at))
        .filter(|found| !(found.others_only && found.owner == *from))
        .and_then(|found| Some((found.owner, found.target?)));
    Some(Hit {
        other_window: under != own,
        root,
        at,
        zone,
    })
}
