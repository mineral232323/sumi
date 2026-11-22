use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

use crate::model::{Note, NoteKind, NoteMeta, Page, ToolSettings};

pub struct Store {
    conn: Connection,
    pdf_dir: PathBuf,
}

impl Store {
    pub fn open() -> Result<Self, String> {
        let data_dir = dirs::data_dir()
            .ok_or_else(|| "データディレクトリが見つかりません".to_string())?
            .join("sumi");
        Self::open_at(data_dir)
    }

    fn open_at(data_dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&data_dir)
            .map_err(|e| format!("データフォルダを作成できません: {e}"))?;
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
             );
             CREATE TABLE IF NOT EXISTS note_pages (
                note_id TEXT NOT NULL,
                page_index INTEGER NOT NULL,
                data TEXT NOT NULL,
                PRIMARY KEY (note_id, page_index)
             );
             CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
             );",
        )
        .map_err(|e| format!("データベースを初期化できません: {e}"))?;
        Ok(Self { conn, pdf_dir })
    }

    /// Global tool settings shared by every note. Missing or broken values
    /// fall back to the built-in defaults.
    pub fn load_tool_settings(&self) -> ToolSettings {
        self.conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'tool'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<ToolSettings>(&json).ok())
            .unwrap_or_default()
            .sanitized()
    }

    pub fn save_tool_settings(&self, settings: &ToolSettings) -> Result<(), String> {
        let json = serde_json::to_string(settings).map_err(|e| e.to_string())?;
        self.conn
            .execute(
                "INSERT INTO settings (key, value) VALUES ('tool', ?1)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![json],
            )
            .map_err(|e| format!("設定を保存できません: {e}"))?;
        Ok(())
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
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn load(&self, id: &str) -> Result<Note, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, title, kind, created_at, updated_at, page_sizes, pages, pdf_file
                 FROM notes WHERE id = ?1",
            )
            .map_err(|e| e.to_string())?;
        let (id, title, kind, created_at, updated_at, sizes_json, legacy_pages, pdf_file) = stmt
            .query_row(params![id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })
            .optional()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "ノートが見つかりません".to_string())?;

        let page_sizes: Vec<[f64; 2]> = serde_json::from_str(&sizes_json).unwrap_or_default();
        let mut pages = self.load_page_rows(&id)?;
        if pages.is_empty() {
            // Notes written by older releases keep all pages in notes.pages.
            // Move them into note_pages once, lazily.
            let legacy: Vec<Page> = serde_json::from_str(&legacy_pages).unwrap_or_default();
            if !legacy.is_empty() {
                self.save_all_pages(&id, &legacy)?;
                self.conn
                    .execute("UPDATE notes SET pages='[]' WHERE id = ?1", params![id])
                    .map_err(|e| e.to_string())?;
                pages = legacy;
            }
        }
        while pages.len() < page_sizes.len() {
            pages.push(Page::default());
        }

        Ok(Note {
            id,
            title,
            kind: NoteKind::from_str(&kind),
            created_at,
            updated_at,
            page_sizes,
            pages,
            pdf_file,
        })
    }

    fn load_page_rows(&self, id: &str) -> Result<Vec<Page>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT page_index, data FROM note_pages
                 WHERE note_id = ?1 ORDER BY page_index",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![id], |row| {
                Ok((row.get::<_, i64>(0)? as usize, row.get::<_, String>(1)?))
            })
            .map_err(|e| e.to_string())?;

        let mut pages = BTreeMap::<usize, Page>::new();
        for row in rows {
            let (index, data) = row.map_err(|e| e.to_string())?;
            if let Ok(page) = serde_json::from_str::<Page>(&data) {
                pages.insert(index, page);
            }
        }
        let count = pages.keys().next_back().map(|index| index + 1).unwrap_or(0);
        let mut result = Vec::with_capacity(count);
        for index in 0..count {
            result.push(pages.remove(&index).unwrap_or_default());
        }
        Ok(result)
    }

    /// Save note metadata and only the pages whose index is in `dirty_pages`.
    pub fn save(&self, note: &Note, dirty_pages: &[usize]) -> Result<(), String> {
        let sizes = serde_json::to_string(&note.page_sizes).map_err(|e| e.to_string())?;
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("保存を開始できません: {e}"))?;
        tx.execute(
            "INSERT INTO notes
                (id, title, kind, created_at, updated_at, page_count, page_sizes, pages, pdf_file)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '[]', ?8)
             ON CONFLICT(id) DO UPDATE SET
                title=excluded.title, kind=excluded.kind, updated_at=excluded.updated_at,
                page_count=excluded.page_count, page_sizes=excluded.page_sizes,
                pages='[]', pdf_file=excluded.pdf_file",
            params![
                note.id,
                note.title,
                note.kind.as_str(),
                note.created_at,
                note.updated_at,
                note.pages.len() as i64,
                sizes,
                note.pdf_file,
            ],
        )
        .map_err(|e| format!("保存できません: {e}"))?;

        for &index in dirty_pages {
            let Some(page) = note.pages.get(index) else {
                continue;
            };
            let data = serde_json::to_string(page).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT INTO note_pages (note_id, page_index, data)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(note_id, page_index) DO UPDATE SET data=excluded.data",
                params![note.id, index as i64, data],
            )
            .map_err(|e| format!("ページを保存できません: {e}"))?;
        }
        tx.execute(
            "DELETE FROM note_pages WHERE note_id = ?1 AND page_index >= ?2",
            params![note.id, note.pages.len() as i64],
        )
        .map_err(|e| format!("古いページを削除できません: {e}"))?;
        tx.commit()
            .map_err(|e| format!("保存を確定できません: {e}"))?;
        Ok(())
    }

    pub fn save_all(&self, note: &Note) -> Result<(), String> {
        let pages: Vec<usize> = (0..note.pages.len()).collect();
        self.save(note, &pages)
    }

    fn save_all_pages(&self, id: &str, pages: &[Page]) -> Result<(), String> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("保存を開始できません: {e}"))?;
        for (index, page) in pages.iter().enumerate() {
            let data = serde_json::to_string(page).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT INTO note_pages (note_id, page_index, data)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(note_id, page_index) DO UPDATE SET data=excluded.data",
                params![id, index as i64, data],
            )
            .map_err(|e| format!("ページを保存できません: {e}"))?;
        }
        tx.execute(
            "DELETE FROM note_pages WHERE note_id = ?1 AND page_index >= ?2",
            params![id, pages.len() as i64],
        )
        .map_err(|e| format!("古いページを削除できません: {e}"))?;
        tx.commit()
            .map_err(|e| format!("保存を確定できません: {e}"))?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        let pdf: Option<String> = self
            .conn
            .query_row(
                "SELECT pdf_file FROM notes WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .flatten();
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM note_pages WHERE note_id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM notes WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Point, Stroke, StrokeTool, HIGHLIGHTER_SIZE_RANGE, PEN_SIZE_RANGE,
    };

    fn temp_store() -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("sumi-test-{}", uuid::Uuid::new_v4()));
        let store = Store::open_at(dir.clone()).unwrap();
        (store, dir)
    }

    fn test_note() -> Note {
        let mut note = Note::new(NoteKind::Paged, "test");
        note.pages[0].strokes.push(Stroke {
            id: 1,
            tool: StrokeTool::Pen,
            color: "#000000".into(),
            width: 3.0,
            points: vec![
                Point {
                    x: 10.0,
                    y: 10.0,
                    p: 0.5,
                },
                Point {
                    x: 20.0,
                    y: 20.0,
                    p: 0.5,
                },
            ],
        });
        note.add_page();
        note
    }

    #[test]
    fn saves_and_loads_pages() {
        let (store, dir) = temp_store();
        let note = test_note();
        store.save_all(&note).unwrap();
        let loaded = store.load(&note.id).unwrap();
        assert_eq!(loaded.pages, note.pages);
        assert_eq!(loaded.title, note.title);

        let mut changed = note.clone();
        changed.pages[1].strokes.push(Stroke {
            id: 2,
            tool: StrokeTool::Pen,
            color: "#ff0000".into(),
            width: 3.0,
            points: vec![Point {
                x: 1.0,
                y: 2.0,
                p: 0.5,
            }],
        });
        store.save(&changed, &[1]).unwrap();
        let loaded = store.load(&note.id).unwrap();
        assert_eq!(loaded.pages, changed.pages);
        drop(store);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn stores_tool_settings_globally() {
        let (store, dir) = temp_store();
        assert_eq!(store.load_tool_settings(), ToolSettings::default());

        let settings = ToolSettings {
            pen_size: 7.5,
            highlighter_size: 30.0,
            eraser_size: 40.0,
        };
        store.save_tool_settings(&settings).unwrap();
        assert_eq!(store.load_tool_settings(), settings);

        // Later notes and pages see the same values.
        let note = test_note();
        store.save_all(&note).unwrap();
        assert_eq!(store.load_tool_settings(), settings);

        // Out-of-range values are clamped on load.
        store
            .conn
            .execute(
                "UPDATE settings SET value = ?1 WHERE key = 'tool'",
                params!["{\"pen_size\":100.0,\"highlighter_size\":-4.0,\"eraser_size\":26.0}"],
            )
            .unwrap();
        let loaded = store.load_tool_settings();
        assert_eq!(loaded.pen_size, PEN_SIZE_RANGE.1);
        assert_eq!(loaded.highlighter_size, HIGHLIGHTER_SIZE_RANGE.0);
        assert_eq!(loaded.eraser_size, 26.0);
        drop(store);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn migrates_legacy_pages_column() {
        let (store, dir) = temp_store();
        let note = test_note();
        let pages = serde_json::to_string(&note.pages).unwrap();
        let sizes = serde_json::to_string(&note.page_sizes).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO notes (id, title, kind, created_at, updated_at, page_count,
                                    page_sizes, pages, pdf_file)
                 VALUES (?1, ?2, 'paged', 0, 0, 2, ?3, ?4, NULL)",
                params![note.id, note.title, sizes, pages],
            )
            .unwrap();
        let loaded = store.load(&note.id).unwrap();
        assert_eq!(loaded.pages, note.pages);
        let loaded_again = store.load(&note.id).unwrap();
        assert_eq!(loaded_again.pages, note.pages);
        drop(store);
        let _ = fs::remove_dir_all(dir);
    }
}
