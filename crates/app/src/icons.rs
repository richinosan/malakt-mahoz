//! ワークスペースに付けられるアイコン。Lucide の全アイコンと、単色の絵文字（動物・食べ物・乗り物など）を
//! データにまとめて同梱し、名前・タグ・カテゴリで探せるようにする。

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex, OnceLock};

use gpui_kit::{App, RenderImage};

/// 1 行に 1 アイコン。「名前、タグ（空白区切り）、SVG の中身」をタブで区切ってある。
static DATA: &str = include_str!("../assets/icons/lucide.tsv");

/// 絵文字のアイコン（gzip）。1 行に「名前、カテゴリ、タグ（日本語）、SVG の中身」。
/// `scripts/emoji-icons.py` で作る。
static EMOJI: &[u8] = include_bytes!("../assets/icons/emoji.tsv.gz");

const SVG_HEAD: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">"#;

const EMOJI_HEAD: &str =
    r#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 32 32">"#;

/// アイコンを選ぶ画面で絞り込めるカテゴリ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Nature,
    Food,
    Travel,
    Activity,
    Object,
}

impl Category {
    pub const ALL: [Category; 5] = [
        Category::Nature,
        Category::Food,
        Category::Travel,
        Category::Activity,
        Category::Object,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Nature => "動物・自然",
            Category::Food => "食べ物・飲み物",
            Category::Travel => "乗り物・場所",
            Category::Activity => "趣味・スポーツ",
            Category::Object => "もの",
        }
    }

    /// カテゴリのボタンに出すアイコン。
    pub fn icon(self) -> &'static str {
        match self {
            Category::Nature => "paw-print",
            Category::Food => "utensils",
            Category::Travel => "car",
            Category::Activity => "trophy",
            Category::Object => "lightbulb",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Category::Nature => "nature",
            Category::Food => "food",
            Category::Travel => "travel",
            Category::Activity => "activity",
            Category::Object => "object",
        }
    }

    /// Lucide のアイコンは、タグにこの言葉のどれかがあればこのカテゴリに入れる。
    fn words(self) -> &'static [&'static str] {
        match self {
            Category::Nature => &[
                "animal", "animals", "pet", "bird", "fish", "insect", "plant", "plants", "tree",
                "flower", "forest", "garden",
            ],
            Category::Food => &[
                "food",
                "drink",
                "beverage",
                "fruit",
                "vegetable",
                "meal",
                "restaurant",
                "cooking",
                "kitchen",
            ],
            Category::Travel => &[
                "transportation",
                "transport",
                "vehicle",
                "travel",
                "trip",
                "flight",
                "journey",
                "building",
                "buildings",
                "city",
                "map",
            ],
            Category::Activity => &[
                "sport",
                "sports",
                "game",
                "games",
                "gaming",
                "music",
                "hobby",
                "toy",
                "party",
                "celebration",
                "art",
                "movie",
            ],
            Category::Object => &[
                "tool",
                "tools",
                "device",
                "office",
                "stationery",
                "clothing",
                "clothes",
                "furniture",
                "household",
                "hardware",
            ],
        }
    }
}

pub struct Entry {
    pub name: &'static str,
    /// 絵文字のカテゴリ。Lucide のアイコンにはなく、タグから決める。
    category: Option<&'static str>,
    tags: &'static str,
    body: &'static str,
}

impl Entry {
    fn is_in(&self, category: Category) -> bool {
        match self.category {
            Some(key) => key == category.key(),
            None => self
                .tags
                .split(' ')
                .any(|tag| category.words().contains(&tag)),
        }
    }
}

/// ひらがなをカタカナにする。「ねこ」でも「ネコ」でも同じものが見つかるようにする。
fn katakana(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'ぁ'..='ゖ' => char::from_u32(c as u32 + 0x60).unwrap_or(c),
            _ => c,
        })
        .collect()
}

/// 絵文字のデータを展開したもの。アプリが動いている間、持ち続ける。
fn emoji_data() -> &'static str {
    static TEXT: OnceLock<String> = OnceLock::new();
    TEXT.get_or_init(|| {
        let mut text = String::new();
        flate2::read::GzDecoder::new(EMOJI)
            .read_to_string(&mut text)
            .expect("同梱の絵文字アイコンのデータを展開できません");
        katakana(&text)
    })
}

