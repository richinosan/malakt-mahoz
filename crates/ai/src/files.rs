//! チャットに添えるファイル。
//!
//! 種類は拡張子ではなく中身で見分ける。AI が直接読める形（画像と PDF）はそのまま渡し、
//! テキストと Office の文書（docx・xlsx・pptx・odt など）は文字を取り出して渡す。
//! それ以外（音声・動画・圧縮ファイルなど）は、名前と大きさと置いた場所を渡し、
//! AI がシェルのコマンドで読めるようにする。
//!
//! 中身は [`Shelf`] が、決めた場所に内容のハッシュごとに 1 つだけ置く。合計の大きさに
//! 上限があり、どの会話からも使われなくなったものと、上限を超えた古いものから消す。

use std::collections::HashSet;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};

/// 中身を写して置くファイルの大きさの上限。これより大きいファイルは写さず、元の場所を AI に伝える。
pub const MAX_COPY_BYTES: u64 = 20 * 1024 * 1024;
/// 置いておく中身の合計の上限。超えたら古いものから消す。
pub const MAX_SHELF_BYTES: u64 = 200 * 1024 * 1024;
/// 1 回のリクエストに入れる、画像と PDF の中身の合計の上限。
/// Claude の PDF を含むリクエストの上限（32 MB）に、base64 で膨らむ分の余裕を見る。
pub const MAX_INLINE_BYTES: usize = 20 * 1024 * 1024;
/// 1 回のリクエストに入れる、画像と PDF の数の上限。
pub const MAX_INLINE_PARTS: usize = 50;
/// 文字にしたファイルを AI に渡す長さの上限（文字数）。
pub const MAX_TEXT_CHARS: usize = 100_000;
/// 画像の長い辺の上限。Claude の高解像度の上限（2,576 px）に合わせる。
pub const IMAGE_EDGE: u32 = 2576;
/// 画像 1 枚の大きさの上限。base64 にして 5 MB（Bedrock などの上限）に収まる大きさ。
pub const IMAGE_BYTES: usize = 3_700_000;

/// ファイルの種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// AI に画像として渡す（PNG・JPEG・GIF・WebP）
    Image,
    /// AI に PDF として渡す
    Pdf,
    /// 文字にして渡す（テキスト、Office の文書）
    Text,
    /// 中身は渡さず、名前と場所を伝える
    Other,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Pdf => "pdf",
            Kind::Text => "text",
            Kind::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "image" => Kind::Image,
            "pdf" => Kind::Pdf,
            "text" => Kind::Text,
            _ => Kind::Other,
        }
    }
}

/// 添えたファイル 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePart {
    pub name: String,
    pub mime: String,
    pub kind: Kind,
    /// 元のファイルの大きさ。
    pub size: u64,
    /// 置いた中身のハッシュ。写さなかった大きいファイルは空。
    pub hash: String,
    /// AI がコマンドで読める場所（置いた場所か、写さなかったファイルの元の場所）。
    pub path: PathBuf,
    /// 画像と PDF の中身。
    pub data: Option<Arc<Vec<u8>>>,
    /// 文字にした中身。
    pub text: Option<Arc<str>>,
    /// AI に伝える補足（長くて途中まで、読めなくなった、など）。
    pub note: Option<String>,
}

impl FilePart {
    /// 会話を保存するときの形。中身は入れない。
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "mime": self.mime,
            "kind": self.kind.as_str(),
            "size": self.size,
            "hash": self.hash,
            "path": self.path.to_string_lossy(),
        })
    }

    /// 画像として描けるか。
    pub fn is_image(&self) -> bool {
        self.kind == Kind::Image
    }
}

/// 読み取った中身。
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub kind: Kind,
    pub mime: String,
    /// 置く中身。画像は小さくしたもの、ほかは元のまま。
    pub bytes: Vec<u8>,
    pub text: Option<String>,
    pub note: Option<String>,
}

// ---- 種類の見分け ----

fn ext_of(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default()
}

/// 拡張子からの MIME タイプ。中身で見分けられなかったときに使う。
pub fn mime_of(name: &str) -> &'static str {
    match ext_of(name).as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "ico" => "image/x-icon",
        "heic" | "heif" => "image/heic",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "doc" => "application/msword",
        "xls" => "application/vnd.ms-excel",
        "ppt" => "application/vnd.ms-powerpoint",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "rtf" => "application/rtf",
        "csv" => "text/csv",
        "md" | "markdown" => "text/markdown",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "xml" => "application/xml",
        "txt" | "log" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// 頭のバイトから見分けた画像の形式。
fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"BM") && bytes.len() > 26 {
        Some("image/bmp")
    } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        Some("image/tiff")
    } else if bytes.starts_with(&[0, 0, 1, 0]) && bytes.len() > 22 {
        Some("image/x-icon")
    } else {
        None
    }
}

/// HEIC・AVIF（このアプリでは画像として開けない）。
fn heif_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        return None;
    }
    match &bytes[8..12] {
        b"heic" | b"heix" | b"heim" | b"heis" | b"mif1" | b"msf1" => Some("image/heic"),
        b"avif" | b"avis" => Some("image/avif"),
        _ => None,
    }
}

