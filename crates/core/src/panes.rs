//! ペインの配置。画面を縦横に何度でも分け、キーで移動・入れ替え・幅の変更・拡大ができる。
//! 画面には依存せず、割合だけで持つ。

/// ペインの番号。配置の中で重ならない。
pub type PaneId = u32;

/// 向き。分ける側、移る先、広げる側に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    fn horizontal(self) -> bool {
        matches!(self, Dir::Left | Dir::Right)
    }
}

/// ペインの場所。全体を 1 とした割合。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Leaf(PaneId),
    /// `horizontal` なら左右、そうでなければ上下に分ける。`ratio` は 1 つ目の取り分。
    Split {
        horizontal: bool,
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}

/// 1 つ目の取り分の範囲。どのペインも見えなくならない幅を残す。
const MIN_RATIO: f32 = 0.1;

/// ペインの配置と、今のペイン。
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    root: Node,
    focus: PaneId,
    zoomed: bool,
    next: PaneId,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            root: Node::Leaf(0),
            focus: 0,
            zoomed: false,
            next: 1,
        }
    }
}

impl Layout {
    pub fn focus(&self) -> PaneId {
        self.focus
    }

    pub fn zoomed(&self) -> bool {
        self.zoomed
    }

    /// 置いてあるペイン。左上から順。
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        leaves(&self.root, &mut out);
        out
    }

    /// 今のペインにする。ないペインなら何もしない。
    pub fn set_focus(&mut self, id: PaneId) {
        if self.panes().contains(&id) {
            self.focus = id;
        }
    }

    /// 今のペインを分け、新しいペインを右（または下）に置いて、そこへ移る。
    pub fn split(&mut self, dir: Dir) -> PaneId {
        let new = self.next;
        self.next += 1;
        let focus = self.focus;
        replace(&mut self.root, focus, |old| {
            let (a, b) = match dir {
                Dir::Right | Dir::Down => (old, Node::Leaf(new)),
                Dir::Left | Dir::Up => (Node::Leaf(new), old),
            };
            Node::Split {
                horizontal: dir.horizontal(),
                ratio: 0.5,
                a: Box::new(a),
                b: Box::new(b),
            }
        });
        self.focus = new;
        self.zoomed = false;
        new
    }

    /// ペインを閉じる。最後の 1 つは閉じない。閉じたら `true`。
    pub fn close(&mut self, id: PaneId) -> bool {
        let panes = self.panes();
        let Some(at) = panes.iter().position(|p| *p == id) else {
            return false;
        };
        if panes.len() == 1 {
            return false;
        }
        remove(&mut self.root, id);
        if self.focus == id {
            self.focus = panes[if at == 0 { 1 } else { at - 1 }];
        }
        self.zoomed = false;
        true
    }

    /// 今のペインを画面いっぱいに出す / 戻す。
    pub fn toggle_zoom(&mut self) {
        self.zoomed = !self.zoomed && self.panes().len() > 1;
    }

    /// 次（`step` が負なら前）のペインへ移る。
    pub fn focus_step(&mut self, step: i32) {
        let panes = self.panes();
        let at = panes.iter().position(|p| *p == self.focus).unwrap_or(0) as i32;
        self.focus = panes[(at + step).rem_euclid(panes.len() as i32) as usize];
    }

    /// その向きの隣のペイン。
    pub fn neighbor(&self, dir: Dir) -> Option<PaneId> {
        let rects = self.rects();
        let from = rects.iter().find(|(id, _)| *id == self.focus)?.1;
        let eps = 1e-4;
        let (cx, cy) = (from.x + from.w / 2., from.y + from.h / 2.);
        rects
            .iter()
            .filter(|(id, r)| {
                *id != self.focus
                    && match dir {
                        Dir::Left => (r.x + r.w - from.x).abs() < eps,
                        Dir::Right => (r.x - (from.x + from.w)).abs() < eps,
                        Dir::Up => (r.y + r.h - from.y).abs() < eps,
                        Dir::Down => (r.y - (from.y + from.h)).abs() < eps,
                    }
                    && if dir.horizontal() {
                        r.y < from.y + from.h - eps && r.y + r.h > from.y + eps
                    } else {
                        r.x < from.x + from.w - eps && r.x + r.w > from.x + eps
                    }
            })
            // いくつも接しているときは、今のペインの真ん中にいちばん近いもの
            .min_by(|(_, a), (_, b)| {
                let distance = |r: &Rect| {
                    if dir.horizontal() {
                        (r.y + r.h / 2. - cy).abs()
                    } else {
                        (r.x + r.w / 2. - cx).abs()
                    }
                };
                distance(a).total_cmp(&distance(b))
            })
            .map(|(id, _)| *id)
    }

    /// その向きの隣へ移る。隣がなければそのまま。
    pub fn focus_dir(&mut self, dir: Dir) {
        if let Some(id) = self.neighbor(dir) {
            self.focus = id;
        }
    }

    /// その向きの隣と場所を入れ替える。今のペインは同じものを指したまま動く。
    pub fn swap_dir(&mut self, dir: Dir) {
        if let Some(other) = self.neighbor(dir) {
            let focus = self.focus;
            relabel(&mut self.root, &|id| {
                if id == focus {
                    other
                } else if id == other {
                    focus
                } else {
                    id
                }
            });
        }
    }

    /// 今のペインの、その向きの境界を `amount`（全体を 1 とした割合）だけ動かす。
    pub fn resize(&mut self, dir: Dir, amount: f32) {
        let focus = self.focus;
        resize(&mut self.root, focus, dir, amount);
    }

    /// 今のペインを広げる（`amount` が負なら縮める）。右か下の境界を動かし、なければ左か上の境界を動かす。
    pub fn grow(&mut self, horizontal: bool, amount: f32) {
        let (near, far) = if horizontal {
            (Dir::Right, Dir::Left)
        } else {
            (Dir::Down, Dir::Up)
        };
        let before = self.root.clone();
        self.resize(near, amount);
        if self.root == before {
            self.resize(far, amount);
        }
    }

    /// 境界をドラッグした。`path` は `borders` が返した境界の道順。
    pub fn set_ratio(&mut self, path: &[bool], ratio: f32) {
        let mut node = &mut self.root;
        for second in path {
            let Node::Split { a, b, .. } = node else {
                return;
            };
            node = if *second { b } else { a };
        }
        if let Node::Split { ratio: r, .. } = node {
            *r = ratio.clamp(MIN_RATIO, 1. - MIN_RATIO);
        }
    }

    /// それぞれのペインの場所。拡大しているときは、今のペインだけが全体を占める。
    pub fn rects(&self) -> Vec<(PaneId, Rect)> {
        let all = Rect {
            x: 0.,
            y: 0.,
            w: 1.,
            h: 1.,
        };
        let mut out = Vec::new();
        place(&self.root, all, &mut out);
        out
    }

    /// 画面に出すペインと場所。
    pub fn visible(&self) -> Vec<(PaneId, Rect)> {
        if self.zoomed {
            vec![(
                self.focus,
                Rect {
                    x: 0.,
                    y: 0.,
                    w: 1.,
                    h: 1.,
                },
            )]
        } else {
            self.rects()
        }
    }

    /// 境界の一覧。道順、左右に分ける境界か、分けている範囲、1 つ目の取り分。
    pub fn borders(&self) -> Vec<(Vec<bool>, bool, Rect, f32)> {
        let mut out = Vec::new();
        if !self.zoomed {
            let all = Rect {
                x: 0.,
                y: 0.,
                w: 1.,
                h: 1.,
            };
            borders(&self.root, all, &mut Vec::new(), &mut out);
        }
        out
    }

    /// 保存する形。`h(0.5,0,v(0.3,1,2))@1` のように書く。
    pub fn encode(&self) -> String {
        let mut out = String::new();
        encode(&self.root, &mut out);
        format!("{out}@{}", self.focus)
    }

    /// 保存した形から戻す。読めないときや、同じ番号が 2 度出るときは `None`。
    pub fn decode(text: &str) -> Option<Self> {
        let (tree, focus) = text.rsplit_once('@')?;
        let mut rest = tree;
        let root = decode(&mut rest)?;
        if !rest.is_empty() {
            return None;
        }
        let mut panes = Vec::new();
        leaves(&root, &mut panes);
        let mut unique = panes.clone();
        unique.sort_unstable();
        unique.dedup();
        let focus: PaneId = focus.parse().ok()?;
        if unique.len() != panes.len() || !panes.contains(&focus) {
            return None;
        }
        Some(Self {
            root,
            focus,
            zoomed: false,
            next: unique.last()? + 1,
        })
    }
}

