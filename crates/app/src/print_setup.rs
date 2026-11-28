//! 印刷のダイアログの設定（Chrome の印刷と同じ項目）。設定は覚えて、次に開いたときも同じにする。
//!
//! - ページ: すべて・奇数ページのみ・偶数ページのみ・カスタム（`1-5, 8, 11-13`）
//! - レイアウト: 縦・横
//! - 用紙サイズ、1 枚あたりのページ数、余白（標準・なし・最小・カスタム）、倍率（標準・カスタム）
//! - オプション: ヘッダーとフッター、背景のグラフィック
//!
//! PDF のフォントは必ず埋め込む。保存した PDF を読み直して、埋め込んでいないフォントがあれば失敗にする。

use std::path::Path;

use serde::{Deserialize, Serialize};

/// 設定を覚えておく場所（設定の表のキー）。
pub const SETTING: &str = "browser.print";

/// 1 枚あたりのページ数に選べる数（Chrome と同じ）。
pub const PER_SHEET: [u8; 6] = [1, 2, 4, 6, 9, 16];

/// 1 枚に何ページも並べられるか（macOS の WKWebView はできない）。
pub const N_UP: bool = cfg!(not(target_os = "macos"));

/// ヘッダーとフッター（日付・題名・URL・ページ番号）を付けられるか（WebKitGTK はできない）。
pub const HEADER_FOOTER: bool = cfg!(not(target_os = "linux"));

/// 倍率に書ける範囲（%）。
pub const SCALE: std::ops::RangeInclusive<u16> = 10..=200;

/// カスタムの余白に書ける上限（mm）。
pub const MAX_MARGIN_MM: f32 = 100.;

/// 用紙サイズ。B4・B5 は日本で使う JIS の大きさ。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Paper {
    A3,
    #[default]
    A4,
    A5,
    B4,
    B5,
    Letter,
    Legal,
    Tabloid,
}

impl Paper {
    pub const ALL: [Paper; 8] = [
        Paper::A3,
        Paper::A4,
        Paper::A5,
        Paper::B4,
        Paper::B5,
        Paper::Letter,
        Paper::Legal,
        Paper::Tabloid,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Paper::A3 => "A3",
            Paper::A4 => "A4",
            Paper::A5 => "A5",
            Paper::B4 => "B4",
            Paper::B5 => "B5",
            Paper::Letter => "レター",
            Paper::Legal => "リーガル",
            Paper::Tabloid => "タブロイド",
        }
    }

    /// 縦に置いたときの幅と高さ（mm）。
    pub fn mm(self) -> (f32, f32) {
        match self {
            Paper::A3 => (297., 420.),
            Paper::A4 => (210., 297.),
            Paper::A5 => (148., 210.),
            Paper::B4 => (257., 364.),
            Paper::B5 => (182., 257.),
            Paper::Letter => (215.9, 279.4),
            Paper::Legal => (215.9, 355.6),
            Paper::Tabloid => (279.4, 431.8),
        }
    }

    /// GTK（PWG 5101.1）の用紙の名前。
    pub fn pwg(self) -> &'static str {
        match self {
            Paper::A3 => "iso_a3",
            Paper::A4 => "iso_a4",
            Paper::A5 => "iso_a5",
            Paper::B4 => "jis_b4",
            Paper::B5 => "jis_b5",
            Paper::Letter => "na_letter",
            Paper::Legal => "na_legal",
            Paper::Tabloid => "na_ledger",
        }
    }
}

/// 余白の選び方。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Margins {
    #[default]
    Default,
    None,
    Minimum,
    Custom,
}

impl Margins {
    pub const ALL: [Margins; 4] = [
        Margins::Default,
        Margins::None,
        Margins::Minimum,
        Margins::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Margins::Default => "標準",
            Margins::None => "なし",
            Margins::Minimum => "最小",
            Margins::Custom => "カスタム",
        }
    }
}

/// 上・右・下・左の余白（mm）。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Edges {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Edges {
    pub const fn all(mm: f32) -> Self {
        Edges {
            top: mm,
            right: mm,
            bottom: mm,
            left: mm,
        }
    }

    pub fn get(&self, side: Side) -> f32 {
        match side {
            Side::Top => self.top,
            Side::Right => self.right,
            Side::Bottom => self.bottom,
            Side::Left => self.left,
        }
    }

    pub fn set(&mut self, side: Side, mm: f32) {
        match side {
            Side::Top => self.top = mm,
            Side::Right => self.right = mm,
            Side::Bottom => self.bottom = mm,
            Side::Left => self.left = mm,
        }
    }
}