/// AI がそのまま読める画像の形式か。
fn direct_image(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

/// 中身を読み取り、AI に渡せる形にする。
pub fn prepare(name: &str, bytes: Vec<u8>) -> Result<Prepared, String> {
    if let Some(mime) = image_mime(&bytes) {
        return prepare_image(mime, bytes);
    }
    if let Some(mime) = heif_mime(&bytes) {
        return Ok(other(mime, bytes));
    }
    if bytes.starts_with(b"%PDF-") {
        return Ok(Prepared {
            kind: Kind::Pdf,
            mime: "application/pdf".into(),
            bytes,
            text: None,
            note: None,
        });
    }
    if bytes.starts_with(b"PK\x03\x04") {
        return Ok(prepare_zip(name, bytes));
    }
    if let Some(text) = decode_text(&bytes) {
        let mime = match mime_of(name) {
            m if m.starts_with("text/")
                || m == "application/json"
                || m == "application/xml"
                || m == "image/svg+xml"
                || m == "application/rtf" =>
            {
                m
            }
            _ => "text/plain",
        };
        let (text, note) = clip_text(text);
        return Ok(Prepared {
            kind: Kind::Text,
            mime: mime.into(),
            bytes,
            text: Some(text),
            note,
        });
    }
    Ok(other(mime_of(name), bytes))
}

fn other(mime: &str, bytes: Vec<u8>) -> Prepared {
    Prepared {
        kind: Kind::Other,
        mime: mime.into(),
        bytes,
        text: None,
        note: None,
    }
}

/// 長い文字は先頭から上限まで。省いたときは、そのことを補足にする。
fn clip_text(text: String) -> (String, Option<String>) {
    let count = text.chars().count();
    if count <= MAX_TEXT_CHARS {
        return (text, None);
    }
    let head: String = text.chars().take(MAX_TEXT_CHARS).collect();
    (
        head,
        Some(format!(
            "全体は {count} 文字あり、先頭の {MAX_TEXT_CHARS} 文字だけを渡しています。続きはパスのファイルを読んでください"
        )),
    )
}

/// 文字として読めれば、その文字。UTF-8（BOM 付きも）、UTF-16（BOM 付き）、Shift_JIS、EUC-JP を試す。
pub fn decode_text(bytes: &[u8]) -> Option<String> {
    if let Some((encoding, skip)) = encoding_rs::Encoding::for_bom(bytes) {
        let (text, had_errors) = encoding.decode_without_bom_handling(&bytes[skip..]);
        return (!had_errors).then(|| text.into_owned());
    }
    // 先頭に NUL があるものは、文字ではないファイルとして扱う
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return None;
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return looks_like_text(text).then(|| text.to_owned());
    }
    for encoding in [encoding_rs::SHIFT_JIS, encoding_rs::EUC_JP] {
        if let Some(text) = encoding.decode_without_bom_handling_and_without_replacement(bytes) {
            if looks_like_text(&text) {
                return Some(text.into_owned());
            }
        }
    }
    None
}

/// 制御文字ばかりのものは、文字のファイルとして扱わない。
fn looks_like_text(text: &str) -> bool {
    let control = text
        .chars()
        .take(8192)
        .filter(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\u{c}'))
        .count();
    control * 100 <= text.chars().take(8192).count().max(1)
}

// ---- 画像 ----

/// 画像を AI に渡せる形にする。そのまま渡せる形式で大きすぎなければ、元のまま。
/// ほかは長い辺を [`IMAGE_EDGE`] までに縮め、PNG（透明があるとき）か JPEG にする。
fn prepare_image(mime: &'static str, bytes: Vec<u8>) -> Result<Prepared, String> {
    let fail = |e: image::ImageError| format!("画像として読めませんでした: {e}");
    let (width, height) = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|e| format!("画像として読めませんでした: {e}"))?
        .into_dimensions()
        .map_err(fail)?;
    let fits = width <= IMAGE_EDGE && height <= IMAGE_EDGE && bytes.len() <= IMAGE_BYTES;
    if direct_image(mime) && fits {
        return Ok(Prepared {
            kind: Kind::Image,
            mime: mime.into(),
            bytes,
            text: None,
            note: None,
        });
    }
    let mut picture = image::load_from_memory(&bytes).map_err(fail)?;
    let mut edge = IMAGE_EDGE;
    loop {
        if picture.width() > edge || picture.height() > edge {
            picture = picture.resize(edge, edge, image::imageops::FilterType::Triangle);
        }
        let (out_mime, out) = encode(&picture).map_err(fail)?;
        if out.len() <= IMAGE_BYTES || edge <= 256 {
            let note = (picture.width() != width || picture.height() != height).then(|| {
                format!(
                    "元の画像は {width}×{height} px で、{}×{} px に縮めて渡しています。元の大きさが要るときは元のファイルを使ってください",
                    picture.width(),
                    picture.height()
                )
            });
            return Ok(Prepared {
                kind: Kind::Image,
                mime: out_mime.into(),
                bytes: out,
                text: None,
                note,
            });
        }
        edge = edge * 3 / 4;
    }
}

