use serde::{Deserialize, Serialize};

/// Width of every page in world units. Height follows the page aspect ratio.
pub const PAGE_WIDTH: f64 = 880.0;
pub const A4_RATIO: f64 = 297.0 / 210.0;
pub const PAGE_GAP: f64 = 56.0;
pub const INFINITE_GRID: f64 = 28.0;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NoteKind {
    Infinite,
    Paged,
    Pdf,
}

impl NoteKind {
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            Self::Infinite => "無限キャンバス",
            Self::Paged => "ページノート",
            Self::Pdf => "PDFノート",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Infinite => "sumi-infinite-symbolic",
            Self::Paged => "sumi-page-symbolic",
            Self::Pdf => "sumi-pdf-symbolic",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Infinite => "infinite",
            Self::Paged => "paged",
            Self::Pdf => "pdf",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "pdf" => Self::Pdf,
            "paged" => Self::Paged,
            _ => Self::Infinite,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StrokeTool {
    Pen,
    Highlighter,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
    #[serde(default = "half_pressure")]
    pub p: f64,
}

fn half_pressure() -> f64 {
    0.5
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Stroke {
    #[serde(default)]
    pub id: u64,
    pub tool: StrokeTool,
    pub color: String,
    pub width: f64,
    pub points: Vec<Point>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Page {
    #[serde(default)]
    pub strokes: Vec<Stroke>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub kind: NoteKind,
    pub created_at: i64,
    pub updated_at: i64,
    /// World-space size of each page (width, height).
    #[serde(default)]
    pub page_sizes: Vec<[f64; 2]>,
    pub pages: Vec<Page>,
    /// File name (inside the pdf data directory) for imported PDF notes.
    #[serde(default)]
    pub pdf_file: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NoteMeta {
    pub id: String,
    pub title: String,
    pub kind: NoteKind,
    #[allow(dead_code)]
    pub created_at: i64,
    pub updated_at: i64,
    pub page_count: usize,
}

impl Note {
    pub fn new(kind: NoteKind, title: impl Into<String>) -> Self {
        let now = now_secs();
        let mut note = Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: title.into(),
            kind,
            created_at: now,
            updated_at: now,
            page_sizes: Vec::new(),
            pages: Vec::new(),
            pdf_file: None,
        };
        match kind {
            NoteKind::Infinite => {
                note.page_sizes.push([PAGE_WIDTH, PAGE_WIDTH * A4_RATIO]);
                note.pages.push(Page::default());
            }
            NoteKind::Paged => {
                note.page_sizes.push([PAGE_WIDTH, PAGE_WIDTH * A4_RATIO]);
                note.pages.push(Page::default());
            }
            NoteKind::Pdf => {}
        }
        note
    }

    pub fn from_pdf(title: impl Into<String>, sizes: Vec<[f64; 2]>) -> Self {
        let now = now_secs();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: title.into(),
            kind: NoteKind::Pdf,
            created_at: now,
            updated_at: now,
            pages: sizes.iter().map(|_| Page::default()).collect(),
            page_sizes: sizes,
            pdf_file: None,
        }
    }

    pub fn meta(&self) -> NoteMeta {
        NoteMeta {
            id: self.id.clone(),
            title: self.title.clone(),
            kind: self.kind,
            created_at: self.created_at,
            updated_at: self.updated_at,
            page_count: self.pages.len(),
        }
    }

    pub fn page_size(&self, index: usize) -> (f64, f64) {
        self.page_sizes
            .get(index)
            .map(|s| (s[0], s[1]))
            .unwrap_or((PAGE_WIDTH, PAGE_WIDTH * A4_RATIO))
    }

    /// Vertical page layout. Infinite notes have no page cards.
    pub fn layout(&self) -> Vec<Rect> {
        if self.kind == NoteKind::Infinite {
            return Vec::new();
        }
        let mut rects = Vec::with_capacity(self.pages.len());
        let mut y = 0.0;
        for index in 0..self.pages.len() {
            if index > 0 {
                y += PAGE_GAP;
            }
            let (w, h) = self.page_size(index);
            rects.push(Rect { x: 0.0, y, w, h });
            y += h;
        }
        rects
    }

    pub fn add_page(&mut self) {
        if self.kind != NoteKind::Paged {
            return;
        }
        self.page_sizes.push([PAGE_WIDTH, PAGE_WIDTH * A4_RATIO]);
        self.pages.push(Page::default());
        self.updated_at = now_secs();
    }

    /// Page index containing a world point. Infinite notes always map to page 0.
    pub fn page_at(&self, layout: &[Rect], x: f64, y: f64) -> Option<usize> {
        if self.kind == NoteKind::Infinite {
            return Some(0);
        }
        layout.iter().position(|r| r.contains(x, y))
    }

    /// Clamp a point to a page, used while drawing inside paged notes.
    pub fn clamp_to_page(&self, layout: &[Rect], page: usize, x: f64, y: f64) -> (f64, f64) {
        if self.kind == NoteKind::Infinite {
            return (x, y);
        }
        let Some(rect) = layout.get(page) else {
            return (x, y);
        };
        (
            x.clamp(rect.x + 1.0, rect.x + rect.w - 1.0),
            y.clamp(rect.y + 1.0, rect.y + rect.h - 1.0),
        )
    }

    pub fn touch(&mut self) {
        self.updated_at = now_secs();
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }

    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn intersects(&self, x: f64, y: f64, w: f64, h: f64) -> bool {
        self.x < x + w && self.x + self.w > x && self.y < y + h && self.y + self.h > y
    }
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn parse_hex(color: &str) -> (f64, f64, f64) {
    let hex = color.trim_start_matches('#');
    if hex.len() < 6 {
        return (0.1, 0.1, 0.12);
    }
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(30) as f64 / 255.0;
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(32) as f64 / 255.0;
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(38) as f64 / 255.0;
    (r, g, b)
}

pub fn hex_to_cairo(color: &str) -> (f64, f64, f64) {
    parse_hex(color)
}
