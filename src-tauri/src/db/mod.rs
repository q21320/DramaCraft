//! SQLite 存取层。
//!
//! 迁移策略：只前进不回退，靠 `PRAGMA user_version` 判断。旧库能直接打开。

use crate::error::AppResult;
use crate::models::{
    Asset, AssetSummary, Cue, Job, JobCounts, Media, MediaProbe, OutputEpisode, OutputSlice,
    PipelineDrama, PipelineRun, Project, PublishAccount, PublishPlan, Segment,
};
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

/// v3：记录画面处理前的原始文件，让「还原」成为可能。
const SCHEMA_V3: &str = r#"
ALTER TABLE segment ADD COLUMN original_path TEXT NOT NULL DEFAULT '';
"#;

/// v4：发布台账。每个素材在每个平台一条记录。
const SCHEMA_V4: &str = r#"
CREATE TABLE IF NOT EXISTS publish_plan (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    media_id    INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    platform    TEXT    NOT NULL,
    title       TEXT    NOT NULL DEFAULT '',
    description TEXT    NOT NULL DEFAULT '',
    tags        TEXT    NOT NULL DEFAULT '',
    cover_path  TEXT    NOT NULL DEFAULT '',
    status      TEXT    NOT NULL DEFAULT 'draft',
    note        TEXT    NOT NULL DEFAULT '',
    updated_at  TEXT    NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_publish_plan_media_platform
    ON publish_plan(media_id, platform);
"#;

/// v5：任务队列。
const SCHEMA_V5: &str = r#"
CREATE TABLE IF NOT EXISTS job (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    media_id   INTEGER,
    kind       TEXT    NOT NULL,
    payload    TEXT    NOT NULL DEFAULT '{}',
    status     TEXT    NOT NULL DEFAULT 'pending',
    message    TEXT    NOT NULL DEFAULT '',
    attempts   INTEGER NOT NULL DEFAULT 0,
    created_at TEXT    NOT NULL,
    updated_at TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_job_status ON job(status, id);
"#;

/// v6：发布台账增加「账号」维度 —— 同一平台可以有多个账号，各自独立记账。
const SCHEMA_V6: &str = r#"
ALTER TABLE publish_plan ADD COLUMN account TEXT NOT NULL DEFAULT '';
DROP INDEX IF EXISTS idx_publish_plan_media_platform;
CREATE UNIQUE INDEX IF NOT EXISTS idx_publish_plan_media_platform_account
    ON publish_plan(media_id, platform, account);
"#;

/// v7：用户自备素材库。
const SCHEMA_V7: &str = r#"
CREATE TABLE IF NOT EXISTS asset (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT    NOT NULL,
    name        TEXT    NOT NULL,
    path        TEXT    NOT NULL,
    category    TEXT    NOT NULL DEFAULT '',
    tags        TEXT    NOT NULL DEFAULT '',
    duration_ms INTEGER NOT NULL DEFAULT 0,
    size_bytes  INTEGER NOT NULL DEFAULT 0,
    meta        TEXT    NOT NULL DEFAULT '',
    note        TEXT    NOT NULL DEFAULT '',
    created_at  TEXT    NOT NULL,
    updated_at  TEXT    NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_asset_path ON asset(path);
CREATE INDEX IF NOT EXISTS idx_asset_kind ON asset(kind, category, name);
"#;

/// v8：流水线。一次运行 = 一个源目录；一部剧一行，记录它走到哪一步。
const SCHEMA_V8: &str = r#"
CREATE TABLE IF NOT EXISTS pipeline_run (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    root_dir    TEXT    NOT NULL,
    out_dir     TEXT    NOT NULL DEFAULT '',
    stages      TEXT    NOT NULL DEFAULT '[]',
    status      TEXT    NOT NULL DEFAULT 'running',
    created_at  TEXT    NOT NULL,
    updated_at  TEXT    NOT NULL
);

CREATE TABLE IF NOT EXISTS pipeline_drama (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id        INTEGER NOT NULL REFERENCES pipeline_run(id) ON DELETE CASCADE,
    name          TEXT    NOT NULL,
    src_dir       TEXT    NOT NULL,
    out_dir       TEXT    NOT NULL DEFAULT '',
    episode_count INTEGER NOT NULL DEFAULT 0,
    stage         TEXT    NOT NULL DEFAULT '',
    status        TEXT    NOT NULL DEFAULT 'pending',
    message       TEXT    NOT NULL DEFAULT '',
    updated_at    TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_pipeline_drama_run ON pipeline_drama(run_id, id);

-- 任务要知道自己属于哪部剧，流水线才能显示进度
ALTER TABLE job ADD COLUMN pipeline_drama_id INTEGER;
"#;

/// 当前 schema 版本。加表/加列时递增，并在 `migrate` 里补一段。
pub const SCHEMA_VERSION: i64 = 8;

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
    if version < 3 {
        conn.execute_batch(SCHEMA_V3)?;
    }
    if version < 4 {
        conn.execute_batch(SCHEMA_V4)?;
    }
    if version < 5 {
        conn.execute_batch(SCHEMA_V5)?;
    }
    if version < 6 {
        conn.execute_batch(SCHEMA_V6)?;
    }
    if version < 7 {
        conn.execute_batch(SCHEMA_V7)?;
    }
    if version < 8 {
        conn.execute_batch(SCHEMA_V8)?;
    }
    if version < SCHEMA_VERSION {
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
    }
    Ok(())
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 供其他模块的测试快速建一份可用 schema（`migrate` 本身不对外）。
#[cfg(test)]
pub fn migrate_for_tests(conn: &Connection) -> AppResult<()> {
    migrate(conn)
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
        "SELECT id, media_id, index_no, start_ms, end_ms, out_path, size_bytes, status, message,
                original_path
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
            original_path: row.get(9)?,
        })
    })?;
    let mut out = Vec::new();
    for item in rows {
        out.push(item?);
    }
    Ok(out)
}

/// 画面处理后更新片段指向的文件。
///
/// `original_path` 只在**第一次**处理时写入：重复处理不会把中间产物当成"原始文件"。
pub fn update_segment_output(
    conn: &Connection,
    id: i64,
    out_path: &str,
    size_bytes: i64,
) -> AppResult<()> {
    let changed = conn.execute(
        "UPDATE segment
            SET original_path = CASE WHEN original_path = '' THEN out_path ELSE original_path END,
                out_path = ?2,
                size_bytes = ?3
          WHERE id = ?1",
        params![id, out_path, size_bytes],
    )?;
    if changed == 0 {
        return Err(crate::error::AppError::Msg(format!("片段 {id} 不存在")));
    }
    Ok(())
}