/// 透明があれば PNG、なければ JPEG にする。PNG が大きすぎれば、白の上に置いた JPEG にする。
fn encode(picture: &image::DynamicImage) -> Result<(&'static str, Vec<u8>), image::ImageError> {
    if picture.color().has_alpha() {
        let mut png = Vec::new();
        picture.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)?;
        if png.len() <= IMAGE_BYTES {
            return Ok(("image/png", png));
        }
    }
    let mut rgba = picture.to_rgba8();
    for pixel in rgba.pixels_mut() {
        let a = pixel[3] as u32;
        for c in 0..3 {
            pixel[c] = ((pixel[c] as u32 * a + 255 * (255 - a)) / 255) as u8;
        }
    }
    let rgb = image::DynamicImage::ImageRgba8(rgba).to_rgb8();
    let mut jpeg = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85);
    rgb.write_with_encoder(encoder)?;
    Ok(("image/jpeg", jpeg))
}

// ---- Office の文書と zip ----

/// zip の形の文書（docx・xlsx・pptx・ODF）なら文字を取り出す。ほかの zip は、入っているファイルの一覧を渡す。
fn prepare_zip(name: &str, bytes: Vec<u8>) -> Prepared {
    let mime = mime_of(name);
    let Ok(mut zip) = zip::ZipArchive::new(Cursor::new(bytes.as_slice())) else {
        return other(mime, bytes);
    };
    let names: Vec<String> = zip.file_names().map(str::to_owned).collect();
    let has = |n: &str| names.iter().any(|x| x == n);
    let extracted = if has("word/document.xml") {
        Some((
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            docx_text(&mut zip),
        ))
    } else if has("xl/workbook.xml") {
        Some((
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            xlsx_text(&mut zip, &names),
        ))
    } else if names.iter().any(|n| n.starts_with("ppt/slides/slide")) {
        Some((
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            pptx_text(&mut zip, &names),
        ))
    } else if has("content.xml") && has("mimetype") {
        let mime = read_entry(&mut zip, "mimetype")
            .map(|m| String::from_utf8_lossy(&m).trim().to_owned())
            .unwrap_or_default();
        let mime: &'static str = match mime.as_str() {
            "application/vnd.oasis.opendocument.text" => "application/vnd.oasis.opendocument.text",
            "application/vnd.oasis.opendocument.spreadsheet" => {
                "application/vnd.oasis.opendocument.spreadsheet"
            }
            "application/vnd.oasis.opendocument.presentation" => {
                "application/vnd.oasis.opendocument.presentation"
            }
            _ => "application/zip",
        };
        Some((mime, odf_text(&mut zip)))
    } else {
        None
    };
    match extracted {
        Some((mime, Ok(text))) => {
            let (text, note) = clip_text(text);
            Prepared {
                kind: Kind::Text,
                mime: mime.into(),
                bytes,
                text: Some(text),
                note,
            }
        }
        Some((mime, Err(error))) => Prepared {
            note: Some(format!("文書から文字を取り出せませんでした: {error}")),
            ..other(mime, bytes)
        },
        None => {
            const MAX_NAMES: usize = 200;
            let mut list = names
                .iter()
                .take(MAX_NAMES)
                .map(|n| format!("- {n}"))
                .collect::<Vec<_>>()
                .join("\n");
            if names.len() > MAX_NAMES {
                list.push_str(&format!("\n（ほかに {} 件）", names.len() - MAX_NAMES));
            }
            Prepared {
                note: Some(format!(
                    "zip に入っているファイル（{} 件）:\n{list}",
                    names.len()
                )),
                ..other(mime, bytes)
            }
        }
    }
}

type Zip<'a> = zip::ZipArchive<Cursor<&'a [u8]>>;

/// 取り出す 1 ファイルの大きさの上限。展開すると膨らむ zip で、メモリを使い切らない。
const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;

fn read_entry(zip: &mut Zip, name: &str) -> Result<Vec<u8>, String> {
    let entry = zip.by_name(name).map_err(|e| format!("{name}: {e}"))?;
    let mut out = Vec::new();
    entry
        .take(MAX_ENTRY_BYTES)
        .read_to_end(&mut out)
        .map_err(|e| format!("{name}: {e}"))?;
    Ok(out)
}

fn read_xml(zip: &mut Zip, name: &str) -> Result<String, String> {
    let bytes = read_entry(zip, name)?;
    String::from_utf8(bytes).map_err(|e| format!("{name}: {e}"))
}

/// XML の木をたどり、文字の要素と区切りの要素から文字を組み立てる。
/// `text` の要素の中身を足し、`breaks` の要素の終わりで改行、`tabs` の要素で tab を足す。
fn xml_text(xml: &str, text: &[&str], breaks: &[&str], tabs: &[&str]) -> Result<String, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| e.to_string())?;
    let mut out = String::new();
    walk(doc.root(), text, breaks, tabs, &mut out);
    Ok(out.trim().to_owned())
}

