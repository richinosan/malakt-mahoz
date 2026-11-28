//! 印刷のプレビュー。今の設定のとおりに作った PDF を、ページごとの画像にする。
//! PDF は保存するときと同じ作り方（[`crate::browser`] の PDF の作り方と [`crate::print_setup::finish`]）で作るので、
//! 見えるものがそのまま保存される。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::RenderImage;

/// 全部のページの画素の合計。ページが多いときは 1 枚を小さく描いて、メモリーを使いすぎない。
const PIXELS: f32 = 16_000_000.;
/// 1 枚の幅の上限（画素）。
const MAX_WIDTH: f32 = 1240.;

/// プレビューの PDF を一時的に置く場所。描いたら消す。作るたびに違う名前にする。
pub fn temp_path() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "malakt-mahoz-print-preview-{}-{n}.pdf",
        std::process::id()
    ))
}

/// PDF のページを画像（gpui が描く B, G, R, A の順）にする。
pub fn render(path: &Path) -> Result<Vec<Arc<RenderImage>>, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("プレビューの PDF を読めませんでした: {e}"))?;
    // 描く部品が壊れた PDF で止まっても、アプリは落とさずに失敗として出す
    std::panic::catch_unwind(move || render_bytes(bytes))
        .unwrap_or_else(|_| Err("プレビューを描けませんでした".to_owned()))
}

fn render_bytes(bytes: Vec<u8>) -> Result<Vec<Arc<RenderImage>>, String> {
    use hayro::hayro_interpret::InterpreterSettings;
    use hayro::vello_cpu::color::palette::css::WHITE;
    use hayro::vello_cpu::peniko::ImageAlphaType;
    use hayro::{PixmapSettings, RenderCache, RenderSettings};

    let pdf = hayro::hayro_syntax::Pdf::new(bytes)
        .map_err(|e| format!("プレビューの PDF を開けませんでした: {e:?}"))?;
    let pages = pdf.pages();
    if pages.is_empty() {
        return Err("印刷するページがありません".to_owned());
    }
    let area: f32 = pages
        .iter()
        .map(|page| {
            let (width, height) = page.render_dimensions();
            width * height
        })
        .sum();
    let widest = pages
        .iter()
        .map(|page| page.render_dimensions().0)
        .fold(1., f32::max);
    let scale = (PIXELS / area.max(1.)).sqrt().min(MAX_WIDTH / widest);
    let cache = RenderCache::new();
    let interpreter = InterpreterSettings::default();
    let settings = RenderSettings::default();
    let pixmap = PixmapSettings {
        x_scale: scale,
        y_scale: scale,
        bg_color: WHITE,
    };
    let mut images = Vec::with_capacity(pages.len());
    for page in pages.iter() {
        let drawn = hayro::render(page, &cache, &interpreter, &settings, &pixmap);
        let (width, height) = (u32::from(drawn.width()), u32::from(drawn.height()));
        let mut pixels = drawn.take_rgba8(ImageAlphaType::Alpha);
        // 紙は白で塗ってから描くので透けない
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        let frame = image::RgbaImage::from_raw(width, height, pixels)
            .ok_or_else(|| "プレビューの画像の大きさが合いません".to_owned())?;
        images.push(Arc::new(RenderImage::new(vec![image::Frame::new(frame)])));
    }
    Ok(images)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 幅と高さ（ポイント）の違うページの PDF を書く。
    fn write_pdf(path: &Path, sizes: &[(i64, i64)]) {
        use lopdf::{dictionary, Document, Object, Stream};
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let kids: Vec<Object> = sizes
            .iter()
            .map(|&(width, height)| {
                // 左上に黒い四角
                let content = doc.add_object(Stream::new(
                    dictionary! {},
                    format!("0 0 0 rg 0 {} 100 100 re f", height - 100).into_bytes(),
                ));
                doc.add_object(dictionary! {
                    "Type" => "Page",
                    "Parent" => pages_id,
                    "Contents" => content,
                    "MediaBox" => vec![0.into(), 0.into(), width.into(), height.into()],
                })
                .into()
            })
            .collect();
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => kids,
                "Count" => sizes.len() as i64,
            }),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        doc.save(path).unwrap();
    }

    #[test]
    fn every_page_becomes_an_image() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.pdf");
        // A4 の縦と横
        write_pdf(&path, &[(595, 842), (842, 595)]);
        let images = render(&path).unwrap();
        assert_eq!(images.len(), 2);
        let first = images[0].size(0);
        let second = images[1].size(0);
        assert!(first.height > first.width, "縦");
        assert!(second.width > second.height, "横");
        assert!(first.width.0 as f32 <= MAX_WIDTH + 1.);
        // 左上は黒、真ん中は紙の白（B, G, R, A）
        let bytes = images[0].as_bytes(0).unwrap();
        let at = |x: usize, y: usize| {
            let i = (y * first.width.0 as usize + x) * 4;
            [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
        };
        assert_eq!(at(5, 5), [0, 0, 0, 255]);
        let (w, h) = (first.width.0 as usize, first.height.0 as usize);
        assert_eq!(at(w / 2, h / 2), [255, 255, 255, 255]);
    }

    #[test]
    fn many_pages_are_drawn_smaller() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("many.pdf");
        write_pdf(&path, &vec![(595, 842); 60]);
        let images = render(&path).unwrap();
        assert_eq!(images.len(), 60);
        let total: f32 = images
            .iter()
            .map(|image| {
                let size = image.size(0);
                size.width.0 as f32 * size.height.0 as f32
            })
            .sum();
        assert!(total <= PIXELS * 1.01, "{total}");
    }

    #[test]
    fn a_broken_pdf_is_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.pdf");
        std::fs::write(&path, b"not a pdf").unwrap();
        assert!(render(&path).is_err());
        assert!(render(&dir.path().join("none.pdf")).is_err());
    }
}
