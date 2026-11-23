use std::path::Path;

use cairo::{Context, PdfSurface};

use crate::canvas::{draw_stroke, page_content_bounds};
use crate::model::{Note, NoteKind, Rect, A4_RATIO, PAGE_WIDTH};
use crate::pdf::PdfDoc;

const A4_WIDTH_PT: f64 = 595.276;
const A4_HEIGHT_PT: f64 = 841.89;
/// Margin around infinite-canvas content, in points.
const INFINITE_MARGIN_PT: f64 = 36.0;
/// Never blow a small sketch up more than this when fitting it to a page.
const INFINITE_MAX_SCALE: f64 = 4.0;

struct Geometry {
    width_pt: f64,
    height_pt: f64,
    /// World-space point mapped to the top-left of the page.
    origin: (f64, f64),
    scale: (f64, f64),
    /// Offset applied after scaling, used to centre infinite content.
    offset: (f64, f64),
}

/// Write a note to a PDF file, one PDF page per note page.
///
/// Imported PDF notes keep their original page as a vector background, with the
/// handwriting drawn on top. Paged notes are rendered as A4 sheets and infinite
/// notes are fitted to a single A4 sheet.
pub fn export(note: &Note, pdf: Option<&PdfDoc>, path: &Path) -> Result<usize, String> {
    if note.pages.is_empty() {
        return Err("書き出すページがありません".into());
    }

    let result = write_pdf(note, pdf, path);
    if result.is_err() {
        // Do not leave a half-written file behind.
        let _ = std::fs::remove_file(path);
    }
    result
}

fn write_pdf(note: &Note, pdf: Option<&PdfDoc>, path: &Path) -> Result<usize, String> {
    let first = geometry(note, pdf, 0);
    let surface = PdfSurface::new(first.width_pt, first.height_pt, path)
        .map_err(|e| format!("PDFを作成できません: {e}"))?;
    let ctx = Context::new(&surface).map_err(|e| format!("PDFを描画できません: {e}"))?;

    for index in 0..note.pages.len() {
        let page = geometry(note, pdf, index);
        if index > 0 {
            surface
                .set_size(page.width_pt, page.height_pt)
                .map_err(|e| format!("ページサイズを変更できません: {e}"))?;
        }
        draw_page(&ctx, note, pdf, index, &page);
        ctx.show_page()
            .map_err(|e| format!("ページを書き出せません: {e}"))?;
    }

    surface.finish();
    Ok(note.pages.len())
}

fn geometry(note: &Note, pdf: Option<&PdfDoc>, index: usize) -> Geometry {
    let (world_w, world_h) = note.page_size(index);

    if note.kind == NoteKind::Infinite {
        let bounds = note
            .pages
            .get(index)
            .and_then(page_content_bounds)
            .filter(|bounds| bounds.w > 0.0 && bounds.h > 0.0)
            .unwrap_or(Rect {
                x: 0.0,
                y: 0.0,
                w: PAGE_WIDTH,
                h: PAGE_WIDTH * A4_RATIO,
            });
        let available_w = (A4_WIDTH_PT - 2.0 * INFINITE_MARGIN_PT).max(1.0);
        let available_h = (A4_HEIGHT_PT - 2.0 * INFINITE_MARGIN_PT).max(1.0);
        let scale = (available_w / bounds.w)
            .min(available_h / bounds.h)
            .min(INFINITE_MAX_SCALE);
        return Geometry {
            width_pt: A4_WIDTH_PT,
            height_pt: A4_HEIGHT_PT,
            origin: (bounds.x, bounds.y),
            scale: (scale, scale),
            offset: (
                (A4_WIDTH_PT - bounds.w * scale) * 0.5,
                (A4_HEIGHT_PT - bounds.h * scale) * 0.5,
            ),
        };
    }

    let (width_pt, height_pt) = if note.kind == NoteKind::Pdf {
        pdf.and_then(|pdf| pdf.page_size_pt(index))
            .filter(|(w, h)| *w > 0.0 && *h > 0.0)
            .unwrap_or((A4_WIDTH_PT, A4_WIDTH_PT * world_h / world_w))
    } else {
        (A4_WIDTH_PT, A4_WIDTH_PT * world_h / world_w)
    };

    let rect = note.layout().get(index).copied().unwrap_or(Rect {
        x: 0.0,
        y: 0.0,
        w: world_w,
        h: world_h,
    });

    Geometry {
        width_pt,
        height_pt,
        origin: (rect.x, rect.y),
        scale: (width_pt / rect.w.max(1.0), height_pt / rect.h.max(1.0)),
        offset: (0.0, 0.0),
    }
}