fn walk(node: roxmltree::Node, text: &[&str], breaks: &[&str], tabs: &[&str], out: &mut String) {
    let name = node.tag_name().name();
    if node.is_element() && text.contains(&name) {
        out.push_str(node.text().unwrap_or_default());
    }
    if node.is_element() && tabs.contains(&name) {
        out.push('\t');
    }
    for child in node.children() {
        walk(child, text, breaks, tabs, out);
    }
    if node.is_element() && breaks.contains(&name) {
        out.push('\n');
    }
}

fn docx_text(zip: &mut Zip) -> Result<String, String> {
    let xml = read_xml(zip, "word/document.xml")?;
    xml_text(&xml, &["t"], &["p", "br"], &["tab"])
}

/// `ppt/slides/slide12.xml` の 12。並べ替えに使う。
fn number_in(name: &str, prefix: &str) -> Option<u32> {
    name.strip_prefix(prefix)?
        .strip_suffix(".xml")?
        .parse()
        .ok()
}

fn pptx_text(zip: &mut Zip, names: &[String]) -> Result<String, String> {
    let mut slides: Vec<(u32, &String)> = names
        .iter()
        .filter_map(|n| number_in(n, "ppt/slides/slide").map(|k| (k, n)))
        .collect();
    slides.sort();
    let mut out = String::new();
    for (k, name) in slides {
        let xml = read_xml(zip, name)?;
        out.push_str(&format!("## スライド {k}\n"));
        out.push_str(&xml_text(&xml, &["t"], &["p"], &[])?);
        out.push_str("\n\n");
    }
    Ok(out.trim().to_owned())
}

fn xlsx_text(zip: &mut Zip, names: &[String]) -> Result<String, String> {
    let shared: Vec<String> = if names.iter().any(|n| n == "xl/sharedStrings.xml") {
        let xml = read_xml(zip, "xl/sharedStrings.xml")?;
        let doc = roxmltree::Document::parse(&xml).map_err(|e| e.to_string())?;
        doc.root_element()
            .children()
            .filter(|n| n.has_tag_name("si") || n.tag_name().name() == "si")
            .map(|si| {
                si.descendants()
                    .filter(|n| n.tag_name().name() == "t")
                    .filter_map(|n| n.text())
                    .collect()
            })
            .collect()
    } else {
        Vec::new()
    };
    let workbook = read_xml(zip, "xl/workbook.xml")?;
    let workbook = roxmltree::Document::parse(&workbook).map_err(|e| e.to_string())?;
    let titles: Vec<String> = workbook
        .descendants()
        .filter(|n| n.tag_name().name() == "sheet")
        .map(|n| n.attribute("name").unwrap_or_default().to_owned())
        .collect();
    let mut sheets: Vec<(u32, &String)> = names
        .iter()
        .filter_map(|n| number_in(n, "xl/worksheets/sheet").map(|k| (k, n)))
        .collect();
    sheets.sort();
    let mut out = String::new();
    for (ix, (k, name)) in sheets.into_iter().enumerate() {
        let xml = read_xml(zip, name)?;
        let doc = roxmltree::Document::parse(&xml).map_err(|e| e.to_string())?;
        let title = titles
            .get(ix)
            .cloned()
            .unwrap_or_else(|| format!("シート {k}"));
        out.push_str(&format!("## {title}\n"));
        for row in doc.descendants().filter(|n| n.tag_name().name() == "row") {
            let cells: Vec<String> = row
                .children()
                .filter(|n| n.tag_name().name() == "c")
                .map(|c| {
                    let value = c
                        .children()
                        .find(|n| n.tag_name().name() == "v")
                        .and_then(|v| v.text())
                        .unwrap_or_default();
                    match c.attribute("t") {
                        Some("s") => value
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| shared.get(i).cloned())
                            .unwrap_or_default(),
                        Some("inlineStr") => c
                            .descendants()
                            .filter(|n| n.tag_name().name() == "t")
                            .filter_map(|n| n.text())
                            .collect(),
                        _ => value.to_owned(),
                    }
                })
                .collect();
            if cells.iter().any(|c| !c.is_empty()) {
                out.push_str(&cells.join("\t"));
                out.push('\n');
            }
        }
        out.push('\n');
    }
    Ok(out.trim().to_owned())
}

fn odf_text(zip: &mut Zip) -> Result<String, String> {
    let xml = read_xml(zip, "content.xml")?;
    let doc = roxmltree::Document::parse(&xml).map_err(|e| e.to_string())?;
    let mut out = String::new();
    odf_walk(doc.root(), &mut out);
    Ok(out.trim().to_owned())
}

fn odf_walk(node: roxmltree::Node, out: &mut String) {
    if node.is_text() {
        out.push_str(node.text().unwrap_or_default());
        return;
    }
    let name = node.tag_name().name();
    match name {
        "tab" => out.push('\t'),
        "s" => out.push(' '),
        "line-break" => out.push('\n'),
        _ => {}
    }
    for child in node.children() {
        odf_walk(child, out);
    }
    match name {
        "p" | "h" => {
            // 表のセルの中の段落は、セルの区切りに任せる
            let in_cell = node
                .ancestors()
                .skip(1)
                .any(|a| a.tag_name().name() == "table-cell");
            if !in_cell {
                out.push('\n');
            }
        }
        "table-cell" => out.push('\t'),
        "table-row" => {
            if out.ends_with('\t') {
                out.pop();
            }
            out.push('\n');
        }
        _ => {}
    }
}

