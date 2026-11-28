//! 左のメニューのワークスペースをドラッグして、並べ替えたり、サブワークスペースにしたり外したりする。
//!
//! 行の上の 1/4 に落とすとその行の前、下の 1/4 なら後ろ、真ん中ならその行のサブワークスペースの末尾に入る。
//! 一覧の下の空いたところに落とすと、最上位の末尾に入る。
//! AI チャットの上に落とすと、並びは変えずに、チャットの入力欄に添える。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui_kit::*;
use malakt_core::{Id, Workspace};

use crate::state::{DropTarget, MainView};

/// 押してからこれだけ動かすまでは、ただのクリックとして扱う。
const SLOP: f32 = 6.;
/// 閉じているワークスペースの真ん中に、これだけ乗せていると開く。
const OPEN_AFTER: Duration = Duration::from_millis(600);

/// 行のどこに落とすか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Before,
    Into,
    After,
}

/// 落とし先。行はワークスペースの ID で持つ（開いて行が増えても、同じ行を指す）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spot {
    Row(Id, Zone),
    /// 一覧の下の空いたところ。最上位の末尾。
    End,
}

/// 置き場所。`index` は、動かすワークスペースを除いた兄弟の中での位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub parent: Option<Id>,
    pub index: usize,
}

/// 行の高さの中の位置（0 が上の縁、1 が下の縁）から、行のどこかを決める。
/// 入れ子にできない行では、上半分が前、下半分が後ろ。
pub fn zone(frac: f32, can_nest: bool) -> Zone {
    if can_nest {
        if frac < 0.25 {
            Zone::Before
        } else if frac > 0.75 {
            Zone::After
        } else {
            Zone::Into
        }
    } else if frac < 0.5 {
        Zone::Before
    } else {
        Zone::After
    }
}

/// `id` が `top` そのものか、その子孫か。
pub fn inside(workspaces: &[Workspace], top: &str, id: &str) -> bool {
    let parents: HashMap<&str, Option<&str>> = workspaces
        .iter()
        .map(|w| (w.id.as_str(), w.parent_id.as_deref()))
        .collect();
    let mut at = Some(id);
    // 壊れたデータで輪になっていても止まるよう、数を限る
    for _ in 0..=workspaces.len() {
        match at {
            Some(now) if now == top => return true,
            Some(now) => at = parents.get(now).copied().flatten(),
            None => return false,
        }
    }
    false
}

/// 子のワークスペースを開いて見せているか。
pub fn expanded(workspaces: &[Workspace], collapsed: &HashSet<Id>, id: &str) -> bool {
    !collapsed.contains(id)
        && workspaces
            .iter()
            .any(|w| w.parent_id.as_deref() == Some(id))
}

/// `dragged` を `spot` に落としたときの置き場所。落とせないとき、今と同じ場所のときは `None`。
pub fn plan(
    workspaces: &[Workspace],
    collapsed: &HashSet<Id>,
    dragged: &str,
    spot: &Spot,
) -> Option<Place> {
    let me = workspaces.iter().find(|w| w.id == dragged)?;
    let key = |w: &Workspace| (w.position, w.id.clone());
    // `parent` の子のうち、動かすものを除いて `before` より前にある数
    let slot = |parent: Option<&str>, before: Option<&Workspace>| {
        workspaces
            .iter()
            .filter(|w| w.parent_id.as_deref() == parent && w.id != dragged)
            .filter(|w| before.is_none_or(|b| key(w) < key(b)))
            .count()
    };
    let place = match spot {
        Spot::End => Place {
            parent: None,
            index: slot(None, None),
        },
        Spot::Row(id, zone) => {
            let target = workspaces.iter().find(|w| &w.id == id)?;
            let parent = target.parent_id.as_deref();
            match zone {
                Zone::Before => Place {
                    parent: parent.map(str::to_owned),
                    index: slot(parent, Some(target)),
                },
                // 子を開いている行の下の縁は、子の先頭
                Zone::After if expanded(workspaces, collapsed, id) => Place {
                    parent: Some(id.clone()),
                    index: 0,
                },
                Zone::After => Place {
                    parent: parent.map(str::to_owned),
                    index: slot(parent, Some(target)) + usize::from(target.id != dragged),
                },
                Zone::Into => Place {
                    parent: Some(id.clone()),
                    index: slot(Some(id), None),
                },
            }
        }
    };
    if let Some(parent) = &place.parent {
        if inside(workspaces, dragged, parent) {
            return None;
        }
    }
    let now = Place {
        parent: me.parent_id.clone(),
        index: slot(me.parent_id.as_deref(), Some(me)),
    };
    (place != now).then_some(place)
}

