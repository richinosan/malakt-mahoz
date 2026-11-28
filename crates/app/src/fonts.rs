//! 書体の優先順。文字ごとに、上から順にその文字を持つ書体で描く。
//!
//! 画面の文字は、優先順の中でこの PC にある最初の書体を主の書体にし、残りを gpui の代わりの書体
//! （`FontFallbacks`）にする。代わりの書体は、主の書体にない文字だけに、並べた順で使われる。
//! ターミナルは自分で描くので、`pick` で文字ごとに書体を選んで描く。Windows の DirectWrite は
//! 代わりの書体を OS に入った書体からしか探さず、アプリに同梱した書体（LINE Seed JP、Nerd Font）は
//! 代わりにならないため。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::base::RootPlugin;
use gpui_kit::component::Theme;
use gpui_kit::{
    font, px, App, Context, Div, Empty, FontFallbacks, Global, Hsla, IntoElement, Render,
    SharedString, Stateful, Styled, TextRun, Window,
};

use crate::theme::{DEFAULT_FONT, SYMBOL_FONT};

/// 覚えておく「この書体はこの文字を持つか」の数。超えたら覚え直す。
const COVER_CACHE: usize = 8192;

/// 1 つの優先順。
#[derive(Debug, Clone, PartialEq)]
pub struct Chain {
    /// 並べた順の書体の名前。
    pub families: Rc<[SharedString]>,
    /// 主の書体。並びの中でこの PC にある最初の書体（どれもなければ先頭）。
    pub primary: SharedString,
    /// 主の書体にない文字に使う書体（並べた順）。
    pub fallbacks: Option<FontFallbacks>,
}

impl Chain {
    /// `installed` は、この PC にあるか。分からないときは、あるものとして扱う。
    pub fn new(families: &[String], installed: impl Fn(&str) -> bool) -> Self {
        let families: Rc<[SharedString]> = families
            .iter()
            .map(|name| SharedString::from(name.clone()))
            .collect();
        let primary = families
            .iter()
            .find(|name| installed(name))
            .or(families.first())
            .cloned()
            .unwrap_or_else(|| DEFAULT_FONT.into());
        let rest: Vec<String> = families
            .iter()
            .filter(|name| **name != primary)
            .map(ToString::to_string)
            .collect();
        Chain {
            fallbacks: (!rest.is_empty()).then(|| FontFallbacks::from_fonts(rest)),
            primary,
            families,
        }
    }
}

/// 今の優先順。画面用と等幅（ターミナル・コード）用。
pub struct Fonts {
    pub ui: Chain,
    pub mono: Chain,
    /// 何も選んでいないときの等幅の書体（配色が選んだ、この PC にある等幅の書体）。
    stock_mono: SharedString,
    /// この PC にある書体の名前。使うときに 1 回だけ読む。
    installed: RefCell<Option<Rc<HashSet<String>>>>,
    /// 書体と文字ごとの、その書体がその文字を持つか。
    covers: RefCell<HashMap<(SharedString, char), bool>>,
}

impl Global for Fonts {}

/// 何も選んでいないときの等幅の優先順: 配色の等幅の書体、同梱の Nerd Font。
/// Nerd Font はプロンプトの記号（oh-my-posh など）を描くため。
pub fn default_mono(cx: &App) -> Vec<String> {
    let stock = cx
        .try_global::<Fonts>()
        .map(|fonts| fonts.stock_mono.clone())
        .unwrap_or_else(|| Theme::global(cx).mono_font_family.clone());
    let mut list = vec![stock.to_string()];
    if stock != SYMBOL_FONT {
        list.push(SYMBOL_FONT.to_owned());
    }
    list
}

/// 何も選んでいないときの画面の優先順。
pub fn default_ui() -> Vec<String> {
    vec![DEFAULT_FONT.to_owned()]
}

/// 画面の書体の優先順（保存したもの、なければ標準）。
pub fn ui_list(cx: &App) -> Vec<String> {
    let fonts = crate::theme::look(cx).fonts;
    if fonts.is_empty() {
        default_ui()
    } else {
        fonts
    }
}

/// 等幅の書体の優先順（保存したもの、なければ標準）。
pub fn mono_list(cx: &App) -> Vec<String> {
    crate::theme::mono_fonts(cx).unwrap_or_else(|| default_mono(cx))
}

/// `theme::init` から呼ぶ。配色が選んだ等幅の書体を、標準として覚えておく。
pub(crate) fn init(cx: &mut App) {
    if !cx.has_global::<Fonts>() {
        let stock = Theme::global(cx).mono_font_family.clone();
        let chain = |name: &str| Chain::new(&[name.to_owned()], |_| true);
        cx.set_global(Fonts {
            ui: chain(DEFAULT_FONT),
            mono: chain(&stock),
            stock_mono: stock,
            installed: RefCell::default(),
            covers: RefCell::default(),
        });
    }
    // 画面の代わりの書体は、ウィンドウのいちばん外の入れ物に付ける
    gpui_kit::base::Root::register_plugin::<FallbackPlugin>(cx, |_, _| FallbackPlugin);
}

