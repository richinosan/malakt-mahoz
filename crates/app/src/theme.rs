//! 配色。ライトとダークを持ち、システムに合わせるか、手で選ぶ。
//!
//! ライトは白の地に黒のインクとハニーゴールド、ダークは黒の地にハニーゴールド。
//! フォーカスの輪はどちらもアイスブルー。文字と背景のコントラストは 4.5:1 以上。

use std::rc::Rc;

use gpui_kit::component::{Theme, ThemeMode, ThemeSet};
use gpui_kit::{App, Hsla, Rgba, Window, WindowAppearance};

use crate::db;

const SETTING: &str = "theme";
const FONT: &str = "font.family";
/// 同梱している書体。何も選んでいないときに使う。
pub const DEFAULT_FONT: &str = "LINE Seed JP";

/// 自分で変えられる色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// ボタンや選択の色
    Accent,
    /// ライトの地の色
    Light,
    /// ダークの地の色
    Dark,
}

impl Slot {
    pub const ALL: [Slot; 3] = [Slot::Accent, Slot::Light, Slot::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Slot::Accent => "アクセント",
            Slot::Light => "背景（ライト）",
            Slot::Dark => "背景（ダーク）",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Slot::Accent => "color.accent",
            Slot::Light => "color.background.light",
            Slot::Dark => "color.background.dark",
        }
    }
}

/// 自分で選んだ書体と色。選んでいないものは `None`（同梱の配色のまま）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Look {
    pub font: Option<String>,
    pub accent: Option<Color>,
    pub light: Option<Color>,
    pub dark: Option<Color>,
}

impl Look {
    pub fn color(&self, slot: Slot) -> Option<Color> {
        match slot {
            Slot::Accent => self.accent,
            Slot::Light => self.light,
            Slot::Dark => self.dark,
        }
    }
}

/// 透けていない色（sRGB、0〜1）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color([f32; 3]);

impl Color {
    const WHITE: Color = Color([1., 1., 1.]);
    const BLACK: Color = Color([0., 0., 0.]);

    /// `#rrggbb` を読む。
    pub fn parse(hex: &str) -> Option<Self> {
        let hex = hex.trim().strip_prefix('#')?;
        if hex.len() < 6 || !hex.is_ascii() {
            return None;
        }
        let part = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
        Some(Color([
            part(0)? as f32 / 255.,
            part(2)? as f32 / 255.,
            part(4)? as f32 / 255.,
        ]))
    }

    pub fn hex(self) -> String {
        let [r, g, b] = self.0.map(|v| (v.clamp(0., 1.) * 255.).round() as u8);
        format!("#{r:02x}{g:02x}{b:02x}")
    }

    fn mix(self, other: Color, t: f32) -> Color {
        let mut out = self.0;
        for (a, b) in out.iter_mut().zip(other.0) {
            *a += (b - *a) * t;
        }
        Color(out)
    }

    /// 明るさ（WCAG の相対輝度）。
    fn luminance(self) -> f32 {
        let [r, g, b] = self.0.map(|v| {
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        });
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    fn contrast(self, other: Color) -> f32 {
        let (a, b) = (self.luminance(), other.luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// `on` の上で `ratio` 以上のコントラストになるまで、`toward` に寄せる。
    fn readable(self, on: Color, toward: Color, ratio: f32) -> Color {
        (0..=20)
            .map(|step| self.mix(toward, step as f32 / 20.))
            .find(|c| c.contrast(on) >= ratio)
            .unwrap_or(toward)
    }

    /// 白と黒のうち、この色の上で読みやすいほう。
    fn ink(self) -> Color {
        let dark = Color([0.09, 0.08, 0.07]);
        if self.contrast(Color::WHITE) >= self.contrast(dark) {
            Color::WHITE
        } else {
            dark
        }
    }
}

impl From<Hsla> for Color {
    fn from(color: Hsla) -> Self {
        let rgb = Rgba::from(color);
        Color([rgb.r, rgb.g, rgb.b])
    }
}

impl From<Color> for Hsla {
    fn from(color: Color) -> Self {
        let [r, g, b] = color.0;
        Rgba { r, g, b, a: 1. }.into()
    }
}

/// どの配色にするか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Choice {
    /// OS の設定に合わせる
    #[default]
    System,
    Light,
    Dark,
}

impl Choice {
    pub const ALL: [Choice; 3] = [Choice::System, Choice::Light, Choice::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Choice::System => "システムに合わせる",
            Choice::Light => "ライト",
            Choice::Dark => "ダーク",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Choice::System => "system",
            Choice::Light => "light",
            Choice::Dark => "dark",
        }
    }

    fn from_key(key: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|c| c.key() == key)
            .unwrap_or_default()
    }
}