// ---- 置き場所 ----

/// 添えたファイルの中身を置く場所。
#[derive(Debug, Clone)]
pub struct Shelf {
    pub dir: PathBuf,
}

impl Shelf {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn folder(&self, hash: &str) -> PathBuf {
        self.dir.join(&hash[..hash.len().min(16)])
    }

    /// 中身を読み取って置く。同じ中身がもう置いてあれば、置き直さない。
    pub fn put(&self, name: &str, bytes: Vec<u8>) -> Result<FilePart, String> {
        let size = bytes.len() as u64;
        let name = malakt_core::attachment::clean_name(name);
        let prepared = prepare(&name, bytes)?;
        let hash = malakt_core::attachment::hash_of(&prepared.bytes);
        // 縮めた画像は、形式に合う拡張子にする
        let name = match prepared.mime.as_str() {
            "image/jpeg" if !matches!(ext_of(&name).as_str(), "jpg" | "jpeg") => {
                format!("{}.jpg", stem(&name))
            }
            "image/png" if ext_of(&name) != "png" => format!("{}.png", stem(&name)),
            _ => name,
        };
        let folder = self.folder(&hash);
        let path = folder.join(&name);
        if !path.exists() {
            std::fs::create_dir_all(&folder)
                .and_then(|()| std::fs::write(&path, &prepared.bytes))
                .map_err(|e| format!("{} に保存できませんでした: {e}", path.display()))?;
        }
        // 新しく使ったものとして、整理で最後まで残す
        touch(&folder);
        Ok(part_of(name, size, hash, path, prepared))
    }

    /// ファイルを添える。大きすぎるファイルは写さず、元の場所を伝える。
    pub fn put_path(&self, path: &Path) -> Result<FilePart, String> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        let meta =
            std::fs::metadata(path).map_err(|e| format!("「{name}」を読めませんでした: {e}"))?;
        if meta.is_dir() {
            return Err(format!("「{name}」はフォルダなので添えられません"));
        }
        if meta.len() <= MAX_COPY_BYTES {
            let bytes =
                std::fs::read(path).map_err(|e| format!("「{name}」を読めませんでした: {e}"))?;
            return self
                .put(&name, bytes)
                .map_err(|e| format!("「{name}」を添えられませんでした: {e}"));
        }
        // 頭だけ読んで種類を見分ける
        let mut head = vec![0; 16];
        let n = std::fs::File::open(path)
            .and_then(|mut f| f.read(&mut head))
            .map_err(|e| format!("「{name}」を読めませんでした: {e}"))?;
        head.truncate(n);
        let mime = image_mime(&head)
            .or_else(|| heif_mime(&head))
            .or_else(|| head.starts_with(b"%PDF-").then_some("application/pdf"))
            .unwrap_or_else(|| mime_of(&name));
        Ok(FilePart {
            name,
            mime: mime.into(),
            kind: Kind::Other,
            size: meta.len(),
            hash: String::new(),
            path: path.to_path_buf(),
            data: None,
            text: None,
            note: Some(format!(
                "{} MB を超えるので、中身は元の場所のまま渡しています",
                MAX_COPY_BYTES / 1024 / 1024
            )),
        })
    }

    /// 保存した会話の添えたファイルを、置いてある中身から戻す。消えていれば、そのことを補足にする。
    pub fn load(&self, v: &Value) -> FilePart {
        let text = |key: &str| v[key].as_str().unwrap_or_default().to_owned();
        let name = text("name");
        let hash = text("hash");
        let path = PathBuf::from(text("path"));
        let mut part = FilePart {
            name: name.clone(),
            mime: text("mime"),
            kind: Kind::parse(&text("kind")),
            size: v["size"].as_u64().unwrap_or(0),
            hash: hash.clone(),
            path: path.clone(),
            data: None,
            text: None,
            note: None,
        };
        if hash.is_empty() {
            // 写さなかった大きいファイル。元の場所を伝えるだけ
            if !path.exists() {
                part.note = Some("元の場所にファイルがなくなっています".into());
            }
            return part;
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(_) => {
                part.kind = Kind::Other;
                part.note = Some(
                    "保存の上限を超えたので、中身は整理で消えました。名前だけが残っています".into(),
                );
                return part;
            }
        };
        touch(&self.folder(&hash));
        match prepare(&name, bytes) {
            Ok(prepared) => {
                let size = part.size;
                part_of(name, size, hash, path, prepared)
            }
            Err(error) => {
                part.kind = Kind::Other;
                part.note = Some(error);
                part
            }
        }
    }

    /// 整理する。`keep` にないハッシュの中身を消し、残りが上限を超えていれば古いものから消す。
    /// `pinned` は今の入力欄と会話のもの。上限を超えていても消さない。
    pub fn sweep(&self, keep: &HashSet<String>, pinned: &HashSet<String>) -> Result<(), String> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("{} を読めませんでした: {e}", self.dir.display())),
        };
        let wanted = |set: &HashSet<String>, folder: &str| {
            set.iter().any(|h| h.len() >= 16 && h.starts_with(folder))
        };
        let mut errors = Vec::new();
        let mut alive: Vec<(std::time::SystemTime, u64, PathBuf, bool)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let folder = entry.file_name().to_string_lossy().into_owned();
            let pin = wanted(pinned, &folder);
            if !pin && !wanted(keep, &folder) {
                if let Err(e) = remove(&path) {
                    errors.push(format!("{}: {e}", path.display()));
                }
                continue;
            }
            let used = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            alive.push((used, dir_size(&path), path, pin));
        }
        let mut total: u64 = alive.iter().map(|a| a.1).sum();
        alive.sort_by_key(|a| a.0);
        for (_, size, path, pin) in alive {
            if total <= MAX_SHELF_BYTES {
                break;
            }
            if pin {
                continue;
            }
            match remove(&path) {
                Ok(()) => total -= size,
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "添えたファイルの古いものを消せませんでした: {}",
                errors.join(" / ")
            ))
        }
    }
}

