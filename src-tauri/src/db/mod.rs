//! SQLite 存取层。
//!
//! 迁移策略：只前进不回退，靠 `PRAGMA user_version` 判断。旧库能直接打开。

use crate::error::AppResult;
use crate::models::{Cue, Media, MediaProbe, Project, Segment};
use rusqlite::{params, Connection};
use std::path::Path;

const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS project (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT    NOT NULL,
    root_dir    TEXT    NOT NULL DEFAULT '',
    created_at  TEXT    NOT NULL
);

CREATE TABLE IF NOT EXISTS media (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id   INTEGER NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    path         TEXT    NOT NULL,
    name         TEXT    NOT NULL,
    duration_ms  INTEGER NOT NULL DEFAULT 0,
    width        INTEGER NOT NULL DEFAULT 0,
    height       INTEGER NOT NULL DEFAULT 0,
    has_audio    INTEGER NOT NULL DEFAULT 0,
    size_bytes   INTEGER NOT NULL DEFAULT 0,
    format_name  TEXT    NOT NULL DEFAULT '',
    created_at   TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_media_project ON media(project_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_media_path ON media(project_id, path);

CREATE TABLE IF NOT EXISTS segment (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    media_id    INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    index_no    INTEGER NOT NULL,
    start_ms    INTEGER NOT NULL,
    end_ms      INTEGER NOT NULL,
    out_path    TEXT    NOT NULL DEFAULT '',
    size_bytes  INTEGER NOT NULL DEFAULT 0,
    status      TEXT    NOT NULL DEFAULT 'pending',
    message     TEXT    NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_segment_media ON segment(media_id, index_no);

CREATE TABLE IF NOT EXISTS setting (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL
);
"#;

/// v2：字幕。挂到 media 上，跟着素材级联删除。
const SCHEMA_V2: &str = r#"
CREATE TABLE IF NOT EXISTS cue (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    media_id  INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    index_no  INTEGER NOT NULL,
    start_ms  INTEGER NOT NULL,
    end_ms    INTEGER NOT NULL,
    text      TEXT    NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_cue_media ON cue(media_id, start_ms);
"#;

/// 当前 schema 版本。加表时递增，并在 `migrate` 里补一段。
pub const SCHEMA_VERSION: i64 = 2;

pub fn open(path: &Path) -> AppResult<Connection> {
    let conn = Connection::open(path)?;
    // execute_batch 会忽略 PRAGMA 的返回行，比 pragma_update 更稳
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;",
    )?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> AppResult<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version < 1 {
        conn.execute_batch(SCHEMA_V1)?;
    }
    if version < 2 {
        conn.execute_batch(SCHEMA_V2)?;
    }
    if version < SCHEMA_VERSION {
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
    }
    Ok(())
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

// ---------- setting ----------

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> AppResult<()> {
    conn.execute(
        "INSERT INTO setting(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn get_setting(conn: &Connection, key: &str) -> AppResult<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM setting WHERE key = ?1")?;
    let mut rows = stmt.query(params![key])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row.get(0)?))
    } else {
        Ok(None)
    }
}

// ---------- project ----------

pub fn create_project(conn: &Connection, name: &str, root_dir: &str) -> AppResult<Project> {
    let created_at = now();
    conn.execute(
        "INSERT INTO project(name, root_dir, created_at) VALUES(?1, ?2, ?3)",
        params![name, root_dir, created_at],
    )?;
    Ok(Project {
        id: conn.last_insert_rowid(),
        name: name.to_string(),
        root_dir: root_dir.to_string(),
        created_at,
    })
}

pub fn list_projects(conn: &Connection) -> AppResult<Vec<Project>> {
    let mut stmt =
        conn.prepare("SELECT id, name, root_dir, created_at FROM project ORDER BY id DESC")?;
    let rows = stmt.query_map([], |row| {
        Ok(Project {
            id: row.get(0)?,
            name: row.get(1)?,
            root_dir: row.get(2)?,
            created_at: row.get(3)?,
        })
    })?;
    let mut out = Vec::new();
    for item in rows {
        out.push(item?);
    }
    Ok(out)
}

pub fn delete_project(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM project WHERE id = ?1", params![id])?;
    Ok(())
}

// ---------- media ----------

#[allow(clippy::too_many_arguments)]
pub fn add_media(
    conn: &Connection,
    project_id: i64,
    path: &str,
    name: &str,
    probe: &MediaProbe,
) -> AppResult<Media> {
    let created_at = now();
    conn.execute(
        "INSERT INTO media(project_id, path, name, duration_ms, width, height, has_audio,
                           size_bytes, format_name, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            project_id,
            path,
            name,
            probe.duration_ms,
            probe.width,
            probe.height,
            probe.has_audio as i64,
            probe.size_bytes,
            probe.format_name,
            created_at
        ],
    )?;
    get_media(conn, conn.last_insert_rowid())
}

