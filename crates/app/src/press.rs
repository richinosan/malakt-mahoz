//! 押したときの処理がないボタンを、コンパイルの時点で作れなくする。
//!
//! ボタンは `button(id)` で作る。できるのは [`Unpressed`] で、見た目（アイコン、ラベル、色、
//! 大きさ）は `Button` と同じように付けられるが、画面には置けない（`IntoElement` ではない）。
//! `on_click` か `pop_menu` / `pop_menu_at` を付けたときにはじめて置けるものになるので、
//! 押しても何も起きないボタンはビルドが通らない。
//!
//! `Button::new` を直接呼ぶことは clippy（`clippy.toml` の `disallowed-methods`）で止めている。

use gpui_kit::base::Disableable;
use gpui_kit::component::button::{Button, ButtonRounded, ButtonVariant, ButtonVariants};
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::component::{Icon, Placement, Selectable, Sizable, Size};
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, ElementId, InteractiveElement, Interactivity,
    IntoElement, ParentElement, SharedString, StyleRefinement, Styled, Window,
};

use crate::scroll::Menus;

/// 押したときの処理がまだないボタン。
#[must_use = "on_click か pop_menu を付けないと、画面に置けない"]
pub struct Unpressed(Button);

/// ボタンを作る。押したときの処理を付けるまで、画面には置けない。
pub fn button(id: impl Into<ElementId>) -> Unpressed {
    #[allow(clippy::disallowed_methods)] // ボタンはここでだけ作る
    Unpressed(Button::new(id))
}

impl Unpressed {
    /// 押したときの処理を付ける。
    pub fn on_click(
        self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Button {
        self.0.on_click(handler)
    }

    /// 押すと開くメニューを付ける（左上をそろえる）。
    pub fn pop_menu(
        self,
        f: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> impl IntoElement {
        Menus::pop_menu(self.0, f)
    }

    /// 押すと開くメニューを付ける。`anchor` の角をそろえる。
    pub fn pop_menu_at(
        self,
        anchor: impl Into<gpui_kit::Anchor>,
        f: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> impl IntoElement {
        Menus::pop_menu_at(self.0, anchor, f)
    }

    /// 場所だけを取る、見えない押せないボタンにする。並びをそろえるために置く。
    pub fn placeholder(self) -> Button {
        self.0.invisible().disabled(true)
    }

    /// `Button` を組み立てる途中で、自分の手で変える（`Framed` などのため）。
    pub(crate) fn map_button(self, f: impl FnOnce(Button) -> Button) -> Self {
        Self(f(self.0))
    }

    pub fn when(self, condition: bool, then: impl FnOnce(Self) -> Self) -> Self {
        if condition {
            then(self)
        } else {
            self
        }
    }

    pub fn label(self, label: impl Into<SharedString>) -> Self {
        Self(self.0.label(label))
    }

    pub fn icon(self, icon: impl Into<Icon>) -> Self {
        Self(self.0.icon(icon.into()))
    }

    pub fn tooltip(self, tooltip: impl Into<SharedString>) -> Self {
        Self(self.0.tooltip(tooltip))
    }

    pub fn tooltip_placement(self, placement: Placement) -> Self {
        Self(self.0.tooltip_placement(placement))
    }

    pub fn outline(self) -> Self {
        Self(self.0.outline())
    }

    pub fn rounded(self, rounded: impl Into<ButtonRounded>) -> Self {
        Self(self.0.rounded(rounded))
    }

    pub fn loading(self, loading: bool) -> Self {
        Self(self.0.loading(loading))
    }

    pub fn dropdown_caret(self, dropdown_caret: bool) -> Self {
        Self(self.0.dropdown_caret(dropdown_caret))
    }
}

impl Styled for Unpressed {
    fn style(&mut self) -> &mut StyleRefinement {
        self.0.style()
    }
}

impl ParentElement for Unpressed {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.0.extend(elements)
    }
}

impl InteractiveElement for Unpressed {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.0.interactivity()
    }
}

impl Sizable for Unpressed {
    fn with_size(self, size: impl Into<Size>) -> Self {
        Self(self.0.with_size(size))
    }
}

impl ButtonVariants for Unpressed {
    fn with_variant(self, variant: ButtonVariant) -> Self {
        Self(self.0.with_variant(variant))
    }
}

impl Disableable for Unpressed {
    fn disabled(self, disabled: bool) -> Self {
        Self(self.0.disabled(disabled))
    }
}

impl Selectable for Unpressed {
    fn selected(self, selected: bool) -> Self {
        Self(self.0.selected(selected))
    }

    fn is_selected(&self) -> bool {
        self.0.is_selected()
    }
}