/// 全アイコン。Lucide を名前の順に、そのあとに絵文字を並べる。
pub fn all() -> &'static [Entry] {
    static ALL: OnceLock<Vec<Entry>> = OnceLock::new();
    ALL.get_or_init(|| {
        let lucide = DATA.lines().filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            Some(Entry {
                name: parts.next()?,
                category: None,
                tags: parts.next()?,
                body: parts.next()?,
            })
        });
        let emoji = emoji_data().lines().filter_map(|line| {
            let mut parts = line.splitn(4, '\t');
            Some(Entry {
                name: parts.next()?,
                category: Some(parts.next()?),
                tags: parts.next()?,
                body: parts.next()?,
            })
        });
        lucide.chain(emoji).collect()
    })
}

fn by_name() -> &'static HashMap<&'static str, &'static Entry> {
    static MAP: OnceLock<HashMap<&'static str, &'static Entry>> = OnceLock::new();
    MAP.get_or_init(|| all().iter().map(|entry| (entry.name, entry)).collect())
}

/// その名前のアイコンの SVG。
pub fn svg(name: &str) -> Option<Vec<u8>> {
    let entry = by_name().get(name)?;
    let head = if entry.category.is_some() {
        EMOJI_HEAD
    } else {
        SVG_HEAD
    };
    Some(format!("{head}{}</svg>", entry.body).into_bytes())
}

/// 日本語で探すときの言い換え。タグは英語なので、よく使う言葉だけ対応させる。
const JAPANESE: &[(&str, &str)] = &[
    ("家", "house"),
    ("いえ", "house"),
    ("ホーム", "house"),
    ("建物", "building"),
    ("ビル", "building"),
    ("会社", "building briefcase"),
    ("仕事", "briefcase work"),
    ("しごと", "briefcase work"),
    ("学校", "school graduation"),
    ("勉強", "book graduation pencil"),
    ("本", "book"),
    ("読書", "book"),
    ("ノート", "notebook"),
    ("メモ", "note sticky"),
    ("書類", "file document"),
    ("ファイル", "file"),
    ("フォルダ", "folder"),
    ("音楽", "music"),
    ("おんがく", "music"),
    ("マイク", "mic"),
    ("映画", "film clapperboard"),
    ("動画", "video"),
    ("写真", "camera image"),
    ("カメラ", "camera"),
    ("絵", "palette brush image"),
    ("デザイン", "palette pen-tool brush"),
    ("ゲーム", "gamepad dice joystick"),
    ("スポーツ", "dumbbell trophy bike volleyball"),
    ("運動", "dumbbell bike activity"),
    ("健康", "heart-pulse activity stethoscope"),
    ("病院", "hospital stethoscope"),
    ("薬", "pill"),
    ("食べ物", "utensils pizza apple"),
    ("料理", "cooking utensils chef"),
    ("ごはん", "utensils soup"),
    ("コーヒー", "coffee"),
    ("お酒", "beer wine"),
    ("買い物", "shopping"),
    ("かいもの", "shopping"),
    ("お金", "money wallet coins yen"),
    ("かね", "money wallet coins yen"),
    ("銀行", "landmark piggy-bank"),
    ("旅行", "plane luggage map"),
    ("りょこう", "plane luggage map"),
    ("飛行機", "plane"),
    ("車", "car"),
    ("くるま", "car"),
    ("電車", "train"),
    ("自転車", "bike"),
    ("船", "ship sailboat"),
    ("地図", "map"),
    ("場所", "map-pin"),
    ("引っ越し", "truck house package"),
    ("荷物", "package box"),
    ("箱", "box package"),
    ("プレゼント", "gift"),
    ("誕生日", "cake gift party"),
    ("イベント", "calendar ticket party"),
    ("祭", "party tent"),
    ("予定", "calendar"),
    ("カレンダー", "calendar"),
    ("時間", "clock timer"),
    ("時計", "clock watch"),
    ("メール", "mail"),
    ("電話", "phone"),
    ("チャット", "message"),
    ("会話", "message speech"),
    ("人", "user"),
    ("ひと", "user"),
    ("家族", "users baby"),
    ("チーム", "users"),
    ("友達", "users handshake"),
    ("子供", "baby"),
    ("犬", "dog"),
    ("猫", "cat"),
    ("動物", "animal"),
    ("鳥", "bird"),
    ("魚", "fish"),
    ("花", "flower"),
    ("木", "tree"),
    ("植物", "plant leaf sprout"),
    ("自然", "nature"),
    ("山", "mountain"),
    ("海", "waves"),
    ("天気", "weather"),
    ("太陽", "sun"),
    ("月", "moon"),
    ("星", "star"),
    ("雨", "rain umbrella"),
    ("雪", "snow"),
    ("火", "flame"),
    ("水", "droplet"),
    ("ハート", "heart"),
    ("好き", "heart star"),
    ("旗", "flag"),
    ("目標", "target goal flag"),
    ("アイデア", "lightbulb"),
    ("電球", "lightbulb"),
    ("ロケット", "rocket"),
    ("コード", "code"),
    ("開発", "code terminal bug git"),
    ("プログラム", "code terminal"),
    ("バグ", "bug"),
    ("パソコン", "laptop monitor computer"),
    ("スマホ", "smartphone"),
    ("サーバー", "server database"),
    ("データ", "database chart"),
    ("グラフ", "chart"),
    ("設定", "settings"),
    ("道具", "tool wrench hammer"),
    ("工具", "wrench hammer"),
    ("修理", "wrench hammer"),
    ("掃除", "brush trash spray"),
    ("ゴミ", "trash"),
    ("洗濯", "washing shirt"),
    ("服", "shirt"),
    ("鍵", "key lock"),
    ("安全", "shield lock"),
    ("法律", "scale gavel"),
    ("科学", "flask atom microscope"),
    ("実験", "flask test-tube"),
    ("宇宙", "rocket orbit satellite"),
    ("ニュース", "newspaper"),
    ("ラジオ", "radio"),
    ("テレビ", "tv"),
    ("ライブ", "mic music ticket"),
    ("会場", "ticket tent map-pin"),
    ("チケット", "ticket"),
    ("王冠", "crown"),
    ("賞", "trophy award medal"),
    ("チェック", "check"),
    ("リスト", "list"),
    ("タスク", "list-todo check"),
];