/// 还原成处理前的文件（单个片段）。
///
/// 当前批量入口走 [`revert_segments`]，这个单条版本保留给后续按片段操作使用。
#[allow(dead_code)]
pub fn revert_segment(conn: &Connection, id: i64) -> AppResult<()> {
    let changed = conn.execute(
        "UPDATE segment
            SET out_path = original_path, original_path = ''
          WHERE id = ?1 AND original_path <> ''",
        params![id],
    )?;
    if changed == 0 {
        return Ok(()); // 没处理过的片段，还原是空操作
    }
    Ok(())
}

pub fn revert_segments(conn: &Connection, media_id: i64) -> AppResult<i64> {
    let changed = conn.execute(
        "UPDATE segment
            SET out_path = original_path, original_path = ''
          WHERE media_id = ?1 AND original_path <> ''",
        params![media_id],
    )?;
    Ok(changed as i64)
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

/// 只数条数，比拉全量轻。当前前端走 `list_cues`，保留给后续统计与测试使用。
#[allow(dead_code)]
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

/// 改单条字幕文本。当前前端保存时整体覆盖（`replace_cues`），
/// 这个单条版本保留给后续「就地编辑单条」的交互。
#[allow(dead_code)]
pub fn update_cue_text(conn: &Connection, id: i64, text: &str) -> AppResult<()> {
    let changed = conn.execute("UPDATE cue SET text = ?2 WHERE id = ?1", params![id, text])?;
    if changed == 0 {
        return Err(crate::error::AppError::Msg(format!("字幕 {id} 不存在")));
    }
    Ok(())
}

// ---------- publish_plan（发布台账） ----------

fn row_to_publish_plan(row: &rusqlite::Row<'_>) -> rusqlite::Result<PublishPlan> {
    Ok(PublishPlan {
        id: row.get(0)?,
        media_id: row.get(1)?,
        platform: row.get(2)?,
        account: row.get(3)?,
        title: row.get(4)?,
        description: row.get(5)?,
        tags: row.get(6)?,
        cover_path: row.get(7)?,
        status: row.get(8)?,
        note: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

const PUBLISH_PLAN_COLUMNS: &str = "id, media_id, platform, account, title, description, tags, \
                                    cover_path, status, note, updated_at";

pub fn list_publish_plans(conn: &Connection, media_id: i64) -> AppResult<Vec<PublishPlan>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {PUBLISH_PLAN_COLUMNS} FROM publish_plan
          WHERE media_id = ?1 ORDER BY platform ASC, account ASC"
    ))?;
    let rows = stmt.query_map(params![media_id], row_to_publish_plan)?;
    let mut out = Vec::new();
    for item in rows {
        out.push(item?);
    }
    Ok(out)
}

pub fn get_publish_plan(
    conn: &Connection,
    media_id: i64,
    platform: &str,
    account: &str,
) -> AppResult<Option<PublishPlan>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {PUBLISH_PLAN_COLUMNS} FROM publish_plan
          WHERE media_id = ?1 AND platform = ?2 AND account = ?3"
    ))?;
    let mut rows = stmt.query(params![media_id, platform, account])?;
    match rows.next()? {
        Some(row) => Ok(Some(row_to_publish_plan(row)?)),
        None => Ok(None),
    }
}

/// 保存发布设置（同一素材 + 平台 + 账号只有一条，存在则整体更新）。
pub fn upsert_publish_plan(conn: &Connection, plan: &PublishPlan) -> AppResult<PublishPlan> {
    let updated_at = now();
    conn.execute(
        "INSERT INTO publish_plan(media_id, platform, account, title, description, tags,
                                  cover_path, status, note, updated_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(media_id, platform, account) DO UPDATE SET
             title       = excluded.title,
             description = excluded.description,
             tags        = excluded.tags,
             cover_path  = excluded.cover_path,
             status      = excluded.status,
             note        = excluded.note,
             updated_at  = excluded.updated_at",
        params![
            plan.media_id,
            plan.platform,
            plan.account,
            plan.title,
            plan.description,
            plan.tags,
            plan.cover_path,
            plan.status,
            plan.note,
            updated_at
        ],
    )?;
    get_publish_plan(conn, plan.media_id, &plan.platform, &plan.account)?
        .ok_or_else(|| crate::error::AppError::Msg("保存发布设置失败：写回后读不到".into()))
}

/// 只改状态与备注，用于「标记为已发布 / 失败」。
pub fn set_publish_status(
    conn: &Connection,
    media_id: i64,
    platform: &str,
    account: &str,
    status: &str,
    note: &str,
) -> AppResult<()> {
    let updated_at = now();
    let changed = conn.execute(
        "UPDATE publish_plan SET status = ?4, note = ?5, updated_at = ?6
          WHERE media_id = ?1 AND platform = ?2 AND account = ?3",
        params![media_id, platform, account, status, note, updated_at],
    )?;
    if changed == 0 {
        return Err(crate::error::AppError::Msg(
            "该平台 / 账号还没有发布设置，请先填写并保存".into(),
        ));
    }
    Ok(())
}

/// 台账总览：按「平台 × 账号」汇总。
pub fn list_publish_accounts(conn: &Connection) -> AppResult<Vec<PublishAccount>> {
    let mut stmt = conn.prepare(
        "SELECT platform, account,
                COUNT(*) AS total,
                SUM(CASE WHEN status = 'draft'     THEN 1 ELSE 0 END),
                SUM(CASE WHEN status = 'ready'     THEN 1 ELSE 0 END),
                SUM(CASE WHEN status = 'published' THEN 1 ELSE 0 END),
                SUM(CASE WHEN status = 'failed'    THEN 1 ELSE 0 END),
                MAX(updated_at)
           FROM publish_plan
          GROUP BY platform, account
          ORDER BY platform ASC, account ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(PublishAccount {
            platform: row.get(0)?,
            platform_label: String::new(),
            account: row.get(1)?,
            total: row.get(2)?,
            draft: row.get(3)?,
            ready: row.get(4)?,
            published: row.get(5)?,
            failed: row.get(6)?,
            last_updated: row.get(7)?,
        })
    })?;
    let mut out = Vec::new();
    for item in rows {
        out.push(item?);
    }
    Ok(out)
}

// ---------- pipeline（流水线） ----------

