//! SQLite 存取层。
//!
//! 迁移策略：只前进不回退，靠 `PRAGMA user_version` 判断。旧库能直接打开。

use crate::error::AppResult;
use crate::models::{
    Asset, AssetSummary, Cue, Job, JobCounts, Media, MediaProbe, OutputEpisode, OutputSlice,
    PipelineDrama, PipelineRun, Project, PublishAccount, PublishPlan, RunLog, Segment,
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

/// v9：把「这次运行用的是哪个工程」记下来。
///
/// 原来产出查询靠 `pipeline_drama.src_dir = project.root_dir` 间接关联 ——
/// 一旦同一个源目录被跑过多次（有多个工程），一次查询会把几次运行的数据
/// 全捞出来。实测：163 行里只有 65 个不同文件，界面上同一集出现两三次。
const SCHEMA_V9: &str = r#"
ALTER TABLE pipeline_drama ADD COLUMN project_id INTEGER;
"#;

/// v10：运行日志。
///
/// 为什么要单独一张表：流水线跑起来是几十上百个任务，用户需要能**回头看**
/// 「刚才到底做了什么、卡在哪一步」。原来只有任务表，看的是状态；
/// 日志看的是**过程**（扫到什么、导入了多少、每一步的产出）。
///
/// 真实软件也是这么做的 —— 它左侧有一页「运行日志」，
/// 日志文件里那种「===== AI 重写简介 =====」的分节就是给人看的。
const SCHEMA_V10: &str = r#"
CREATE TABLE IF NOT EXISTS run_log (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id     INTEGER,
    drama_id   INTEGER,
    level      TEXT    NOT NULL DEFAULT 'info',
    message    TEXT    NOT NULL,
    created_at TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_run_log_run ON run_log(run_id, id);
"#;

/// v11：素材的「去重后文件」。
///
/// 真实软件的流程是**先对整集去重、再切片**，去重产物才是发布用的东西。
/// 我原来反了 —— 先切再对切片去重，同样一部剧要多跑十几倍的去重。
/// 这个字段记住去重产物；切片时优先读它，读不到才退回原片。
const SCHEMA_V11: &str = r#"
ALTER TABLE media ADD COLUMN processed_path TEXT NOT NULL DEFAULT '';
"#;

/// v12：日志行记住「这一行对应哪个产出文件」。
///
/// 用户的想法：「产出的内容在日志里尽量能直接点击播放」——
/// 对。产出不该是另一个列表，它就该长在产生它的那一行上：
/// 点那行就能看，不用切 tab、不用再找一遍。
const SCHEMA_V12: &str = r#"
ALTER TABLE run_log ADD COLUMN artifact TEXT NOT NULL DEFAULT '';
"#;

/// v13：下载索引。列表要判断「这部剧下载过没有」必须按 **seriesId** ——
/// 剧名会变（接口标题带后缀、标点差异），拿标题当键迟早对不上。
const SCHEMA_V13: &str = r#"
CREATE TABLE IF NOT EXISTS download_index (
    series_id  TEXT PRIMARY KEY,
    title      TEXT NOT NULL,
    dir        TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
"#;

/// 当前 schema 版本。加表/加列时递增，并在 `migrate` 里补一段。
pub const SCHEMA_VERSION: i64 = 13;

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

/// 确保某一列存在。**不靠 `user_version` 判断。**
///
/// 为什么不能只靠版本号：版本号可能被「只改了版本号、没写迁移」的构建
/// 错误地推上去 —— 一旦推上去，那个 ALTER 就**永远不会再跑**，
/// 而库表面上还是"最新版"。真实踩过：`user_version = 12` 但
/// `run_log.artifact` 根本没加上，界面直接报列不存在。
///
/// 直接查表结构最可靠，而且天然幂等：有就跳过，没有就加。
fn ensure_column(conn: &Connection, table: &str, column: &str, ddl: &str) -> AppResult<()> {
    let exists = conn
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .any(|c| c == column);
    if !exists {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {ddl}"))?;
    }
    Ok(())
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
    if version < 9 {
        conn.execute_batch(SCHEMA_V9)?;
    }
    if version < 10 {
        conn.execute_batch(SCHEMA_V10)?;
    }
    if version < 11 {
        conn.execute_batch(SCHEMA_V11)?;
    }
    if version < 12 {
        conn.execute_batch(SCHEMA_V12)?;
    }
    if version < 13 {
        conn.execute_batch(SCHEMA_V13)?;
    }
    // **兜底：按实际表结构补齐关键列。**
    //
    // 上面的版本分支只在版本号正确时才跑到。一旦版本号被错误地推高
    // （比如某个中间构建只改了 SCHEMA_VERSION、忘了写迁移），
    // 那段 ALTER 就永远不会执行，而库看起来还是最新的。
    // 这两句和版本号无关，每次启动都核对一遍 —— 幂等，代价可忽略。
    ensure_column(
        conn,
        "media",
        "processed_path",
        "processed_path TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        conn,
        "run_log",
        "artifact",
        "artifact TEXT NOT NULL DEFAULT ''",
    )?;

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

// ---------- download_index ----------

/// 记下某部剧下载到哪个目录。列表页靠它按 seriesId 判断「已下载」。
pub fn upsert_download(
    conn: &Connection,
    series_id: &str,
    title: &str,
    dir: &str,
) -> AppResult<()> {
    conn.execute(
        "INSERT INTO download_index(series_id, title, dir, updated_at) VALUES(?1, ?2, ?3, ?4)
         ON CONFLICT(series_id) DO UPDATE SET
             title = excluded.title,
             dir = excluded.dir,
             updated_at = excluded.updated_at",
        params![series_id, title, dir, now()],
    )?;
    Ok(())
}

/// 这部剧上次下载到哪个目录（没记录返回 `None`）。
pub fn download_dir(conn: &Connection, series_id: &str) -> AppResult<Option<String>> {
    let mut stmt = conn.prepare("SELECT dir FROM download_index WHERE series_id = ?1")?;
    let mut rows = stmt.query(params![series_id])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row.get(0)?))
    } else {
        Ok(None)
    }
}

// ---------- project ----------

/// 改工程名。
///
/// **必须在「AI 起了新剧名」之后调用** —— 工程名就是「上一次的产出目录名」，
/// 不改的话下次跑找不到要改名的旧目录，产出文件夹就会一次多一个。
/// 实测踩过：一个二创目录里堆了 4 个剧名文件夹（家庭首付的拉扯 / 姜南星的接亲车
/// / 借车之后 / 首付车与亲情的边界）。
pub fn rename_project(conn: &Connection, id: i64, name: &str) -> AppResult<()> {
    conn.execute("UPDATE project SET name = ?2 WHERE id = ?1", params![id, name])?;
    Ok(())
}
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

/// 按源目录找已有工程。
///
/// 流水线每次跑都新建工程的话，同一批素材会被导入好几遍 ——
/// 实测撞到过：三个工程指向同一个源目录，163 条素材里只有 65 个不同文件。
pub fn find_project_by_root(conn: &Connection, root_dir: &str) -> AppResult<Option<Project>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, root_dir, created_at FROM project WHERE root_dir = ?1 ORDER BY id LIMIT 1",
    )?;
    let mut rows = stmt.query(params![root_dir])?;
    match rows.next()? {
        Some(row) => Ok(Some(Project {
            id: row.get(0)?,
            name: row.get(1)?,
            root_dir: row.get(2)?,
            created_at: row.get(3)?,
        })),
        None => Ok(None),
    }
}