/// 探す言葉とカテゴリに合うアイコンの名前。言葉が空でカテゴリもなければ `first`、その次に残りを返す。
pub fn search(
    query: &str,
    category: Option<Category>,
    first: &[&'static str],
    limit: usize,
) -> Vec<&'static str> {
    let query = query.trim().to_lowercase();
    let within = |entry: &&Entry| category.is_none_or(|category| entry.is_in(category));
    if query.is_empty() {
        let rest = all().iter().filter(within).map(|entry| entry.name);
        if category.is_some() {
            // カテゴリの中は絵文字を先に出す。動物なら、種類の多い絵文字が上に並ぶ
            let (emoji, lucide): (Vec<_>, Vec<_>) =
                rest.partition(|name| name.starts_with("emoji-"));
            return emoji.into_iter().chain(lucide).take(limit).collect();
        }
        let rest = rest.filter(|name| !first.contains(name));
        return first.iter().copied().chain(rest).take(limit).collect();
    }
    // 空白で区切った言葉のすべてに合うものを出す。日本語は、対応する英語のどれかに合えばよい
    let words: Vec<Vec<String>> = query
        .split_whitespace()
        .map(|word| {
            let mut terms = vec![katakana(word)];
            for (japanese, english) in JAPANESE {
                if japanese.contains(word) || word.contains(japanese) {
                    terms.extend(english.split(' ').map(str::to_owned));
                }
            }
            terms
        })
        .collect();
    let matches = |entry: &&Entry| {
        words.iter().all(|terms| {
            terms.iter().any(|term| {
                entry.name.contains(term.as_str()) || entry.tags.contains(term.as_str())
            })
        })
    };
    // 名前に合うものを先に、タグだけに合うものを後に並べる
    let (mut named, mut tagged) = (Vec::new(), Vec::new());
    for entry in all().iter().filter(within).filter(matches) {
        let in_name = words
            .iter()
            .all(|terms| terms.iter().any(|term| entry.name.contains(term.as_str())));
        if in_name {
            named.push(entry.name);
        } else {
            tagged.push(entry.name);
        }
    }
    named.into_iter().chain(tagged).take(limit).collect()
}

/// 言葉の並びから、いちばん合うアイコンを 1 つ選ぶ。前の言葉ほど優先する。
/// 名前がその言葉と同じものがあればそれを、なければ探して最初に見つかったものを返す。
pub fn pick(words: &[&str]) -> Option<&'static str> {
    let words: Vec<String> = words
        .iter()
        .map(|word| {
            word.trim()
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
                .replace([' ', '_'], "-")
        })
        .filter(|word| !word.is_empty())
        .collect();
    let exact = words.iter().find_map(|word| {
        let emoji = format!("emoji-{word}");
        let lucide = by_name().get(word.as_str());
        lucide
            .or_else(|| by_name().get(emoji.as_str()))
            .map(|entry| entry.name)
    });
    exact.or_else(|| {
        words
            .iter()
            .find_map(|word| search(word, None, &[], 1).into_iter().next())
    })
}

