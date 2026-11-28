//! スクロールできる所に付けるスクロールバー。
//!
//! スクロールできる所には、どこでもスクロールバーを出す（`theme.rs` の `repaint` で、いつも出す設定にしている）。
//! 色はテーマの `scrollbar.thumb.background`。乗せると `scrollbar.thumb.hover.background` に少し濃くなる。
//!
//! - ハンドルを持たないスクロールは `overflow_y_scrollbar()` にする（`overflow_y_scroll()` は使わない）
//! - ハンドルで動かすスクロール（`track_scroll`）は、スクロールする要素の親に [`bar`] を重ねる
//! - メニューは [`Menus`] の `pop_menu` / `pop_menu_at` / `right_menu` で開く。入りきらない長さになるとスクロールする
//! - ターミナルは [`TermScroll`] で、さかのぼれる行をスクロールバーにする

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Weak};

use gpui_kit::component::menu::{ContextMenu, ContextMenuExt, DropdownMenu, PopupMenu};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarHandle};
use gpui_kit::*;

/// ハンドルで動かすスクロールに重ねるスクロールバー。スクロールする要素を含む親の、最後の子に置く。
/// バーはハンドルが覚えている、スクロールする要素の場所に出る。中身が入りきるときは出ない。
///
/// `name` はテストで見分ける名前（`scrollbar-<name>`）。
pub fn bar<H: ScrollbarHandle + Clone>(name: &'static str, handle: &H) -> Div {
    overlay(name)
        .child(Scrollbar::vertical(handle).id(SharedString::from(format!("scrollbar-{name}"))))
}

/// ターミナルのような暗い地に重ねるスクロールバー。テーマにかかわらず地が暗いので、白の薄い色にする。
pub fn bar_on_dark<H: ScrollbarHandle + Clone>(name: &'static str, handle: &H) -> Div {
    overlay(name).child(
        Scrollbar::vertical(handle)
            .id(SharedString::from(format!("scrollbar-{name}")))
            .styles(|styles| {
                styles
                    .thumb(|thumb| thumb.bg(white().opacity(0.28)))
                    .thumb_hover(|thumb| thumb.bg(white().opacity(0.5)))
                    .thumb_active(|thumb| thumb.bg(white().opacity(0.6)))
            }),
    )
}

fn overlay(name: &'static str) -> Div {
    div()
        .absolute()
        .inset_0()
        .debug_selector(move || format!("scrollbar-{name}"))
}