/// 某个工程里是不是已经导入过这个文件。
pub fn find_media_by_path(
    conn: &Connection,
    project_id: i64,
    path: &str,
) -> AppResult<Option<Media>> {
    let mut stmt = conn.prepare(
        "SELECT id, project_id, path, name, duration_ms, width, height, has_audio,
                size_bytes, format_name, created_at, processed_path
           FROM media WHERE project_id = ?1 AND path = ?2 ORDER BY id LIMIT 1",
    )?;
    let mut rows = stmt.query(params![project_id, path])?;
    match rows.next()? {
        Some(row) => Ok(Some(Media {
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
            processed_path: row.get(11)?,
        })),
        None => Ok(None),
    }
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

/// 删工程：把它下面的素材逐个清干净再删。
///
/// 光 `DELETE FROM project` 会留下一堆孤儿行，而且指向这些素材的任务
/// 一跑就撞外键。所以走 [`remove_media`]，让连带清理只有一处实现。
pub fn delete_project(conn: &Connection, id: i64) -> AppResult<()> {
    let media_ids: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT id FROM media WHERE project_id = ?1")?;
        let rows = stmt
            .query_map(params![id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        rows
    };
    for mid in media_ids {
        remove_media(conn, mid)?;
    }
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
    // 直接构造返回，不再回头查一遍。
    //
    // 一来省一次往返；二来 —— 迁移测试是「先塞旧数据、再升级」，
    // 塞的时候表上还没有 `processed_path`，回查会撞「no such column」。
    // 插进去什么自己最清楚，没必要问库。
    Ok(Media {
        id: conn.last_insert_rowid(),
        project_id,
        path: path.to_string(),
        name: name.to_string(),
        duration_ms: probe.duration_ms,
        width: probe.width,
        height: probe.height,
        has_audio: probe.has_audio,
        size_bytes: probe.size_bytes,
        format_name: probe.format_name.clone(),
        created_at,
        processed_path: String::new(),
    })
}

pub fn get_media(conn: &Connection, id: i64) -> AppResult<Media> {
    let media = conn.query_row(
        "SELECT id, project_id, path, name, duration_ms, width, height, has_audio,
                size_bytes, format_name, created_at, processed_path
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
        processed_path: row.get(11)?,
    })
}

pub fn list_media(conn: &Connection, project_id: i64) -> AppResult<Vec<Media>> {
    let mut stmt = conn.prepare(
        "SELECT id, project_id, path, name, duration_ms, width, height, has_audio,
                size_bytes, format_name, created_at, processed_path
         FROM media WHERE project_id = ?1 ORDER BY id DESC",
    )?;
    let rows = stmt.query_map(params![project_id], row_to_media)?;
    let mut out = Vec::new();
    for item in rows {
        out.push(item?);
    }
    Ok(out)
}

/// 删素材：**连带它的任务、切片、字幕一起删**。
///
/// 不这么做的话，队列里还留着指向这条素材的任务，等它跑到写字幕那一步
/// 就会撞 `FOREIGN KEY constraint failed` —— 实测发生过：
/// run#7 整条流水线就是这么挂掉的（删了工程，任务还在队列里）。
/// 记下某个素材去重后的文件。传空串表示清掉（比如重跑）。
pub fn set_media_processed(conn: &Connection, media_id: i64, path: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE media SET processed_path = ?2 WHERE id = ?1",
        params![media_id, path],
    )?;
    Ok(())
}

/// 用新探测到的元信息覆盖素材记录。
///
/// 用在「文件被重下过」时：换了清晰度或换了另一条流，宽高、时长都会变，
/// 而库里的旧值会被当成去重的裁切目标 —— 滤镜会报
/// "Invalid too big or non positive size"，用户只看到一集莫名失败。
pub fn update_media_probe(
    conn: &Connection,
    media_id: i64,
    probe: &MediaProbe,
) -> AppResult<()> {
    conn.execute(
        "UPDATE media
            SET duration_ms = ?2, width = ?3, height = ?4, has_audio = ?5,
                size_bytes = ?6, format_name = ?7
          WHERE id = ?1",
        params![
            media_id,
            probe.duration_ms,
            probe.width,
            probe.height,
            probe.has_audio as i64,
            probe.size_bytes,
            probe.format_name
        ],
    )?;
    Ok(())
}

