//! 配色。ライトとダークを持ち、システムに合わせるか、手で選ぶ。
//!
//! ライトは白の地に黒のインクとハニーゴールド、ダークは黒の地にハニーゴールド。
//! フォーカスの輪はどちらもアイスブルー。文字と背景のコントラストは 4.5:1 以上。

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::{Theme, ThemeMode, ThemeSet};
use gpui_kit::{App, Global, Hsla, RenderImage, Rgba, Window, WindowAppearance};

use crate::db;

const SETTING: &str = "theme";
const FONT: &str = "font.family";
/// 同梱している書体。何も選んでいないときに使う。
pub const DEFAULT_FONT: &str = "LINE Seed JP";
/// 同梱している、Nerd Font のアイコンを持つ等幅の書体。ターミナルがプロンプトの記号を描くのに使う。
pub const SYMBOL_FONT: &str = "JetBrainsMono Nerd Font Mono";

const SCENE: &str = "scene";
const SCENE_CLEAR: &str = "scene.clear";
const SCENE_BLUR: &str = "scene.blur";
const CARDS: &str = "list.cards";
const FLOAT: &str = "panels.float";
const PRESETS: &str = "presets";
const OVERLAY: &str = "panel.overlay";
const DENSITY: &str = "density";
const FEEL: &str = "feel";
const DUE_FORMAT: &str = "due.format";
/// 背景の画像を出したときの、面の透明度とぼかしの最初の値（0〜100）。
pub const DEFAULT_CLEAR: u8 = 40;
pub const DEFAULT_BLUR: u8 = 30;
/// 面をこれより透かすと文字が読めなくなる。
pub const MAX_CLEAR: u8 = 70;

/// 画面の後ろに敷く画像。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Scene {
    #[default]
    None,
    /// 青い空（ダークでは夜空）
    Sky,
    /// 森（ダークでは夜の森）
    Forest,
    /// 夕暮れ（ダークでは宵の空）
    Dusk,
    /// 自分で選んだ画像
    File(PathBuf),
}

impl Scene {
    pub const BUNDLED: [Scene; 4] = [Scene::None, Scene::Sky, Scene::Forest, Scene::Dusk];

    pub fn label(&self) -> String {
        match self {
            Scene::None => "なし".into(),
            Scene::Sky => "青い空".into(),
            Scene::Forest => "森".into(),
            Scene::Dusk => "夕暮れ".into(),
            Scene::File(path) => path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into(),
            ),
        }
    }

    fn key(&self) -> Option<String> {
        match self {
            Scene::None => None,
            Scene::Sky => Some("sky".into()),
            Scene::Forest => Some("forest".into()),
            Scene::Dusk => Some("dusk".into()),
            Scene::File(path) => Some(format!("file:{}", path.display())),
        }
    }

    fn from_key(key: &str) -> Self {
        match key {
            "sky" => Scene::Sky,
            "forest" => Scene::Forest,
            "dusk" => Scene::Dusk,
            _ => key
                .strip_prefix("file:")
                .map_or(Scene::None, |path| Scene::File(path.into())),
        }
    }

    /// 同梱の画像（JPEG）。
    pub fn bundled(&self, dark: bool) -> Option<&'static [u8]> {
        Some(match (self, dark) {
            (Scene::Sky, false) => include_bytes!("../assets/scenes/sky-light.jpg"),
            (Scene::Sky, true) => include_bytes!("../assets/scenes/sky-dark.jpg"),
            (Scene::Forest, false) => include_bytes!("../assets/scenes/forest-light.jpg"),
            (Scene::Forest, true) => include_bytes!("../assets/scenes/forest-dark.jpg"),
            (Scene::Dusk, false) => include_bytes!("../assets/scenes/dusk-light.jpg"),
            (Scene::Dusk, true) => include_bytes!("../assets/scenes/dusk-dark.jpg"),
            _ => return None,
        })
    }

    /// この画像に合う色（アクセント、ライトの地、ダークの地）。自分で選んだ色があれば、そちらを使う。
    pub fn colors(&self) -> Option<[Color; 3]> {
        let hex = match self {
            Scene::Sky => ["#1f6fd0", "#eaf3fc", "#0c1830"],
            Scene::Forest => ["#4f8a3c", "#edf4ea", "#0b1712"],
            Scene::Dusk => ["#c0507a", "#fbf0ec", "#160f26"],
            _ => return None,
        };
        Some(hex.map(|hex| Color::parse(hex).expect("正しい色")))
    }
}