/// 余白の辺。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

impl Side {
    pub const ALL: [Side; 4] = [Side::Top, Side::Right, Side::Bottom, Side::Left];

    /// テストで探す名前。
    pub fn key(self) -> &'static str {
        match self {
            Side::Top => "top",
            Side::Right => "right",
            Side::Bottom => "bottom",
            Side::Left => "left",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Side::Top => "上",
            Side::Right => "右",
            Side::Bottom => "下",
            Side::Left => "左",
        }
    }
}

/// 標準の余白（Chrome の標準と同じ 約 1 cm）。
pub const DEFAULT_MARGIN_MM: f32 = 10.;
/// 最小の余白。
pub const MINIMUM_MARGIN_MM: f32 = 4.;

/// 印刷するページの選び方。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pages {
    #[default]
    All,
    Odd,
    Even,
    Custom,
}

impl Pages {
    pub const ALL: [Pages; 4] = [Pages::All, Pages::Odd, Pages::Even, Pages::Custom];

    pub fn label(self) -> &'static str {
        match self {
            Pages::All => "すべて",
            Pages::Odd => "奇数ページのみ",
            Pages::Even => "偶数ページのみ",
            Pages::Custom => "カスタム",
        }
    }
}

/// 印刷の設定。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrintSetup {
    pub paper: Paper,
    pub landscape: bool,
    pub margins: Margins,
    /// 余白がカスタムのときの値。
    pub custom_margins: Edges,
    /// 倍率（%）。`None` は標準（100%）。
    pub scale: Option<u16>,
    pub pages: Pages,
    /// ページがカスタムのときの範囲（`1-5, 8, 11-13`）。
    pub ranges: String,
    /// 1 枚あたりのページ数。
    pub per_sheet: u8,
    pub header_footer: bool,
    pub backgrounds: bool,
    /// 詳細設定を開いている。
    pub more: bool,
}

impl Default for PrintSetup {
    fn default() -> Self {
        PrintSetup {
            paper: Paper::default(),
            landscape: false,
            margins: Margins::default(),
            custom_margins: Edges::all(DEFAULT_MARGIN_MM),
            scale: None,
            pages: Pages::default(),
            ranges: String::new(),
            per_sheet: 1,
            header_footer: false,
            backgrounds: false,
            more: false,
        }
    }
}

impl PrintSetup {
    /// 用紙の幅と高さ（mm）。横なら入れ替える。
    pub fn paper_mm(&self) -> (f32, f32) {
        let (w, h) = self.paper.mm();
        if self.landscape {
            (h, w)
        } else {
            (w, h)
        }
    }

    /// 余白（mm）。
    pub fn margins_mm(&self) -> Edges {
        match self.margins {
            Margins::Default => Edges::all(DEFAULT_MARGIN_MM),
            Margins::None => Edges::all(0.),
            Margins::Minimum => Edges::all(MINIMUM_MARGIN_MM),
            Margins::Custom => self.custom_margins,
        }
    }

    /// 倍率（1.0 が 100%）。
    pub fn scale_factor(&self) -> f64 {
        f64::from(self.scale.unwrap_or(100)) / 100.
    }

    /// 1 枚あたりのページ数。選べない数なら 1。macOS の部品は並べられないので、いつも 1。
    pub fn per_sheet(&self) -> u8 {
        if N_UP && PER_SHEET.contains(&self.per_sheet) {
            self.per_sheet
        } else {
            1
        }
    }

    /// 保存できる設定か。できなければ、何がおかしいかを返す。
    pub fn check(&self) -> Result<(), String> {
        if self.pages == Pages::Custom {
            parse_ranges(&self.ranges)?;
        }
        if let Some(scale) = self.scale {
            if !SCALE.contains(&scale) {
                return Err(format!(
                    "倍率は {}〜{}% で書いてください",
                    SCALE.start(),
                    SCALE.end()
                ));
            }
        }
        if self.margins == Margins::Custom {
            let (w, h) = self.paper_mm();
            let m = self.custom_margins;
            let sides = [m.top, m.right, m.bottom, m.left];
            if sides
                .iter()
                .any(|mm| !mm.is_finite() || *mm < 0. || *mm > MAX_MARGIN_MM)
            {
                return Err(format!("余白は 0〜{MAX_MARGIN_MM} mm で書いてください"));
            }
            if m.left + m.right >= w || m.top + m.bottom >= h {
                return Err("余白が用紙より大きくなっています".to_owned());
            }
        }
        Ok(())
    }