/// 切片该读哪个文件：**优先去重产物**，没有才读原片。
///
/// 这是「先去重再切片」这条链路的接口 —— 真实软件就是这个顺序，
/// 去重产物才是发布用的东西，切片只是它的下游。
pub fn media_source_path(conn: &Connection, media_id: i64) -> AppResult<String> {
    let (raw, processed): (String, String) = conn.query_row(
        "SELECT path, processed_path FROM media WHERE id = ?1",
        params![media_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let p = processed.trim();
    if !p.is_empty() && std::path::Path::new(p).is_file() {
        Ok(p.to_string())
    } else {
        Ok(raw)
    }
}

pub fn remove_media(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM cue WHERE media_id = ?1", params![id])?;
    conn.execute("DELETE FROM segment WHERE media_id = ?1", params![id])?;
    // 任务直接删掉：素材都没了，任务留着只会失败
    conn.execute("DELETE FROM job WHERE media_id = ?1", params![id])?;
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
    "id, run_id, name, src_dir, out_dir, episode_count, stage, status, message, updated_at, project_id";

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
        // v9 之前的老记录没有这个字段，是 NULL
        project_id: row.get(10).ok().flatten(),
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
    project_id: Option<i64>,
) -> AppResult<i64> {
    conn.execute(
        "INSERT INTO pipeline_drama(run_id, name, src_dir, out_dir, episode_count, stage, status, message, updated_at, project_id)
         VALUES(?1, ?2, ?3, ?4, ?5, '', 'pending', '', ?6, ?7)",
        params![run_id, name, src_dir, out_dir, episode_count, now(), project_id],
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

/// 还没定论的剧（`pending` / `running`），按队列顺序。
///
/// 队列停在半路时，这些剧需要一个真实结论 —— 否则流水线会永远
/// 停在「运行中」，界面上那个按钮就一直是「停止」。
/// 排队的先后就是 `add_pipeline_drama` 的插入顺序，也就是 `id` 升序。
pub fn list_open_pipeline_dramas(conn: &Connection) -> AppResult<Vec<PipelineDrama>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {DRAMA_COLUMNS} FROM pipeline_drama
          WHERE status IN ('pending','running') ORDER BY id ASC"
    ))?;
    let rows = stmt.query_map([], row_to_pipeline_drama)?;
    let mut out = Vec::new();
    for item in rows {
        out.push(item?);
    }
    Ok(out)
}

/// 把没跑完的流水线退回"待跑"状态。
///
/// 和队列一样：进程被杀之后，`running` 是个假状态，没有人在跑它。
///
/// 文案要説清楚"会自己接着跑"，不能只说"退回待跑" —— 之前写的是
/// 「上次运行被中断，已退回待跑」，用户看了以为要手动重启，问我
/// "没办法续接上嘛"。实际上队列起来就会接着干。
///
/// **但手里一个待跑任务都没有的剧不能退回待跑** —— 那会变成永远跑不掉的幽灵：
/// 界面上一条「进行中」，日志里什么都不会发生，流水线也就永远收不了口。
/// 它的任务早被取消光了，定论就是「已停止」。
pub fn recover_interrupted_pipelines(conn: &Connection) -> AppResult<i64> {
    let changed = conn.execute(
        "UPDATE pipeline_drama SET status = 'pending', message = '上次被中断，排队的任务会自动接着跑', updated_at = ?1
          WHERE status = 'running'
            AND EXISTS (SELECT 1 FROM job WHERE job.pipeline_drama_id = pipeline_drama.id
                         AND job.status IN ('pending','running'))",
        params![now()],
    )?;
    // 没任务可跑的那些：当场给结论
    conn.execute(
        "UPDATE pipeline_drama SET status = 'canceled', message = '任务已被取消完，这条运行已停止', updated_at = ?1
          WHERE status = 'running'",
        params![now()],
    )?;
    Ok(changed as i64)
}

/// 没有待跑/进行中任务、却还挂着 `pending` 的剧。
///
/// 每次队列跑完都打扫一遍，把这类幽灵剧给个定论。
/// 它们不会自己好：队列只会从 `job` 表取活，一部剧一条任务都没有就永远不会被轮到。
///
/// 踩过：库里躺着 2026-10-18 那次的剧，一直写「进行中」，
/// 用户看到的就是「日志早停了，页面还写着正在跑」。
pub fn settle_orphan_dramas(conn: &Connection) -> AppResult<i64> {
    let changed = conn.execute(
        "UPDATE pipeline_drama SET status = 'canceled',
                message = '没有待跑任务了，这条剧已停下', updated_at = ?1
          WHERE status IN ('pending','running')
            AND NOT EXISTS (SELECT 1 FROM job WHERE job.pipeline_drama_id = pipeline_drama.id
                             AND job.status IN ('pending','running'))",
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
/// —— v9 起直接按 pipeline_drama.project_id 查，精确到本次运行；
/// 老记录没有这个字段时才退回用源目录匹配。
pub fn run_outputs(conn: &Connection, run_id: i64) -> AppResult<Vec<OutputEpisode>> {
    let dramas = list_pipeline_dramas(conn, run_id)?;
    let mut out = Vec::new();

    for drama in dramas {
        // **按「这次运行的任务碰过哪些素材」查。**
        //
        // 这是最准的判据：任务表上带着 `pipeline_drama_id` 和 `media_id`，
        // 谁被这次运行处理过一清二楚。
        //
        // 踩过的两个坑：
        // - 按源目录关联：同一目录跑过多次就把几次的数据全捞出来
        // - 按工程关联：工程是**跨运行复用**的，会把历史残留一起带出来
        //   （实测工程里积了 68 条素材、55 条原片早删了，界面显示"68 集"）
        let mut stmt = conn.prepare(
            "SELECT m.id, m.name, m.duration_ms, m.width, m.height
               FROM media m
              WHERE m.id IN (
                    SELECT DISTINCT j.media_id FROM job j
                     WHERE j.pipeline_drama_id = ?1 AND j.media_id IS NOT NULL
              )
              ORDER BY m.id ASC",
        )?;
        let medias: Vec<(i64, String, i64, i64, i64)> = stmt
            .query_map(params![drama.id], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

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

// ---------- media 清理 ----------

/// 删掉这个工程里、路径在这些里的素材（连同它们的任务/切片/字幕）。
///
/// 用途：**每次运行重新导入，不复用**（用户明确要求）。
///
/// 复用会让「上次的处理结果」渗进这一次 —— 界面上写着「复用 7 条」，
/// 用户不知道到底重跑了没有。宁可每次都是干净的。
///
/// 代价说清楚：上一次运行的产出列表会变空（**文件还在磁盘上**，
/// 只是指向它们的记录没了）。所以只在「这次确实要重新导入这些路径」时调。
pub fn remove_media_by_paths(
    conn: &Connection,
    project_id: i64,
    paths: &[String],
) -> AppResult<i64> {
    if paths.is_empty() {
        return Ok(0);
    }
    let wanted: std::collections::HashSet<&str> = paths.iter().map(|s| s.as_str()).collect();
    let rows: Vec<(i64, String)> = {
        let mut stmt = conn.prepare("SELECT id, path FROM media WHERE project_id = ?1")?;
        let r = stmt
            .query_map(params![project_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };

    let mut removed = 0i64;
    for (id, path) in rows {
        if !wanted.contains(path.as_str()) {
            continue;
        }
        // 有任务在跑的先留着 —— 删了会让正在跑的任务撞外键（这个坑踩过）
        let busy: i64 = conn.query_row(
            "SELECT COUNT(*) FROM job WHERE media_id = ?1 AND status IN ('pending','running')",
            params![id],
            |r| r.get(0),
        )?;
        if busy > 0 {
            continue;
        }
        remove_media(conn, id)?;
        removed += 1;
    }
    Ok(removed)
}

/// 清掉这个工程里「原片已经不在了、而且没有任务在跑」的素材。
///
/// 工程是**跨运行复用**的（同源目录不重复建），源目录内容换掉之后，
/// 旧的素材记录会一直留着。实测一个工程积了 68 条，其中 55 条原片早删了 ——
/// 界面上显示成「68 集」，用户看着莫名其妙。
///
/// **只删文件真的不在的**：网盘/移动硬盘临时断线时不该把有效记录清掉。
/// **有任务在跑的也不删** —— 那会让正在跑的任务撞外键（这个坑踩过）。
pub fn prune_missing_media(conn: &Connection, project_id: i64) -> AppResult<i64> {
    let rows: Vec<(i64, String)> = {
        let mut stmt = conn.prepare("SELECT id, path FROM media WHERE project_id = ?1")?;
        let r = stmt
            .query_map(params![project_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        r
    };

    let mut removed = 0i64;
    for (id, path) in rows {
        if std::path::Path::new(&path).is_file() {
            continue;
        }
        // 还有待跑/正在跑的任务挂着就先留着
        let busy: i64 = conn.query_row(
            "SELECT COUNT(*) FROM job WHERE media_id = ?1 AND status IN ('pending','running')",
            params![id],
            |r| r.get(0),
        )?;
        if busy > 0 {
            continue;
        }
        remove_media(conn, id)?;
        removed += 1;
    }
    Ok(removed)
}

/// 清空运行记录。返回删掉了几次运行。
///
/// **两条边界，都是刻意的：**
///
/// 1. **正在跑的那次不删** —— 删了队列里的任务就找不到归属，
///    日志也断了，界面会显示成一次凭空的运行。
/// 2. **只清记录，不动磁盘上的产出** —— 跑出来的视频是用户的东西，
///    他只是想清理界面上的历史，不是想删文件。这两件事必须分开。
///
/// 连带清掉这些运行的剧目行、任务和日志（那些是纯记录，留着没意义）。
/// 素材（media）**不动** —— 它属于工程，不属于某次运行。
pub fn clear_pipeline_runs(conn: &Connection) -> AppResult<i64> {
    // 哪些运行可以删？
    //
    // 1. 状态不是 running 的 —— 明确结束了
    // 2. **状态是 running、但一个待跑/在跑的任务都没有的** ——
    //    这是被中断后卡住的那种（应用被杀、队列停了）。不加这条判据的话，
    //    它会永远显示"运行中"，用户永远清不掉。
    let run_ids: Vec<i64> = {
        let mut stmt = conn.prepare(
            "SELECT id FROM pipeline_run r
              WHERE r.status != 'running'
                 OR NOT EXISTS (
                       SELECT 1 FROM job j
                        JOIN pipeline_drama d ON d.id = j.pipeline_drama_id
                       WHERE d.run_id = r.id AND j.status IN ('pending','running')
                    )",
        )?;
        let r = stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        r
    };

    for run_id in &run_ids {
        conn.execute(
            "DELETE FROM job WHERE pipeline_drama_id IN
                (SELECT id FROM pipeline_drama WHERE run_id = ?1)",
            params![run_id],
        )?;
        conn.execute("DELETE FROM pipeline_drama WHERE run_id = ?1", params![run_id])?;
        conn.execute("DELETE FROM run_log WHERE run_id = ?1", params![run_id])?;
        conn.execute("DELETE FROM pipeline_run WHERE id = ?1", params![run_id])?;
    }

    Ok(run_ids.len() as i64)
}

/// 某部剧属于哪次运行。日志要按运行过滤，而任务只带 drama_id。
pub fn run_id_of_drama(conn: &Connection, drama_id: Option<i64>) -> AppResult<Option<i64>> {
    let Some(id) = drama_id else { return Ok(None) };
    let mut stmt = conn.prepare("SELECT run_id FROM pipeline_drama WHERE id = ?1")?;
    let mut rows = stmt.query(params![id])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

/// 这次运行里，这部剧是不是已经写过这条日志了？
///
/// 用来判断「步骤标题要不要打」——
/// 标题是**分节**，一个步骤在一次运行里只该出现一次。
///
/// **不能用「最近一条是不是标题」当判据**：明细行之后它又会认为该打了，
/// 61 集分十几批跑就会打十几次标题 —— 那正是用户抱怨的毛病。
pub fn has_log_with_message(
    conn: &Connection,
    drama_id: i64,
    run_id: i64,
    message: &str,
) -> AppResult<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM run_log WHERE drama_id = ?1 AND run_id = ?2 AND message = ?3",
        params![drama_id, run_id, message],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// 还有多少个这种类型的待跑任务。
///
/// 旧版的跨剧计数，现在只在测试里用 —— 生产走
/// [`count_pending_jobs_of_kind_for_drama`]（队列已经按剧串行）。
#[allow(dead_code)]
pub fn count_pending_jobs_of_kind(conn: &Connection, kind: &str) -> AppResult<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM job WHERE status = 'pending' AND kind = ?1",
        params![kind],
        |r| r.get(0),
    )?)
}

/// 某部剧里，某一类任务的 `(状态, 参数)`。
///
/// 完成通知邮件用它核「勾了的步骤是不是真跑了、产物在不在」——
/// 光数条数不够，还得拿着参数里的路径去磁盘上看一眼。
pub fn jobs_of_kind_for_drama(
    conn: &Connection,
    drama_id: i64,
    kind: &str,
) -> AppResult<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT status, payload FROM job WHERE pipeline_drama_id = ?1 AND kind = ?2
          ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(params![drama_id, kind], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 某部剧还有多少个这种类型的待跑任务。
///
/// 队列按剧串行，步骤收口的汇总行也得只看当前这部剧 —— 否则下一部剧的
/// 同名步骤还在排队，汇总行永远不出来。`None` = 手动任务。
pub fn count_pending_jobs_of_kind_for_drama(
    conn: &Connection,
    kind: &str,
    drama_id: Option<i64>,
) -> AppResult<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM job
          WHERE status = 'pending' AND kind = ?1 AND pipeline_drama_id IS ?2",
        params![kind, drama_id],
        |r| r.get(0),
    )?)
}

/// 这次运行里，某个步骤完成了多少个。
pub fn count_done_jobs_of_kind(conn: &Connection, run_id: i64, kind: &str) -> AppResult<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM job
          WHERE status = 'done' AND kind = ?2
            AND pipeline_drama_id IN (SELECT id FROM pipeline_drama WHERE run_id = ?1)",
        params![run_id, kind],
        |r| r.get(0),
    )?)
}

/// 同一步骤里失败的任务数。
///
/// 汇总行只报「完成 n 个」时，失败的那一集会被这句话盖过去 ——
/// 用户看到「全部完成」就以为真的全成了。
pub fn count_failed_jobs_of_kind(conn: &Connection, run_id: i64, kind: &str) -> AppResult<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM job
          WHERE status = 'failed' AND kind = ?2
            AND pipeline_drama_id IN (SELECT id FROM pipeline_drama WHERE run_id = ?1)",
        params![run_id, kind],
        |r| r.get(0),
    )?)
}