const THEMES: &str = r##"{
  "name": "Malakt Mahoz",
  "themes": [
    {
      "name": "Malakt Mahoz Light",
      "mode": "light",
      "font.family": "LINE Seed JP",
      "font.size": 16,
      "radius": 8,
      "radius.lg": 12,
      "colors": {
        "background": "#f4f3f4",
        "foreground": "#1f1b26",
        "border": "#dddae3",
        "input.border": "#cfcbd8",
        "ring": "#3f88c5",
        "caret": "#1f1b26",
        "selection.background": "#3f88c540",
        "primary.background": "#86600f",
        "primary.foreground": "#ffffff",
        "primary.hover.background": "#75540d",
        "primary.active.background": "#66490b",
        "secondary.background": "#eeecf2",
        "secondary.foreground": "#3a3445",
        "secondary.hover.background": "#e4e1ea",
        "secondary.active.background": "#d9d5e1",
        "button.hover.background": "#f7ead0",
        "button.active.background": "#e6cf9c",
        "muted.background": "#ececed",
        "muted.foreground": "#5d5868",
        "accent.background": "#f7ead0",
        "accent.foreground": "#4f3a0c",
        "danger.background": "#b3344f",
        "danger.foreground": "#ffffff",
        "danger.hover.background": "#9f2d45",
        "danger.active.background": "#8c273c",
        "success.background": "#2c7a52",
        "success.foreground": "#ffffff",
        "warning.background": "#b04e1c",
        "warning.foreground": "#ffffff",
        "info.background": "#3f88c5",
        "info.foreground": "#ffffff",
        "link.foreground": "#86600f",
        "link.hover.foreground": "#75540d",
        "link.active.foreground": "#66490b",
        "popover.background": "#ffffff",
        "popover.foreground": "#1f1b26",
        "list.background": "#ffffff",
        "list.hover.background": "#f4f1f8",
        "list.active.background": "#f7ead080",
        "list.active.border": "#86600f",
        "list.head.background": "#f4f3f4",
        "list.even.background": "#ffffff",
        "table.background": "#ffffff",
        "table.hover.background": "#f4f1f8",
        "table.active.background": "#f7ead080",
        "table.active.border": "#86600f",
        "table.head.background": "#f4f3f4",
        "table.head.foreground": "#5d5868",
        "table.row.border": "#eceaf0",
        "sidebar.background": "#ededef",
        "sidebar.foreground": "#1f1b26",
        "sidebar.border": "#dddae3",
        "sidebar.accent.background": "#e0dde6",
        "sidebar.accent.foreground": "#1f1b26",
        "sidebar.primary.background": "#86600f",
        "sidebar.primary.foreground": "#ffffff",
        "title_bar.background": "#ededef",
        "title_bar.border": "#dddae3",
        "tab.background": "#00000000",
        "tab.foreground": "#5d5868",
        "tab.active.background": "#ffffff",
        "tab.active.foreground": "#1f1b26",
        "tab_bar.background": "#ececed",
        "switch.background": "#cfcbd8",
        "progress.bar.background": "#86600f",
        "slider.background": "#86600f",
        "slider.thumb.background": "#ffffff",
        "scrollbar.background": "#00000000",
        "scrollbar.thumb.background": "#5d586855",
        "scrollbar.thumb.hover.background": "#5d586899",
        "skeleton.background": "#e4e1ea",
        "drag.border": "#3f88c5",
        "drop_target.background": "#3f88c530",
        "overlay": "#1f1b2666"
      }
    },
    {
      "name": "Malakt Mahoz Dark",
      "mode": "dark",
      "font.family": "LINE Seed JP",
      "font.size": 16,
      "radius": 8,
      "radius.lg": 12,
      "colors": {
        "background": "#09090b",
        "foreground": "#f2eff5",
        "border": "#36313e",
        "input.border": "#453f4f",
        "ring": "#8cc6f0",
        "caret": "#f2eff5",
        "selection.background": "#8cc6f040",
        "primary.background": "#efc25c",
        "primary.foreground": "#1f1606",
        "primary.hover.background": "#f3cd78",
        "primary.active.background": "#e0b246",
        "secondary.background": "#211e27",
        "secondary.foreground": "#e6e1ec",
        "secondary.hover.background": "#2b2733",
        "secondary.active.background": "#36313e",
        "button.hover.background": "#3b3121",
        "button.active.background": "#6a5630",
        "muted.background": "#262528",
        "muted.foreground": "#b1abba",
        "accent.background": "#3b3121",
        "accent.foreground": "#f9e8c0",
        "danger.background": "#f08ba0",
        "danger.foreground": "#24090f",
        "danger.hover.background": "#f4a0b1",
        "danger.active.background": "#e9788f",
        "success.background": "#7dcfa0",
        "success.foreground": "#0d1a13",
        "warning.background": "#ec9a63",
        "warning.foreground": "#1f1006",
        "info.background": "#8cc6f0",
        "info.foreground": "#06141f",
        "link.foreground": "#efc25c",
        "link.hover.foreground": "#f3cd78",
        "link.active.foreground": "#e0b246",
        "popover.background": "#1b1a1d",
        "popover.foreground": "#f2eff5",
        "list.background": "#1b1a1d",
        "list.hover.background": "#242228",
        "list.active.background": "#3b312180",
        "list.active.border": "#efc25c",
        "list.head.background": "#09090b",
        "list.even.background": "#1b1a1d",
        "table.background": "#1b1a1d",
        "table.hover.background": "#242228",
        "table.active.background": "#3b312180",
        "table.active.border": "#efc25c",
        "table.head.background": "#09090b",
        "table.head.foreground": "#b1abba",
        "table.row.border": "#2a2730",
        "sidebar.background": "#201f23",
        "sidebar.foreground": "#f2eff5",
        "sidebar.border": "#36313e",
        "sidebar.accent.background": "#322f38",
        "sidebar.accent.foreground": "#f2eff5",
        "sidebar.primary.background": "#efc25c",
        "sidebar.primary.foreground": "#1f1606",
        "title_bar.background": "#201f23",
        "title_bar.border": "#36313e",
        "tab.background": "#00000000",
        "tab.foreground": "#b1abba",
        "tab.active.background": "#1b1a1d",
        "tab.active.foreground": "#f2eff5",
        "tab_bar.background": "#262528",
        "switch.background": "#453f4f",
        "progress.bar.background": "#efc25c",
        "slider.background": "#efc25c",
        "slider.thumb.background": "#1b1a1d",
        "scrollbar.background": "#00000000",
        "scrollbar.thumb.background": "#b1abba55",
        "scrollbar.thumb.hover.background": "#b1abba99",
        "skeleton.background": "#2b2733",
        "drag.border": "#8cc6f0",
        "drop_target.background": "#8cc6f030",
        "overlay": "#000000a0"
      }
    }
  ]
}"##;

