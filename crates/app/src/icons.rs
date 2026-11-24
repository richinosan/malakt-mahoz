//! ワークスペースに付けられるアイコン。Lucide の全アイコンを 1 つのデータにまとめて同梱し、
//! 名前とタグで探せるようにする。

use std::collections::HashMap;
use std::sync::OnceLock;

/// 1 行に 1 アイコン。「名前、タグ（空白区切り）、SVG の中身」をタブで区切ってある。
static DATA: &str = include_str!("../assets/icons/lucide.tsv");

const SVG_HEAD: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">"#;

pub struct Entry {
    pub name: &'static str,
    tags: &'static str,
    body: &'static str,
}

/// 名前の順に並んだ全アイコン。
pub fn all() -> &'static [Entry] {
    static ALL: OnceLock<Vec<Entry>> = OnceLock::new();
    ALL.get_or_init(|| {
        DATA.lines()
            .filter_map(|line| {
                let mut parts = line.splitn(3, '\t');
                Some(Entry {
                    name: parts.next()?,
                    tags: parts.next()?,
                    body: parts.next()?,
                })
            })
            .collect()
    })
}

fn by_name() -> &'static HashMap<&'static str, &'static Entry> {
    static MAP: OnceLock<HashMap<&'static str, &'static Entry>> = OnceLock::new();
    MAP.get_or_init(|| all().iter().map(|entry| (entry.name, entry)).collect())
}

/// その名前のアイコンの SVG。
pub fn svg(name: &str) -> Option<Vec<u8>> {
    let entry = by_name().get(name)?;
    Some(format!("{SVG_HEAD}{}</svg>", entry.body).into_bytes())
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

/// 探す言葉に合うアイコンの名前。空なら `first`、その次に残りを名前の順で返す。
pub fn search(query: &str, first: &[&'static str], limit: usize) -> Vec<&'static str> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        let rest = all()
            .iter()
            .map(|entry| entry.name)
            .filter(|name| !first.contains(name));
        return first.iter().copied().chain(rest).take(limit).collect();
    }
    // 空白で区切った言葉のすべてに合うものを出す。日本語は、対応する英語のどれかに合えばよい
    let words: Vec<Vec<String>> = query
        .split_whitespace()
        .map(|word| {
            let mut terms = vec![word.to_owned()];
            for (japanese, english) in JAPANESE {
                if japanese.contains(word) || word.contains(japanese) {
                    terms.extend(english.split(' ').map(str::to_owned));
                }
            }
            terms
        })
        .collect();
    let matches = |entry: &Entry| {
        words.iter().all(|terms| {
            terms.iter().any(|term| {
                entry.name.contains(term.as_str()) || entry.tags.contains(term.as_str())
            })
        })
    };
    // 名前に合うものを先に、タグだけに合うものを後に並べる
    let (mut named, mut tagged) = (Vec::new(), Vec::new());
    for entry in all().iter().filter(|entry| matches(entry)) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_a_valid_svg_with_a_short_name() {
        assert!(all().len() > 2000);
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
        assert_eq!(search("", &first, 3), ["folder", "rocket", "a-arrow-down"]);
        // 名前に合うものが先
        assert_eq!(search("rocket", &first, 10)[0], "rocket");
        // タグでも見つかる（house のタグに home がある）
        assert!(search("home", &first, 200).contains(&"house"));
        // 日本語
        assert!(search("音楽", &first, 200).contains(&"music"));
        assert!(search("猫", &first, 200).contains(&"cat"));
        // 2 語はどちらにも合うものだけ
        let both = search("calendar check", &first, 200);
        assert!(both.contains(&"calendar-check"));
        assert!(!both.contains(&"calendar"));
        // 合うものがなければ空
        assert!(search("zzzzqqq", &first, 10).is_empty());
        // 上限を守る
        assert_eq!(search("a", &first, 5).len(), 5);
    }
}