/// 写一行日志。
///
/// **失败不抛错**：日志写不进去不该让流水线挂掉。调用方也不必处理错误。
pub fn append_log(
    conn: &Connection,
    run_id: Option<i64>,
    drama_id: Option<i64>,
    level: &str,
    message: &str,
) {
    append_log_with_file(conn, run_id, drama_id, level, message, "");
}

/// 写一行**带产出文件**的日志。
///
/// 有文件的那种，界面上这一行能直接点开播 ——
/// 产出不该是另一个列表，它就该长在产生它的那一行上。
/// 用户原话：「产出的内容在日志里尽量能直接点击播放」。
pub fn append_log_with_file(
    conn: &Connection,
    run_id: Option<i64>,
    drama_id: Option<i64>,
    level: &str,
    message: &str,
    artifact: &str,
) {
    let _ = conn.execute(
        "INSERT INTO run_log(run_id, drama_id, level, message, artifact, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![run_id, drama_id, level, message, artifact, now()],
    );
}

/// 当前最大的日志 id —— 用来给「开始流水线」划一条线。
pub fn max_log_id(conn: &Connection) -> AppResult<i64> {
    Ok(conn.query_row("SELECT COALESCE(MAX(id), 0) FROM run_log", [], |r| r.get(0))?)
}

/// 把某条线之后写的、还没归属运行的日志挂到这次运行上。
///
/// 为什么需要：改剧名是在**建 run 记录之前**跑的（那时才知道新剧名），
/// 所以它的日志没有 run_id。日志页按运行过滤时，用户就**看不到
/// 「改剧名失败」这条** —— 明明失败了，界面上却像什么都没发生。
pub fn attach_recent_logs_to_run(conn: &Connection, run_id: i64, mark: i64) -> AppResult<i64> {
    let n = conn.execute(
        "UPDATE run_log SET run_id = ?1 WHERE run_id IS NULL AND id > ?2",
        params![run_id, mark],
    )?;
    Ok(n as i64)
}

/// 读日志。`after_id` 用来做增量拉取（前端轮询只要新行）。
///
/// 按运行过滤时，**这次运行开始之后写的、还没归属运行的日志也要带上**：
/// 下载原片、手工操作发生在建 run 之前或运行之间，不带的话用户按运行
/// 过滤就看不到「刚才下载失败在哪一集」。
///
/// 下界取运行自己的 `created_at`，不取它的第一条日志：某个 run 一集
/// 日志都没有时 `MIN(id)` 是 NULL，COALESCE 兜到底就成了「什么都不显示」——
/// 用户看到的就是一个空日志页。
///
/// 两种情形要分开：run 有日志时仍用第一条日志的 id（精确，不会把运行前
/// 同一秒的旧日志卷进来）；只有它一条都没有时才退回按时间筛。
pub fn list_logs(
    conn: &Connection,
    run_id: Option<i64>,
    after_id: i64,
    limit: i64,
) -> AppResult<Vec<RunLog>> {
    let limit = limit.clamp(1, 2000);
    let mut out = Vec::new();
    match run_id {
        Some(id) => {
            let mut stmt = conn.prepare(
                "SELECT id, run_id, drama_id, level, message, artifact, created_at FROM run_log
                  WHERE (
                      run_id = ?1
                      OR (run_id IS NULL AND (
                          -- 这次运行已经有日志：以第一条为界（精确）
                          id > (SELECT MIN(id) FROM run_log WHERE run_id = ?1)
                          -- 一条都没有（比如刚建就失败）：退回用开始时间，
                          -- 否则 COALESCE 到极限值会把下载日志全滤掉
                          OR (
                              NOT EXISTS (SELECT 1 FROM run_log WHERE run_id = ?1)
                              AND created_at >= COALESCE(
                                  (SELECT created_at FROM pipeline_run WHERE id = ?1),
                                  ''
                              )
                          )
                      ))
                  )
                    AND id > ?2 ORDER BY id ASC LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![id, after_id, limit], row_to_run_log)?;
            for r in rows {
                out.push(r?);
            }
        }
        None => {
            let mut stmt = conn.prepare(
                "SELECT id, run_id, drama_id, level, message, artifact, created_at FROM run_log
                  WHERE id > ?1 ORDER BY id ASC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![after_id, limit], row_to_run_log)?;
            for r in rows {
                out.push(r?);
            }
        }
    }
    Ok(out)
}

/// 某部剧在这次运行里写过的错误日志（最关键的几行）。
///
/// 完成通知邮件要写「哪部剧没成、卡在哪一步」—— 任务表只剩一句
/// 「整集去重 失败」，真正的原因在日志的 `✗` 行里。
/// 只取 error / warn 级，那几行里写着 `✗ 第03集.mp4 整集去重：…`。
pub fn list_drama_error_logs(conn: &Connection, run_id: i64, drama_id: i64) -> AppResult<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT message FROM run_log
          WHERE run_id = ?1 AND drama_id = ?2 AND level IN ('error','warn')
          ORDER BY id ASC LIMIT 40",
    )?;
    let rows = stmt.query_map(params![run_id, drama_id], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn row_to_run_log(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunLog> {
    Ok(RunLog {
        id: row.get(0)?,
        run_id: row.get(1)?,
        drama_id: row.get(2)?,
        level: row.get(3)?,
        message: row.get(4)?,
        artifact: row.get(5)?,
        created_at: row.get(6)?,
    })
}

/// 清掉某个运行之前的旧日志，免得无限增长。
pub fn trim_logs(conn: &Connection, keep: i64) -> AppResult<i64> {
    let n = conn.execute(
        "DELETE FROM run_log WHERE id <= (SELECT MAX(id) FROM run_log) - ?1",
        params![keep.max(100)],
    )?;
    Ok(n as i64)
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

/// 队首那条待跑任务。
///
/// 队列要**按剧串行**：先看队首属于哪部剧，再把这部剧的任务成批取出来。
/// 只看类型的话，整剧一步的任务（封面/成本/分镜）会把 A 剧和 B 剧
/// 拼进同一批并发跑 —— 用户看到的就是「步骤乱了」。
pub fn next_pending_job(conn: &Connection) -> AppResult<Option<Job>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {JOB_COLUMNS} FROM job
          WHERE status = 'pending' ORDER BY id ASC LIMIT 1"
    ))?;
    let mut rows = stmt.query([])?;
    match rows.next()? {
        Some(row) => Ok(Some(row_to_job(row)?)),
        None => Ok(None),
    }
}