/// 保存してある選択。
pub fn choice(cx: &App) -> Choice {
    db::shared(cx)
        .read(cx)
        .store
        .setting(SETTING)
        .ok()
        .flatten()
        .map(|key| Choice::from_key(&key))
        .unwrap_or_default()
}

/// 保存してある書体と色。
pub fn look(cx: &App) -> Look {
    let store = &db::shared(cx).read(cx).store;
    let read = |key: &str| store.setting(key).ok().flatten();
    let color = |slot: Slot| read(slot.key()).and_then(|hex| Color::parse(&hex));
    Look {
        font: read(FONT).filter(|name| !name.is_empty()),
        accent: color(Slot::Accent),
        light: color(Slot::Light),
        dark: color(Slot::Dark),
    }
}

/// 今の画面の色。選んでいないものは同梱の配色の色。
pub fn shown(slot: Slot, cx: &App) -> Color {
    if let Some(color) = look(cx).color(slot) {
        return color;
    }
    let theme = Theme::global(cx);
    let config = match slot {
        Slot::Accent if theme.mode.is_dark() => &theme.dark_theme,
        Slot::Accent | Slot::Light => &theme.light_theme,
        Slot::Dark => &theme.dark_theme,
    };
    let key = match slot {
        Slot::Accent => "primary.background",
        Slot::Light | Slot::Dark => "background",
    };
    stock_color(config.mode.is_dark(), key)
}