pub fn get_media(conn: &Connection, id: i64) -> AppResult<Media> {
    let media = conn.query_row(
        "SELECT id, project_id, path, name, duration_ms, width, height, has_audio,
                size_bytes, format_name, created_at
         FROM media WHERE id = ?1",
        params![id],
        row_to_media,
    )?;
    Ok(media)
}

fn row_to_media(row: &rusqlite::Row<'_>) -> rusqlite::Result<Media> {
    Ok(Media {
        id: row.get(0)?,
        project_id: row.get(1)?,
        path: row.get(2)?,
        name: row.get(3)?,
        duration_ms: row.get(4)?,
        width: row.get(5)?,
        height: row.get(6)?,
        has_audio: row.get::<_, i64>(7)? != 0,
        size_bytes: row.get(8)?,
        format_name: row.get(9)?,
        created_at: row.get(10)?,
    })
}

pub fn list_media(conn: &Connection, project_id: i64) -> AppResult<Vec<Media>> {
    let mut stmt = conn.prepare(
        "SELECT id, project_id, path, name, duration_ms, width, height, has_audio,
                size_bytes, format_name, created_at
         FROM media WHERE project_id = ?1 ORDER BY id DESC",
    )?;
    let rows = stmt.query_map(params![project_id], row_to_media)?;
    let mut out = Vec::new();
    for item in rows {
        out.push(item?);
    }
    Ok(out)
}

pub fn remove_media(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM media WHERE id = ?1", params![id])?;
    Ok(())
}

// ---------- segment ----------

pub fn clear_segments(conn: &Connection, media_id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM segment WHERE media_id = ?1", params![media_id])?;
    Ok(())
}