/// 取**同一部剧、同一类型**的待跑任务，用来并行执行。
///
/// 两个条件缺一不可：
/// - 同类型：切片必须等去重全跑完；
/// - 同剧：一部剧做完再做下一部，不能把 A 的封面和 B 的封面一起跑。
///
/// `drama_id` 为 `None` 时取的是**手动任务**（不属于任何剧），
/// 用 `IS` 而不是 `=`，NULL 才能正确匹配。
pub fn next_pending_jobs_of_kind_for_drama(
    conn: &Connection,
    kind: &str,
    drama_id: Option<i64>,
    limit: i64,
) -> AppResult<Vec<Job>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {JOB_COLUMNS} FROM job
          WHERE status = 'pending' AND kind = ?1 AND pipeline_drama_id IS ?2
          ORDER BY id ASC LIMIT ?3"
    ))?;
    let rows = stmt.query_map(params![kind, drama_id, limit.max(1)], row_to_job)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 取一批**同类型**的待跑任务，用来并行执行。
///
/// **为什么必须是同类型**：任务之间并不是都能并行的。
/// 「切片」得在「去重」之后 —— 切片读的是去重产物，去重还没跑完就切片，
/// 切出来的是原片，去重白做。
///
/// 但**同一类型内部彼此独立**：第 3 集去重和第 7 集去重互不相干，
/// 可以一起跑。所以规则很简单：看队首是什么类型，就只并发取这个类型。
/// 这样天然保证「上一步全部跑完，才轮到下一步」。
///
/// 旧版的跨剧批量，现在只在测试里用 —— 生产走
/// [`next_pending_jobs_of_kind_for_drama`]（队列已经按剧串行）。
#[allow(dead_code)]
pub fn next_pending_jobs_of_kind(
    conn: &Connection,
    kind: &str,
    limit: i64,
) -> AppResult<Vec<Job>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {JOB_COLUMNS} FROM job
          WHERE status = 'pending' AND kind = ?1
          ORDER BY id ASC LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![kind, limit.max(1)], row_to_job)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 用过的任务类型（除了给定类型之外，还有没有别的在排队）。
///
/// 用来判断「这一步是不是全跑完了」—— 队首换类型了就说明上一步结束。
///
/// 旧版判断，现在只在测试里用 —— 生产走 [`next_pending_job`]，直接拿队首任务。
#[allow(dead_code)]
pub fn next_pending_kind(conn: &Connection) -> AppResult<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT kind FROM job WHERE status = 'pending' ORDER BY id ASC LIMIT 1",
    )?;
    let mut rows = stmt.query([])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
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