    /// 印刷するページ（1 から数える）。`count` はページの数。
    /// 範囲がページの数を超えたら、何ページまであるかを書いて失敗にする。
    pub fn selected(&self, count: u32) -> Result<Vec<u32>, String> {
        let pages: Vec<u32> = match self.pages {
            Pages::All => (1..=count).collect(),
            Pages::Odd => (1..=count).step_by(2).collect(),
            Pages::Even => (2..=count).step_by(2).collect(),
            Pages::Custom => {
                let mut pages = Vec::new();
                for (first, last) in parse_ranges(&self.ranges)? {
                    let last = last.unwrap_or(count);
                    if first > count || last > count {
                        return Err(format!(
                            "ページの範囲「{}」が、ページの数（{count} ページ）を超えています",
                            self.ranges.trim()
                        ));
                    }
                    pages.extend(first..=last);
                }
                pages
            }
        };
        if pages.is_empty() {
            return Err("印刷するページがありません".to_owned());
        }
        Ok(pages)
    }

    /// すべてのページを印刷するか。
    pub fn every_page(&self) -> bool {
        self.pages == Pages::All
    }
}

/// `1-5, 8, 11-13` のようなページの範囲を読む。`3-` は 3 ページから最後まで。
/// 区切りは `,`・`、`・空白のどれでもよい。
pub fn parse_ranges(text: &str) -> Result<Vec<(u32, Option<u32>)>, String> {
    if text.trim().is_empty() {
        return Err("ページの範囲を書いてください（例: 1-5, 8, 11-13）".to_owned());
    }
    let wrong = || {
        format!(
            "ページの範囲「{}」を読めません（例: 1-5, 8, 11-13）",
            text.trim()
        )
    };
    let normalized: String = text
        .chars()
        .map(|c| match c {
            // 全角の数字と記号も読む
            '０'..='９' => char::from_u32(c as u32 - '０' as u32 + '0' as u32).unwrap_or(c),
            '－' | 'ー' | '〜' | '~' | '–' | '—' => '-',
            '、' | '，' => ',',
            c => c,
        })
        .collect();
    let mut ranges = Vec::new();
    for part in normalized
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
    {
        let number = |s: &str| s.trim().parse::<u32>().ok().filter(|n| *n > 0);
        let range = match part.split_once('-') {
            None => {
                let n = number(part).ok_or_else(wrong)?;
                (n, Some(n))
            }
            Some((first, "")) => (number(first).ok_or_else(wrong)?, None),
            Some((first, last)) => {
                let (first, last) = (
                    number(first).ok_or_else(wrong)?,
                    number(last).ok_or_else(wrong)?,
                );
                if last < first {
                    return Err(wrong());
                }
                (first, Some(last))
            }
        };
        ranges.push(range);
    }
    if ranges.is_empty() {
        return Err(wrong());
    }
    Ok(ranges)
}

/// ページの番号の並びを、`1-3,5` の形にする（WebView2 の PageRanges）。
pub fn ranges_text(pages: &[u32]) -> String {
    let mut parts = Vec::new();
    let mut i = 0;
    while i < pages.len() {
        let first = pages[i];
        let mut last = first;
        while i + 1 < pages.len() && pages[i + 1] == last + 1 {
            i += 1;
            last = pages[i];
        }
        parts.push(if first == last {
            first.to_string()
        } else {
            format!("{first}-{last}")
        });
        i += 1;
    }
    parts.join(",")
}

/// PDF のページの数。
pub fn page_count(path: &Path) -> Result<u32, String> {
    let doc =
        lopdf::Document::load(path).map_err(|e| format!("{} を読めません: {e}", path.display()))?;
    Ok(doc.get_pages().len() as u32)
}