pub const RUN_RUNNING: &str = "running";
pub const RUN_DONE: &str = "done";
pub const RUN_FAILED: &str = "failed";
pub const RUN_CANCELED: &str = "canceled";

const DRAMA_COLUMNS: &str =
    "id, run_id, name, src_dir, out_dir, episode_count, stage, status, message, updated_at";

fn row_to_pipeline_drama(row: &rusqlite::Row<'_>) -> rusqlite::Result<PipelineDrama> {
    Ok(PipelineDrama {
        id: row.get(0)?,
        run_id: row.get(1)?,
        name: row.get(2)?,
        src_dir: row.get(3)?,
        out_dir: row.get(4)?,
        episode_count: row.get(5)?,
        stage: row.get(6)?,
        status: row.get(7)?,
        message: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

pub fn create_pipeline_run(
    conn: &Connection,
    root_dir: &str,
    out_dir: &str,
    stages: &[String],
) -> AppResult<i64> {
    let now = now();
    conn.execute(
        "INSERT INTO pipeline_run(root_dir, out_dir, stages, status, created_at, updated_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?5)",
        params![
            root_dir,
            out_dir,
            serde_json::to_string(stages)?,
            RUN_RUNNING,
            now
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn add_pipeline_drama(
    conn: &Connection,
    run_id: i64,
    name: &str,
    src_dir: &str,
    out_dir: &str,
    episode_count: i64,
) -> AppResult<i64> {
    conn.execute(
        "INSERT INTO pipeline_drama(run_id, name, src_dir, out_dir, episode_count, stage, status, message, updated_at)
         VALUES(?1, ?2, ?3, ?4, ?5, '', 'pending', '', ?6)",
        params![run_id, name, src_dir, out_dir, episode_count, now()],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_pipeline_drama(
    conn: &Connection,
    id: i64,
    stage: &str,
    status: &str,
    message: &str,
) -> AppResult<()> {
    conn.execute(
        "UPDATE pipeline_drama SET stage = ?2, status = ?3, message = ?4, updated_at = ?5
          WHERE id = ?1",
        params![id, stage, status, message, now()],
    )?;
    Ok(())
}

pub fn set_pipeline_run_status(conn: &Connection, id: i64, status: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE pipeline_run SET status = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, status, now()],
    )?;
    Ok(())
}

pub fn list_pipeline_runs(conn: &Connection, limit: i64) -> AppResult<Vec<PipelineRun>> {
    let mut stmt = conn.prepare(
        "SELECT id, root_dir, out_dir, stages, status, created_at, updated_at
           FROM pipeline_run ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit.clamp(1, 200)], |row| {
        Ok(PipelineRun {
            id: row.get(0)?,
            root_dir: row.get(1)?,
            out_dir: row.get(2)?,
            stages: row.get(3)?,
            status: row.get(4)?,
            drama_count: 0,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
            dramas: Vec::new(),
        })
    })?;
    let mut out = Vec::new();
    for item in rows {
        let mut run = item?;
        run.dramas = list_pipeline_dramas(conn, run.id)?;
        run.drama_count = run.dramas.len() as i64;
        out.push(run);
    }
    Ok(out)
}

pub fn list_pipeline_dramas(conn: &Connection, run_id: i64) -> AppResult<Vec<PipelineDrama>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {DRAMA_COLUMNS} FROM pipeline_drama WHERE run_id = ?1 ORDER BY id ASC"
    ))?;
    let rows = stmt.query_map(params![run_id], row_to_pipeline_drama)?;
    let mut out = Vec::new();
    for item in rows {
        out.push(item?);
    }
    Ok(out)
}

pub fn get_pipeline_run(conn: &Connection, id: i64) -> AppResult<Option<PipelineRun>> {
    let runs = list_pipeline_runs(conn, 200)?;
    Ok(runs.into_iter().find(|r| r.id == id))
}

/// 把没跑完的流水线退回"待跑"状态。
///
/// 和队列一样：进程被杀之后，`running` 是个假状态，没有人在跑它。
///
/// 文案要説清楚"会自己接着跑"，不能只说"退回待跑" —— 之前写的是
/// 「上次运行被中断，已退回待跑」，用户看了以为要手动重启，问我
/// "没办法续接上嘛"。实际上队列起来就会接着干。
pub fn recover_interrupted_pipelines(conn: &Connection) -> AppResult<i64> {
    let changed = conn.execute(
        "UPDATE pipeline_drama SET status = 'pending', message = '上次被中断，排队的任务会自动接着跑', updated_at = ?1
          WHERE status = 'running'",
        params![now()],
    )?;
    Ok(changed as i64)
}

/// 一部剧的所有任务都结束了，就把这条流水线的状态收口。
///
/// 不这么做的话，运行状态会永远停在 `running` —— 界面上看着像还在跑，
/// 其实早就没事干了。
pub fn refresh_pipeline_run_status(conn: &Connection) -> AppResult<i64> {
    let mut stmt = conn.prepare("SELECT id FROM pipeline_run WHERE status = ?1")?;
    let ids: Vec<i64> = stmt
        .query_map(params![RUN_RUNNING], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    drop(stmt);

    let mut updated = 0i64;
    for id in ids {
        let dramas = list_pipeline_dramas(conn, id)?;
        if dramas.is_empty() {
            continue;
        }
        // 还有 pending / running 的就没跑完
        if dramas
            .iter()
            .any(|d| d.status == "pending" || d.status == "running")
        {
            continue;
        }
        let failed = dramas.iter().any(|d| d.status == "failed");
        set_pipeline_run_status(conn, id, if failed { RUN_FAILED } else { RUN_DONE })?;
        updated += 1;
    }
    Ok(updated)
}

/// 一次流水线跑出来的东西，按「集 → 切片」组织，每片带上它的字幕。
///
/// 链路是：pipeline_drama.src_dir ↔ project.root_dir ↔ media.project_id ↔ segment.media_id
/// —— 没有在 pipeline_drama 上存 project_id，但 src_dir 就是当初建工程时
/// 用的那个目录，等价且不用加列。
pub fn run_outputs(conn: &Connection, run_id: i64) -> AppResult<Vec<OutputEpisode>> {
    let dramas = list_pipeline_dramas(conn, run_id)?;
    let mut out = Vec::new();

    for drama in dramas {
        let mut stmt = conn.prepare(
            "SELECT m.id, m.name, m.duration_ms, m.width, m.height
               FROM media m
               JOIN project p ON p.id = m.project_id
              WHERE p.root_dir = ?1
              ORDER BY m.id ASC",
        )?;
        let medias: Vec<(i64, String, i64, i64, i64)> = stmt
            .query_map(params![drama.src_dir], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);

        for (media_id, name, duration_ms, width, height) in medias {
            let mut seg_stmt = conn.prepare(
                "SELECT id, index_no, COALESCE(NULLIF(original_path,''), out_path), size_bytes,
                        start_ms, end_ms, status, message, out_path
                   FROM segment WHERE media_id = ?1 ORDER BY index_no ASC",
            )?;
            let segs: Vec<(i64, i64, String, i64, i64, i64, String, String, String)> = seg_stmt
                .query_map(params![media_id], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(seg_stmt);

            let mut slices = Vec::new();
            for (id, index_no, path, size, start_ms, end_ms, status, message, out_path) in segs {
                // 这一段的字幕
                let mut cue_stmt = conn.prepare(
                    "SELECT text FROM cue WHERE media_id = ?1 AND start_ms >= ?2 AND start_ms < ?3
                      ORDER BY start_ms ASC",
                )?;
                let cues: Vec<String> = cue_stmt
                    .query_map(params![media_id, start_ms, end_ms], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                drop(cue_stmt);

                // 处理后优先给处理版；处理版没了就退回原片
                let shown = if !out_path.is_empty() && std::path::Path::new(&out_path).is_file() {
                    out_path
                } else {
                    path
                };

                slices.push(OutputSlice {
                    segment_id: id,
                    index_no,
                    path: shown,
                    size_bytes: size,
                    start_ms,
                    end_ms,
                    status,
                    message,
                    cues,
                });
            }

            out.push(OutputEpisode {
                media_id,
                name,
                duration_ms,
                width,
                height,
                slices,
            });
        }
    }
    Ok(out)
}

// ---------- asset（用户自备素材库） ----------

const ASSET_COLUMNS: &str = "id, kind, name, path, category, tags, duration_ms, size_bytes, \
                             meta, note, created_at, updated_at";

fn row_to_asset(row: &rusqlite::Row<'_>) -> rusqlite::Result<Asset> {
    Ok(Asset {
        id: row.get(0)?,
        kind: row.get(1)?,
        name: row.get(2)?,
        path: row.get(3)?,
        category: row.get(4)?,
        tags: row.get(5)?,
        duration_ms: row.get(6)?,
        size_bytes: row.get(7)?,
        meta: row.get(8)?,
        note: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

/// 按路径去重：同一个文件重复导入是更新，不是新增。
pub fn upsert_asset(conn: &Connection, asset: &Asset) -> AppResult<Asset> {
    let now = now();
    conn.execute(
        "INSERT INTO asset(kind, name, path, category, tags, duration_ms, size_bytes, meta, note,
                           created_at, updated_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
         ON CONFLICT(path) DO UPDATE SET
             kind        = excluded.kind,
             name        = excluded.name,
             category    = excluded.category,
             tags        = excluded.tags,
             duration_ms = excluded.duration_ms,
             size_bytes  = excluded.size_bytes,
             meta        = excluded.meta,
             note        = excluded.note,
             updated_at  = excluded.updated_at",
        params![
            asset.kind,
            asset.name,
            asset.path,
            asset.category,
            asset.tags,
            asset.duration_ms,
            asset.size_bytes,
            asset.meta,
            asset.note,
            now
        ],
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {ASSET_COLUMNS} FROM asset WHERE path = ?1"
    ))?;
    let mut rows = stmt.query(params![asset.path])?;
    match rows.next()? {
        Some(row) => Ok(row_to_asset(row)?),
        None => Err(crate::error::AppError::Msg(
            "保存素材失败：写回后读不到".into(),
        )),
    }
}

pub fn list_assets(conn: &Connection, kind: Option<&str>) -> AppResult<Vec<Asset>> {
    let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = match kind {
        Some(k) => (
            format!(
                "SELECT {ASSET_COLUMNS} FROM asset WHERE kind = ?1
                 ORDER BY category ASC, name ASC"
            ),
            vec![Box::new(k.to_string())],
        ),
        None => (
            format!(
                "SELECT {ASSET_COLUMNS} FROM asset ORDER BY kind ASC, category ASC, name ASC"
            ),
            vec![],
        ),
    };
    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(refs.as_slice(), row_to_asset)?;
    let mut out = Vec::new();
    for item in rows {
        out.push(item?);
    }
    Ok(out)
}

pub fn get_asset(conn: &Connection, id: i64) -> AppResult<Asset> {
    let asset = conn.query_row(
        &format!("SELECT {ASSET_COLUMNS} FROM asset WHERE id = ?1"),
        params![id],
        row_to_asset,
    )?;
    Ok(asset)
}

/// 只改用户可编辑的字段（名称/分类/标签/备注），路径与大小由导入决定。
pub fn update_asset(conn: &Connection, id: i64, name: &str, category: &str, tags: &str, note: &str) -> AppResult<Asset> {
    let changed = conn.execute(
        "UPDATE asset SET name = ?2, category = ?3, tags = ?4, note = ?5, updated_at = ?6 WHERE id = ?1",
        params![id, name, category, tags, note, now()],
    )?;
    if changed == 0 {
        return Err(crate::error::AppError::Msg("这条素材不存在".into()));
    }
    get_asset(conn, id)
}

/// 从库里移除记录。**不会删磁盘上的文件** —— 那是用户自己的素材。
pub fn delete_asset(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM asset WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn asset_summary(conn: &Connection) -> AppResult<AssetSummary> {
    let mut summary = AssetSummary::default();
    let mut stmt = conn.prepare("SELECT kind, COUNT(*), COALESCE(SUM(size_bytes), 0) FROM asset GROUP BY kind")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    for item in rows {
        let (kind, count, bytes) = item?;
        summary.total += count;
        summary.total_bytes += bytes;
        match kind.as_str() {
            "audio" => summary.audio = count,
            "lut" => summary.lut = count,
            _ => {}
        }
    }
    Ok(summary)
}

// ---------- job（任务队列） ----------

pub const JOB_PENDING: &str = "pending";
pub const JOB_RUNNING: &str = "running";
pub const JOB_DONE: &str = "done";
pub const JOB_FAILED: &str = "failed";
pub const JOB_CANCELED: &str = "canceled";

const JOB_COLUMNS: &str =
    "id, media_id, kind, payload, status, message, attempts, created_at, updated_at, pipeline_drama_id";

fn row_to_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    Ok(Job {
        id: row.get(0)?,
        media_id: row.get(1)?,
        kind: row.get(2)?,
        payload: row.get(3)?,
        status: row.get(4)?,
        message: row.get(5)?,
        attempts: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        pipeline_drama_id: row.get(9)?,
    })
}

pub fn enqueue_job(
    conn: &Connection,
    media_id: Option<i64>,
    kind: &str,
    payload: &str,
) -> AppResult<Job> {
    enqueue_job_for(conn, media_id, kind, payload, None)
}

/// 入队，并可选地挂在某部剧下（流水线用）。
///
/// 入队前**先验一遍参数** —— 参数写错应当当场报，而不是排进队列、
/// 跑到它的时候才炸（真实素材那轮 32 个切片任务就是这么全废的）。
pub fn enqueue_job_for(
    conn: &Connection,
    media_id: Option<i64>,
    kind: &str,
    payload: &str,
    pipeline_drama_id: Option<i64>,
) -> AppResult<Job> {
    crate::queue::validate_payload(kind, payload)?;
    let now = now();
    conn.execute(
        "INSERT INTO job(media_id, kind, payload, status, message, attempts, created_at, updated_at, pipeline_drama_id)
         VALUES(?1, ?2, ?3, 'pending', '', 0, ?4, ?4, ?5)",
        params![media_id, kind, payload, now, pipeline_drama_id],
    )?;
    get_job(conn, conn.last_insert_rowid())
}

pub fn get_job(conn: &Connection, id: i64) -> AppResult<Job> {
    let job = conn.query_row(
        &format!("SELECT {JOB_COLUMNS} FROM job WHERE id = ?1"),
        params![id],
        row_to_job,
    )?;
    Ok(job)
}

/// 列表：正在跑的排最前，然后按入队顺序。
pub fn list_jobs(conn: &Connection, limit: i64) -> AppResult<Vec<Job>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {JOB_COLUMNS} FROM job
         ORDER BY CASE status WHEN 'running' THEN 0 WHEN 'pending' THEN 1 ELSE 2 END, id ASC
         LIMIT ?1"
    ))?;
    let rows = stmt.query_map(params![limit.clamp(1, 2000)], row_to_job)?;
    let mut out = Vec::new();
    for item in rows {
        out.push(item?);
    }
    Ok(out)
}

/// 取下一条待跑任务（先入先出）。
pub fn next_pending_job(conn: &Connection) -> AppResult<Option<Job>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {JOB_COLUMNS} FROM job WHERE status = 'pending' ORDER BY id ASC LIMIT 1"
    ))?;
    let mut rows = stmt.query([])?;
    match rows.next()? {
        Some(row) => Ok(Some(row_to_job(row)?)),
        None => Ok(None),
    }
}

pub fn mark_job_running(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute(
        "UPDATE job SET status = 'running', message = '', attempts = attempts + 1, updated_at = ?2
          WHERE id = ?1",
        params![id, now()],
    )?;
    Ok(())
}

pub fn finish_job(conn: &Connection, id: i64, status: &str, message: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE job SET status = ?2, message = ?3, updated_at = ?4 WHERE id = ?1",
        params![id, status, message, now()],
    )?;
    Ok(())
}