/// この PC に入っている書体の名前。同梱の書体が先頭。
pub fn fonts(cx: &App) -> Vec<String> {
    let mut names = cx.text_system().all_font_names();
    // 名前が「.」で始まるものは OS の内部用
    names.retain(|name| !name.starts_with('.') && name != DEFAULT_FONT);
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup();
    names.insert(0, DEFAULT_FONT.to_owned());
    names
}

/// 配色を読み込んで、保存してある選択で塗る。`gpui_kit::init` のあとに 1 回呼ぶ。
pub fn init(cx: &mut App) {
    install(&look(cx), cx);
    apply(choice(cx), None, cx);
}

/// すべてのウィンドウを塗り直して、選択を保存する。保存に失敗しても画面の色は変える。
pub fn set(choice: Choice, window: &mut Window, cx: &mut App) -> malakt_core::Result<()> {
    apply(choice, Some(window), cx);
    db::shared(cx).update(cx, |db, _| {
        db.store.set_setting(SETTING, Some(choice.key()))
    })
}

/// 書体を保存して、すべてのウィンドウに使う。`None` は同梱の書体。
pub fn set_font(font: Option<&str>, cx: &mut App) -> malakt_core::Result<()> {
    let font = font.filter(|name| *name != DEFAULT_FONT);
    db::shared(cx).update(cx, |db, _| db.store.set_setting(FONT, font))?;
    reload(cx);
    Ok(())
}

/// 色を保存して、すべてのウィンドウを塗り直す。`None` は同梱の配色の色。
pub fn set_color(slot: Slot, color: Option<Color>, cx: &mut App) -> malakt_core::Result<()> {
    let hex = color.map(Color::hex);
    db::shared(cx).update(cx, |db, _| db.store.set_setting(slot.key(), hex.as_deref()))?;
    reload(cx);
    Ok(())
}

fn reload(cx: &mut App) {
    install(&look(cx), cx);
    let mode = Theme::global(cx).mode;
    repaint(mode, cx);
}

/// システムの外観が変わったとき。「システムに合わせる」のときだけ塗り直す。
pub fn system_changed(window: &mut Window, cx: &mut App) {
    if choice(cx) == Choice::System {
        apply(Choice::System, Some(window), cx);
    }
}

fn stock() -> serde_json::Value {
    serde_json::from_str(THEMES).expect("同梱の配色は正しい JSON")
}

fn stock_color(dark: bool, key: &str) -> Color {
    stock()["themes"][usize::from(dark)]["colors"][key]
        .as_str()
        .and_then(Color::parse)
        .expect("同梱の配色にある色")
}

/// 同梱の配色に、自分で選んだ書体と色を重ねたもの。
fn themes(look: &Look) -> ThemeSet {
    serde_json::from_value(overlaid(look)).expect("重ねたあとも正しい配色")
}