/// 今、画面の後ろに敷いている画像。
pub struct Backdrop {
    pub image: Arc<RenderImage>,
    /// 同じ画像を強くぼかしたもの。一覧の上に重ねるパネルの後ろに敷き、すりガラスに見せる。
    pub frosted: Arc<RenderImage>,
    /// 画像の上に重ねる面の濃さ（0〜1）。
    pub alpha: f32,
    made_from: (Scene, bool, u8),
}

#[derive(Default)]
struct Shown {
    backdrop: Option<Backdrop>,
    /// 起動のときに画像を読めなかった理由。最初のウィンドウが画面に出す。
    failed: Option<String>,
}

impl Global for Shown {}

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

/// 一覧の行の詰め方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Density {
    Roomy,
    #[default]
    Standard,
    Compact,
}

impl Density {
    pub const ALL: [Density; 3] = [Density::Roomy, Density::Standard, Density::Compact];

    pub fn label(self) -> &'static str {
        match self {
            Density::Roomy => "ゆったり",
            Density::Standard => "標準",
            Density::Compact => "詰める",
        }
    }

    fn key(self) -> Option<&'static str> {
        match self {
            Density::Roomy => Some("roomy"),
            Density::Standard => None,
            Density::Compact => Some("compact"),
        }
    }

    fn from_key(key: Option<&str>) -> Self {
        match key {
            Some("roomy") => Density::Roomy,
            Some("compact") => Density::Compact,
            _ => Density::Standard,
        }
    }

    /// 1 行の表の行と見出しの行の高さに掛ける倍率。
    pub fn rows(self) -> f32 {
        match self {
            Density::Roomy => 1.2,
            Density::Standard => 1.,
            Density::Compact => 0.75,
        }
    }

    /// 2 段のカードの行の高さに掛ける倍率（詰めても 2 段が入る高さを残す）。
    pub fn cards(self) -> f32 {
        match self {
            Density::Roomy => 1.15,
            Density::Standard => 1.,
            Density::Compact => 0.85,
        }
    }
}

/// 乗せたとき・押したとき・選んでいるときの描き方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Feel {
    /// 配色の色で塗る
    #[default]
    Standard,
    /// 白い反射と縁の光。押すと沈む
    Glass,
    /// 文字の色を薄く重ねるだけ
    Crisp,
    /// アクセントの色を薄く重ねる
    Tint,
}

impl Feel {
    pub const ALL: [Feel; 4] = [Feel::Standard, Feel::Glass, Feel::Crisp, Feel::Tint];

    pub fn label(self) -> &'static str {
        match self {
            Feel::Standard => "標準",
            Feel::Glass => "ガラス",
            Feel::Crisp => "くっきり",
            Feel::Tint => "色づく",
        }
    }

    fn key(self) -> Option<&'static str> {
        match self {
            Feel::Standard => None,
            Feel::Glass => Some("glass"),
            Feel::Crisp => Some("crisp"),
            Feel::Tint => Some("tint"),
        }
    }

    fn from_key(key: Option<&str>) -> Self {
        match key {
            Some("glass") => Feel::Glass,
            Some("crisp") => Feel::Crisp,
            Some("tint") => Feel::Tint,
            _ => Feel::Standard,
        }
    }
}

thread_local! {
    /// 今の描き方。描くたびに読むので、DB ではなくここから読む（画面を描くスレッドは 1 つ）。
    static FEEL_NOW: std::cell::Cell<Feel> = const { std::cell::Cell::new(Feel::Standard) };
}