fn stem(name: &str) -> &str {
    name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name)
}

fn part_of(name: String, size: u64, hash: String, path: PathBuf, prepared: Prepared) -> FilePart {
    let data = matches!(prepared.kind, Kind::Image | Kind::Pdf).then(|| Arc::new(prepared.bytes));
    FilePart {
        name,
        mime: prepared.mime,
        kind: prepared.kind,
        size,
        hash,
        path,
        data,
        text: prepared.text.map(Arc::from),
        note: prepared.note,
    }
}

fn remove(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

fn dir_size(path: &Path) -> u64 {
    match std::fs::read_dir(path) {
        Ok(entries) => entries
            .flatten()
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum(),
        Err(_) => std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
    }
}

/// フォルダの更新時刻を今にする（整理で最近使ったものとして残す）。
fn touch(folder: &Path) {
    // 時刻が付けられなくても、整理の順番が変わるだけで、中身はそのまま使える
    if let Ok(dir) = std::fs::File::open(folder) {
        let _ = dir.set_modified(std::time::SystemTime::now());
    }
}

// ---- AI への渡し方 ----

/// 大きさを読みやすく。
pub fn size_label(size: u64) -> String {
    const K: f64 = 1024.;
    let s = size as f64;
    if s < K {
        format!("{size} B")
    } else if s < K * K {
        format!("{:.1} KB", s / K)
    } else if s < K * K * K {
        format!("{:.1} MB", s / K / K)
    } else {
        format!("{:.1} GB", s / K / K / K)
    }
}

/// 中身をこのリクエストに入れるか（新しいものから、合計の上限まで）。
/// 戻り値は `turns` の順に、ファイルの順に並ぶ。
pub fn inline_flags<'a>(groups: impl DoubleEndedIterator<Item = &'a [FilePart]>) -> Vec<Vec<bool>> {
    let mut out: Vec<Vec<bool>> = Vec::new();
    let (mut bytes, mut parts) = (0usize, 0usize);
    for group in groups.rev() {
        let mut flags = vec![false; group.len()];
        for (ix, part) in group.iter().enumerate().rev() {
            let Some(data) = &part.data else {
                continue;
            };
            if parts < MAX_INLINE_PARTS && bytes + data.len() <= MAX_INLINE_BYTES {
                bytes += data.len();
                parts += 1;
                flags[ix] = true;
            }
        }
        out.push(flags);
    }
    out.reverse();
    out
}

/// AI に伝える、添えたファイルの説明。`inline` は中身をこのリクエストに入れたか。
/// `text_below` は、文字にした中身をこの説明の下に続けるか（入れられる文書の形がないとき）。
pub fn describe(parts: &[FilePart], inline: &[bool], text_below: bool) -> String {
    let mut out = String::from("[添えたファイル]\n");
    for (ix, part) in parts.iter().enumerate() {
        let sent = inline.get(ix).copied().unwrap_or(false);
        let how = match part.kind {
            Kind::Image if sent => "画像として添付しています",
            Kind::Pdf if sent => "PDF として添付しています",
            Kind::Image | Kind::Pdf => {
                "中身はこのリクエストに入っていません。要るときはパスのファイルを読んでください"
            }
            Kind::Text if text_below => "文字にした中身を下に続けています",
            Kind::Text => "文字にした中身を文書として添付しています",
            Kind::Other => {
                "この形式は直接は渡せません。中身が要るときは、シェルのコマンド（変換や文字起こしの道具など）でパスのファイルを読んでください"
            }
        };
        out.push_str(&format!(
            "- 「{}」（{}、{}）: {how}。パス: {}\n",
            part.name,
            part.mime,
            size_label(part.size),
            part.path.display()
        ));
        if let Some(note) = &part.note {
            out.push_str(&format!("  補足: {note}\n"));
        }
    }
    if text_below {
        for part in parts.iter().filter(|p| p.text.is_some()) {
            out.push_str(&format!(
                "\n[「{}」の中身]\n{}\n",
                part.name,
                part.text.as_deref().unwrap_or_default()
            ));
        }
    }
    out
}