fn overlaid(look: &Look) -> serde_json::Value {
    let mut set = stock();
    for theme in set["themes"].as_array_mut().into_iter().flatten() {
        if let Some(font) = &look.font {
            theme["font.family"] = font.as_str().into();
        }
        let dark = theme["mode"] == "dark";
        let background = if dark { look.dark } else { look.light };
        let colors = theme["colors"].as_object_mut().expect("colors がある");
        let mut put = |key: &str, value: String| {
            colors.insert(key.to_owned(), value.into());
        };
        let read = |key: &str| stock_color(dark, key);
        let (mut page, mut card, mut ink) = (
            read("background"),
            read("popover.background"),
            read("foreground"),
        );
        if let Some(chosen) = background {
            // 地の色から、文字・枠・面の色を作る。文字は地の明るさで白か黒に寄せる
            page = chosen;
            ink = [
                stock_color(false, "foreground"),
                stock_color(true, "foreground"),
                Color::BLACK,
                Color::WHITE,
            ]
            .into_iter()
            .find(|ink| ink.contrast(page) >= 7.)
            .unwrap_or(
                if page.contrast(Color::BLACK) >= page.contrast(Color::WHITE) {
                    Color::BLACK
                } else {
                    Color::WHITE
                },
            );
            let deep = ink.luminance() > page.luminance();
            card = page.mix(Color::WHITE, if deep { 0.07 } else { 0.6 });
            // 中くらいの明るさの地では、面を明るくすると文字が読めなくなる
            if ink.contrast(card) < 4.5 {
                card = page;
            }
            let step = |light: f32, dark: f32| page.mix(ink, if deep { dark } else { light });
            let (side, soft, line, field) = (
                step(0.04, 0.09),
                step(0.04, 0.11),
                step(0.11, 0.2),
                step(0.17, 0.27),
            );
            let faint = ink.mix(page, 0.3).readable(page, ink, 4.5);
            for key in [
                "background",
                "list.head.background",
                "table.head.background",
            ] {
                put(key, page.hex());
            }
            for key in [
                "foreground",
                "caret",
                "popover.foreground",
                "sidebar.foreground",
                "sidebar.accent.foreground",
                "tab.active.foreground",
                "secondary.foreground",
            ] {
                put(key, ink.hex());
            }
            for key in [
                "popover.background",
                "list.background",
                "list.even.background",
                "table.background",
                "tab.active.background",
                "slider.thumb.background",
            ] {
                put(key, card.hex());
            }
            for key in ["border", "sidebar.border", "title_bar.border"] {
                put(key, line.hex());
            }
            for key in ["input.border", "switch.background"] {
                put(key, field.hex());
            }
            for key in ["sidebar.background", "title_bar.background"] {
                put(key, side.hex());
            }
            for key in ["muted.background", "tab_bar.background"] {
                put(key, soft.hex());
            }
            for key in [
                "muted.foreground",
                "table.head.foreground",
                "tab.foreground",
            ] {
                put(key, faint.hex());
            }
            put("secondary.background", step(0.03, 0.09).hex());
            put("secondary.hover.background", step(0.08, 0.14).hex());
            put("secondary.active.background", step(0.13, 0.2).hex());
            put("skeleton.background", step(0.08, 0.14).hex());
            put("sidebar.accent.background", step(0.12, 0.18).hex());
            put("table.row.border", step(0.07, 0.13).hex());
            put("list.hover.background", card.mix(ink, 0.05).hex());
            put("table.hover.background", card.mix(ink, 0.05).hex());
            put("scrollbar.thumb.background", format!("{}55", faint.hex()));
            put(
                "scrollbar.thumb.hover.background",
                format!("{}99", faint.hex()),
            );
        }
        if look.accent.is_none() && background.is_none() {
            continue;
        }
        // アクセントは、面の上で形が分かり、リンクは地の上で読めるところまで文字の色に寄せる
        let accent = look.accent.unwrap_or_else(|| read("primary.background"));
        let deep = ink.luminance() > page.luminance();
        let main = accent.readable(card, ink, 3.);
        let text = accent.readable(page, ink, 4.5).readable(card, ink, 4.5);
        let (hover, active) = if deep {
            (main.mix(Color::WHITE, 0.2), main.mix(Color::BLACK, 0.1))
        } else {
            (main.mix(Color::BLACK, 0.12), main.mix(Color::BLACK, 0.24))
        };
        let tint = card.mix(accent, if deep { 0.22 } else { 0.18 });
        for key in [
            "primary.background",
            "sidebar.primary.background",
            "list.active.border",
            "table.active.border",
            "progress.bar.background",
            "slider.background",
        ] {
            put(key, main.hex());
        }
        for key in ["primary.foreground", "sidebar.primary.foreground"] {
            put(key, main.ink().hex());
        }
        put("primary.hover.background", hover.hex());
        put("primary.active.background", active.hex());
        put("link.foreground", text.hex());
        put("link.hover.foreground", text.mix(ink, 0.2).hex());
        put("link.active.foreground", text.mix(ink, 0.35).hex());
        put("button.hover.background", tint.hex());
        put("accent.background", tint.hex());
        put("button.active.background", card.mix(accent, 0.4).hex());
        put("accent.foreground", ink.mix(accent, 0.25).hex());
        put("list.active.background", format!("{}80", tint.hex()));
        put("table.active.background", format!("{}80", tint.hex()));
    }
    set
}