thread_local! {
    /// パネルを浮かせているか。角の丸みを決めるたびに読むので、ここに持っておく。
    static FLOAT_NOW: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// パネルを浮かせているか。`cx` を持たない所から読む。
pub(crate) fn floating_now() -> bool {
    FLOAT_NOW.get()
}

/// 浮かせたパネル（角の丸みが大きい）の中では、部品の丸みも大きくする。
/// 内側の丸みが小さいままだと、外の角と並んだときにそろって見えない。
fn sync_corners(cx: &mut App) {
    let on = floating(cx);
    FLOAT_NOW.set(on);
    if on {
        Theme::update(cx, |theme| {
            theme.radius = gpui_kit::px(FLOAT_RADIUS);
            theme.radius_lg = gpui_kit::px(FLOAT_RADIUS_LG);
        });
    }
}

/// 浮かせたパネルの中の、部品とカードの角の丸み。
pub(crate) const FLOAT_RADIUS: f32 = 12.;
pub(crate) const FLOAT_RADIUS_LG: f32 = 14.;

/// 今の、乗せたとき・押したときの描き方。
pub fn feel(_cx: &App) -> Feel {
    feel_now()
}

/// 今の描き方。`cx` を持たない所から読む。
pub(crate) fn feel_now() -> Feel {
    FEEL_NOW.get()
}

fn saved_feel(cx: &App) -> Feel {
    let store = &db::shared(cx).read(cx).store;
    Feel::from_key(store.setting(FEEL).ok().flatten().as_deref())
}

/// 乗せたとき・押したときの描き方を変える。
pub fn set_feel(feel: Feel, cx: &mut App) -> malakt_core::Result<()> {
    db::shared(cx).update(cx, |db, _| db.store.set_setting(FEEL, feel.key()))?;
    FEEL_NOW.set(feel);
    cx.refresh_windows();
    Ok(())
}

/// 保存してある、一覧の行の詰め方。
pub fn density(cx: &App) -> Density {
    let store = &db::shared(cx).read(cx).store;
    Density::from_key(store.setting(DENSITY).ok().flatten().as_deref())
}

/// 一覧の行の詰め方を変える。
pub fn set_density(density: Density, cx: &mut App) -> malakt_core::Result<()> {
    db::shared(cx).update(cx, |db, _| db.store.set_setting(DENSITY, density.key()))?;
    cx.refresh_windows();
    Ok(())
}

/// 保存してある、一覧での期限の見せ方。
pub fn due_format(cx: &App) -> malakt_core::view::DueFormat {
    let store = &db::shared(cx).read(cx).store;
    malakt_core::view::DueFormat::from_key(store.setting(DUE_FORMAT).ok().flatten().as_deref())
}

/// 一覧での期限の見せ方を変える。すべてのウィンドウの一覧が変わる。
pub fn set_due_format(
    format: malakt_core::view::DueFormat,
    cx: &mut App,
) -> malakt_core::Result<()> {
    // 標準（日付）は消しておく
    let saved = (format != malakt_core::view::DueFormat::default()).then(|| format.key());
    db::shared(cx).update(cx, |db, _| db.store.set_setting(DUE_FORMAT, saved))?;
    cx.refresh_windows();
    Ok(())
}

/// 等幅の書体の候補。この PC に入っている最初のものを、エディタ風のテーマが使う。
const MONO_FONTS: [&str; 10] = [
    "JetBrains Mono",
    "Cascadia Code",
    "Cascadia Mono",
    "Consolas",
    "SF Mono",
    "Menlo",
    "Source Han Code JP",
    "Noto Sans Mono CJK JP",
    "Noto Sans Mono",
    "DejaVu Sans Mono",
];

/// 見た目のまとまり（テーマ）。背景の画像・色・透明度・ぼかし・カード・書体を 1 回で切り替える。
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub name: String,
    /// ライトかダークか。`None` は今のまま。
    pub mode: Option<Choice>,
    pub scene: Scene,
    pub look: Look,
    pub clear: u8,
    pub blur: u8,
    pub cards: bool,
    pub floating: bool,
    pub density: Density,
    pub feel: Feel,
}

impl Preset {
    fn plain(name: &str) -> Self {
        Preset {
            name: name.into(),
            mode: None,
            scene: Scene::None,
            look: Look::default(),
            clear: DEFAULT_CLEAR,
            blur: DEFAULT_BLUR,
            cards: false,
            floating: false,
            density: Density::Standard,
            feel: Feel::Standard,
        }
    }

    /// 背景の画像の上に、透かしたカードを浮かせるテーマ。
    fn glass(name: &str, scene: Scene, mode: Choice, clear: u8, blur: u8) -> Self {
        Preset {
            mode: Some(mode),
            scene,
            clear,
            blur,
            floating: true,
            feel: Feel::Glass,
            ..Preset::plain(name)
        }
    }

    /// 画像を使わず、色だけのテーマ。
    fn tinted(name: &str, mode: Choice, accent: &str, page: &str) -> Self {
        let (accent, page) = (Color::parse(accent), Color::parse(page));
        let dark = mode == Choice::Dark;
        Preset {
            mode: Some(mode),
            look: Look {
                font: None,
                accent,
                light: page.filter(|_| !dark),
                dark: page.filter(|_| dark),
            },
            feel: Feel::Tint,
            ..Preset::plain(name)
        }
    }

    /// おすすめのテーマ。先頭は、何も変えていないときの見た目。
    pub fn recommended() -> Vec<Preset> {
        Preset::recommended_with(&[])
    }

