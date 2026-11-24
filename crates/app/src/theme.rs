//! 配色。ライトとダークを持ち、システムに合わせるか、手で選ぶ。
//!
//! ライトは白の地に黒のインクとハニーゴールド、ダークは黒の地にハニーゴールド。
//! フォーカスの輪はどちらもアイスブルー。文字と背景のコントラストは 4.5:1 以上。

use std::rc::Rc;

use gpui_kit::component::{Theme, ThemeMode, ThemeSet};
use gpui_kit::{App, Window, WindowAppearance};

use crate::db;

const SETTING: &str = "theme";

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

/// 配色を読み込んで、保存してある選択で塗る。`gpui_kit::init` のあとに 1 回呼ぶ。
pub fn init(cx: &mut App) {
    let set: ThemeSet = serde_json::from_str(THEMES).expect("同梱の配色は正しい JSON");
    let theme = Theme::global_mut(cx);
    for config in set.themes {
        if config.mode.is_dark() {
            theme.dark_theme = Rc::new(config);
        } else {
            theme.light_theme = Rc::new(config);
        }
    }
    apply(choice(cx), None, cx);
}

/// すべてのウィンドウを塗り直して、選択を保存する。保存に失敗しても画面の色は変える。
pub fn set(choice: Choice, window: &mut Window, cx: &mut App) -> malakt_core::Result<()> {
    apply(choice, Some(window), cx);
    db::shared(cx).update(cx, |db, _| {
        db.store.set_setting(SETTING, Some(choice.key()))
    })
}

/// システムの外観が変わったとき。「システムに合わせる」のときだけ塗り直す。
pub fn system_changed(window: &mut Window, cx: &mut App) {
    if choice(cx) == Choice::System {
        apply(Choice::System, Some(window), cx);
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
    Theme::change(mode, None, cx);
    // 配色を読み込むと文字の大きさが配色の値に戻るので、そのたびに拡大率を掛け直す
    Theme::update(cx, |theme| {
        theme.font_size *= crate::TEXT_SCALE;
        // スクロールできる場所が分かるように、スクロールバーはいつも出す
        theme.scrollbar_mode = gpui_kit::component::scroll::ScrollbarMode::Always;
    });
}