/// 把「正在跑」的任务退回待跑。**这是断点续跑的关键一步。**
///
/// 进程被杀或崩溃后，库里会留下 `running` —— 那是假状态：没有人在跑它。
/// 启动时必须退回去，否则这些任务永远不会再被拾起，界面上还会一直显示"进行中"。
pub fn recover_interrupted_jobs(conn: &Connection) -> AppResult<i64> {
    let changed = conn.execute(
        "UPDATE job SET status = 'pending', message = '上次运行被中断，已退回队列', updated_at = ?1
          WHERE status = 'running'",
        params![now()],
    )?;
    Ok(changed as i64)
}

pub fn cancel_pending_jobs(conn: &Connection) -> AppResult<i64> {
    let changed = conn.execute(
        "UPDATE job SET status = 'canceled', message = '已取消', updated_at = ?1
          WHERE status = 'pending'",
        params![now()],
    )?;
    Ok(changed as i64)
}

/// 撤掉某部剧还没跑的任务（流水线取消时用）。
pub fn cancel_jobs_of_drama(conn: &Connection, drama_id: i64) -> AppResult<i64> {
    let changed = conn.execute(
        "UPDATE job SET status = 'canceled', message = '流水线已取消', updated_at = ?1
          WHERE status = 'pending' AND pipeline_drama_id = ?2",
        params![now(), drama_id],
    )?;
    Ok(changed as i64)
}