    /// おすすめのテーマ。エディタ風は、`installed`（この PC の書体）に等幅の書体があればそれを使い、
    /// なければ標準の書体のままにする。
    pub fn recommended_with(installed: &[String]) -> Vec<Preset> {
        use Choice::{Dark, Light};
        let mono = MONO_FONTS
            .iter()
            .find(|name| installed.iter().any(|have| have == *name))
            .map(|name| (*name).to_owned());
        let dense = |mut preset: Preset, font: Option<String>| {
            preset.density = Density::Compact;
            preset.feel = Feel::Crisp;
            preset.look.font = font;
            preset
        };
        vec![
            Preset::plain("標準"),
            Preset::glass("ガラスと青い空", Scene::Sky, Light, 50, 30),
            Preset::glass("ダークフォレスト", Scene::Forest, Dark, 45, 25),
            Preset::glass("夕暮れ", Scene::Dusk, Light, 45, 30),
            Preset::glass("星空", Scene::Sky, Dark, 50, 10),
            Preset::glass("朝の森", Scene::Forest, Light, 40, 30),
            Preset::glass("宵の空", Scene::Dusk, Dark, 50, 20),
            Preset::tinted("さくら", Light, "#c2477a", "#fdf1f5"),
            Preset::tinted("紙とインク", Light, "#7a4a1e", "#f6f0e2"),
            Preset::tinted("ミッドナイト", Dark, "#6aa7ff", "#0d1424"),
            // ノートアプリやコードエディタでよく見る、灰色の地に 1 色だけを置く配色
            // 行を詰める。エディタ風は等幅の書体も使う
            dense(Preset::tinted("ノート風", Dark, "#8f7bf0", "#1e1e1e"), None),
            dense(
                Preset::tinted("エディタ風", Dark, "#2f8fe0", "#1f1f1f"),
                mono.clone(),
            ),
            dense(
                Preset::tinted("明るいエディタ風", Light, "#005fb8", "#f8f8f8"),
                mono,
            ),
        ]
    }

    /// 名前のほかが同じ見た目か。ライトとダークを決めていないテーマは、どちらでも同じとみなす。
    pub fn same_look(&self, other: &Preset) -> bool {
        let mode = self.mode.is_none() || other.mode.is_none() || self.mode == other.mode;
        // 画像を出していないときは、透明度とぼかしは見た目に出ない
        let glass =
            self.scene == Scene::None || (self.clear, self.blur) == (other.clear, other.blur);
        mode && glass
            && self.scene == other.scene
            && self.look == other.look
            && self.cards == other.cards
            && self.floating == other.floating
            && self.density == other.density
            && self.feel == other.feel
    }

    /// このテーマで画面がダークになるか。決めていないテーマは `fallback`。
    pub fn dark(&self, fallback: bool) -> bool {
        match self.mode {
            Some(Choice::Dark) => true,
            Some(Choice::Light) => false,
            _ => fallback,
        }
    }

    /// 小さい見本に使う色（アクセント、地）。
    pub fn swatch(&self, dark: bool) -> (Color, Color) {
        let look = with_scene(self.look.clone(), &self.scene);
        let accent = look
            .accent
            .unwrap_or_else(|| stock_color(dark, "primary.background"));
        let page = if dark { look.dark } else { look.light };
        (
            accent,
            page.unwrap_or_else(|| stock_color(dark, "background")),
        )
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "mode": self.mode.map(Choice::key),
            "scene": self.scene.key(),
            "font": self.look.font,
            "accent": self.look.accent.map(Color::hex),
            "light": self.look.light.map(Color::hex),
            "dark": self.look.dark.map(Color::hex),
            "clear": self.clear,
            "blur": self.blur,
            "cards": self.cards,
            "floating": self.floating,
            "density": self.density.key(),
            "feel": self.feel.key(),
        })
    }

    fn from_json(value: &serde_json::Value) -> Option<Self> {
        let text = |key: &str| value[key].as_str();
        let color = |key: &str| text(key).and_then(Color::parse);
        let number = |key: &str, max: u8| value[key].as_u64().map(|n| n.min(u64::from(max)) as u8);
        Some(Preset {
            name: text("name")?.to_owned(),
            mode: text("mode").map(Choice::from_key),
            scene: text("scene").map_or(Scene::None, Scene::from_key),
            look: Look {
                font: text("font").map(str::to_owned),
                accent: color("accent"),
                light: color("light"),
                dark: color("dark"),
            },
            clear: number("clear", MAX_CLEAR)?,
            blur: number("blur", 100)?,
            cards: value["cards"].as_bool()?,
            floating: value["floating"].as_bool()?,
            density: Density::from_key(text("density")),
            feel: Feel::from_key(text("feel")),
        })
    }
}