/// ペインのキー（Ctrl+B のあとに押すキー）でできること。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Split(Dir),
    Focus(Dir),
    FocusNext,
    FocusPrev,
    Swap(Dir),
    Zoom,
    Close,
    Duplicate,
    ResizeMode,
    Presets,
    PopOut,
    Help,
}

/// キーの割り当て。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Keys {
    #[default]
    Herdr,
    Tmux,
}

impl Keys {
    pub const ALL: [Keys; 2] = [Keys::Herdr, Keys::Tmux];

    pub fn label(self) -> &'static str {
        match self {
            Keys::Herdr => "herdr 風",
            Keys::Tmux => "tmux 風",
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Keys::Herdr => "herdr",
            Keys::Tmux => "tmux",
        }
    }

    pub fn from_code(code: &str) -> Self {
        if code == "tmux" {
            Keys::Tmux
        } else {
            Keys::Herdr
        }
    }

    /// キー、できること、一覧に出す説明。一覧に出す順。
    pub fn bindings(self) -> Vec<(&'static str, Action, &'static str)> {
        use Action::*;
        let mut keys = match self {
            Keys::Herdr => vec![
                ("v", Split(Dir::Right), "右に分割"),
                ("-", Split(Dir::Down), "下に分割"),
                ("h", Focus(Dir::Left), "左のペインへ"),
                ("j", Focus(Dir::Down), "下のペインへ"),
                ("k", Focus(Dir::Up), "上のペインへ"),
                ("l", Focus(Dir::Right), "右のペインへ"),
                ("Tab", FocusNext, "次のペインへ"),
                ("Shift+Tab", FocusPrev, "前のペインへ"),
                ("H", Swap(Dir::Left), "左のペインと入れ替え"),
                ("J", Swap(Dir::Down), "下のペインと入れ替え"),
                ("K", Swap(Dir::Up), "上のペインと入れ替え"),
                ("L", Swap(Dir::Right), "右のペインと入れ替え"),
                ("z", Zoom, "ペインを拡大表示 / 戻す（Esc でも戻る）"),
                ("x", Close, "ペインを閉じる"),
                ("d", Duplicate, "ペインを複製（右に）"),
                (
                    "r",
                    ResizeMode,
                    "リサイズモード（hjkl・矢印、Esc / Enter で終了）",
                ),
                ("b", Presets, "配置のプリセット"),
                ("w", PopOut, "ペインを別ウィンドウに出す"),
                ("?", Help, "この一覧"),
            ],
            Keys::Tmux => vec![
                ("%", Split(Dir::Right), "右に分割"),
                ("\"", Split(Dir::Down), "下に分割"),
                ("o", FocusNext, "次のペインへ"),
                (";", FocusPrev, "前のペインへ"),
                ("{", Swap(Dir::Left), "前のペインと入れ替え"),
                ("}", Swap(Dir::Right), "次のペインと入れ替え"),
                ("z", Zoom, "ペインを拡大表示 / 戻す（Esc でも戻る）"),
                ("x", Close, "ペインを閉じる"),
                ("d", Duplicate, "ペインを複製（右に）"),
                (
                    "r",
                    ResizeMode,
                    "リサイズモード（矢印・hjkl、Esc / Enter で終了）",
                ),
                ("b", Presets, "配置のプリセット"),
                ("!", PopOut, "ペインを別ウィンドウに出す"),
                ("?", Help, "この一覧"),
            ],
        };
        keys.extend([
            ("←", Focus(Dir::Left), "左のペインへ"),
            ("↓", Focus(Dir::Down), "下のペインへ"),
            ("↑", Focus(Dir::Up), "上のペインへ"),
            ("→", Focus(Dir::Right), "右のペインへ"),
        ]);
        keys
    }

    /// Ctrl+B のあとに押したキーでできること。`key` は打った文字か、`Tab`・`←` などのキーの名前。
    pub fn action(self, key: &str, shift: bool) -> Option<Action> {
        let key = if key == "Tab" && shift {
            "Shift+Tab"
        } else {
            key
        };
        self.bindings()
            .into_iter()
            .find(|(k, _, _)| *k == key)
            .map(|(_, action, _)| action)
    }

    /// 一覧に出す行。同じことをするキーは 1 行にまとめる（`h / ←`）。
    pub fn help(self) -> Vec<(String, &'static str)> {
        let mut out: Vec<(String, Action, &'static str)> = Vec::new();
        for (key, action, about) in self.bindings() {
            match out.iter_mut().find(|(_, a, _)| *a == action) {
                Some((keys, _, _)) => {
                    keys.push_str(" / ");
                    keys.push_str(key);
                }
                None => out.push((key.to_owned(), action, about)),
            }
        }
        out.into_iter()
            .map(|(keys, _, about)| (keys, about))
            .collect()
    }
}