/// 上次被中断、没跑完的那批任务（`(id, 类型, 参数)`）。
///
/// 重启时要用它去磁盘上验一下：**半截的产出要先清掉**，
/// 不然接着跑会当成「已经做好了」直接跳过（用户实测：去字幕写了两份 38MB
/// 的文件，mp4 连 moov 都没写完，播放器打不开，日志里一个字没有）。
pub fn interrupted_jobs(conn: &Connection) -> AppResult<Vec<(i64, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, payload FROM job
          WHERE status = 'pending' AND message LIKE '上次运行被中断%'
          ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
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

// `count_pending_jobs_of_drama` 已删 —— 换成 `drama_job_state`。
//
// 旧的只数 `pending`/`running`，而**某一步失败后 `cancel_jobs_of_drama`
// 会把后面没跑的任务标成 `canceled`** —— 那一刻 pending 也是 0，
// 于是这部「没做完」的剧被写成「全部步骤已完成」。
// 踩过：成本报告没跑，剧目却显示完成。

/// 某部剧的任务统计：`(还没跑完的, 失败的或被撤的, 已经跑完的)`。
///
/// **前两个都要看。** 只看「还没跑完的 == 0」会误判 —— 某一步失败后
/// `cancel_jobs_of_drama` 把后面没跑的任务标成 `canceled`，那时
/// pending 也是 0，但这部剧明明没做完。
///
/// 踩过：成本报告被撤掉，剧目却显示「全部步骤已完成」，产出少一份。
///
/// **第三个也要看。** 只按「连一个错都没有」判完成同样会误判 ——
/// 用户点了「停止」，队列把 32 个任务里还没跑的全标成 `canceled`，
/// 那一刻一个错都没有、一个没跑完的也没有，于是这部剧被写成
/// 「全部步骤已完成」，而它只跑了 3 集。
/// 跑完了就是跑完了，**被撤掉的不等于做完了**。
pub fn drama_job_state(conn: &Connection, drama_id: i64) -> AppResult<(i64, i64, i64)> {
    let row: (i64, i64, i64) = conn.query_row(
        "SELECT
           COALESCE(SUM(CASE WHEN status IN ('pending','running') THEN 1 ELSE 0 END), 0),
           COALESCE(SUM(CASE WHEN status IN ('failed','canceled') THEN 1 ELSE 0 END), 0),
           COALESCE(SUM(CASE WHEN status = 'done' THEN 1 ELSE 0 END), 0)
         FROM job WHERE pipeline_drama_id = ?1",
        params![drama_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(row)
}

/// 还有没有别的剧正在跑？
///
/// 队列按剧串行：只要这部不是正在跑的那部，就是「还没轮到」——
/// 停队列时这种剧不该被标成「已取消」，它只是排在后面没轮上。
pub fn drama_has_running_jobs(conn: &Connection, drama_id: i64) -> AppResult<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM job WHERE pipeline_drama_id = ?1 AND status = 'running'",
        params![drama_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// 收口一条流水线：所有剧都有定论了，状态才是真的。
///
/// **撤掉的不算完成。** `refresh_pipeline_run_status` 只看
/// failed / done，把「被取消」这一种吞掉了 —— 于是停掉的那次运行
/// 只要有一部剧跑完，整条运行就被盖成「已完成」。
pub fn settle_pipeline_run(conn: &Connection, run_id: i64) -> AppResult<()> {
    let dramas = list_pipeline_dramas(conn, run_id)?;
    if dramas.is_empty() {
        return Ok(());
    }
    let status = if dramas
        .iter()
        .any(|d| d.status == "pending" || d.status == "running")
    {
        return Ok(());
    } else if dramas.iter().any(|d| d.status == "canceled") {
        RUN_CANCELED
    } else if dramas.iter().any(|d| d.status == "failed") {
        RUN_FAILED
    } else {
        RUN_DONE
    };
    set_pipeline_run_status(conn, run_id, status)
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

    /// 文件被重下过（换了清晰度/换了流）：宽高和时长要能改，其他字段别动。
    #[test]
    fn media_probe_can_be_refreshed_after_redownload() {
        let conn = mem();
        let p = create_project(&conn, "p", "").unwrap();
        let old = MediaProbe {
            duration_ms: 61_741,
            width: 360,
            height: 640,
            has_audio: true,
            size_bytes: 900_000,
            format_name: "mov,mp4".into(),
        };
        let m = add_media(&conn, p.id, "D:/源/第08集.mp4", "第08集.mp4", &old).unwrap();

        let fresh = MediaProbe {
            duration_ms: 72_075,
            width: 1280,
            height: 720,
            has_audio: true,
            size_bytes: 2_100_000,
            format_name: "mov,mp4".into(),
        };
        assert!(!fresh.matches(&m), "文件变了就该认出来");
        update_media_probe(&conn, m.id, &fresh).unwrap();

        let got = get_media(&conn, m.id).unwrap();
        assert_eq!((got.width, got.height), (1280, 720), "宽高要用新值");
        assert_eq!(got.duration_ms, 72_075);
        assert_eq!(got.size_bytes, 2_100_000);
        assert!(fresh.matches(&got), "更新后应判定一致，不再重复写库");
        assert_eq!(got.path, "D:/源/第08集.mp4", "路径不该被动");
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

        // 队列按 id 升序取 —— 先排的先跑
        assert_eq!(next_pending_kind(&conn).unwrap().unwrap(), "slice");

        mark_job_running(&conn, a.id).unwrap();
        let running = get_job(&conn, a.id).unwrap();
        assert_eq!(running.status, JOB_RUNNING);
        assert_eq!(running.attempts, 1, "尝试次数要累加");

        // 正在跑的不会再被取出来；队首换成下一条
        assert_eq!(
            next_pending_kind(&conn).unwrap().unwrap(),
            "ocr",
            "正在跑的那条不该被再次取出"
        );
        let batch = next_pending_jobs_of_kind(&conn, "ocr", 10).unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].id, b.id);
    }

    /// 并行取任务：**同一部剧、同一类型**。
    ///
    /// 踩过的坑：只按类型取，整剧一步的任务（封面/成本/分镜）会把 A 剧和
    /// B 剧拼进同一批 —— 用户看到的是「多个视频一起下，流水线步骤乱了」。
    #[test]
    fn parallel_batches_stay_inside_one_drama() {
        let conn = mem();
        let run = create_pipeline_run(&conn, "D:/src", "D:/out", &[]).unwrap();
        let a = add_pipeline_drama(&conn, run, "A", "D:/src/A", "D:/out/A", 2, None).unwrap();
        let b = add_pipeline_drama(&conn, run, "B", "D:/src/B", "D:/out/B", 2, None).unwrap();

        // A 的去重排在 B 的去重之前（整剧串行的最小例子）
        enqueue_job_for(&conn, Some(1), "dedup", OK_DEDUP, Some(a)).unwrap();
        enqueue_job_for(&conn, Some(2), "dedup", OK_DEDUP, Some(b)).unwrap();
        enqueue_job_for(&conn, Some(3), "dedup", OK_DEDUP, Some(a)).unwrap();

        let head = next_pending_job(&conn).unwrap().unwrap();
        assert_eq!(head.pipeline_drama_id, Some(a));
        let batch =
            next_pending_jobs_of_kind_for_drama(&conn, &head.kind, head.pipeline_drama_id, 10)
                .unwrap();
        assert_eq!(batch.len(), 2, "只该取 A 的，B 的不行");
        assert!(batch.iter().all(|j| j.pipeline_drama_id == Some(a)));

        // 手动任务（不属于任何剧）单独成批，不会把流水线任务卷进来
        let manual = enqueue_job(&conn, Some(4), "dedup", OK_DEDUP).unwrap();
        let batch = next_pending_jobs_of_kind_for_drama(&conn, "dedup", None, 10).unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].id, manual.id);
    }

    /// 并行取任务：**只能取同一类型**。
    ///
    /// 跨类型并行会出事：「切片」读的是去重产物，去重还没跑完就切片，
    /// 切出来的是原片 —— 去重白做。
    #[test]
    fn parallel_batches_only_take_one_kind() {
        let conn = mem();
        for i in 0..3 {
            enqueue_job(&conn, Some(i), "dedup", OK_SLICE).unwrap();
        }
        enqueue_job(&conn, Some(9), "slice", OK_SLICE).unwrap();

        // 队首是 dedup，就只拿 dedup —— 哪怕要 10 条
        assert_eq!(next_pending_kind(&conn).unwrap().unwrap(), "dedup");
        let batch = next_pending_jobs_of_kind(&conn, "dedup", 10).unwrap();
        assert_eq!(batch.len(), 3, "只有 3 条 dedup，不该把 slice 也捞进来");
        assert!(batch.iter().all(|j| j.kind == "dedup"));

        // limit 真的起作用
        let two = next_pending_jobs_of_kind(&conn, "dedup", 2).unwrap();
        assert_eq!(two.len(), 2);
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
    const OK_DEDUP: &str = r#"{"mediaId":1,"outDir":"E:/o"}"#;
    const OK_OCR: &str = r#"{"mediaId":2}"#;
    /// 清空运行记录：**正在跑的不删、素材不动**。
    ///
    /// 用户问「运行记录能否清空」时，这两条边界是必须守住的：
    /// 删了正在跑的，队列里的任务会找不到归属；
    /// 动了素材，界面上的产出就跟着没了 —— 而他只是想清理历史列表。
    #[test]
    fn clearing_runs_keeps_the_running_one_and_all_media() {
        let conn = mem();
        let done = create_pipeline_run(&conn, "D:/源", "D:/二创", &["slice".into()]).unwrap();
        let running = create_pipeline_run(&conn, "D:/源", "D:/二创", &["slice".into()]).unwrap();
        conn.execute(
            "UPDATE pipeline_run SET status = 'done' WHERE id = ?1",
            params![done],
        )
        .unwrap();

        let d1 = add_pipeline_drama(&conn, done, "旧剧", "D:/源", "D:/二创/旧剧", 1, None).unwrap();
        let d2 =
            add_pipeline_drama(&conn, running, "在跑", "D:/源", "D:/二创/在跑", 1, None).unwrap();
        let probe = MediaProbe {
            duration_ms: 1000,
            width: 720,
            height: 1280,
            has_audio: true,
            size_bytes: 10,
            format_name: "mp4".into(),
        };
        let project = create_project(&conn, "旧剧", "D:/源").unwrap();
        let m = add_media(&conn, project.id, "D:/源/第01集.mp4", "第01集.mp4", &probe).unwrap();
        enqueue_job_for(
            &conn,
            Some(m.id),
            "dedup",
            r#"{"mediaId":1,"outDir":"D:/o"}"#,
            Some(d1),
        )
        .unwrap();
        enqueue_job_for(
            &conn,
            Some(m.id),
            "dedup",
            r#"{"mediaId":1,"outDir":"D:/o"}"#,
            Some(d2),
        )
        .unwrap();
        append_log(&conn, Some(done), Some(d1), "info", "旧日志");
        append_log(&conn, Some(running), Some(d2), "info", "在跑的日志");

        let n = clear_pipeline_runs(&conn).unwrap();
        assert_eq!(n, 1, "只该清掉已结束的那一次");

        // 正在跑的还在，它的剧目、任务、日志都还在
        let runs = list_pipeline_runs(&conn, 20).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, running);
        assert_eq!(list_pipeline_dramas(&conn, running).unwrap().len(), 1);
        assert_eq!(list_logs(&conn, Some(running), 0, 10).unwrap().len(), 1);

        // 被清掉的那次：剧目、任务、日志一起走
        assert!(list_pipeline_dramas(&conn, done).unwrap().is_empty());
        assert!(list_logs(&conn, Some(done), 0, 10).unwrap().is_empty());
        assert_eq!(job_counts(&conn).unwrap().total, 1, "只剩在跑那次的任务");

        // **素材不动** —— 它属于工程，不属于某次运行
        assert!(get_media(&conn, m.id).is_ok(), "清运行记录不该动素材");
    }

    /// 被中断后卡在「运行中」的运行也要能清掉 —— 否则永远清不掉。
    ///
    /// 判据是「状态 running **且** 没有待跑/在跑的任务」：
    /// 真有任务在跑的绝不能删（删了队列找不到归属）。
    #[test]
    fn clearing_runs_also_removes_stuck_ones_without_jobs() {
        let conn = mem();
        // 卡住的那种：状态还是 running，但一个任务都没有
        let stuck = create_pipeline_run(&conn, "D:/源", "D:/二创", &["slice".into()]).unwrap();

        // 真在跑的那种：状态 running，而且有任务在跑
        let live = create_pipeline_run(&conn, "D:/源", "D:/二创", &["slice".into()]).unwrap();
        let d = add_pipeline_drama(&conn, live, "在跑", "D:/源", "D:/二创/在跑", 1, None).unwrap();
        let job = enqueue_job_for(
            &conn,
            None,
            "dedup",
            r#"{"mediaId":1,"outDir":"D:/o"}"#,
            Some(d),
        )
        .unwrap();
        mark_job_running(&conn, job.id).unwrap();

        let n = clear_pipeline_runs(&conn).unwrap();
        assert_eq!(n, 1, "只该清掉卡住的那个");
        let left = list_pipeline_runs(&conn, 20).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, live, "真在跑的必须留着");
        assert!(list_pipeline_dramas(&conn, stuck).unwrap().is_empty());
    }

    /// 复用的工程里，原片已经不在的素材记录要清掉。
    ///
    /// 实测：一个工程积了 68 条素材，其中 55 条原片早删了，
    /// 界面上显示成「68 集」，用户看着莫名其妙（实际只有 6 集）。
    #[test]
    fn stale_media_records_are_pruned_but_live_ones_are_kept() {
        let conn = mem();
        let project = create_project(&conn, "某剧", "D:/某剧").unwrap();
        let probe = MediaProbe {
            duration_ms: 1000,
            width: 720,
            height: 1280,
            has_audio: true,
            size_bytes: 10,
            format_name: "mp4".into(),
        };

        // 造一个真的存在的文件，和一个不存在的
        let dir = std::env::temp_dir().join("dramadist_prune");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let alive = dir.join("还在.mp4");
        std::fs::write(&alive, b"x").unwrap();

        let keep = add_media(&conn, project.id, &alive.to_string_lossy(), "还在.mp4", &probe)
            .unwrap();
        let gone = add_media(
            &conn,
            project.id,
            &dir.join("已删除.mp4").to_string_lossy(),
            "已删除.mp4",
            &probe,
        )
        .unwrap();

        let n = prune_missing_media(&conn, project.id).unwrap();
        assert_eq!(n, 1, "只该清掉原片不在的那一条");
        assert_eq!(list_media(&conn, project.id).unwrap().len(), 1);
        assert!(get_media(&conn, keep.id).is_ok(), "原片还在的必须保留");
        assert!(get_media(&conn, gone.id).is_err(), "原片不在的该清掉");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **有任务在跑的素材不能清** —— 删了会让正在跑的任务撞外键。
    /// 这个坑真踩过（run#7 整条流水线报 FOREIGN KEY constraint failed）。
    #[test]
    fn pruning_never_touches_media_with_running_jobs() {
        let conn = mem();
        let project = create_project(&conn, "某剧", "D:/某剧").unwrap();
        let probe = MediaProbe {
            duration_ms: 1000,
            width: 720,
            height: 1280,
            has_audio: true,
            size_bytes: 10,
            format_name: "mp4".into(),
        };
        let m = add_media(&conn, project.id, "D:/不存在的.mp4", "不存在的.mp4", &probe).unwrap();
        let job = enqueue_job(&conn, Some(m.id), "dedup", r#"{"mediaId":1,"outDir":"D:/o"}"#)
            .unwrap();
        mark_job_running(&conn, job.id).unwrap();

        assert_eq!(prune_missing_media(&conn, project.id).unwrap(), 0, "有任务在跑就别动它");
        assert!(get_media(&conn, m.id).is_ok());
    }

    /// **版本号被错误推高时，该补的列还是要补上。**
    ///
    /// 真实踩过：应用曾用一个「只改了 SCHEMA_VERSION、忘了写迁移分支」的
    /// 中间构建启动过 —— 版本号推到 12，但 `run_log.artifact` 根本没加。
    /// 之后再怎么正确构建，`if version < 12` 都进不去，那个 ALTER
    /// **永远不会执行**，而库看起来还是最新的。
    ///
    /// 所以 `ensure_column` 不靠版本号，直接查表结构 —— 幂等，每次启动核对。
    #[test]
    fn missing_columns_are_repaired_even_if_the_version_looks_current() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        // 造出那个坏现场：**schema 停在 v11，但版本号被推到了最新**。
        // 按真实顺序把 V1~V11 全跑一遍 —— 少跑任何一段，
        // 后面的 SCHEMA 会因为依赖的表不存在而失败。
        for schema in [
            SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7,
            SCHEMA_V8, SCHEMA_V9, SCHEMA_V10, SCHEMA_V11,
        ] {
            conn.execute_batch(schema).unwrap();
        }
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
            .unwrap();

        // 确认现场：run_log 在，但没有 artifact
        let before: Vec<String> = conn
            .prepare("PRAGMA table_info(run_log)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(before.contains(&"message".to_string()), "run_log 该在");
        assert!(!before.contains(&"artifact".to_string()), "这时候还没这列");

        // 版本号说"已经是最新"，但 migrate 仍然要把列补上
        migrate(&conn).unwrap();

        let after: Vec<String> = conn
            .prepare("PRAGMA table_info(run_log)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(after.contains(&"artifact".to_string()), "列必须被补上：{after:?}");

        // 再跑一次不能出错（幂等）
        migrate(&conn).unwrap();
    }

    /// 日志行能带上「这一行对应哪个产出文件」。
    ///
    /// 用户的想法：「产出的内容在日志里尽量能直接点击播放」——
    /// 对。产出不该是另一个列表，它就该长在产生它的那一行上。
    #[test]
    fn log_lines_can_carry_the_artifact_they_produced() {
        let conn = mem();
        append_log(&conn, Some(1), None, "info", "===== 整集去重 =====");
        append_log_with_file(
            &conn,
            Some(1),
            Some(7),
            "ok",
            "✓ 第01集.mp4 整集去重：去重完成（56.0 MB）",
            "D:/二创/某剧/去重/第01集_去重.mp4",
        );

        let rows = list_logs(&conn, Some(1), 0, 10).unwrap();
        assert_eq!(rows.len(), 2);
        // 标题行没有文件 —— 界面上那一行就不可点
        assert_eq!(rows[0].artifact, "", "标题行不该带文件");
        // 明细行有文件 —— 那一行能点开播
        assert_eq!(rows[1].artifact, "D:/二创/某剧/去重/第01集_去重.mp4");
        assert!(rows[1].message.contains("第01集.mp4"));
    }

    /// 步骤标题是**分节**，一次运行里一个步骤只出现一次。
    ///
    /// 用户先后问过两次「怎么执行这么多次，执行一次不行嘛」。
    ///
    /// **关键是判据不能是「最近一条是不是标题」**：61 集分十几批跑，
    /// 每批跑完最近一条都是明细行，于是每批都会再打一次标题 ——
    /// 那正是用户截图里看到的样子。判据必须是「这次运行里写过没有」。
    #[test]
    fn stage_header_is_written_once_per_run() {
        let conn = mem();
        let header = "===== 整集去重 =====";

        // 第一次：没写过 → 该写
        assert!(!has_log_with_message(&conn, 7, 1, header).unwrap());
        append_log(&conn, Some(1), Some(7), "info", header);
        assert!(has_log_with_message(&conn, 7, 1, header).unwrap());

        // **中间插了明细行之后，仍然算写过** —— 这正是旧判据栽的地方
        append_log(&conn, Some(1), Some(7), "ok", "✓ 第01集.mp4 整集去重：去重完成…");
        append_log(&conn, Some(1), Some(7), "ok", "✓ 第02集.mp4 整集去重：去重完成…");
        assert!(
            has_log_with_message(&conn, 7, 1, header).unwrap(),
            "明细行插进来之后，标题仍然算写过 —— 否则每一批都会再打一次"
        );

        // 另一次运行要重新打（那是新的一轮）
        assert!(!has_log_with_message(&conn, 7, 2, header).unwrap());
        // 别的剧不受影响
        assert!(!has_log_with_message(&conn, 8, 1, header).unwrap());
        // 别的步骤不受影响
        assert!(!has_log_with_message(&conn, 7, 1, "===== 视频切片 =====").unwrap());
    }

    #[test]
    fn counting_pending_and_done_jobs_by_kind() {
        let conn = mem();
        let run = create_pipeline_run(&conn, "D:/源", "D:/二创", &[]).unwrap();
        let drama = add_pipeline_drama(&conn, run, "某剧", "D:/源", "D:/二创/某剧", 3, None)
            .unwrap();

        for i in 1..=3 {
            enqueue_job_for(
                &conn,
                None,
                "dedup",
                &format!(r#"{{"mediaId":{i},"outDir":"D:/o"}}"#),
                Some(drama),
            )
            .unwrap();
        }
        enqueue_job_for(
            &conn,
            None,
            "slice",
            r#"{"mediaId":1,"mode":{"mode":"everySeconds","seconds":10},"outDir":"D:/o"}"#,
            Some(drama),
        )
        .unwrap();

        assert_eq!(count_pending_jobs_of_kind(&conn, "dedup").unwrap(), 3);
        assert_eq!(count_pending_jobs_of_kind(&conn, "slice").unwrap(), 1);
        assert_eq!(count_done_jobs_of_kind(&conn, run, "dedup").unwrap(), 0);

        // 跑完两个之后
        let jobs = next_pending_jobs_of_kind(&conn, "dedup", 2).unwrap();
        for j in &jobs {
            finish_job(&conn, j.id, JOB_DONE, "好了").unwrap();
        }
        assert_eq!(count_pending_jobs_of_kind(&conn, "dedup").unwrap(), 1, "还剩一个");
        assert_eq!(
            count_done_jobs_of_kind(&conn, run, "dedup").unwrap(),
            2,
            "完成数要分开统计 —— 收口汇总要用"
        );
    }

    /// 日志：能按运行过滤、能增量拉取、写坏了不抛错。
    #[test]
    fn logs_are_filterable_and_incremental() {
        let conn = mem();
        append_log(&conn, Some(1), None, "info", "扫描完成");
        append_log(&conn, Some(1), Some(7), "warn", "第 03 集有重复副本");
        append_log(&conn, Some(2), None, "info", "另一次运行");

        // 全部
        let all = list_logs(&conn, None, 0, 100).unwrap();
        assert_eq!(all.len(), 3);

        // 只看某次运行 —— 日志页的主要用法
        let run1 = list_logs(&conn, Some(1), 0, 100).unwrap();
        assert_eq!(run1.len(), 2);
        assert!(run1.iter().all(|l| l.run_id == Some(1)));
        assert_eq!(run1[0].message, "扫描完成");
        assert_eq!(run1[1].drama_id, Some(7));

        // 增量：只要新行，不重复搬已经看过的
        let last = run1.last().unwrap().id;
        append_log(&conn, Some(1), None, "ok", "已排 12 个任务");
        let fresh = list_logs(&conn, Some(1), last, 100).unwrap();
        assert_eq!(fresh.len(), 1, "只该拿到新增的那一行");
        assert_eq!(fresh[0].message, "已排 12 个任务");
    }

    /// 这次运行开始之后写的、还没归属的日志也要看到。
    ///
    /// 下载原片写在 run 记录之前（或两次运行之间）。不带上它们，
    /// 用户按运行过滤时只会看到流水线自己那几行，下载失败像没发生过。
    #[test]
    fn logs_include_unattached_lines_written_after_the_run_started() {
        let conn = mem();
        append_log(&conn, None, None, "info", "很久以前的下载");
        append_log(&conn, Some(1), None, "info", "运行开始");
        append_log(&conn, None, None, "error", "这次运行旁边的下载失败");

        let rows = list_logs(&conn, Some(1), 0, 100).unwrap();
        assert_eq!(rows.len(), 2, "运行之前的旧日志不该混进来");
        assert_eq!(rows[0].message, "运行开始");
        assert_eq!(rows[1].message, "这次运行旁边的下载失败");
    }

    /// 某个运行一条自己的日志都没写过时，它旁边的下载日志也得看得见。
    ///
    /// 真实撞到过：只下载没开流水线，最新的 run 是早先失败的那次，
    /// 它没有任何 run_log —— `MIN(id)` 为 NULL，日志页一片空白。
    #[test]
    fn logs_fall_back_to_run_start_time_when_run_has_no_logs() {
        let conn = mem();
        append_log(&conn, None, None, "info", "这次下载写了这一行");
        conn.execute(
            "INSERT INTO pipeline_run(root_dir, out_dir, stages, status, created_at, updated_at)
             VALUES('D:/src', '', '[]', 'failed', '2000-01-01 00:00:00', '2000-01-01 00:00:00')",
            [],
        )
        .unwrap();
        let run_id = conn.last_insert_rowid();

        let rows = list_logs(&conn, Some(run_id), 0, 100).unwrap();
        assert_eq!(rows.len(), 1, "运行没有日志时，按开始时间把下载日志带上");
        assert_eq!(rows[0].message, "这次下载写了这一行");
    }

    #[test]
    fn log_writes_never_fail_the_caller() {
        // 日志写不进去不该让流水线挂掉 —— 表被删了也不 panic
        let conn = mem();
        conn.execute("DROP TABLE run_log", []).unwrap();
        append_log(&conn, Some(1), None, "info", "这条写不进去");
        // 到这里没 panic 就算过
        assert!(list_logs(&conn, Some(1), 0, 10).is_err(), "读要如实报错");
    }

    /// 删素材 / 删工程必须连带清掉挂着的任务。
    ///
    /// 不清的话，队列里剩下的任务跑到写字幕那一步会撞外键 ——
    /// 实测发生过：run#7 整条流水线报「FOREIGN KEY constraint failed」。
    #[test]
    fn deleting_media_takes_its_jobs_and_data_with_it() {
        let mut conn = mem();
        let project = create_project(&conn, "某剧", "D:/某剧").unwrap();
        let probe = MediaProbe {
            duration_ms: 1000,
            width: 720,
            height: 1280,
            has_audio: true,
            size_bytes: 10,
            format_name: "mp4".into(),
        };
        let media = add_media(&conn, project.id, "D:/某剧/第01集.mp4", "第01集.mp4", &probe)
            .unwrap();

        insert_segment(&conn, media.id, 1, 0, 1000, "D:/out/001.mp4", 10, "ok", "").unwrap();
        replace_cues(
            &mut conn,
            media.id,
            &[Cue {
                id: 0,
                media_id: media.id,
                index_no: 1,
                start_ms: 0,
                end_ms: 500,
                text: "你好".into(),
            }],
        )
        .unwrap();
        enqueue_job(&conn, Some(media.id), "slice", OK_SLICE).unwrap();

        remove_media(&conn, media.id).unwrap();

        let count = |sql: &str| -> i64 {
            conn.query_row(sql, params![media.id], |r| r.get(0)).unwrap()
        };
        assert_eq!(count("SELECT COUNT(*) FROM cue WHERE media_id=?1"), 0, "字幕该跟着删");
        assert_eq!(
            count("SELECT COUNT(*) FROM segment WHERE media_id=?1"),
            0,
            "切片该跟着删"
        );
        assert_eq!(
            job_counts(&conn).unwrap().total,
            0,
            "挂着这个素材的任务该跟着删 —— 否则它跑起来会撞外键"
        );
    }

    #[test]
    fn deleting_a_project_clears_everything_under_it() {
        let conn = mem();
        let probe = MediaProbe {
            duration_ms: 1000,
            width: 720,
            height: 1280,
            has_audio: true,
            size_bytes: 10,
            format_name: "mp4".into(),
        };
        let project = create_project(&conn, "某剧", "D:/某剧").unwrap();
        for n in 1..=3 {
            let m = add_media(
                &conn,
                project.id,
                &format!("D:/某剧/第0{n}集.mp4"),
                &format!("第0{n}集.mp4"),
                &probe,
            )
            .unwrap();
            enqueue_job(&conn, Some(m.id), "slice", OK_SLICE).unwrap();
        }
        assert_eq!(list_media(&conn, project.id).unwrap().len(), 3);

        delete_project(&conn, project.id).unwrap();

        assert!(list_projects(&conn).unwrap().is_empty());
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM media", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0,
            "工程下的素材该跟着删"
        );
        assert_eq!(job_counts(&conn).unwrap().total, 0, "任务也该跟着删");
    }

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