/// 某部剧还有多少没跑的任务。
///
/// 用来判断「这部剧做完了没有」—— 任务全跑完就该把剧目标成完成，
/// 否则它会永远停在「进行中」。真实素材那轮就是这样：
/// 64 个任务早在两小时前就跑完了，界面上却显示还在跑，用户以为卡死。
pub fn count_pending_jobs_of_drama(conn: &Connection, drama_id: i64) -> AppResult<i64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM job WHERE pipeline_drama_id = ?1 AND status IN ('pending', 'running')",
        params![drama_id],
        |row| row.get(0),
    )?;
    Ok(n)
}

/// 清掉已结束的任务（done / failed / canceled），返回删除条数。
pub fn clear_finished_jobs(conn: &Connection) -> AppResult<i64> {
    let changed = conn.execute(
        "DELETE FROM job WHERE status IN ('done', 'failed', 'canceled')",
        [],
    )?;
    Ok(changed as i64)
}

pub fn delete_job(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute(
        "DELETE FROM job WHERE id = ?1 AND status <> 'running'",
        params![id],
    )?;
    Ok(())
}

pub fn job_counts(conn: &Connection) -> AppResult<JobCounts> {
    let mut counts = JobCounts::default();
    let mut stmt = conn.prepare("SELECT status, COUNT(*) FROM job GROUP BY status")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for item in rows {
        let (status, n) = item?;
        counts.total += n;
        match status.as_str() {
            JOB_PENDING => counts.pending = n,
            JOB_RUNNING => counts.running = n,
            JOB_DONE => counts.done = n,
            JOB_FAILED => counts.failed = n,
            JOB_CANCELED => counts.canceled = n,
            _ => {}
        }
    }
    Ok(counts)
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

    #[test]
    fn v2_database_gains_original_path_without_losing_data() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn.execute_batch(SCHEMA_V2).unwrap();
        conn.execute_batch("PRAGMA user_version = 2;").unwrap();

        let p = create_project(&conn, "旧库", "").unwrap();
        let m = add_media(&conn, p.id, "E:/a.mp4", "a.mp4", &MediaProbe::default()).unwrap();
        insert_segment(&conn, m.id, 1, 0, 1_000, "E:/out/001.mp4", 10, "ok", "").unwrap();

        migrate(&conn).unwrap();

        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        let segs = list_segments(&conn, m.id).unwrap();
        assert_eq!(segs.len(), 1, "旧数据必须保留");
        assert_eq!(segs[0].original_path, "", "新列应有默认值");
    }

    #[test]
    fn transform_updates_output_and_remembers_original_only_once() {
        let conn = mem();
        let p = create_project(&conn, "p", "").unwrap();
        let m = add_media(&conn, p.id, "E:/a.mp4", "a.mp4", &MediaProbe::default()).unwrap();
        let id = insert_segment(&conn, m.id, 1, 0, 1_000, "E:/out/001.mp4", 10, "ok", "").unwrap();

        // 第一次处理：记住原始文件
        update_segment_output(&conn, id, "E:/out/001_t.mp4", 20).unwrap();
        let seg = &list_segments(&conn, m.id).unwrap()[0];
        assert_eq!(seg.out_path, "E:/out/001_t.mp4");
        assert_eq!(seg.original_path, "E:/out/001.mp4");
        assert_eq!(seg.size_bytes, 20);

        // 第二次处理：original_path 不能被中间产物覆盖
        update_segment_output(&conn, id, "E:/out/001_t.mp4", 30).unwrap();
        let seg = &list_segments(&conn, m.id).unwrap()[0];
        assert_eq!(seg.original_path, "E:/out/001.mp4");
        assert_eq!(seg.size_bytes, 30);

        // 还原
        assert_eq!(revert_segments(&conn, m.id).unwrap(), 1);
        let seg = &list_segments(&conn, m.id).unwrap()[0];
        assert_eq!(seg.out_path, "E:/out/001.mp4");
        assert_eq!(seg.original_path, "");
        // 再还原是空操作，不该报错
        assert_eq!(revert_segments(&conn, m.id).unwrap(), 0);
    }

    #[test]
    fn updating_missing_segment_is_an_error_not_a_silent_noop() {
        let conn = mem();
        let err = update_segment_output(&conn, 99_999, "E:/x.mp4", 1)
            .unwrap_err()
            .to_string();
        assert!(err.contains("不存在"), "实际：{err}");
    }

    fn plan(media_id: i64, platform: &str, title: &str) -> PublishPlan {
        plan_for(media_id, platform, publish_account_default(), title)
    }

    fn plan_for(media_id: i64, platform: &str, account: &str, title: &str) -> PublishPlan {
        PublishPlan {
            id: 0,
            media_id,
            platform: platform.to_string(),
            account: account.to_string(),
            title: title.to_string(),
            description: "简介".into(),
            tags: "短剧,推荐".into(),
            cover_path: String::new(),
            status: "draft".into(),
            note: String::new(),
            updated_at: String::new(),
        }
    }

    fn publish_account_default() -> &'static str {
        "默认账号"
    }

    #[test]
    fn publish_plan_upserts_per_platform_and_cascades() {
        let conn = mem();
        let p = create_project(&conn, "p", "").unwrap();
        let m = add_media(&conn, p.id, "E:/a.mp4", "a.mp4", &MediaProbe::default()).unwrap();

        // 首次插入
        let saved = upsert_publish_plan(&conn, &plan(m.id, "douyin", "第一版")).unwrap();
        assert!(saved.id > 0);
        assert!(!saved.updated_at.is_empty());

        // 同平台同账号再存是更新，不是新增
        let saved = upsert_publish_plan(&conn, &plan(m.id, "douyin", "改过的标题")).unwrap();
        assert_eq!(saved.title, "改过的标题");
        assert_eq!(list_publish_plans(&conn, m.id).unwrap().len(), 1);

        // 另一个平台是另一条
        upsert_publish_plan(&conn, &plan(m.id, "wechat_channels", "视频号版")).unwrap();
        assert_eq!(list_publish_plans(&conn, m.id).unwrap().len(), 2);

        // 删素材连带删台账
        remove_media(&conn, m.id).unwrap();
        assert_eq!(list_publish_plans(&conn, m.id).unwrap().len(), 0);
    }

    #[test]
    fn same_platform_different_accounts_are_separate_rows() {
        let conn = mem();
        let p = create_project(&conn, "p", "").unwrap();
        let m = add_media(&conn, p.id, "E:/a.mp4", "a.mp4", &MediaProbe::default()).unwrap();

        upsert_publish_plan(&conn, &plan_for(m.id, "douyin", "账号甲", "甲版")).unwrap();
        upsert_publish_plan(&conn, &plan_for(m.id, "douyin", "账号乙", "乙版")).unwrap();
        upsert_publish_plan(&conn, &plan_for(m.id, "douyin", "账号甲", "甲版改")).unwrap();

        let all = list_publish_plans(&conn, m.id).unwrap();
        assert_eq!(all.len(), 2, "同平台不同账号各占一行，同账号是更新");
        let jia = get_publish_plan(&conn, m.id, "douyin", "账号甲").unwrap().unwrap();
        assert_eq!(jia.title, "甲版改");
        let yi = get_publish_plan(&conn, m.id, "douyin", "账号乙").unwrap().unwrap();
        assert_eq!(yi.title, "乙版");
        // 不存在的账号组合
        assert!(get_publish_plan(&conn, m.id, "douyin", "账号丙").unwrap().is_none());
    }

    #[test]
    fn publish_accounts_overview_aggregates_by_platform_and_account() {
        let conn = mem();
        let p = create_project(&conn, "p", "").unwrap();
        let a = add_media(&conn, p.id, "E:/a.mp4", "a.mp4", &MediaProbe::default()).unwrap();
        let b = add_media(&conn, p.id, "E:/b.mp4", "b.mp4", &MediaProbe::default()).unwrap();

        upsert_publish_plan(&conn, &plan_for(a.id, "douyin", "甲", "t1")).unwrap();
        upsert_publish_plan(&conn, &plan_for(b.id, "douyin", "甲", "t2")).unwrap();
        upsert_publish_plan(&conn, &plan_for(a.id, "douyin", "乙", "t3")).unwrap();
        set_publish_status(&conn, a.id, "douyin", "甲", "published", "").unwrap();
        set_publish_status(&conn, b.id, "douyin", "甲", "failed", "被限流").unwrap();

        let accounts = list_publish_accounts(&conn).unwrap();
        assert_eq!(accounts.len(), 2, "两个账号两行");

        let jia = accounts.iter().find(|x| x.account == "甲").unwrap();
        assert_eq!(jia.total, 2);
        assert_eq!(jia.published, 1);
        assert_eq!(jia.failed, 1);
        assert!(!jia.last_updated.is_empty());

        let yi = accounts.iter().find(|x| x.account == "乙").unwrap();
        assert_eq!(yi.total, 1);
        assert_eq!(yi.draft, 1);
    }

    #[test]
    fn publish_status_can_be_marked_and_missing_plan_is_an_error() {
        let conn = mem();
        let p = create_project(&conn, "p", "").unwrap();
        let m = add_media(&conn, p.id, "E:/a.mp4", "a.mp4", &MediaProbe::default()).unwrap();

        let err = set_publish_status(&conn, m.id, "douyin", "默认账号", "published", "")
            .unwrap_err()
            .to_string();
        assert!(err.contains("还没有发布设置"), "实际：{err}");

        upsert_publish_plan(&conn, &plan(m.id, "douyin", "t")).unwrap();
        set_publish_status(&conn, m.id, "douyin", "默认账号", "published", "已投 3 条").unwrap();
        let stored = get_publish_plan(&conn, m.id, "douyin", "默认账号")
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, "published");
        assert_eq!(stored.note, "已投 3 条");
        assert_eq!(stored.title, "t", "改状态不应动标题");
    }

    #[test]
    fn v3_database_gains_publish_plan_without_losing_data() {        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn.execute_batch(SCHEMA_V2).unwrap();
        conn.execute_batch(SCHEMA_V3).unwrap();
        conn.execute_batch("PRAGMA user_version = 3;").unwrap();
        create_project(&conn, "旧工程", "").unwrap();

        migrate(&conn).unwrap();

        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert_eq!(list_projects(&conn).unwrap()[0].name, "旧工程");
        assert_eq!(list_publish_plans(&conn, 1).unwrap().len(), 0, "新表可用");
    }

    #[test]
    fn jobs_are_first_in_first_out() {
        let conn = mem();
        let a = enqueue_job(&conn, Some(1), "slice", OK_SLICE).unwrap();
        let b = enqueue_job(&conn, Some(2), "ocr", OK_OCR).unwrap();
        assert_eq!(a.status, JOB_PENDING);
        assert_eq!(a.attempts, 0);
        assert_eq!(a.payload, OK_SLICE);

        assert_eq!(next_pending_job(&conn).unwrap().unwrap().id, a.id);

        mark_job_running(&conn, a.id).unwrap();
        let running = get_job(&conn, a.id).unwrap();
        assert_eq!(running.status, JOB_RUNNING);
        assert_eq!(running.attempts, 1, "尝试次数要累加");
        assert_eq!(
            next_pending_job(&conn).unwrap().unwrap().id,
            b.id,
            "正在跑的那条不该被再次取出"
        );
    }

    #[test]
    fn restart_returns_running_jobs_to_pending() {
        let conn = mem();
        let job = enqueue_job(&conn, None, "slice", OK_SLICE).unwrap();
        mark_job_running(&conn, job.id).unwrap();

        // 模拟进程被杀后重启
        assert_eq!(recover_interrupted_jobs(&conn).unwrap(), 1);
        let after = get_job(&conn, job.id).unwrap();
        assert_eq!(after.status, JOB_PENDING, "假的 running 必须退回待跑");
        assert!(!after.message.is_empty(), "要说明为什么退回");

        // 没有 running 时是空操作
        assert_eq!(recover_interrupted_jobs(&conn).unwrap(), 0);
    }

    #[test]
    fn finishing_jobs_and_counting() {
        let conn = mem();
        let a = enqueue_job(&conn, None, "slice", OK_SLICE).unwrap();
        let b = enqueue_job(&conn, None, "slice", OK_SLICE).unwrap();
        enqueue_job(&conn, None, "slice", OK_SLICE).unwrap();

        mark_job_running(&conn, a.id).unwrap();
        finish_job(&conn, a.id, JOB_DONE, "切了 10 片").unwrap();
        mark_job_running(&conn, b.id).unwrap();
        finish_job(&conn, b.id, JOB_FAILED, "ffmpeg 报错").unwrap();

        let counts = job_counts(&conn).unwrap();
        assert_eq!(counts.done, 1);
        assert_eq!(counts.failed, 1);
        assert_eq!(counts.pending, 1);
        assert_eq!(counts.total, 3);
        assert_eq!(get_job(&conn, a.id).unwrap().message, "切了 10 片");

        assert_eq!(clear_finished_jobs(&conn).unwrap(), 2);
        assert_eq!(job_counts(&conn).unwrap().total, 1);
    }

    #[test]
    fn cancel_pending_leaves_running_alone() {
        let conn = mem();
        let running = enqueue_job(&conn, None, "slice", OK_SLICE).unwrap();
        let waiting = enqueue_job(&conn, None, "slice", OK_SLICE).unwrap();
        mark_job_running(&conn, running.id).unwrap();

        assert_eq!(cancel_pending_jobs(&conn).unwrap(), 1);
        assert_eq!(get_job(&conn, running.id).unwrap().status, JOB_RUNNING);
        assert_eq!(get_job(&conn, waiting.id).unwrap().status, JOB_CANCELED);
    }

    #[test]
    fn running_job_cannot_be_deleted() {
        let conn = mem();
        let job = enqueue_job(&conn, None, "slice", OK_SLICE).unwrap();
        mark_job_running(&conn, job.id).unwrap();
        delete_job(&conn, job.id).unwrap();
        assert!(get_job(&conn, job.id).is_ok(), "正在跑的任务不该被删掉");

        finish_job(&conn, job.id, JOB_DONE, "").unwrap();
        delete_job(&conn, job.id).unwrap();
        assert!(get_job(&conn, job.id).is_err(), "结束后才可以删");
    }

    #[test]
    fn listing_puts_running_first() {
        let conn = mem();
        let a = enqueue_job(&conn, None, "slice", OK_SLICE).unwrap();
        let b = enqueue_job(&conn, None, "slice", OK_SLICE).unwrap();
        mark_job_running(&conn, b.id).unwrap();
        let list = list_jobs(&conn, 10).unwrap();
        assert_eq!(list[0].id, b.id, "正在跑的排最前");
        assert_eq!(list[1].id, a.id);
    }

    #[test]
    fn v4_database_gains_job_table_without_losing_data() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn.execute_batch(SCHEMA_V2).unwrap();
        conn.execute_batch(SCHEMA_V3).unwrap();
        conn.execute_batch(SCHEMA_V4).unwrap();
        conn.execute_batch("PRAGMA user_version = 4;").unwrap();
        create_project(&conn, "旧工程", "").unwrap();

        migrate(&conn).unwrap();

        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert_eq!(list_projects(&conn).unwrap()[0].name, "旧工程");
        assert_eq!(job_counts(&conn).unwrap().total, 0, "新表可用");
    }

    #[test]
    fn v5_database_gains_account_dimension_without_losing_data() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn.execute_batch(SCHEMA_V2).unwrap();
        conn.execute_batch(SCHEMA_V3).unwrap();
        conn.execute_batch(SCHEMA_V4).unwrap();
        conn.execute_batch(SCHEMA_V5).unwrap();
        conn.execute_batch("PRAGMA user_version = 5;").unwrap();

        let p = create_project(&conn, "旧工程", "").unwrap();
        let m = add_media(&conn, p.id, "E:/a.mp4", "a.mp4", &MediaProbe::default()).unwrap();
        // 旧库里的台账没有 account 列，用旧写法塞一条
        conn.execute(
            "INSERT INTO publish_plan(media_id, platform, title, description, tags, cover_path, status, note, updated_at)
             VALUES(?1, 'douyin', '老标题', '', '', '', 'published', '发过了', '2026-01-01 00:00:00')",
            params![m.id],
        )
        .unwrap();

        migrate(&conn).unwrap();

        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);

        let all = list_publish_plans(&conn, m.id).unwrap();
        assert_eq!(all.len(), 1, "旧数据必须保留");
        assert_eq!(all[0].title, "老标题");
        assert_eq!(all[0].status, "published");
        assert_eq!(all[0].account, "", "新列默认空，可由上层归一到默认账号");
    }

    /// 一个能通过校验的最小 payload。
    ///
    /// 入队时会验参数（这是有意的），所以测试里不能再用 `"{}"` 这种假数据 ——
    /// 那种写法以前能过，现在会被当场拦下。这里给的是真形状。
    const OK_SLICE: &str =
        r#"{"mediaId":1,"mode":{"mode":"equalParts","parts":3},"outDir":"E:/o"}"#;
    const OK_OCR: &str = r#"{"mediaId":2}"#;
    fn asset(kind: &str, name: &str, path: &str, bytes: i64) -> Asset {
        Asset {
            id: 0,
            kind: kind.to_string(),
            name: name.to_string(),
            path: path.to_string(),
            category: String::new(),
            tags: String::new(),
            duration_ms: 0,
            size_bytes: bytes,
            meta: String::new(),
            note: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn assets_dedupe_by_path() {
        let conn = mem();
        let first = upsert_asset(&conn, &asset("audio", "提示音", "E:/sfx/a.mp3", 100)).unwrap();
        assert!(first.id > 0);

        // 同一个路径再导一次是更新，不是新增
        let again = upsert_asset(&conn, &asset("audio", "提示音改名", "E:/sfx/a.mp3", 200)).unwrap();
        assert_eq!(again.id, first.id);
        assert_eq!(again.name, "提示音改名");
        assert_eq!(again.size_bytes, 200);
        assert_eq!(list_assets(&conn, None).unwrap().len(), 1);

        upsert_asset(&conn, &asset("lut", "冷调", "E:/luts/cool.cube", 3000)).unwrap();
        assert_eq!(list_assets(&conn, None).unwrap().len(), 2);
        assert_eq!(list_assets(&conn, Some("audio")).unwrap().len(), 1);
        assert_eq!(list_assets(&conn, Some("lut")).unwrap().len(), 1);
    }

    #[test]
    fn asset_summary_groups_by_kind() {
        let conn = mem();
        upsert_asset(&conn, &asset("audio", "a", "E:/s/a.mp3", 100)).unwrap();
        upsert_asset(&conn, &asset("audio", "b", "E:/s/b.mp3", 200)).unwrap();
        upsert_asset(&conn, &asset("lut", "c", "E:/l/c.cube", 400)).unwrap();

        let sum = asset_summary(&conn).unwrap();
        assert_eq!(sum.audio, 2);
        assert_eq!(sum.lut, 1);
        assert_eq!(sum.total, 3);
        assert_eq!(sum.total_bytes, 700);
    }

    #[test]
    fn asset_edits_and_removal_do_not_touch_the_disk() {
        let conn = mem();
        let dir = std::env::temp_dir().join("dramadist_asset_keep");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("keep.mp3");
        std::fs::write(&file, b"pretend audio").unwrap();
        let path = file.to_string_lossy().to_string();

        let saved = upsert_asset(&conn, &asset("audio", "原名", &path, 13)).unwrap();
        let edited = update_asset(&conn, saved.id, "新名", "分类", "标签", "备注").unwrap();
        assert_eq!(edited.name, "新名");
        assert_eq!(edited.category, "分类");
        assert_eq!(edited.tags, "标签");
        assert_eq!(edited.note, "备注");
        assert_eq!(edited.path, path, "改元信息不该动路径");
        assert_eq!(edited.kind, "audio", "也不该动类型");

        delete_asset(&conn, saved.id).unwrap();
        assert!(get_asset(&conn, saved.id).is_err());
        assert!(file.is_file(), "从库里移除不能删用户的文件");

        // 不存在的记录要报错，而不是静默成功
        assert!(update_asset(&conn, 9999, "x", "", "", "").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn v6_database_gains_asset_table_without_losing_data() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn.execute_batch(SCHEMA_V2).unwrap();
        conn.execute_batch(SCHEMA_V3).unwrap();
        conn.execute_batch(SCHEMA_V4).unwrap();
        conn.execute_batch(SCHEMA_V5).unwrap();
        conn.execute_batch(SCHEMA_V6).unwrap();
        conn.execute_batch("PRAGMA user_version = 6;").unwrap();

        let p = create_project(&conn, "旧工程", "").unwrap();
        add_media(&conn, p.id, "E:/a.mp4", "a.mp4", &MediaProbe::default()).unwrap();

        migrate(&conn).unwrap();

        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert_eq!(list_projects(&conn).unwrap()[0].name, "旧工程");
        assert_eq!(list_media(&conn, p.id).unwrap().len(), 1);
        assert_eq!(asset_summary(&conn).unwrap().total, 0, "新表可用");
    }
}