/// リサイズモードのキー。広げる向き（右か下）と、動かす量（px）。Shift で大きく動く。
pub fn resize_step(key: &str, shift: bool) -> Option<(bool, f32)> {
    let step = if shift { 80. } else { 20. };
    match key {
        "l" | "L" | "→" => Some((true, step)),
        "h" | "H" | "←" => Some((true, -step)),
        "j" | "J" | "↓" => Some((false, step)),
        "k" | "K" | "↑" => Some((false, -step)),
        _ => None,
    }
}

/// 最初から入っている配置。名前と、保存する形。
pub const PRESETS: [(&str, &str); 5] = [
    ("1 画面", "0@0"),
    ("左右に 2 つ", "h(0.5000,0,1)@0"),
    ("上下に 2 つ", "v(0.5000,0,1)@0"),
    ("左に 1 つ、右に 2 つ", "h(0.5000,0,v(0.5000,1,2))@0"),
    ("4 つ", "h(0.5000,v(0.5000,0,2),v(0.5000,1,3))@0"),
];

fn leaves(node: &Node, out: &mut Vec<PaneId>) {
    match node {
        Node::Leaf(id) => out.push(*id),
        Node::Split { a, b, .. } => {
            leaves(a, out);
            leaves(b, out);
        }
    }
}

