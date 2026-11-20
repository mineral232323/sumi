use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

use crate::model::{Note, NoteKind, NoteMeta};

pub struct Store {
    conn: Connection,
    pdf_dir: PathBuf,
}

impl Store {
    pub fn open() -> Result<Self, String> {
        let data_dir = dirs::data_dir()
            .ok_or_else(|| "データディレクトリが見つかりません".to_string())?
            .join("sumi");
        fs::create_dir_all(&data_dir).map_err(|e| format!("データフォルダを作成できません: {e}"))?;
        let pdf_dir = data_dir.join("pdfs");
        fs::create_dir_all(&pdf_dir).map_err(|e| format!("PDFフォルダを作成できません: {e}"))?;

        let conn = Connection::open(data_dir.join("sumi.db"))
            .map_err(|e| format!("データベースを開けません: {e}"))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS notes (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                kind TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                page_count INTEGER NOT NULL,
                page_sizes TEXT NOT NULL,
                pages TEXT NOT NULL,
                pdf_file TEXT
             );",
        )
        .map_err(|e| format!("データベースを初期化できません: {e}"))?;
        Ok(Self { conn, pdf_dir })
    }

    pub fn list(&self) -> Result<Vec<NoteMeta>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, title, kind, created_at, updated_at, page_count
                 FROM notes ORDER BY updated_at DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(NoteMeta {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    kind: NoteKind::from_str(&row.get::<_, String>(2)?),
                    created_at: row.get(3)?,
                    updated_at: row.get(4)?,
                    page_count: row.get::<_, i64>(5)? as usize,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn load(&self, id: &str) -> Result<Note, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, title, kind, created_at, updated_at, page_sizes, pages, pdf_file
                 FROM notes WHERE id = ?1",
            )
            .map_err(|e| e.to_string())?;
        stmt.query_row(params![id], |row| {
            let sizes_json: String = row.get(5)?;
            let pages_json: String = row.get(6)?;
            Ok(Note {
                id: row.get(0)?,
                title: row.get(1)?,
                kind: NoteKind::from_str(&row.get::<_, String>(2)?),
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
                page_sizes: serde_json::from_str(&sizes_json).unwrap_or_default(),
                pages: serde_json::from_str(&pages_json).unwrap_or_default(),
                pdf_file: row.get(7)?,
            })
        })
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "ノートが見つかりません".to_string())
    }

    pub fn save(&self, note: &Note) -> Result<(), String> {
        let sizes = serde_json::to_string(&note.page_sizes).map_err(|e| e.to_string())?;
        let pages = serde_json::to_string(&note.pages).map_err(|e| e.to_string())?;
        self.conn
            .execute(
                "INSERT INTO notes
                    (id, title, kind, created_at, updated_at, page_count, page_sizes, pages, pdf_file)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET
                    title=excluded.title, kind=excluded.kind, updated_at=excluded.updated_at,
                    page_count=excluded.page_count, page_sizes=excluded.page_sizes,
                    pages=excluded.pages, pdf_file=excluded.pdf_file",
                params![
                    note.id,
                    note.title,
                    note.kind.as_str(),
                    note.created_at,
                    note.updated_at,
                    note.pages.len() as i64,
                    sizes,
                    pages,
                    note.pdf_file,
                ],
            )
            .map_err(|e| format!("保存できません: {e}"))?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        let pdf: Option<String> = self
            .conn
            .query_row("SELECT pdf_file FROM notes WHERE id = ?1", params![id], |row| row.get(0))
            .optional()
            .map_err(|e| e.to_string())?
            .flatten();
        self.conn
            .execute("DELETE FROM notes WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        if let Some(name) = pdf {
            let _ = fs::remove_file(self.pdf_dir.join(name));
        }
        Ok(())
    }

    /// Copy an imported PDF into the data directory and return its file name.
    pub fn import_pdf(&self, id: &str, source: &Path) -> Result<String, String> {
        let name = format!("{id}.pdf");
        fs::copy(source, self.pdf_dir.join(&name))
            .map_err(|e| format!("PDFをコピーできません: {e}"))?;
        Ok(name)
    }

    pub fn pdf_path(&self, name: &str) -> PathBuf {
        self.pdf_dir.join(name)
    }
}