/// 自分で足す画像の、長い辺の大きさ（ピクセル）。いちばん大きく出す所の 2 倍より少し大きい。
pub const CUSTOM_ICON_PX: u32 = 96;
/// 選べる画像ファイルの大きさの上限。
const MAX_SOURCE_BYTES: u64 = 20 * 1024 * 1024;

/// 画像ファイル（PNG・JPEG・SVG）を読んで、アイコン用の小さな PNG にする。
pub fn shrink_file(path: &std::path::Path) -> Result<Vec<u8>, String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let size = std::fs::metadata(path)
        .map_err(|error| format!("画像 {name} を読めませんでした。{error}"))?
        .len();
    if size > MAX_SOURCE_BYTES {
        return Err(format!(
            "画像 {name} が大きすぎます（{} MB まで）。",
            MAX_SOURCE_BYTES / 1024 / 1024
        ));
    }
    let bytes =
        std::fs::read(path).map_err(|error| format!("画像 {name} を読めませんでした。{error}"))?;
    shrink(&bytes).map_err(|error| {
        format!("画像 {name} を開けませんでした（PNG・JPEG・SVG を選んでください）。{error}")
    })
}

/// 画像を、長い辺が [`CUSTOM_ICON_PX`] に収まる PNG にする。
pub fn shrink(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let text = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let is_svg = text
        .iter()
        .find(|b| !b.is_ascii_whitespace())
        .is_some_and(|&b| b == b'<');
    if is_svg {
        return shrink_svg(bytes);
    }
    let decoded = image::load_from_memory(bytes).map_err(|error| error.to_string())?;
    let small = if decoded.width() > CUSTOM_ICON_PX || decoded.height() > CUSTOM_ICON_PX {
        decoded.resize(
            CUSTOM_ICON_PX,
            CUSTOM_ICON_PX,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        decoded
    };
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(small.into_rgba8())
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    Ok(png.into_inner())
}

fn shrink_svg(bytes: &[u8]) -> Result<Vec<u8>, String> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_data(bytes, &usvg::Options::default())
        .map_err(|error| error.to_string())?;
    let size = tree.size();
    let scale = CUSTOM_ICON_PX as f32 / size.width().max(size.height());
    let width = ((size.width() * scale).round() as u32).clamp(1, CUSTOM_ICON_PX);
    let height = ((size.height() * scale).round() as u32).clamp(1, CUSTOM_ICON_PX);
    let mut pixmap =
        tiny_skia::Pixmap::new(width, height).ok_or("画像の大きさが正しくありません")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().map_err(|error| error.to_string())
}