/// 保存した PDF を、設定に合わせて仕上げる。
/// - `select` なら、選んだページだけを残す（OS の部品がページを選べないとき）
/// - フォントを埋め込んでいるかを確かめる。埋め込んでいないフォントがあれば、ファイルを消して失敗にする
pub fn finish(path: &Path, setup: &PrintSetup, select: bool) -> Result<(), String> {
    let result = (|| {
        let mut doc =
            lopdf::Document::load(path).map_err(|e| format!("保存した PDF を読めません: {e}"))?;
        let missing = unembedded_fonts(&doc);
        if !missing.is_empty() {
            return Err(format!(
                "PDF にフォントを埋め込めませんでした: {}",
                missing.join("、")
            ));
        }
        if select && !setup.every_page() {
            let count = doc.get_pages().len() as u32;
            let keep = setup.selected(count)?;
            let drop: Vec<u32> = (1..=count).filter(|p| !keep.contains(p)).collect();
            if !drop.is_empty() {
                doc.delete_pages(&drop);
                doc.prune_objects();
                doc.save(path)
                    .map_err(|e| format!("PDF を書き込めませんでした: {e}"))?;
            }
        }
        Ok(())
    })();
    if let Err(failed) = &result {
        // 中途半端な PDF は残さない
        if let Err(e) = std::fs::remove_file(path) {
            return Err(format!(
                "{failed}（{} を消せませんでした: {e}）",
                path.display()
            ));
        }
    }
    result
}