fn install(look: &Look, cx: &mut App) {
    let theme = Theme::global_mut(cx);
    for config in themes(look).themes {
        if config.mode.is_dark() {
            theme.dark_theme = Rc::new(config);
        } else {
            theme.light_theme = Rc::new(config);
        }
    }
}

fn apply(choice: Choice, window: Option<&mut Window>, cx: &mut App) {
    let mode = match choice {
        Choice::Light => ThemeMode::Light,
        Choice::Dark => ThemeMode::Dark,
        Choice::System => {
            let appearance = window
                .as_ref()
                .map(|window| window.appearance())
                .unwrap_or_else(|| cx.window_appearance());
            match appearance {
                WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeMode::Dark,
                WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeMode::Light,
            }
        }
    };
    repaint(mode, cx);
}

fn repaint(mode: ThemeMode, cx: &mut App) {
    Theme::change(mode, None, cx);
    // 配色を読み込むと文字の大きさが配色の値に戻るので、そのたびに拡大率を掛け直す
    Theme::update(cx, |theme| {
        theme.font_size *= crate::TEXT_SCALE;
        // スクロールできる場所が分かるように、スクロールバーはいつも出す
        theme.scrollbar_mode = gpui_kit::component::scroll::ScrollbarMode::Always;
    });
}

#[cfg(test)]
mod tests {
    use super::{overlaid, themes, Color, Look, THEMES};

    #[test]
    fn nothing_chosen_keeps_the_bundled_colors() {
        let stock: serde_json::Value = serde_json::from_str(THEMES).unwrap();
        assert_eq!(overlaid(&Look::default()), stock);
    }

    #[test]
    fn chosen_colors_stay_readable() {
        // 地と同じような色をアクセントに選んでも、ボタンとリンクが地に沈まない
        for (accent, light, dark) in [
            ("#ffffff", "#ffffff", "#000000"),
            ("#000000", "#fdf6e3", "#002b36"),
            ("#ffee00", "#101010", "#f0f0f0"),
            ("#3f88c5", "#3f88c5", "#3f88c5"),
        ] {
            let look = Look {
                font: Some("Noto Sans".into()),
                accent: Color::parse(accent),
                light: Color::parse(light),
                dark: Color::parse(dark),
            };
            // 重ねたものが、配色として読める
            assert_eq!(themes(&look).themes.len(), 2);
            let set = overlaid(&look);
            for theme in set["themes"].as_array().unwrap() {
                assert_eq!(theme["font.family"], "Noto Sans");
                let color =
                    |key: &str| Color::parse(theme["colors"][key].as_str().unwrap()).unwrap();
                let pairs = [
                    ("foreground", "background", 4.5),
                    ("popover.foreground", "popover.background", 4.5),
                    ("muted.foreground", "background", 4.5),
                    ("primary.foreground", "primary.background", 4.5),
                    ("primary.background", "popover.background", 3.),
                    ("link.foreground", "background", 4.5),
                    ("accent.foreground", "accent.background", 4.5),
                ];
                for (front, back, ratio) in pairs {
                    let found = color(front).contrast(color(back));
                    assert!(
                        found >= ratio,
                        "{accent} {light} {dark}: {front} / {back} = {found}"
                    );
                }
            }
        }
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(Color::parse("#86600F").unwrap().hex(), "#86600f");
        assert_eq!(Color::parse("86600f"), None);
        assert_eq!(Color::parse("#86"), None);
        assert_eq!(Color::parse("#あいうえおか"), None);
    }
}
