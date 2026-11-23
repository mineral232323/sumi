use std::path::Path;

use cairo::{Context, Format, ImageSurface};
use poppler::Document;

use crate::model::PAGE_WIDTH;

pub struct PdfDoc {
    document: Document,
    pub page_count: usize,
}

impl PdfDoc {
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = gio::File::for_path(path);
        let document = Document::from_gfile(&file, None, None::<&gio::Cancellable>)
            .map_err(|e| format!("PDFを開けません: {e}"))?;
        let page_count = document.n_pages().max(0) as usize;
        Ok(Self { document, page_count })
    }

    pub fn page_size_pt(&self, index: usize) -> Option<(f64, f64)> {
        self.document.page(index as i32).map(|page| page.size())
    }

    /// Draw a page into an existing context, scaled to the given point size.
    /// Used by the PDF export so the original page stays vector.
    pub fn render_into(&self, index: usize, ctx: &Context, width: f64, height: f64) -> bool {
        let Some(page) = self.document.page(index as i32) else {
            return false;
        };
        let (pw, ph) = page.size();
        if pw <= 0.0 || ph <= 0.0 || width <= 0.0 || height <= 0.0 {
            return false;
        }
        let _ = ctx.save();
        ctx.scale(width / pw, height / ph);
        page.render(ctx);
        let _ = ctx.restore();
        true
    }

    /// Render a page at the given pixel width. Returns a white-backed surface.
    pub fn render(&self, index: usize, width_px: f64) -> Option<ImageSurface> {
        let page = self.document.page(index as i32)?;
        let (pw, ph) = page.size();
        if pw <= 0.0 || ph <= 0.0 {
            return None;
        }
        let mut width = width_px.clamp(32.0, 4096.0);
        let mut height = width * ph / pw;
        if height > 4096.0 {
            height = 4096.0;
            width = height * pw / ph;
        }
        let surface = ImageSurface::create(Format::ARgb32, width.ceil() as i32, height.ceil() as i32)
            .ok()?;
        {
            let ctx = Context::new(&surface).ok()?;
            ctx.set_source_rgb(1.0, 1.0, 1.0);
            ctx.paint().ok()?;
            ctx.scale(width / pw, height / ph);
            page.render(&ctx);
        }
        surface.flush();
        Some(surface)
    }
}

/// Read the page count and proportional world sizes (width = PAGE_WIDTH).
pub fn inspect(path: &Path) -> Result<(usize, Vec<[f64; 2]>), String> {
    let doc = PdfDoc::open(path)?;
    if doc.page_count == 0 {
        return Err("ページのないPDFです".into());
    }
    let mut sizes = Vec::with_capacity(doc.page_count);
    for index in 0..doc.page_count {
        let (w, h) = doc
            .page_size_pt(index)
            .ok_or_else(|| "PDFページを読み込めません".to_string())?;
        if w <= 0.0 || h <= 0.0 {
            sizes.push([PAGE_WIDTH, PAGE_WIDTH * 1.414]);
        } else {
            sizes.push([PAGE_WIDTH, PAGE_WIDTH * h / w]);
        }
    }
    Ok((doc.page_count, sizes))
}