/// ページを数えるためだけに作った PDF を消す。
/// すぐあとで同じ場所に作り直すか、失敗にするので、消せなくても困らない
pub fn discard(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// 埋め込んでいないフォントの名前。Type3 は形を PDF の中に持つので、埋め込みと同じ。
pub fn unembedded_fonts(doc: &lopdf::Document) -> Vec<String> {
    use lopdf::Object;

    let resolve = |object: &Object| -> Option<Object> {
        match object {
            Object::Reference(id) => doc.get_object(*id).ok().cloned(),
            other => Some(other.clone()),
        }
    };
    let dict_of = |object: &Object| resolve(object).and_then(|o| o.as_dict().ok().cloned());
    let embedded = |font: &lopdf::Dictionary| {
        font.get(b"FontDescriptor")
            .ok()
            .and_then(dict_of)
            .is_some_and(|d| d.has(b"FontFile") || d.has(b"FontFile2") || d.has(b"FontFile3"))
    };
    let mut missing = Vec::new();
    for object in doc.objects.values() {
        let Ok(font) = object.as_dict() else {
            continue;
        };
        if !font
            .get(b"Type")
            .and_then(|t| t.as_name())
            .is_ok_and(|t| t == b"Font")
        {
            continue;
        }
        let subtype = font
            .get(b"Subtype")
            .and_then(|t| t.as_name())
            .unwrap_or(b"");
        let ok = match subtype {
            b"Type3" => true,
            // CID フォントは、下にあるフォント（DescendantFonts）を見る
            b"Type0" => font
                .get(b"DescendantFonts")
                .ok()
                .and_then(resolve)
                .and_then(|a| a.as_array().ok().cloned())
                .is_some_and(|fonts| {
                    !fonts.is_empty()
                        && fonts
                            .iter()
                            .all(|f| dict_of(f).is_some_and(|d| embedded(&d)))
                }),
            // CIDFontType0/2 は Type0 から見る
            b"CIDFontType0" | b"CIDFontType2" => true,
            _ => embedded(font),
        };
        if !ok {
            let name = font
                .get(b"BaseFont")
                .and_then(|n| n.as_name())
                .map(|n| String::from_utf8_lossy(n).into_owned())
                .unwrap_or_else(|_| "名前のないフォント".to_owned());
            if !missing.contains(&name) {
                missing.push(name);
            }
        }
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `pages` ページの PDF を書く。`embedded` でなければ、埋め込んでいない Helvetica を使う。
    fn write_pdf(path: &std::path::Path, pages: usize, embedded: bool) {
        use lopdf::{dictionary, Document, Object, Stream};
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let mut font = dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => "Helvetica",
        };
        if embedded {
            let file = doc.add_object(Stream::new(dictionary! {}, b"font".to_vec()));
            let descriptor = doc.add_object(dictionary! {
                "Type" => "FontDescriptor",
                "FontName" => "Helvetica",
                "FontFile2" => file,
            });
            font.set("FontDescriptor", descriptor);
        }
        let font = doc.add_object(font);
        let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
        let kids: Vec<Object> = (0..pages)
            .map(|i| {
                let content = doc.add_object(Stream::new(
                    dictionary! {},
                    format!("BT /F1 12 Tf 72 720 Td ({}) Tj ET", i + 1).into_bytes(),
                ));
                doc.add_object(dictionary! {
                    "Type" => "Page",
                    "Parent" => pages_id,
                    "Contents" => content,
                })
                .into()
            })
            .collect();
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => kids,
                "Count" => pages as i64,
                "Resources" => resources,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            }),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        doc.save(path).unwrap();
    }

    #[test]
    fn a_pdf_without_embedded_fonts_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.pdf");
        write_pdf(&path, 1, false);
        let e = finish(&path, &PrintSetup::default(), false).unwrap_err();
        assert!(e.contains("Helvetica"), "{e}");
        assert!(!path.exists(), "中途半端な PDF は残さない");
        write_pdf(&path, 1, true);
        finish(&path, &PrintSetup::default(), false).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn pages_are_chosen_after_saving() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.pdf");
        write_pdf(&path, 5, true);
        let setup = PrintSetup {
            pages: Pages::Even,
            ..Default::default()
        };
        finish(&path, &setup, true).unwrap();
        assert_eq!(page_count(&path).unwrap(), 2);
        // ページの数を超えた範囲は失敗にして、ファイルを残さない
        write_pdf(&path, 2, true);
        let setup = PrintSetup {
            pages: Pages::Custom,
            ranges: "1-4".into(),
            ..Default::default()
        };
        assert!(finish(&path, &setup, true).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn page_ranges_read_like_chrome() {
        assert_eq!(
            parse_ranges("1-5, 8, 11-13").unwrap(),
            vec![(1, Some(5)), (8, Some(8)), (11, Some(13))]
        );
        assert_eq!(parse_ranges("３－").unwrap(), vec![(3, None)]);
        assert_eq!(
            parse_ranges("2、4").unwrap(),
            vec![(2, Some(2)), (4, Some(4))]
        );
        assert!(parse_ranges("").is_err());
        assert!(parse_ranges("0").is_err());
        assert!(parse_ranges("5-2").is_err());
        assert!(parse_ranges("a").is_err());
    }

    #[test]
    fn the_chosen_pages_follow_the_count() {
        let mut setup = PrintSetup::default();
        assert_eq!(setup.selected(3).unwrap(), vec![1, 2, 3]);
        setup.pages = Pages::Odd;
        assert_eq!(setup.selected(5).unwrap(), vec![1, 3, 5]);
        setup.pages = Pages::Even;
        assert_eq!(setup.selected(5).unwrap(), vec![2, 4]);
        assert!(setup.selected(1).is_err(), "偶数ページがない");
        setup.pages = Pages::Custom;
        setup.ranges = "2-".into();
        assert_eq!(setup.selected(4).unwrap(), vec![2, 3, 4]);
        setup.ranges = "1-9".into();
        let error = setup.selected(4).unwrap_err();
        assert!(error.contains("4 ページ"), "{error}");
    }

    #[test]
    fn ranges_are_written_back_compactly() {
        assert_eq!(ranges_text(&[1, 2, 3, 5, 7, 8]), "1-3,5,7-8");
        assert_eq!(ranges_text(&[1, 3, 5]), "1,3,5");
    }

    #[test]
    fn settings_out_of_range_are_refused() {
        let mut setup = PrintSetup {
            scale: Some(5),
            ..PrintSetup::default()
        };
        assert!(setup.check().is_err());
        setup.scale = Some(150);
        assert!(setup.check().is_ok());
        setup.margins = Margins::Custom;
        setup.custom_margins = Edges::all(110.);
        assert!(setup.check().is_err());
        setup.custom_margins = Edges::all(5.);
        assert!(setup.check().is_ok());
        setup.landscape = true;
        assert_eq!(setup.paper_mm(), (297., 210.));
    }

    #[test]
    fn old_or_partial_settings_still_load() {
        let setup: PrintSetup =
            serde_json::from_str(r#"{"paper":"B5","backgrounds":true}"#).unwrap();
        assert_eq!(setup.paper, Paper::B5);
        assert!(setup.backgrounds);
        assert_eq!(setup.per_sheet(), 1);
    }
}