/// 保存してある優先順を読み直して、主の書体を配色に入れる。配色を塗り直すたびに呼ぶ。
pub(crate) fn refresh(cx: &mut App) {
    if !cx.has_global::<Fonts>() {
        init(cx);
    }
    let (ui, mono) = (ui_list(cx), mono_list(cx));
    // 標準の並びは、この PC にあると分かっている（同梱か、配色が確かめた）ので、書体の一覧を読まない
    let check = |list: &[String], standard: &[String], cx: &App| {
        if list == standard {
            Chain::new(list, |_| true)
        } else {
            let names = installed(cx);
            Chain::new(list, |name| names.is_empty() || names.contains(name))
        }
    };
    let ui = check(&ui, &default_ui(), cx);
    let mono = check(&mono, &default_mono(cx), cx);
    let (sans, code) = (ui.primary.clone(), mono.primary.clone());
    {
        let fonts = cx.global_mut::<Fonts>();
        fonts.ui = ui;
        fonts.mono = mono;
    }
    let theme = Theme::global(cx);
    if theme.font_family != sans || theme.mono_font_family != code {
        Theme::update(cx, |theme| {
            theme.font_family = sans;
            theme.mono_font_family = code;
        });
    }
}

/// この PC にある書体の一覧を読み直す（書体の一覧を開いたとき）。
pub(crate) fn forget_installed(cx: &App) {
    if let Some(fonts) = cx.try_global::<Fonts>() {
        fonts.installed.replace(None);
        fonts.covers.borrow_mut().clear();
    }
}

fn installed(cx: &App) -> Rc<HashSet<String>> {
    let Some(fonts) = cx.try_global::<Fonts>() else {
        return Rc::default();
    };
    if let Some(names) = fonts.installed.borrow().as_ref() {
        return names.clone();
    }
    let names: Rc<HashSet<String>> =
        Rc::new(cx.text_system().all_font_names().into_iter().collect());
    fonts.installed.replace(Some(names.clone()));
    names
}

/// この PC にその書体があるか。
pub fn is_installed(name: &str, cx: &App) -> bool {
    let names = installed(cx);
    names.is_empty() || names.contains(name)
}

/// 画面の書体の優先順。
pub fn ui(cx: &App) -> Chain {
    cx.try_global::<Fonts>().map_or_else(
        || Chain::new(&default_ui(), |_| true),
        |fonts| fonts.ui.clone(),
    )
}

/// 等幅の書体の優先順。
pub fn mono(cx: &App) -> Chain {
    cx.try_global::<Fonts>().map_or_else(
        || {
            let stock = Theme::global(cx).mono_font_family.to_string();
            Chain::new(&[stock], |_| true)
        },
        |fonts| fonts.mono.clone(),
    )
}

/// `c` を描く書体。優先順の上から、その文字を持つ最初の書体。どれも持たなければ主の書体
/// （OS が代わりの書体を探す）。
pub fn pick(chain: &Chain, c: char, window: &Window, cx: &App) -> SharedString {
    if chain.families.len() > 1 {
        if let Some(found) = chain
            .families
            .iter()
            .find(|family| covers(family, c, window, cx))
        {
            return found.clone();
        }
    }
    chain.primary.clone()
}

/// その書体が `c` の字形を持つか。その書体だけで 1 文字を並べて、OS が別の書体に替えなかったかを見る。
fn covers(family: &SharedString, c: char, window: &Window, cx: &App) -> bool {
    let Some(fonts) = cx.try_global::<Fonts>() else {
        return false;
    };
    let key = (family.clone(), c);
    if let Some(known) = fonts.covers.borrow().get(&key) {
        return *known;
    }
    let found = is_installed(family, cx) && {
        let text = window.text_system();
        let mut buffer = [0; 4];
        let one = c.encode_utf8(&mut buffer);
        // 代わりの書体を空にして、その書体だけで並べる
        let mut only = font(family.clone());
        only.fallbacks = Some(FontFallbacks::default());
        let id = text.resolve_font(&only);
        let run = TextRun {
            len: one.len(),
            font: only,
            color: Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let layout = text.layout_line(SharedString::from(one.to_string()), px(16.), &[run], None);
        !layout.runs.is_empty()
            && layout
                .runs
                .iter()
                .all(|run| run.font_id == id && run.glyphs.iter().all(|glyph| glyph.id.0 != 0))
    };
    let mut known = fonts.covers.borrow_mut();
    if known.len() >= COVER_CACHE {
        known.clear();
    }
    known.insert(key, found);
    found
}

/// 等幅の書体の優先順で描く（ターミナルのほかの、コードやキーを出す所）。
pub trait MonoFont: Styled + Sized {
    fn mono_font(mut self, cx: &App) -> Self {
        let chain = mono(cx);
        let style = self.text_style();
        style.font_family = Some(chain.primary);
        style.font_fallbacks = chain.fallbacks;
        self
    }
}

impl<T: Styled> MonoFont for T {}

/// ウィンドウのいちばん外の入れ物に、画面の書体の代わりの書体を付ける。
struct FallbackPlugin;

impl Render for FallbackPlugin {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

impl RootPlugin for FallbackPlugin {
    fn style(&self, surface: &mut Stateful<Div>, _: &mut Window, cx: &mut App) {
        let chain = ui(cx);
        let style = surface.text_style();
        style.font_family = Some(chain.primary);
        style.font_fallbacks = chain.fallbacks;
    }
}

#[cfg(test)]
mod tests {
    use super::Chain;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn the_first_installed_font_leads_and_the_rest_keep_their_order() {
        let list = names(&["Missing", "Inter", "Noto Sans SC", "LINE Seed JP"]);
        let chain = Chain::new(&list, |name| name != "Missing");
        assert_eq!(chain.primary, "Inter");
        assert_eq!(
            chain.fallbacks.unwrap().fallback_list(),
            ["Missing", "Noto Sans SC", "LINE Seed JP"]
        );
        assert_eq!(chain.families.len(), 4);
    }

    #[test]
    fn nothing_installed_keeps_the_first_font() {
        let chain = Chain::new(&names(&["A", "B"]), |_| false);
        assert_eq!(chain.primary, "A");
        let single = Chain::new(&names(&["A"]), |_| true);
        assert_eq!(single.fallbacks, None);
    }
}