/// 自分で足した画像を、描ける形で返す。一度読んだものは覚えておく（名前は中身から決まるので、変わらない）。
pub fn custom_image(name: &str, cx: &App) -> Option<Arc<RenderImage>> {
    static LOADED: OnceLock<Mutex<HashMap<String, Arc<RenderImage>>>> = OnceLock::new();
    let loaded = LOADED.get_or_init(Default::default);
    if let Some(image) = loaded.lock().ok()?.get(name) {
        return Some(image.clone());
    }
    let png = crate::db::shared(cx)
        .read(cx)
        .store
        .custom_icon(name)
        .ok()??;
    let mut pixels = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    // 描く側は BGRA で持つ
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    let image = Arc::new(RenderImage::new(vec![image::Frame::new(pixels)]));
    loaded.lock().ok()?.insert(name.to_owned(), image.clone());
    Some(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_a_valid_svg_with_a_short_name() {
        assert!(all().len() > 2900);
        assert_eq!(all().len(), by_name().len(), "名前が重なっています");
        for entry in all() {
            // データベースに入れられる名前（英小文字・数字・ハイフン、40 文字まで）
            assert!(entry.name.len() <= 40, "{}", entry.name);
            assert!(
                entry
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "{}",
                entry.name
            );
            assert!(entry.body.starts_with('<'), "{}", entry.name);
        }
        let house = String::from_utf8(svg("house").unwrap()).unwrap();
        assert!(house.starts_with("<svg") && house.ends_with("</svg>"));
        assert!(svg("no-such-icon").is_none());
    }

    #[test]
    fn search_finds_by_name_tag_and_japanese() {
        let first = ["folder", "rocket"];
        // 空なら、よく使うものを先に
        assert_eq!(
            search("", None, &first, 3),
            ["folder", "rocket", "a-arrow-down"]
        );
        // 名前に合うものが先
        assert_eq!(search("rocket", None, &first, 10)[0], "rocket");
        // タグでも見つかる（house のタグに home がある）
        assert!(search("home", None, &first, 200).contains(&"house"));
        // 日本語
        assert!(search("音楽", None, &first, 200).contains(&"music"));
        assert!(search("猫", None, &first, 200).contains(&"cat"));
        // 2 語はどちらにも合うものだけ
        let both = search("calendar check", None, &first, 200);
        assert!(both.contains(&"calendar-check"));
        assert!(!both.contains(&"calendar"));
        // 合うものがなければ空
        assert!(search("zzzzqqq", None, &first, 10).is_empty());
        // ひらがなでもカタカナでも、絵文字のアイコンが見つかる
        assert!(search("ぺんぎん", None, &first, 200).contains(&"emoji-penguin"));
        assert!(search("ペンギン", None, &first, 200).contains(&"emoji-penguin"));
        assert!(search("penguin", None, &first, 200).contains(&"emoji-penguin"));
        // 上限を守る
        assert_eq!(search("a", None, &first, 5).len(), 5);
    }

    #[test]
    fn pick_takes_the_first_word_that_has_an_icon() {
        // 同じ名前の線画が先、なければ絵文字
        assert_eq!(pick(&["cat", "dog"]), Some("cat"));
        assert_eq!(pick(&["penguin", "bird"]), Some("emoji-penguin"));
        assert_eq!(pick(&["zzzzqqq", " Rocket. "]), Some("rocket"));
        // 同じ名前がなければ、探して見つかったもの
        assert_eq!(pick(&["zzzzqqq", "takeoff"]), Some("plane-takeoff"));
        assert_eq!(pick(&["zzzzqqq"]), None);
        assert_eq!(pick(&[]), None);
    }

    #[test]
    fn categories_narrow_the_list() {
        let first = ["folder", "rocket"];
        for category in Category::ALL {
            let found = search("", Some(category), &first, 10_000);
            assert!(found.len() > 80, "{category:?}: {}", found.len());
            // カテゴリのボタンのアイコンがある
            assert!(svg(category.icon()).is_some(), "{category:?}");
        }
        let nature = search("", Some(Category::Nature), &first, 10_000);
        // 絵文字の動物が先、Lucide の動物があと
        assert!(nature.contains(&"cat") && nature.contains(&"emoji-elephant"));
        assert_eq!(nature[0], "emoji-monkey-face");
        assert!(
            nature.iter().position(|n| *n == "emoji-cat") < nature.iter().position(|n| *n == "cat")
        );
        assert!(!nature.contains(&"folder") && !nature.contains(&"emoji-pizza"));
        // カテゴリの中で探す
        assert!(search("pizza", Some(Category::Food), &first, 100).contains(&"emoji-pizza"));
        assert!(search("pizza", Some(Category::Nature), &first, 100).is_empty());
        // 絵文字の SVG は 32 の枠で、塗りで描く
        let svg = String::from_utf8(svg("emoji-penguin").unwrap()).unwrap();
        assert!(svg.contains(r#"viewBox="0 0 32 32""#) && svg.ends_with("</svg>"));
    }

    fn png_size(png: &[u8]) -> (u32, u32) {
        let image = image::load_from_memory(png).unwrap();
        (image.width(), image.height())
    }

    #[test]
    fn images_are_shrunk_to_small_pngs() {
        // 大きな画像は、縦横の比をそのままに小さくする
        let big = image::RgbaImage::from_pixel(400, 200, image::Rgba([200, 40, 40, 255]));
        let mut source = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(big)
            .write_to(&mut source, image::ImageFormat::Png)
            .unwrap();
        let png = shrink(source.get_ref()).unwrap();
        assert_eq!(png_size(&png), (CUSTOM_ICON_PX, CUSTOM_ICON_PX / 2));
        assert!(png.len() <= malakt_core::icon::MAX_CUSTOM_ICON_BYTES);

        // 小さな画像は大きくしない
        let small = image::RgbImage::from_pixel(20, 30, image::Rgb([0, 0, 0]));
        let mut source = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(small)
            .write_to(&mut source, image::ImageFormat::Jpeg)
            .unwrap();
        assert_eq!(png_size(&shrink(source.get_ref()).unwrap()), (20, 30));

        // SVG は色のまま絵にする
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 20"><rect width="10" height="20" fill="#3366ff"/></svg>"##;
        let png = shrink(svg).unwrap();
        assert_eq!(png_size(&png), (CUSTOM_ICON_PX / 2, CUSTOM_ICON_PX));
        let pixel = *image::load_from_memory(&png)
            .unwrap()
            .into_rgba8()
            .get_pixel(5, 5);
        assert_eq!(pixel.0, [0x33, 0x66, 0xff, 0xff]);

        assert!(shrink(b"lorem ipsum").is_err());
        assert!(shrink(b"<p>lorem</p>").is_err());
    }
}