fn draw_page(ctx: &Context, note: &Note, pdf: Option<&PdfDoc>, index: usize, g: &Geometry) {
    // Paper.
    let _ = ctx.save();
    let _ = ctx.set_source_rgb(1.0, 1.0, 1.0);
    let _ = ctx.paint();

    // Original PDF page, kept as vectors.
    if note.kind == NoteKind::Pdf {
        if let Some(pdf) = pdf {
            pdf.render_into(index, ctx, g.width_pt, g.height_pt);
        }
    }
    let _ = ctx.restore();

    // Handwriting on top.
    let Some(page) = note.pages.get(index) else {
        return;
    };
    let _ = ctx.save();
    ctx.translate(g.offset.0, g.offset.1);
    ctx.scale(g.scale.0, g.scale.1);
    ctx.translate(-g.origin.0, -g.origin.1);
    for stroke in &page.strokes {
        draw_stroke(ctx, stroke, 1.0);
    }
    let _ = ctx.restore();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Note, NoteKind, Point, Stroke, StrokeTool};

    fn temp_path(name: &str) -> std::path::PathBuf {
        let unique = format!(
            "sumi-export-test-{}-{}-{}.pdf",
            std::process::id(),
            crate::model::now_secs(),
            name
        );
        std::env::temp_dir().join(unique)
    }

    fn stroke() -> Stroke {
        Stroke {
            id: 1,
            tool: StrokeTool::Pen,
            color: "#2f6fed".into(),
            width: 4.0,
            points: vec![
                Point {
                    x: 120.0,
                    y: 140.0,
                    p: 0.5,
                },
                Point {
                    x: 300.0,
                    y: 360.0,
                    p: 0.5,
                },
                Point {
                    x: 520.0,
                    y: 240.0,
                    p: 0.5,
                },
            ],
            shape: None,
        }
    }

    #[test]
    fn paged_note_writes_a_pdf_with_one_page_per_note_page() {
        let mut note = Note::new(NoteKind::Paged, "テスト");
        note.pages[0].strokes.push(stroke());
        note.add_page();
        note.pages[1].strokes.push(stroke());

        let path = temp_path("paged");
        let count = export(&note, None, &path).expect("export");
        assert_eq!(count, 2);

        let bytes = std::fs::read(&path).expect("read pdf");
        assert!(bytes.starts_with(b"%PDF-"));
        assert!(bytes.len() > 500);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn exported_pdf_keeps_its_page_count_and_size() {
        let mut note = Note::new(NoteKind::Paged, "再読み込み");
        note.pages[0].strokes.push(stroke());
        note.add_page();

        let path = temp_path("reopen");
        export(&note, None, &path).expect("export");
        let doc = PdfDoc::open(&path).expect("reopen pdf");
        assert_eq!(doc.page_count, 2);
        let (w, h) = doc.page_size_pt(0).expect("page size");
        assert!((w - A4_WIDTH_PT).abs() < 1.0, "width {w}");
        assert!((h - A4_HEIGHT_PT).abs() < 1.0, "height {h}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn infinite_note_exports_its_strokes() {
        let mut note = Note::new(NoteKind::Infinite, "無限");
        note.pages[0].strokes.push(stroke());

        let path = temp_path("infinite");
        let count = export(&note, None, &path).expect("export");
        assert_eq!(count, 1);

        let bytes = std::fs::read(&path).expect("read pdf");
        assert!(bytes.starts_with(b"%PDF-"));
        assert!(bytes.len() > 500);
        let _ = std::fs::remove_file(&path);
    }
}