/// メニューを開く。どのメニューも、入りきらない長さになるとスクロールしてスクロールバーが出る。
/// `dropdown_menu` / `context_menu` は直接使わず、これを使う。
pub trait Menus: Sized {
    /// 押すと開くメニュー（左上をそろえる）。
    fn pop_menu(
        self,
        f: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> impl IntoElement
    where
        Self: DropdownMenu,
    {
        self.pop_menu_at(Anchor::TopLeft, f)
    }

    /// 押すと開くメニュー。`anchor` の角をそろえる。
    fn pop_menu_at(
        self,
        anchor: impl Into<Anchor>,
        f: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> impl IntoElement
    where
        Self: DropdownMenu,
    {
        self.dropdown_menu_with_anchor(anchor, move |menu, window, cx| {
            f(menu.scrollable(true), window, cx)
        })
    }

    /// 右クリックで開くメニュー。
    #[track_caller]
    fn right_menu(
        self,
        f: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> ContextMenu<Self>
    where
        Self: ContextMenuExt,
    {
        self.context_menu(move |menu, window, cx| {
            // ブラウザのページはメニューより手前に出るので、開いている間は隠す
            crate::browser::note_menu(cx.weak_entity(), cx);
            f(menu.scrollable(true), window, cx)
        })
    }
}

impl<E> Menus for E {}

/// ターミナルの、さかのぼれる行をスクロールバーで動かすためのハンドル。
/// 描くたびに [`TermScroll::update`] で今のターミナルと 1 行の高さを渡す。
#[derive(Clone, Default)]
pub struct TermScroll(Rc<RefCell<TermView>>);

#[derive(Default)]
struct TermView {
    /// 弱い参照にする。タブを閉じたら、ターミナル（とシェル）はここに残らず終わる。
    terminal: Weak<malakt_term::Terminal>,
    /// 1 行の高さ。
    line: Pixels,
    /// 画面（文字を描く所）の場所と大きさ。
    bounds: Bounds<Pixels>,
}

impl TermScroll {
    /// 描く前に、今のターミナルを渡す。
    pub fn set_terminal(&self, terminal: Option<Arc<malakt_term::Terminal>>) {
        self.0.borrow_mut().terminal = terminal.as_ref().map(Arc::downgrade).unwrap_or_default();
    }

    /// 描いたときの、画面の場所と 1 行の高さを覚える。
    pub fn update(&self, bounds: Bounds<Pixels>, line: Pixels) {
        let mut view = self.0.borrow_mut();
        view.bounds = bounds;
        view.line = line;
    }

    /// さかのぼっている行数と、さかのぼれる行数。
    fn lines(&self) -> Option<(usize, usize, Pixels)> {
        let view = self.0.borrow();
        let terminal = view.terminal.upgrade()?;
        if view.line <= px(0.) {
            return None;
        }
        let (offset, history) = terminal.scroll_lines();
        Some((offset, history, view.line))
    }
}

impl ScrollbarHandle for TermScroll {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.0.borrow().bounds
    }

    /// いちばん上（いちばん古い行）が 0。今の出力（いちばん下）は、さかのぼれる行の分だけ下。
    fn offset(&self) -> Point<Pixels> {
        match self.lines() {
            Some((offset, history, line)) => point(px(0.), -(line * (history - offset) as f32)),
            None => Point::default(),
        }
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        let Some((_, history, line)) = self.lines() else {
            return;
        };
        let from_top = (f32::from(-offset.y) / f32::from(line)).round().max(0.) as usize;
        let back = history.saturating_sub(from_top.min(history));
        let terminal = self.0.borrow().terminal.upgrade();
        if let Some(terminal) = terminal {
            terminal.scroll_to(back);
        }
    }

    fn content_size(&self) -> Size<Pixels> {
        let bounds = self.0.borrow().bounds;
        let extra = match self.lines() {
            Some((_, history, line)) => line * history as f32,
            None => px(0.),
        };
        size(bounds.size.width, bounds.size.height + extra)
    }
}

#[cfg(test)]
mod tests {
    //! スクロールの書き方の決まりを、ソースで確かめる。スクロールできる所にバーを付け忘れないため。

    const SOURCES: &[(&str, &str)] = &[
        ("ai_settings.rs", include_str!("ai_settings.rs")),
        ("ai_view.rs", include_str!("ai_view.rs")),
        ("panes.rs", include_str!("panes.rs")),
        ("render.rs", include_str!("render.rs")),
        ("state.rs", include_str!("state.rs")),
        ("term.rs", include_str!("term.rs")),
        ("feel.rs", include_str!("feel.rs")),
        ("theme.rs", include_str!("theme.rs")),
        ("shortcuts.rs", include_str!("shortcuts.rs")),
        ("history_view.rs", include_str!("history_view.rs")),
        ("browser.rs", include_str!("browser.rs")),
        ("date_pick.rs", include_str!("date_pick.rs")),
    ];

    /// コメントを除いたコードの行。
    fn code(source: &str) -> Vec<(usize, &str)> {
        let body = source.split("#[cfg(test)]").next().unwrap_or(source);
        body.lines()
            .enumerate()
            .map(|(ix, line)| (ix + 1, line.split("//").next().unwrap_or("").trim()))
            .collect()
    }

    #[test]
    fn src_files_are_all_listed() {
        let listed: Vec<&str> = SOURCES.iter().map(|(name, _)| *name).collect();
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        for entry in std::fs::read_dir(dir).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            if name == "scroll.rs" || !name.ends_with(".rs") {
                continue;
            }
            let text = std::fs::read_to_string(format!("{dir}/{name}")).unwrap();
            let draws = [
                "overflow_y_scroll",
                "overflow_x_scroll",
                "overflow_scroll",
                "track_scroll",
            ]
            .iter()
            .chain(["dropdown_menu", "context_menu", "PopupMenu::build"].iter())
            .any(|word| text.contains(word));
            assert!(
                !draws || listed.contains(&name.as_str()),
                "{name} がスクロールかメニューを描いている。scroll.rs の SOURCES に足す"
            );
        }
    }

    /// `overflow_y_scroll()` は、`track_scroll(&self.x)` と組にして [`super::bar`] を重ねるときだけ使う。
    #[test]
    fn every_scroll_has_a_bar() {
        let mut missing = Vec::new();
        for (name, source) in SOURCES {
            let lines = code(source);
            for (at, (number, line)) in lines.iter().enumerate() {
                let bare = [
                    ".overflow_y_scroll()",
                    ".overflow_x_scroll()",
                    ".overflow_scroll()",
                ]
                .iter()
                .any(|call| line.contains(call));
                if !bare {
                    continue;
                }
                // 前後 2 行に、同じ要素の track_scroll がある
                let near = &lines[at.saturating_sub(2)..(at + 3).min(lines.len())];
                let tracked = near.iter().find_map(|(_, l)| handle_of(l));
                match tracked {
                    Some(handle) if has_bar(source, &handle) => {}
                    _ => missing.push(format!("{name}:{number}")),
                }
            }
        }
        assert!(
            missing.is_empty(),
            "スクロールバーのないスクロール（overflow_y_scrollbar() か scroll::bar を使う）: {missing:?}"
        );
    }

    /// `uniform_list` や `v_virtual_list` の `track_scroll(&self.x)` にも、同じハンドルのバーを重ねる。
    #[test]
    fn every_tracked_list_has_a_bar() {
        let mut missing = Vec::new();
        for (name, source) in SOURCES {
            for (number, line) in code(source) {
                if let Some(handle) = handle_of(line) {
                    if !has_bar(source, &handle) {
                        missing.push(format!("{name}:{number} {handle}"));
                    }
                }
            }
        }
        assert!(missing.is_empty(), "バーのないハンドル: {missing:?}");
    }

    /// メニューは `scroll::Menus` で開き、入りきらないときにスクロールさせる。
    #[test]
    fn every_menu_scrolls() {
        let mut missing = Vec::new();
        for (name, source) in SOURCES {
            let lines = code(source);
            for (at, (number, line)) in lines.iter().enumerate() {
                if line.contains(".dropdown_menu") || line.contains(".context_menu(") {
                    missing.push(format!("{name}:{number}"));
                }
                if line.contains("PopupMenu::build(") {
                    let near = &lines[at..(at + 3).min(lines.len())];
                    if !near.iter().any(|(_, l)| l.contains(".scrollable(true)")) {
                        missing.push(format!("{name}:{number}"));
                    }
                }
            }
        }
        assert!(
            missing.is_empty(),
            "スクロールしないメニュー（scroll::Menus の pop_menu / pop_menu_at / right_menu を使う）: {missing:?}"
        );
    }

    fn handle_of(line: &str) -> Option<String> {
        let rest = line.split(".track_scroll(").nth(1)?;
        let handle = rest.split(')').next()?.trim().trim_start_matches('&');
        Some(handle.to_owned())
    }

    /// `scroll::bar(…, &handle)` か `vertical_scrollbar(&handle)` がある。
    fn has_bar(source: &str, handle: &str) -> bool {
        let needle = format!("&{handle})");
        source.match_indices(&needle).any(|(at, _)| {
            let before = &source[source.floor_char_boundary(at.saturating_sub(120))..at];
            before.contains("vertical_scrollbar(") || before.contains("scroll::bar")
        })
    }
}