fn replace(node: &mut Node, id: PaneId, with: impl FnOnce(Node) -> Node + Copy) -> bool {
    match node {
        Node::Leaf(leaf) if *leaf == id => {
            *node = with(Node::Leaf(id));
            true
        }
        Node::Leaf(_) => false,
        Node::Split { a, b, .. } => replace(a, id, with) || replace(b, id, with),
    }
}

fn remove(node: &mut Node, id: PaneId) {
    let Node::Split { a, b, .. } = node else {
        return;
    };
    if **a == Node::Leaf(id) {
        *node = (**b).clone();
    } else if **b == Node::Leaf(id) {
        *node = (**a).clone();
    } else {
        remove(a, id);
        remove(b, id);
    }
}

fn relabel(node: &mut Node, f: &impl Fn(PaneId) -> PaneId) {
    match node {
        Node::Leaf(id) => *id = f(*id),
        Node::Split { a, b, .. } => {
            relabel(a, f);
            relabel(b, f);
        }
    }
}

fn contains(node: &Node, id: PaneId) -> bool {
    match node {
        Node::Leaf(leaf) => *leaf == id,
        Node::Split { a, b, .. } => contains(a, id) || contains(b, id),
    }
}

/// `id` にいちばん近い、その向きに境界を持つ分割の割合を動かす。動かせたら `true`。
fn resize(node: &mut Node, id: PaneId, dir: Dir, amount: f32) -> bool {
    let Node::Split {
        horizontal,
        ratio,
        a,
        b,
    } = node
    else {
        return false;
    };
    let in_a = contains(a, id);
    if !in_a && !contains(b, id) {
        return false;
    }
    if resize(if in_a { a } else { b }, id, dir, amount) {
        return true;
    }
    // 右・下の境界は 1 つ目の側、左・上の境界は 2 つ目の側にいるときに持っている
    let owns = *horizontal == dir.horizontal() && in_a == matches!(dir, Dir::Right | Dir::Down);
    if owns {
        let sign = if matches!(dir, Dir::Right | Dir::Down) {
            1.
        } else {
            -1.
        };
        *ratio = (*ratio + sign * amount).clamp(MIN_RATIO, 1. - MIN_RATIO);
    }
    owns
}

fn halves(horizontal: bool, ratio: f32, r: Rect) -> (Rect, Rect) {
    if horizontal {
        let w = r.w * ratio;
        (
            Rect { w, ..r },
            Rect {
                x: r.x + w,
                w: r.w - w,
                ..r
            },
        )
    } else {
        let h = r.h * ratio;
        (
            Rect { h, ..r },
            Rect {
                y: r.y + h,
                h: r.h - h,
                ..r
            },
        )
    }
}

fn place(node: &Node, r: Rect, out: &mut Vec<(PaneId, Rect)>) {
    match node {
        Node::Leaf(id) => out.push((*id, r)),
        Node::Split {
            horizontal,
            ratio,
            a,
            b,
        } => {
            let (ra, rb) = halves(*horizontal, *ratio, r);
            place(a, ra, out);
            place(b, rb, out);
        }
    }
}