pub fn insert_segment(
    conn: &Connection,
    media_id: i64,
    index_no: i64,
    start_ms: i64,
    end_ms: i64,
    out_path: &str,
    size_bytes: i64,
    status: &str,
    message: &str,
) -> AppResult<i64> {
    conn.execute(
        "INSERT INTO segment(media_id, index_no, start_ms, end_ms, out_path, size_bytes, status, message)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![media_id, index_no, start_ms, end_ms, out_path, size_bytes, status, message],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn list_segments(conn: &Connection, media_id: i64) -> AppResult<Vec<Segment>> {
    let mut stmt = conn.prepare(
        "SELECT id, media_id, index_no, start_ms, end_ms, out_path, size_bytes, status, message
         FROM segment WHERE media_id = ?1 ORDER BY index_no ASC",
    )?;
    let rows = stmt.query_map(params![media_id], |row| {
        Ok(Segment {
            id: row.get(0)?,
            media_id: row.get(1)?,
            index_no: row.get(2)?,
            start_ms: row.get(3)?,
            end_ms: row.get(4)?,
            out_path: row.get(5)?,
            size_bytes: row.get(6)?,
            status: row.get(7)?,
            message: row.get(8)?,
        })
    })?;
    let mut out = Vec::new();
    for item in rows {
        out.push(item?);
    }
    Ok(out)
}

// ---------- cue（字幕） ----------

pub fn list_cues(conn: &Connection, media_id: i64) -> AppResult<Vec<Cue>> {
    let mut stmt = conn.prepare(
        "SELECT id, media_id, index_no, start_ms, end_ms, text
         FROM cue WHERE media_id = ?1 ORDER BY start_ms ASC, id ASC",
    )?;
    let rows = stmt.query_map(params![media_id], |row| {
        Ok(Cue {
            id: row.get(0)?,
            media_id: row.get(1)?,
            index_no: row.get(2)?,
            start_ms: row.get(3)?,
            end_ms: row.get(4)?,
            text: row.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for item in rows {
        out.push(item?);
    }
    Ok(out)
}

pub fn cue_count(conn: &Connection, media_id: i64) -> AppResult<i64> {
    let count = conn.query_row(
        "SELECT COUNT(*) FROM cue WHERE media_id = ?1",
        params![media_id],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// 整体替换某个素材的字幕。走事务：要么全换，要么不动，不会留下半份字幕。
pub fn replace_cues(conn: &mut Connection, media_id: i64, cues: &[Cue]) -> AppResult<i64> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM cue WHERE media_id = ?1", params![media_id])?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO cue(media_id, index_no, start_ms, end_ms, text)
             VALUES(?1, ?2, ?3, ?4, ?5)",
        )?;
        for (i, cue) in cues.iter().enumerate() {
            stmt.execute(params![
                media_id,
                i as i64 + 1,
                cue.start_ms,
                cue.end_ms,
                cue.text
            ])?;
        }
    }
    tx.commit()?;
    Ok(cues.len() as i64)
}

pub fn update_cue_text(conn: &Connection, id: i64, text: &str) -> AppResult<()> {
    let changed = conn.execute("UPDATE cue SET text = ?2 WHERE id = ?1", params![id, text])?;
    if changed == 0 {
        return Err(crate::error::AppError::Msg(format!("字幕 {id} 不存在")));
    }
    Ok(())
}

pub fn delete_cue(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM cue WHERE id = ?1", params![id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::MediaProbe;
    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn migration_is_idempotent_and_sets_version() {
        let conn = mem();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn project_round_trips() {
        let conn = mem();
        let p = create_project(&conn, "测试项目", "E:/tmp").unwrap();
        assert!(p.id > 0);
        let all = list_projects(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "测试项目");
    }

    #[test]
    fn media_round_trips_and_cascades() {
        let conn = mem();
        let p = create_project(&conn, "p", "").unwrap();
        let probe = MediaProbe {
            duration_ms: 2_400_000,
            width: 1920,
            height: 1080,
            has_audio: true,
            size_bytes: 123,
            format_name: "mov,mp4".into(),
        };
        let m = add_media(&conn, p.id, "E:/a.mp4", "a.mp4", &probe).unwrap();
        assert_eq!(m.duration_ms, 2_400_000);
        assert!(m.has_audio);
        assert_eq!(list_media(&conn, p.id).unwrap().len(), 1);

        delete_project(&conn, p.id).unwrap();
        assert_eq!(list_media(&conn, p.id).unwrap().len(), 0);
    }

    #[test]
    fn setting_upserts() {
        let conn = mem();
        assert!(get_setting(&conn, "ffmpeg").unwrap().is_none());
        set_setting(&conn, "ffmpeg", "C:/ffmpeg.exe").unwrap();
        set_setting(&conn, "ffmpeg", "D:/ffmpeg.exe").unwrap();
        assert_eq!(
            get_setting(&conn, "ffmpeg").unwrap().unwrap(),
            "D:/ffmpeg.exe"
        );
    }

    #[test]
    fn segments_are_ordered_by_index() {
        let conn = mem();
        let p = create_project(&conn, "p", "").unwrap();
        let m = add_media(&conn, p.id, "E:/a.mp4", "a.mp4", &MediaProbe::default()).unwrap();
        insert_segment(&conn, m.id, 2, 2000, 3000, "b.mp4", 10, "ok", "").unwrap();
        insert_segment(&conn, m.id, 1, 0, 1000, "a.mp4", 10, "ok", "").unwrap();
        let segs = list_segments(&conn, m.id).unwrap();
        assert_eq!(segs.iter().map(|s| s.index_no).collect::<Vec<_>>(), vec![1, 2]);
    }

    fn sample_cues(media_id: i64) -> Vec<Cue> {
        vec![
            Cue {
                id: 0,
                media_id,
                index_no: 0,
                start_ms: 0,
                end_ms: 1_000,
                text: "一".into(),
            },
            Cue {
                id: 0,
                media_id,
                index_no: 0,
                start_ms: 1_000,
                end_ms: 2_000,
                text: "二".into(),
            },
        ]
    }

    #[test]
    fn cues_are_replaced_as_a_whole_and_cascade_deleted() {
        let mut conn = mem();
        let p = create_project(&conn, "p", "").unwrap();
        let m = add_media(&conn, p.id, "E:/a.mp4", "a.mp4", &MediaProbe::default()).unwrap();

        let cues = sample_cues(m.id);
        assert_eq!(replace_cues(&mut conn, m.id, &cues).unwrap(), 2);
        assert_eq!(cue_count(&conn, m.id).unwrap(), 2);

        // 再存一次是整体替换，不是追加；序号重新从 1 开始
        assert_eq!(replace_cues(&mut conn, m.id, &cues[..1]).unwrap(), 1);
        let stored = list_cues(&conn, m.id).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].index_no, 1);
        assert_eq!(stored[0].text, "一");

        // 删素材连带删字幕
        remove_media(&conn, m.id).unwrap();
        assert_eq!(cue_count(&conn, m.id).unwrap(), 0);
    }

    #[test]
    fn update_missing_cue_is_an_error_not_a_silent_noop() {
        let mut conn = mem();
        let p = create_project(&conn, "p", "").unwrap();
        let m = add_media(&conn, p.id, "E:/a.mp4", "a.mp4", &MediaProbe::default()).unwrap();
        replace_cues(&mut conn, m.id, &sample_cues(m.id)).unwrap();
        let id = list_cues(&conn, m.id).unwrap()[0].id;

        update_cue_text(&conn, id, "改过了").unwrap();
        assert_eq!(list_cues(&conn, m.id).unwrap()[0].text, "改过了");

        let err = update_cue_text(&conn, 99_999, "x").unwrap_err().to_string();
        assert!(err.contains("不存在"), "实际：{err}");
    }

    #[test]
    fn v1_database_upgrades_to_v2_without_losing_data() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        // 只建 v1 schema，模拟旧版本留下的库
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn.execute_batch("PRAGMA user_version = 1;").unwrap();
        create_project(&conn, "旧项目", "E:/old").unwrap();

        migrate(&conn).unwrap();

        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert_eq!(list_projects(&conn).unwrap()[0].name, "旧项目");
        assert_eq!(cue_count(&conn, 1).unwrap(), 0, "新表应当可用");
    }
}