/// 画面の並べ方（レイアウト）。詳細の出し方、一覧の形、パネルの形をまとめて切り替える。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub name: &'static str,
    /// 詳細を一覧の上に重ねる。`false` は一覧の右のカラム。
    pub overlay: bool,
    pub cards: bool,
    pub floating: bool,
    /// AI チャットを右に並べるか。`None` は今のまま。
    pub chat: Option<bool>,
}

impl Layout {
    /// 選べるレイアウト。先頭は、何も変えていないときの並べ方。
    pub const PATTERNS: [Layout; 5] = [
        Layout::new("一覧と詳細", false, false, false, Some(false)),
        Layout::new("3 カラム", false, false, false, Some(true)),
        Layout::new("重ねる詳細", true, false, false, None),
        Layout::new("カード", false, true, false, None),
        Layout::new("浮いたパネル", false, false, true, None),
    ];

    const fn new(
        name: &'static str,
        overlay: bool,
        cards: bool,
        floating: bool,
        chat: Option<bool>,
    ) -> Self {
        Layout {
            name,
            overlay,
            cards,
            floating,
            chat,
        }
    }

    /// 今の画面がこのレイアウトか。`chat` は、今 AI チャットを右に並べているか。
    pub fn shown(&self, chat: bool, cx: &App) -> bool {
        (self.overlay, self.cards, self.floating) == (overlay(cx), cards(cx), floating(cx))
            && self.chat.is_none_or(|wanted| wanted == chat)
    }
}

/// 詳細を一覧の上に重ねる。
pub fn overlay(cx: &App) -> bool {
    let store = &db::shared(cx).read(cx).store;
    store.setting(OVERLAY).ok().flatten().as_deref() == Some("1")
}

/// 詳細を一覧の上に重ねるかを変える。
pub fn set_overlay(on: bool, cx: &mut App) -> malakt_core::Result<()> {
    db::shared(cx).update(cx, |db, _| db.store.set_setting(OVERLAY, on.then_some("1")))?;
    cx.refresh_windows();
    Ok(())
}