fn borders(
    node: &Node,
    r: Rect,
    path: &mut Vec<bool>,
    out: &mut Vec<(Vec<bool>, bool, Rect, f32)>,
) {
    if let Node::Split {
        horizontal,
        ratio,
        a,
        b,
    } = node
    {
        out.push((path.clone(), *horizontal, r, *ratio));
        let (ra, rb) = halves(*horizontal, *ratio, r);
        path.push(false);
        borders(a, ra, path, out);
        path.pop();
        path.push(true);
        borders(b, rb, path, out);
        path.pop();
    }
}

fn encode(node: &Node, out: &mut String) {
    match node {
        Node::Leaf(id) => out.push_str(&id.to_string()),
        Node::Split {
            horizontal,
            ratio,
            a,
            b,
        } => {
            out.push_str(if *horizontal { "h(" } else { "v(" });
            out.push_str(&format!("{ratio:.4},"));
            encode(a, out);
            out.push(',');
            encode(b, out);
            out.push(')');
        }
    }
}

fn decode(rest: &mut &str) -> Option<Node> {
    let horizontal = match rest.as_bytes().first()? {
        b'h' => true,
        b'v' => false,
        _ => {
            let end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            let id = rest[..end].parse().ok()?;
            *rest = &rest[end..];
            return Some(Node::Leaf(id));
        }
    };
    *rest = rest[1..].strip_prefix('(')?;
    let comma = rest.find(',')?;
    let ratio: f32 = rest[..comma].parse().ok()?;
    if !(MIN_RATIO..=1. - MIN_RATIO).contains(&ratio) {
        return None;
    }
    *rest = &rest[comma + 1..];
    let a = decode(rest)?;
    *rest = rest.strip_prefix(',')?;
    let b = decode(rest)?;
    *rest = rest.strip_prefix(')')?;
    Some(Node::Split {
        horizontal,
        ratio,
        a: Box::new(a),
        b: Box::new(b),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 0 | (1 / 2) の配置。
    fn three() -> Layout {
        let mut l = Layout::default();
        assert_eq!(l.split(Dir::Right), 1);
        assert_eq!(l.split(Dir::Down), 2);
        l
    }

    #[test]
    fn split_places_the_new_pane_and_moves_there() {
        let l = three();
        assert_eq!(l.panes(), [0, 1, 2]);
        assert_eq!(l.focus(), 2);
        let rects = l.rects();
        assert_eq!(
            rects[0].1,
            Rect {
                x: 0.,
                y: 0.,
                w: 0.5,
                h: 1.
            }
        );
        assert_eq!(
            rects[1].1,
            Rect {
                x: 0.5,
                y: 0.,
                w: 0.5,
                h: 0.5
            }
        );
        assert_eq!(
            rects[2].1,
            Rect {
                x: 0.5,
                y: 0.5,
                w: 0.5,
                h: 0.5
            }
        );
        // 左や上にも分けられる
        let mut l = Layout::default();
        l.split(Dir::Left);
        assert_eq!(l.panes(), [1, 0]);
    }

    #[test]
    fn focus_moves_by_direction_and_in_order() {
        let mut l = three();
        l.focus_dir(Dir::Up);
        assert_eq!(l.focus(), 1);
        l.focus_dir(Dir::Left);
        assert_eq!(l.focus(), 0);
        // 隣がなければ動かない
        l.focus_dir(Dir::Left);
        assert_eq!(l.focus(), 0);
        // 右に 2 つ接しているときは、真ん中に近いほう（同じなら上）
        l.focus_dir(Dir::Right);
        assert_eq!(l.focus(), 1);
        l.focus_step(1);
        assert_eq!(l.focus(), 2);
        l.focus_step(1);
        assert_eq!(l.focus(), 0);
        l.focus_step(-1);
        assert_eq!(l.focus(), 2);
        l.set_focus(99);
        assert_eq!(l.focus(), 2);
    }

    #[test]
    fn swap_keeps_the_focused_pane_and_moves_it() {
        let mut l = three();
        l.swap_dir(Dir::Up);
        assert_eq!(l.panes(), [0, 2, 1]);
        assert_eq!(l.focus(), 2);
        l.swap_dir(Dir::Left);
        assert_eq!(l.panes(), [2, 0, 1]);
        l.swap_dir(Dir::Left);
        assert_eq!(l.panes(), [2, 0, 1]);
    }

    #[test]
    fn close_gives_the_space_back_and_keeps_the_last_pane() {
        let mut l = three();
        assert!(l.close(2));
        assert_eq!((l.panes(), l.focus()), (vec![0, 1], 1));
        assert_eq!(
            l.rects()[1].1,
            Rect {
                x: 0.5,
                y: 0.,
                w: 0.5,
                h: 1.
            }
        );
        assert!(!l.close(7));
        assert!(l.close(0));
        assert_eq!((l.panes(), l.focus()), (vec![1], 1));
        assert!(!l.close(1));
        // 閉じたあとに分けても、番号は重ならない
        assert_eq!(l.split(Dir::Right), 3);
    }

    #[test]
    fn resize_moves_the_nearest_border_within_limits() {
        let mut l = three();
        // 2 の上の境界を上げる
        l.resize(Dir::Up, 0.2);
        assert_eq!(
            l.rects()[2].1,
            Rect {
                x: 0.5,
                y: 0.3,
                w: 0.5,
                h: 0.7
            }
        );
        // 2 の左の境界は、0 との境界
        l.resize(Dir::Left, 0.1);
        assert!((l.rects()[0].1.w - 0.4).abs() < 1e-6);
        // その向きに境界がなければ動かない
        let before = l.clone();
        l.resize(Dir::Right, 0.1);
        l.resize(Dir::Down, 0.1);
        assert_eq!(l, before);
        // 見えなくなるほどは縮まない
        l.resize(Dir::Left, 5.);
        assert!((l.rects()[0].1.w - 0.1).abs() < 1e-6);
        // 境界のドラッグでも同じ範囲に収まる
        let borders = l.borders();
        assert_eq!(borders.len(), 2);
        assert!(borders[0].1 && !borders[1].1);
        l.set_ratio(&borders[1].0, 0.99);
        assert!((l.rects()[1].1.h - 0.9).abs() < 1e-6);
    }

    #[test]
    fn zoom_shows_only_the_focused_pane() {
        let mut l = three();
        l.toggle_zoom();
        assert_eq!(
            l.visible(),
            [(
                2,
                Rect {
                    x: 0.,
                    y: 0.,
                    w: 1.,
                    h: 1.
                }
            )]
        );
        assert!(l.borders().is_empty());
        l.toggle_zoom();
        assert_eq!(l.visible().len(), 3);
        // 分けたり閉じたりすると、拡大は解ける
        l.toggle_zoom();
        l.split(Dir::Right);
        assert!(!l.zoomed());
        // 1 つだけのときは拡大しない
        let mut one = Layout::default();
        one.toggle_zoom();
        assert!(!one.zoomed());
    }

    #[test]
    fn keys_map_to_actions_in_both_schemes() {
        use Action::*;
        let h = Keys::Herdr;
        assert_eq!(h.action("v", false), Some(Split(Dir::Right)));
        assert_eq!(h.action("-", false), Some(Split(Dir::Down)));
        assert_eq!(h.action("?", true), Some(Help));
        assert_eq!(h.action("r", false), Some(ResizeMode));
        assert_eq!(h.action("←", false), Some(Focus(Dir::Left)));
        assert_eq!(h.action("Tab", true), Some(FocusPrev));
        assert_eq!(h.action("Tab", false), Some(FocusNext));
        assert_eq!(h.action("L", true), Some(Swap(Dir::Right)));
        assert_eq!(h.action("q", false), None);
        let t = Keys::Tmux;
        assert_eq!(t.action("%", true), Some(Split(Dir::Right)));
        assert_eq!(t.action("\"", true), Some(Split(Dir::Down)));
        assert_eq!(t.action("o", false), Some(FocusNext));
        assert_eq!(t.action("!", true), Some(PopOut));
        assert_eq!(t.action("v", false), None);
        // 一覧は、同じことをするキーを 1 行にまとめる
        let help = h.help();
        assert_eq!(help[2], ("h / ←".to_owned(), "左のペインへ"));
        assert_eq!(help.len(), h.bindings().len() - 4);
        assert_eq!(Keys::from_code(Keys::Tmux.code()), Keys::Tmux);
        assert_eq!(Keys::from_code("?"), Keys::Herdr);
        // リサイズモード
        assert_eq!(resize_step("j", false), Some((false, 20.)));
        assert_eq!(resize_step("←", true), Some((true, -80.)));
        assert_eq!(resize_step("x", false), None);
        // プリセットはどれも読める
        for (name, text) in PRESETS {
            assert!(Layout::decode(text).is_some(), "{name}");
        }
    }

    #[test]
    fn grow_uses_the_border_the_pane_has() {
        let mut l = three();
        // 2 は右にも下にも境界がないので、左と上の境界が動く
        l.grow(true, 0.1);
        assert!((l.rects()[2].1.w - 0.6).abs() < 1e-6);
        l.grow(false, -0.1);
        assert!((l.rects()[2].1.h - 0.4).abs() < 1e-6);
        // 0 は右の境界が動く
        l.set_focus(0);
        l.grow(true, 0.1);
        assert!((l.rects()[0].1.w - 0.5).abs() < 1e-6);
    }

    #[test]
    fn layout_round_trips_through_its_saved_form() {
        let mut l = three();
        l.resize(Dir::Up, 0.2);
        l.focus_dir(Dir::Left);
        let text = l.encode();
        assert_eq!(text, "h(0.5000,0,v(0.3000,1,2))@0");
        let back = Layout::decode(&text).unwrap();
        assert_eq!(back.rects(), l.rects());
        assert_eq!(back.focus(), 0);
        assert_eq!(back.clone().split(Dir::Right), 3);
        for broken in [
            "",
            "0",
            "h(0.5,0,1)",
            "h(0.5,0,0)@0",
            "h(2,0,1)@0",
            "h(0.5,0,1)@9",
            "x@0",
            "0@0junk",
            "h(0.5,0,1))@0",
        ] {
            assert_eq!(Layout::decode(broken), None, "{broken}");
        }
        assert_eq!(Layout::decode("0@0"), Some(Layout::default()));
    }

    /// 決まった手順で作る、いろいろな形の配置。
    fn 乱数の配置(seed: u32) -> Layout {
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(12_345);
        let mut next = move |n: u32| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 16) % n
        };
        let mut l = Layout::default();
        let panes = 2 + next(12);
        for _ in 1..panes {
            let all = l.panes();
            let at = all[next(all.len() as u32) as usize];
            l.set_focus(at);
            let dir = [Dir::Left, Dir::Right, Dir::Up, Dir::Down][next(4) as usize];
            l.split(dir);
            if next(3) == 0 {
                let all = l.panes();
                l.set_focus(all[next(all.len() as u32) as usize]);
                let amount = [0.1, 0.2, 0.3][next(3) as usize];
                l.resize(
                    [Dir::Left, Dir::Right, Dir::Up, Dir::Down][next(4) as usize],
                    amount,
                );
            }
        }
        l
    }

    /// 隣になれるペインと、今のペインの真ん中からの遠さ（別の書き方で確かめる）。
    fn 隣の候補(l: &Layout, dir: Dir) -> Vec<(PaneId, f32)> {
        let rects = l.rects();
        let Some((_, me)) = rects.iter().find(|(id, _)| *id == l.focus()) else {
            return Vec::new();
        };
        let (left, right, top, bottom) = (me.x, me.x + me.w, me.y, me.y + me.h);
        let mut out = Vec::new();
        for (id, r) in &rects {
            if *id == l.focus() {
                continue;
            }
            let (rl, rr, rt, rb) = (r.x, r.x + r.w, r.y, r.y + r.h);
            let touches = match dir {
                Dir::Left => (rr - left).abs() < 1e-4,
                Dir::Right => (rl - right).abs() < 1e-4,
                Dir::Up => (rb - top).abs() < 1e-4,
                Dir::Down => (rt - bottom).abs() < 1e-4,
            };
            let overlap = match dir {
                Dir::Left | Dir::Right => rt < bottom - 1e-4 && rb > top + 1e-4,
                Dir::Up | Dir::Down => rl < right - 1e-4 && rr > left + 1e-4,
            };
            if !(touches && overlap) {
                continue;
            }
            let gap = match dir {
                Dir::Left | Dir::Right => ((rt + rb) / 2. - (top + bottom) / 2.).abs(),
                Dir::Up | Dir::Down => ((rl + rr) / 2. - (left + right) / 2.).abs(),
            };
            out.push((*id, gap));
        }
        out
    }

    #[test]
    fn 隣のペインはどんな配置でも辺の接し方と真ん中の近さで決まる() {
        let mut found = [0usize; 4];
        for seed in 0..6000 {
            let mut l = 乱数の配置(seed);
            for pane in l.panes() {
                l.set_focus(pane);
                for (n, dir) in [Dir::Left, Dir::Right, Dir::Up, Dir::Down]
                    .into_iter()
                    .enumerate()
                {
                    let candidates = 隣の候補(&l, dir);
                    let got = l.neighbor(dir);
                    let note = format!("seed {seed} pane {pane} {dir:?}");
                    if candidates.is_empty() {
                        assert_eq!(got, None, "{note}");
                        continue;
                    }
                    found[n] += 1;
                    let best = candidates.iter().map(|c| c.1).fold(f32::MAX, f32::min);
                    let got = got.unwrap_or_else(|| panic!("{note}: 隣がない"));
                    let gap = candidates.iter().find(|c| c.0 == got).map(|c| c.1);
                    // 同じ遠さのペインが並ぶときは、どちらでもよい
                    assert!(gap.is_some_and(|g| g - best < 1e-4), "{note}: {got}");
                }
            }
        }
        assert!(found.iter().all(|n| *n > 1000), "{found:?}");
    }

    #[test]
    fn 角だけが接しているペインは隣にならない() {
        // 右の列は上半分（1）と下半分を細かく分けたもの。左の列は上半分の中に細かく積む
        let mut l = Layout::default();
        l.split(Dir::Right); // 1
        l.split(Dir::Down); // 2（右の列の下半分）
        l.split(Dir::Down); // 3（2 の下）
        l.set_focus(0);
        l.split(Dir::Down); // 4（左の下半分）
        l.set_focus(0);
        l.split(Dir::Down); // 5
        l.set_focus(5);
        l.split(Dir::Down); // 6
        l.set_focus(6);
        let last = l.split(Dir::Down); // 7。左の列の、上半分の底にある細いペイン
        assert_eq!(last, 7);
        // 右には、同じ高さの 1 がある。角だけが接している 2 は選ばない
        assert_eq!(l.neighbor(Dir::Right), Some(1));
        assert_eq!(l.neighbor(Dir::Left), None);
    }

    #[test]
    fn 先頭のペインを閉じたら次のペインに移り他は前のペインに移る() {
        let mut l = Layout::default();
        l.split(Dir::Right);
        l.split(Dir::Right);
        assert_eq!(l.panes(), [0, 1, 2]);
        l.set_focus(0);
        assert!(l.close(0));
        assert_eq!(l.focus(), 1);
        let mut l = Layout::default();
        l.split(Dir::Right);
        l.split(Dir::Right);
        l.set_focus(2);
        assert!(l.close(2));
        assert_eq!(l.focus(), 1);
        let mut l = Layout::default();
        l.split(Dir::Right);
        l.split(Dir::Right);
        l.set_focus(2);
        assert!(l.close(1));
        assert_eq!(l.focus(), 2);
    }

    #[test]
    fn 拡大は二つ以上あるときだけできる() {
        let mut l = Layout::default();
        l.toggle_zoom();
        assert!(!l.zoomed());
        l.split(Dir::Right);
        l.toggle_zoom();
        assert!(l.zoomed());
        l.toggle_zoom();
        assert!(!l.zoomed());
    }

    #[test]
    fn キー割り当ての名前() {
        assert_eq!(Keys::Herdr.label(), "herdr 風");
        assert_eq!(Keys::Tmux.label(), "tmux 風");
    }

    #[test]
    fn リサイズモードのキーは広げる向きと量を返す() {
        assert_eq!(resize_step("l", false), Some((true, 20.)));
        assert_eq!(resize_step("L", true), Some((true, 80.)));
        assert_eq!(resize_step("→", false), Some((true, 20.)));
        assert_eq!(resize_step("h", false), Some((true, -20.)));
        assert_eq!(resize_step("←", true), Some((true, -80.)));
        assert_eq!(resize_step("j", false), Some((false, 20.)));
        assert_eq!(resize_step("↓", true), Some((false, 80.)));
        assert_eq!(resize_step("k", false), Some((false, -20.)));
        assert_eq!(resize_step("K", true), Some((false, -80.)));
        assert_eq!(resize_step("↑", false), Some((false, -20.)));
        assert_eq!(resize_step("x", false), None);
    }

    #[test]
    fn 境界は動かしても範囲の外へは出ない() {
        let mut l = Layout::default();
        l.split(Dir::Right);
        l.set_focus(0);
        l.resize(Dir::Right, 0.9);
        let w = l.rects()[0].1.w;
        assert!((w - 0.9).abs() < 1e-6, "{w}");
        l.resize(Dir::Left, 0.9);
        l.resize(Dir::Right, -2.);
        let w = l.rects()[0].1.w;
        assert!((w - 0.1).abs() < 1e-6, "{w}");
    }

    #[test]
    fn 保存した割合は範囲の中だけ読む() {
        assert!(Layout::decode("h(0.9000,0,1)@0").is_some());
        assert!(Layout::decode("h(0.9500,0,1)@0").is_none());
        assert!(Layout::decode("h(0.1000,0,1)@0").is_some());
        assert!(Layout::decode("h(0.0500,0,1)@0").is_none());
    }
}