/// 描いた行の位置。ドラッグ中のマウスの位置から、落とし先を探すのに使う。
#[derive(Default)]
pub struct Rows {
    rows: RefCell<Vec<(Id, Bounds<Pixels>)>>,
    end: RefCell<Option<Bounds<Pixels>>>,
}

impl Rows {
    pub fn begin(&self) {
        self.rows.borrow_mut().clear();
        self.end.borrow_mut().take();
    }

    /// 親の要素いっぱいに重ねて、その位置を `id` の行として覚える。
    pub fn mark(self: &std::rc::Rc<Self>, id: Id) -> impl IntoElement {
        let rows = self.clone();
        canvas(
            move |bounds, window, _| {
                let bounds = bounds.intersect(&window.content_mask().bounds);
                rows.rows.borrow_mut().push((id, bounds));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    /// 親の要素いっぱいに重ねて、その位置を一覧の下の空いたところとして覚える。
    pub fn mark_end(self: &std::rc::Rc<Self>) -> impl IntoElement {
        let rows = self.clone();
        canvas(
            move |bounds, window, _| {
                let bounds = bounds.intersect(&window.content_mask().bounds);
                *rows.end.borrow_mut() = Some(bounds);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    /// マウスの下の行と、その行の高さの中の位置。
    fn at(&self, at: Point<Pixels>) -> Option<Option<(Id, f32)>> {
        let rows = self.rows.borrow();
        // 行の間のすき間も、近い方の行として扱う
        let near = rows.iter().find(|(_, b)| {
            at.x >= b.left() - px(48.)
                && at.x <= b.right()
                && at.y >= b.top() - px(2.)
                && at.y <= b.bottom() + px(2.)
        });
        if let Some((id, b)) = near {
            let height = f32::from(b.size.height).max(1.);
            let frac = (f32::from(at.y - b.top()) / height).clamp(0., 1.);
            return Some(Some((id.clone(), frac)));
        }
        self.end.borrow().filter(|b| b.contains(&at)).map(|_| None)
    }
}

/// ワークスペースのドラッグ。
#[derive(Debug, Clone, PartialEq)]
pub struct WsDrag {
    pub id: Id,
    pub start: Point<Pixels>,
    pub at: Point<Pixels>,
    pub moved: bool,
    /// 今の落とし先。落とせる場所の上にいるときだけ。
    pub over: Option<Spot>,
    /// 閉じた行を開くのを待っている回。乗せ直すと数が変わる。
    pub wait: usize,
    /// 上にある AI チャットの画面。落とすと、そのチャットに添える。
    pub chat: Option<WeakEntity<MainView>>,
}

impl MainView {
    /// ワークスペースの行を押した。少し動かすまでは、ただのクリックとして扱う。
    pub fn start_ws_drag(&mut self, id: &str, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.workspace_naming.is_some() {
            return;
        }
        self.ws_drag = Some(WsDrag {
            id: id.to_owned(),
            start: at,
            at,
            moved: false,
            over: None,
            wait: 0,
            chat: None,
        });
        cx.notify();
    }

    /// ワークスペースをドラッグしている、または落とした直後。この間のクリックは、ドラッグの終わりとして扱う。
    pub fn dragging_ws(&self) -> bool {
        self.ws_dropped || self.ws_drag.as_ref().is_some_and(|d| d.moved)
    }

    /// ドラッグ中のマウスの位置から、落とし先を探す。AI チャットの上なら、チャットが落とし先になる。
    pub fn drag_ws_at(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let me = cx.entity().downgrade();
        let chat = crate::drops::hit(&me, at, window, cx)
            .and_then(|hit| hit.zone)
            .filter(|(_, target)| *target == DropTarget::Chat)
            .map(|(owner, _)| owner);
        let spot = match chat {
            Some(_) => None,
            None => self.ws_rows.at(at),
        };
        self.drag_ws_to(at, spot, cx);
        self.drag_ws_chat(chat, cx);
    }

    /// ドラッグ中のワークスペースが上にある AI チャットの画面。動かし始める前は何もしない。
    pub fn drag_ws_chat(&mut self, chat: Option<WeakEntity<MainView>>, cx: &mut Context<Self>) {
        let Some(drag) = self.ws_drag.as_mut().filter(|d| d.moved) else {
            return;
        };
        if drag.chat == chat {
            return;
        }
        let old = std::mem::replace(&mut drag.chat, chat.clone());
        self.show_ws_chat(old, false, cx);
        self.show_ws_chat(chat, true, cx);
        cx.notify();
    }

    /// チャットの画面に、ワークスペースが上にあることを見せる・消す。
    fn show_ws_chat(
        &mut self,
        view: Option<WeakEntity<MainView>>,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = view else {
            return;
        };
        if view == cx.entity().downgrade() {
            self.ws_chat_drop = on;
            return;
        }
        // 閉じた画面には、見せる先がない
        let _ = view.update(cx, |view, cx| {
            view.ws_chat_drop = on;
            cx.notify();
        });
    }

    /// `spot` はマウスの下の行と行の中の位置（`Some(None)` は一覧の下の空いたところ）。
    pub fn drag_ws_to(
        &mut self,
        at: Point<Pixels>,
        under: Option<Option<(Id, f32)>>,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.ws_drag.as_mut() else {
            return;
        };
        let (dx, dy) = (
            f32::from(at.x - drag.start.x),
            f32::from(at.y - drag.start.y),
        );
        if !drag.moved && dx.abs().max(dy.abs()) <= SLOP {
            return;
        }
        drag.moved = true;
        drag.at = at;
        let dragged = drag.id.clone();
        let spot = under.map(|row| match row {
            None => Spot::End,
            Some((id, frac)) => {
                let can_nest = !inside(&self.workspaces, &dragged, &id);
                Spot::Row(id, zone(frac, can_nest))
            }
        });
        let spot = spot.filter(|s| plan(&self.workspaces, &self.collapsed, &dragged, s).is_some());
        let Some(drag) = self.ws_drag.as_mut() else {
            return;
        };
        if drag.over != spot {
            drag.over = spot.clone();
            drag.wait += 1;
            // 閉じているワークスペースの真ん中に乗せ続けると、開いて中にも置けるようにする
            if let Some(Spot::Row(id, Zone::Into)) = &spot {
                if self.collapsed.contains(id) {
                    let (id, wait) = (id.clone(), drag.wait);
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(OPEN_AFTER).await;
                        // 閉じた画面では、開く先がない
                        let _ = this.update(cx, |this, cx| {
                            if this.ws_drag.as_ref().is_some_and(|d| d.wait == wait) {
                                this.toggle_collapsed(&id, cx);
                            }
                        });
                    })
                    .detach();
                }
            }
        }
        cx.notify();
    }

    /// ドラッグをやめる（Esc）。何も動かさない。
    pub fn cancel_ws_drag(&mut self, cx: &mut Context<Self>) -> bool {
        let drag = self.ws_drag.take();
        let moved = drag.as_ref().is_some_and(|d| d.moved);
        self.show_ws_chat(drag.and_then(|d| d.chat), false, cx);
        if moved {
            self.suppress_ws_click(cx);
        }
        cx.notify();
        moved
    }

    fn suppress_ws_click(&mut self, cx: &mut Context<Self>) {
        // マウスを離したことが、下にある行のクリックにならないようにする
        self.ws_dropped = true;
        let me = cx.entity().downgrade();
        cx.defer(move |cx| {
            // 閉じた画面なら、戻す先がない
            let _ = me.update(cx, |this, _| this.ws_dropped = false);
        });
    }

    /// 今の落とし先に落とす。
    pub fn finish_ws_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.ws_drag.take() else {
            return;
        };
        cx.notify();
        self.show_ws_chat(drag.chat.clone(), false, cx);
        if !drag.moved {
            return;
        }
        self.suppress_ws_click(cx);
        if drag.chat.is_some() {
            // チャットに落とした。並びは変えない
            let name = self.workspace_name(Some(&drag.id));
            self.ai_session
                .update(cx, |s, cx| s.attach_workspace(&drag.id, name, cx));
            return;
        }
        let Some(spot) = drag.over else {
            return;
        };
        let Some(place) = plan(&self.workspaces, &self.collapsed, &drag.id, &spot) else {
            return;
        };
        let done = self.write(cx, |s| {
            s.place_workspace(&drag.id, place.parent.as_deref(), place.index)
        });
        if done.is_some() {
            // 入れた先が閉じていたら開いて、入ったところを見せる
            if let Some(parent) = &place.parent {
                if self.collapsed.remove(parent) {
                    self.tree = crate::state::build_tree(&self.workspaces, &self.collapsed);
                }
            }
            self.ws_landed = Some((drag.id, self.ws_landed.as_ref().map_or(1, |(_, n)| n + 1)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{expanded, inside, plan, zone, Place, Spot, Zone};
    use malakt_core::Workspace;
    use std::collections::HashSet;

    fn ws(id: &str, parent: Option<&str>, position: i64) -> Workspace {
        Workspace {
            id: id.into(),
            parent_id: parent.map(Into::into),
            name: id.into(),
            date: None,
            icon: None,
            color: None,
            time_basis: None,
            position,
            created_at: 0,
            updated_at: 0,
        }
    }

    /// A（A1、A2）、B、C
    fn sample() -> Vec<Workspace> {
        vec![
            ws("A", None, 0),
            ws("A1", Some("A"), 0),
            ws("A2", Some("A"), 1),
            ws("B", None, 1),
            ws("C", None, 2),
        ]
    }

    fn row(id: &str, zone: Zone) -> Spot {
        Spot::Row(id.into(), zone)
    }

    fn at(parent: Option<&str>, index: usize) -> Option<Place> {
        Some(Place {
            parent: parent.map(Into::into),
            index,
        })
    }

    #[test]
    fn zones_split_the_row() {
        assert_eq!(zone(0.1, true), Zone::Before);
        assert_eq!(zone(0.5, true), Zone::Into);
        assert_eq!(zone(0.9, true), Zone::After);
        assert_eq!(zone(0.4, false), Zone::Before);
        assert_eq!(zone(0.6, false), Zone::After);
    }

    #[test]
    fn reorders_among_siblings() {
        let all = sample();
        let open = HashSet::new();
        assert_eq!(plan(&all, &open, "C", &row("A", Zone::Before)), at(None, 0));
        assert_eq!(plan(&all, &open, "A", &row("C", Zone::After)), at(None, 2));
        assert_eq!(plan(&all, &open, "A", &row("C", Zone::Before)), at(None, 1));
        assert_eq!(plan(&all, &open, "A", &Spot::End), at(None, 2));
        assert_eq!(
            plan(&all, &open, "A2", &row("A1", Zone::Before)),
            at(Some("A"), 0)
        );
    }

    #[test]
    fn nests_and_unnests() {
        let all = sample();
        let open = HashSet::new();
        // 真ん中に落とすと、その行のサブワークスペースの末尾
        assert_eq!(
            plan(&all, &open, "C", &row("B", Zone::Into)),
            at(Some("B"), 0)
        );
        assert_eq!(
            plan(&all, &open, "C", &row("A", Zone::Into)),
            at(Some("A"), 2)
        );
        // 子を開いている行の下の縁は、子の先頭
        assert_eq!(
            plan(&all, &open, "C", &row("A", Zone::After)),
            at(Some("A"), 0)
        );
        // 閉じていれば、その行の後ろ
        let closed: HashSet<String> = ["A".to_string()].into();
        assert_eq!(
            plan(&all, &closed, "C", &row("A", Zone::After)),
            at(None, 1)
        );
        // 子を最上位へ外す
        assert_eq!(plan(&all, &open, "A1", &row("B", Zone::After)), at(None, 2));
        assert_eq!(plan(&all, &open, "A1", &Spot::End), at(None, 3));
        // ほかの子の間へ
        assert_eq!(
            plan(&all, &open, "B", &row("A2", Zone::Before)),
            at(Some("A"), 1)
        );
    }

    #[test]
    fn refuses_itself_descendants_and_no_change() {
        let all = sample();
        let open = HashSet::new();
        assert_eq!(plan(&all, &open, "A", &row("A", Zone::Into)), None);
        assert_eq!(plan(&all, &open, "A", &row("A1", Zone::Into)), None);
        assert_eq!(plan(&all, &open, "A", &row("A2", Zone::After)), None);
        // 今と同じ場所
        assert_eq!(plan(&all, &open, "A", &row("A", Zone::Before)), None);
        assert_eq!(
            plan(&all, &open, "B", &row("A2", Zone::After)),
            at(Some("A"), 2)
        );
        assert_eq!(plan(&all, &open, "B", &row("B", Zone::After)), None);
        assert_eq!(plan(&all, &open, "C", &row("B", Zone::After)), None);
        assert_eq!(plan(&all, &open, "C", &Spot::End), None);
        assert_eq!(plan(&all, &open, "A2", &row("A", Zone::Into)), None);
        assert_eq!(plan(&all, &open, "missing", &Spot::End), None);
    }

    #[test]
    fn knows_descendants_and_open_rows() {
        let all = sample();
        assert!(inside(&all, "A", "A"));
        assert!(inside(&all, "A", "A2"));
        assert!(!inside(&all, "A1", "A"));
        assert!(!inside(&all, "B", "C"));
        assert!(expanded(&all, &HashSet::new(), "A"));
        assert!(!expanded(&all, &["A".to_string()].into(), "A"));
        assert!(!expanded(&all, &HashSet::new(), "B"));
    }
}