/// レイアウトを使う（AI チャットの開け閉めは、呼んだ側がする）。
pub fn apply_layout(layout: &Layout, cx: &mut App) -> malakt_core::Result<()> {
    let flag = |on: bool| on.then_some("1");
    let values = [
        (OVERLAY, flag(layout.overlay)),
        (CARDS, flag(layout.cards)),
        (FLOAT, flag(layout.floating)),
    ];
    let saved = db::shared(cx).update(cx, |db, _| {
        values
            .iter()
            .try_for_each(|(key, value)| db.store.set_setting(key, *value))
    });
    cx.refresh_windows();
    saved
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

fn number(cx: &App, key: &str, default: u8, max: u8) -> u8 {
    db::shared(cx)
        .read(cx)
        .store
        .setting(key)
        .ok()
        .flatten()
        .and_then(|value| value.parse::<u8>().ok())
        .map_or(default, |value| value.min(max))
}

/// 保存してある背景の画像。
pub fn scene(cx: &App) -> Scene {
    let key = db::shared(cx).read(cx).store.setting(SCENE).ok().flatten();
    key.map_or(Scene::None, |key| Scene::from_key(&key))
}

/// 面の透明度（0〜`MAX_CLEAR`）。
pub fn clear(cx: &App) -> u8 {
    number(cx, SCENE_CLEAR, DEFAULT_CLEAR, MAX_CLEAR)
}

/// 画像のぼかし（0〜100）。
pub fn blur(cx: &App) -> u8 {
    number(cx, SCENE_BLUR, DEFAULT_BLUR, 100)
}

/// タスクの一覧を、広い画面でもカードで出す。
pub fn cards(cx: &App) -> bool {
    let store = &db::shared(cx).read(cx).store;
    store.setting(CARDS).ok().flatten().as_deref() == Some("1")
}

/// 左のメニュー・上のバー・中身を、角丸のカードとして浮かせる。
pub fn floating(cx: &App) -> bool {
    let store = &db::shared(cx).read(cx).store;
    store.setting(FLOAT).ok().flatten().as_deref() == Some("1")
}

/// パネルを浮かせるかどうかを変える。
pub fn set_floating(on: bool, cx: &mut App) -> malakt_core::Result<()> {
    db::shared(cx).update(cx, |db, _| db.store.set_setting(FLOAT, on.then_some("1")))?;
    // 配色を読み直して、角の丸みを合わせる
    let mode = Theme::global(cx).mode;
    repaint(mode, cx);
    cx.refresh_windows();
    Ok(())
}

/// 今の見た目を、テーマとしてまとめたもの。
pub fn current(name: &str, cx: &App) -> Preset {
    Preset {
        name: name.into(),
        mode: Some(choice(cx)),
        scene: scene(cx),
        look: look(cx),
        clear: clear(cx),
        blur: blur(cx),
        cards: cards(cx),
        floating: floating(cx),
        density: density(cx),
        feel: feel(cx),
    }
}

/// 自分で保存したテーマ。
pub fn own_presets(cx: &App) -> Result<Vec<Preset>, String> {
    let store = &db::shared(cx).read(cx).store;
    let saved = store
        .setting(PRESETS)
        .map_err(|error| format!("保存したテーマを読めませんでした。{error}"))?;
    let Some(saved) = saved else {
        return Ok(Vec::new());
    };
    let broken = || "保存したテーマを読めませんでした（中身が壊れています）。".to_owned();
    let list: Vec<serde_json::Value> = serde_json::from_str(&saved).map_err(|_| broken())?;
    list.iter()
        .map(|value| Preset::from_json(value).ok_or_else(broken))
        .collect()
}

/// 自分のテーマの一覧を保存する。
pub fn set_own_presets(presets: &[Preset], cx: &mut App) -> Result<(), String> {
    let list: Vec<_> = presets.iter().map(Preset::to_json).collect();
    let text = (!list.is_empty()).then(|| serde_json::Value::from(list).to_string());
    db::shared(cx)
        .update(cx, |db, _| db.store.set_setting(PRESETS, text.as_deref()))
        .map_err(|error| format!("テーマを保存できませんでした。{error}"))
}

/// テーマを使う。背景の画像を読めないときは、何も変えずに理由を返す。
pub fn apply_preset(preset: &Preset, window: &mut Window, cx: &mut App) -> Result<(), String> {
    let mode = preset.mode.unwrap_or_else(|| choice(cx));
    // 先に読んで確かめる。読めなければ保存もしない
    render(&preset.scene, false, 0)
        .map_err(|error| format!("テーマ「{}」を使えませんでした。{error}", preset.name))?;
    let hex = |color: Option<Color>| color.map(Color::hex);
    let flag = |on: bool| on.then(|| "1".to_owned());
    let look = &preset.look;
    let font = look.font.clone().filter(|name| name != DEFAULT_FONT);
    let values = [
        (SETTING, Some(mode.key().to_owned())),
        (SCENE, preset.scene.key()),
        (FONT, font),
        (Slot::Accent.key(), hex(look.accent)),
        (Slot::Light.key(), hex(look.light)),
        (Slot::Dark.key(), hex(look.dark)),
        (SCENE_CLEAR, Some(preset.clear.min(MAX_CLEAR).to_string())),
        (SCENE_BLUR, Some(preset.blur.min(100).to_string())),
        (CARDS, flag(preset.cards)),
        (FLOAT, flag(preset.floating)),
        (DENSITY, preset.density.key().map(str::to_owned)),
        (FEEL, preset.feel.key().map(str::to_owned)),
    ];
    let saved = db::shared(cx).update(cx, |db, _| {
        values
            .iter()
            .try_for_each(|(key, value)| db.store.set_setting(key, value.as_deref()))
    });
    // 途中まで保存されたときも、画面は保存されている中身に合わせる
    install(&with_scene(self::look(cx), &scene(cx)), cx);
    apply(choice(cx), Some(window), cx);
    FEEL_NOW.set(saved_feel(cx));
    let alpha = 1. - f32::from(clear(cx)) / 100.;
    if let Some(backdrop) = cx.global_mut::<Shown>().backdrop.as_mut() {
        backdrop.alpha = alpha;
    }
    cx.refresh_windows();
    saved.map_err(|error| format!("テーマ「{}」を保存できませんでした。{error}", preset.name))?;
    match take_failure(cx) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// 今、画面の後ろに敷いている画像。
pub fn backdrop(cx: &App) -> Option<&Backdrop> {
    cx.try_global::<Shown>()?.backdrop.as_ref()
}

/// 起動のときに画像を読めなかった理由を、1 回だけ受け取る。
pub fn take_failure(cx: &mut App) -> Option<String> {
    cx.try_global::<Shown>()?;
    cx.global_mut::<Shown>().failed.take()
}

/// 背景の画像を変える。読めない画像のときは、何も変えずに理由を返す。
pub fn set_scene(scene: Scene, cx: &mut App) -> Result<(), String> {
    let dark = Theme::global(cx).mode.is_dark();
    // 先に読んで確かめる。読めなければ保存もしない
    let alpha = 1. - f32::from(clear(cx)) / 100.;
    let made = make((scene.clone(), dark, blur(cx)), alpha)?;
    let key = scene.key();
    db::shared(cx)
        .update(cx, |db, _| db.store.set_setting(SCENE, key.as_deref()))
        .map_err(|error| format!("背景の画像の選択を保存できませんでした。{error}"))?;
    cx.global_mut::<Shown>().backdrop = made;
    reload(cx);
    Ok(())
}

/// 面の透明度を変える。
pub fn set_clear(value: u8, cx: &mut App) -> malakt_core::Result<()> {
    let value = value.min(MAX_CLEAR);
    db::shared(cx).update(cx, |db, _| {
        db.store.set_setting(SCENE_CLEAR, Some(&value.to_string()))
    })?;
    if let Some(backdrop) = cx.global_mut::<Shown>().backdrop.as_mut() {
        backdrop.alpha = 1. - f32::from(value) / 100.;
    }
    cx.refresh_windows();
    Ok(())
}

/// 画像のぼかしを変える。
pub fn set_blur(value: u8, cx: &mut App) -> Result<(), String> {
    let value = value.min(100);
    db::shared(cx)
        .update(cx, |db, _| {
            db.store.set_setting(SCENE_BLUR, Some(&value.to_string()))
        })
        .map_err(|error| format!("ぼかしを保存できませんでした。{error}"))?;
    sync_backdrop(cx)?;
    cx.refresh_windows();
    Ok(())
}

/// タスクの一覧をカードで出すかどうかを変える。
pub fn set_cards(on: bool, cx: &mut App) -> malakt_core::Result<()> {
    db::shared(cx).update(cx, |db, _| db.store.set_setting(CARDS, on.then_some("1")))?;
    cx.refresh_windows();
    Ok(())
}

/// 保存してある画像・ぼかしと、今のライト/ダークに合わせて、敷く画像を作り直す。
fn sync_backdrop(cx: &mut App) -> Result<(), String> {
    let wanted = (scene(cx), Theme::global(cx).mode.is_dark(), blur(cx));
    if backdrop(cx).is_some_and(|b| b.made_from == wanted) {
        return Ok(());
    }
    let alpha = 1. - f32::from(clear(cx)) / 100.;
    let made = make(wanted, alpha);
    let shown = cx.global_mut::<Shown>();
    match made {
        Ok(backdrop) => {
            shown.backdrop = backdrop;
            Ok(())
        }
        Err(error) => {
            shown.backdrop = None;
            Err(error)
        }
    }
}

/// すりガラスの画像のぼかしの強さ（`blur` と同じ 0〜100）。画像のぼかしがこれより強ければ、そちらに合わせる。
const FROST_BLUR: u8 = 70;

/// 敷く画像と、すりガラスに使う強くぼかした画像を作る。
fn make(made_from: (Scene, bool, u8), alpha: f32) -> Result<Option<Backdrop>, String> {
    let (scene, dark, blur) = &made_from;
    let Some(image) = render(scene, *dark, *blur)? else {
        return Ok(None);
    };
    let frosted = render(scene, *dark, (*blur).max(FROST_BLUR))?.unwrap_or_else(|| image.clone());
    Ok(Some(Backdrop {
        image,
        frosted,
        alpha,
        made_from,
    }))
}

/// 敷く画像を作る。ぼかすときは小さくしてからぼかす（引き伸ばして描くので、粗さは見えない）。
fn render(scene: &Scene, dark: bool, blur: u8) -> Result<Option<Arc<RenderImage>>, String> {
    let decoded = match scene {
        Scene::None => return Ok(None),
        Scene::File(path) => {
            let name = path.display();
            let bytes = std::fs::read(path)
                .map_err(|error| format!("背景の画像 {name} を読めませんでした。{error}"))?;
            image::load_from_memory(&bytes).map_err(|error| {
                format!(
                    "背景の画像 {name} を開けませんでした（PNG か JPEG を選んでください）。{error}"
                )
            })?
        }
        bundled => image::load_from_memory(bundled.bundled(dark).expect("同梱の画像"))
            .expect("同梱の画像は正しい JPEG"),
    };
    let width = if blur == 0 { 1920 } else { 480 };
    let small = if decoded.width() > width {
        decoded.resize(width, u32::MAX, image::imageops::FilterType::Triangle)
    } else {
        decoded
    };
    let mut pixels = small.into_rgba8();
    if blur > 0 {
        let sigma = f32::from(blur) * 0.12 * pixels.width() as f32 / 480.;
        pixels = image::imageops::blur(&pixels, sigma);
    }
    // 描く側は BGRA で持つ
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Ok(Some(Arc::new(RenderImage::new(vec![image::Frame::new(
        pixels,
    )]))))
}

/// 自分で選んだ色がないところに、背景の画像に合う色を入れる。
fn with_scene(mut look: Look, scene: &Scene) -> Look {
    if let Some([accent, light, dark]) = scene.colors() {
        look.accent.get_or_insert(accent);
        look.light.get_or_insert(light);
        look.dark.get_or_insert(dark);
    }
    look
}

/// 今の画面の色。選んでいないものは、背景の画像に合う色か、同梱の配色の色。
pub fn shown(slot: Slot, cx: &App) -> Color {
    if let Some(color) = with_scene(look(cx), &scene(cx)).color(slot) {
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
    cx.set_global(Shown::default());
    FEEL_NOW.set(saved_feel(cx));
    install(&with_scene(look(cx), &scene(cx)), cx);
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
    install(&with_scene(look(cx), &scene(cx)), cx);
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
    // ライトとダークで、敷く画像が変わる
    if let Err(error) = sync_backdrop(cx) {
        cx.global_mut::<Shown>().failed = Some(error);
    }
    // 配色を読み込むと文字の大きさが配色の値に戻るので、そのたびに拡大率を掛け直す
    Theme::update(cx, |theme| {
        theme.font_size *= crate::TEXT_SCALE;
        // スクロールできる場所が分かるように、スクロールバーはいつも出す
        theme.scrollbar_mode = gpui_kit::component::scroll::ScrollbarMode::Always;
        // フォーカスの印は枠の外に足す輪ではなく、入力欄の枠の色にする。
        // 輪は角の丸みが枠とずれて二重に見え、スクロールする所では端が切れる
        theme.focus_ring = false;
    });
    sync_corners(cx);
}

#[cfg(test)]
mod tests {
    use super::{make, overlaid, themes, Color, Look, Scene, THEMES};

    /// 横に隣り合う画素の差の平均。ぼけているほど小さい。
    fn roughness(image: &gpui_kit::RenderImage) -> f64 {
        let width = image.size(0).width.0 as usize * 4;
        let bytes = image.as_bytes(0).unwrap();
        let mut total = 0u64;
        let mut count = 0u64;
        for line in bytes.chunks(width) {
            for (a, b) in line.iter().zip(&line[4..]) {
                total += u64::from(a.abs_diff(*b));
                count += 1;
            }
        }
        total as f64 / count as f64
    }

    #[test]
    fn panels_over_the_list_get_a_more_blurred_backdrop() {
        // 一覧の上に重ねるパネルの後ろには、敷いた画像より強くぼかした画像を使う
        let backdrop = make((Scene::Sky, true, 10), 0.5).unwrap().unwrap();
        assert!(roughness(&backdrop.frosted) < roughness(&backdrop.image));
        assert!(make((Scene::None, true, 10), 0.5).unwrap().is_none());
    }

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
    fn presets_round_trip_and_stay_readable() {
        use super::{Preset, Scene};
        let mut names = std::collections::HashSet::new();
        for mut preset in Preset::recommended() {
            assert!(
                names.insert(preset.name.clone()),
                "{} が 2 つある",
                preset.name
            );
            // 配色として読めて、保存した形から同じものが戻る
            themes(&super::with_scene(preset.look.clone(), &preset.scene));
            preset.look.font = Some("Noto Serif".into());
            assert_eq!(Preset::from_json(&preset.to_json()), Some(preset.clone()));
            // 同梱の画像は、ライトとダークの両方がある
            if preset.scene != Scene::None {
                assert!(
                    preset.scene.bundled(false).is_some() && preset.scene.bundled(true).is_some()
                );
            }
        }
        let own = Preset {
            scene: Scene::File("/絵/海.png".into()),
            ..Preset::recommended().remove(1)
        };
        assert_eq!(Preset::from_json(&own.to_json()), Some(own));
        assert_eq!(Preset::from_json(&serde_json::json!({ "name": "x" })), None);
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(Color::parse("#86600F").unwrap().hex(), "#86600f");
        assert_eq!(Color::parse("86600f"), None);
        assert_eq!(Color::parse("#86"), None);
        assert_eq!(Color::parse("#あいうえおか"), None);
    }
}