/// base64 にする。
pub fn base64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// `data:` の URL にする。
pub fn data_url(mime: &str, data: &[u8]) -> String {
    format!("data:{mime};base64,{}", base64(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, alpha: bool) -> Vec<u8> {
        let picture = if alpha {
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                width,
                height,
                image::Rgba([10, 20, 30, 128]),
            ))
        } else {
            image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(width, height, |x, y| {
                image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x * y) % 256) as u8])
            }))
        };
        let mut out = Vec::new();
        picture
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        for (name, body) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut zip, body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        out.into_inner()
    }

    #[test]
    fn small_images_pass_through_unchanged() {
        let bytes = png(10, 10, false);
        let prepared = prepare("a.png", bytes.clone()).unwrap();
        assert_eq!(prepared.kind, Kind::Image);
        assert_eq!(prepared.mime, "image/png");
        assert_eq!(prepared.bytes, bytes);
        assert_eq!(prepared.note, None);
    }

    #[test]
    fn the_kind_comes_from_the_content_not_the_name() {
        let prepared = prepare("写真.txt", png(4, 4, false)).unwrap();
        assert_eq!(prepared.kind, Kind::Image);
        let prepared = prepare("memo.png", "ただのメモ".as_bytes().to_vec()).unwrap();
        assert_eq!(prepared.kind, Kind::Text);
        assert_eq!(prepared.text.as_deref(), Some("ただのメモ"));
    }

    #[test]
    fn large_images_are_shrunk_to_the_long_edge() {
        let prepared = prepare("big.png", png(3000, 1000, false)).unwrap();
        assert_eq!(prepared.kind, Kind::Image);
        let (w, h) = image::ImageReader::new(Cursor::new(&prepared.bytes))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap();
        assert_eq!(w, IMAGE_EDGE);
        assert!(h < 1000);
        assert!(prepared.note.unwrap().contains("3000×1000"));
    }

    #[test]
    fn other_image_formats_become_png_or_jpeg() {
        let mut bmp = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::new(8, 8))
            .write_to(&mut Cursor::new(&mut bmp), image::ImageFormat::Bmp)
            .unwrap();
        let prepared = prepare("a.bmp", bmp).unwrap();
        assert_eq!(prepared.kind, Kind::Image);
        assert_eq!(prepared.mime, "image/jpeg");
        let mut tiff = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::new(8, 8))
            .write_to(&mut Cursor::new(&mut tiff), image::ImageFormat::Tiff)
            .unwrap();
        let prepared = prepare("a.tiff", tiff).unwrap();
        assert_eq!(prepared.mime, "image/png");
    }

    #[test]
    fn broken_images_are_an_error() {
        let mut bytes = png(10, 10, false);
        bytes.truncate(30);
        let error = prepare("a.png", bytes).unwrap_err();
        assert!(error.contains("画像として読めませんでした"), "{error}");
    }

    #[test]
    fn pdfs_are_kept_as_pdf() {
        let prepared = prepare("x.bin", b"%PDF-1.7\n...".to_vec()).unwrap();
        assert_eq!(prepared.kind, Kind::Pdf);
        assert_eq!(prepared.mime, "application/pdf");
    }

    #[test]
    fn japanese_text_in_shift_jis_is_read() {
        let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode("見積もりの一覧\n合計 1,200 円");
        let prepared = prepare("見積.csv", bytes.into_owned()).unwrap();
        assert_eq!(prepared.kind, Kind::Text);
        assert_eq!(prepared.mime, "text/csv");
        assert_eq!(
            prepared.text.as_deref(),
            Some("見積もりの一覧\n合計 1,200 円")
        );
    }

    #[test]
    fn long_text_is_clipped_with_a_note() {
        let text = "あ".repeat(MAX_TEXT_CHARS + 10);
        let prepared = prepare("a.txt", text.into_bytes()).unwrap();
        assert_eq!(prepared.text.unwrap().chars().count(), MAX_TEXT_CHARS);
        assert!(prepared.note.unwrap().contains("先頭の"));
    }

    #[test]
    fn binary_files_are_other() {
        let prepared = prepare("song.mp3", vec![0xFF, 0xFB, 0x90, 0, 0, 1, 2, 3]).unwrap();
        assert_eq!(prepared.kind, Kind::Other);
        assert_eq!(prepared.mime, "audio/mpeg");
        let heic = [&[0u8, 0, 0, 24][..], b"ftypheic", &[0; 8]].concat();
        let prepared = prepare("IMG_1.HEIC", heic).unwrap();
        assert_eq!(prepared.kind, Kind::Other);
        assert_eq!(prepared.mime, "image/heic");
    }

    #[test]
    fn word_documents_become_text() {
        let docx = zip_of(&[
            ("[Content_Types].xml", "<Types/>"),
            (
                "word/document.xml",
                r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
                <w:p><w:r><w:t>議事録</w:t></w:r></w:p>
                <w:p><w:r><w:t>決定:</w:t><w:tab/><w:t>来週リリース</w:t></w:r></w:p>
                </w:body></w:document>"#,
            ),
        ]);
        let prepared = prepare("会議.docx", docx).unwrap();
        assert_eq!(prepared.kind, Kind::Text);
        assert_eq!(
            prepared.text.as_deref(),
            Some("議事録\n決定:\t来週リリース")
        );
    }

    #[test]
    fn spreadsheets_become_tab_separated_text() {
        let xlsx = zip_of(&[
            (
                "xl/workbook.xml",
                r#"<workbook><sheets><sheet name="予算" sheetId="1"/></sheets></workbook>"#,
            ),
            (
                "xl/sharedStrings.xml",
                r#"<sst><si><t>項目</t></si><si><t>金額</t></si><si><r><t>交</t></r><r><t>通費</t></r></si></sst>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData>
                <row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row>
                <row r="2"><c r="A2" t="s"><v>2</v></c><c r="B2"><v>1200</v></c></row>
                </sheetData></worksheet>"#,
            ),
        ]);
        let prepared = prepare("予算.xlsx", xlsx).unwrap();
        assert_eq!(prepared.kind, Kind::Text);
        assert_eq!(
            prepared.text.as_deref(),
            Some("## 予算\n項目\t金額\n交通費\t1200")
        );
    }

    #[test]
    fn slides_become_text_in_order() {
        let slide = |t: &str| {
            format!(
                r#"<p:sld xmlns:a="a" xmlns:p="p"><a:p><a:r><a:t>{t}</a:t></a:r></a:p></p:sld>"#
            )
        };
        let (s1, s2, s10) = (slide("表紙"), slide("目次"), slide("まとめ"));
        let pptx = zip_of(&[
            ("ppt/slides/slide10.xml", &s10),
            ("ppt/slides/slide2.xml", &s2),
            ("ppt/slides/slide1.xml", &s1),
        ]);
        let prepared = prepare("発表.pptx", pptx).unwrap();
        assert_eq!(
            prepared.text.as_deref(),
            Some("## スライド 1\n表紙\n\n## スライド 2\n目次\n\n## スライド 10\nまとめ")
        );
    }

    #[test]
    fn other_zips_list_their_files() {
        let prepared = prepare("a.zip", zip_of(&[("a/b.txt", "x"), ("c.png", "y")])).unwrap();
        assert_eq!(prepared.kind, Kind::Other);
        assert!(prepared.note.unwrap().contains("- a/b.txt\n- c.png"));
    }

    #[test]
    fn the_shelf_keeps_one_copy_and_sweeps_unused_files() {
        let dir = tempfile::tempdir().unwrap();
        let shelf = Shelf::new(dir.path());
        let a = shelf.put("a.txt", b"one".to_vec()).unwrap();
        let again = shelf.put("a.txt", b"one".to_vec()).unwrap();
        assert_eq!(a.path, again.path);
        assert_eq!(std::fs::read(&a.path).unwrap(), b"one");
        let b = shelf.put("b.txt", b"two".to_vec()).unwrap();
        // 会話から読み直せる
        let loaded = shelf.load(&a.to_json());
        assert_eq!(loaded.text.as_deref(), Some("one"));
        // 使われていない b は消え、a は残る
        shelf
            .sweep(&HashSet::from([a.hash.clone()]), &HashSet::new())
            .unwrap();
        assert!(a.path.exists());
        assert!(!b.path.exists());
        let gone = shelf.load(&b.to_json());
        assert_eq!(gone.kind, Kind::Other);
        assert!(gone.note.unwrap().contains("整理で消えました"));
    }

    #[test]
    fn big_files_are_not_copied() {
        let dir = tempfile::tempdir().unwrap();
        let shelf = Shelf::new(dir.path().join("shelf"));
        let big = dir.path().join("movie.mp4");
        let file = std::fs::File::create(&big).unwrap();
        file.set_len(MAX_COPY_BYTES + 1).unwrap();
        let part = shelf.put_path(&big).unwrap();
        assert_eq!(part.path, big);
        assert_eq!(part.mime, "video/mp4");
        assert!(part.hash.is_empty());
        assert!(!dir.path().join("shelf").exists());
    }

    #[test]
    fn only_the_newest_media_fits_in_one_request() {
        let part = |n: usize| FilePart {
            name: "a.png".into(),
            mime: "image/png".into(),
            kind: Kind::Image,
            size: n as u64,
            hash: "h".into(),
            path: PathBuf::from("a.png"),
            data: Some(Arc::new(vec![0; n])),
            text: None,
            note: None,
        };
        let old = vec![part(MAX_INLINE_BYTES / 2 + 1)];
        let new = vec![part(MAX_INLINE_BYTES / 2), part(1)];
        let flags = inline_flags([old.as_slice(), new.as_slice()].into_iter());
        assert_eq!(flags, vec![vec![false], vec![true, true]]);
        let text = describe(&old, &flags[0], false);
        assert!(text.contains("このリクエストに入っていません"), "{text}");
    }
}
