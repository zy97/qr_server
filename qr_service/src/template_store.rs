//! 标签模板存储：SQLite 本地库，支持多模板管理与默认模板约束。
//! - 默认模板（is_default=1）用于 /label 渲染，不可删除；
//! - 默认模板的渲染 HTML 会同步写入 templates/template.html，
//!   供各渲染分支按文件加载（避免渲染链路感知数据库）；
//! - 首次启动若库为空，用仓库自带的 label_template.json/template.html 播种默认模板。
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::err::CustomError;

const TEMPLATE_DIR: &str = "templates";
const DB_FILE: &str = "templates.db";
const SEED_JSON_FILE: &str = "label_template.json";
const RENDER_HTML_FILE: &str = "template.html";
const DEFAULT_TEMPLATE_NAME: &str = "默认标签模板";

/// 新建模板时的空白 hiprint 面板（150mm x 70mm）
const BLANK_DESIGN_JSON: &str = r#"{"panels":[{"index":0,"width":150,"height":70,"paperHeader":0,"paperFooter":198.4,"paperNumberDisabled":true,"printElements":[]}]}"#;

fn blank_render_html() -> String {
    "<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n<meta charset=\"UTF-8\">\n<style>html, body { margin: 0; padding: 0; }\n#app { position: relative; width: 150mm; height: 70mm; overflow: hidden; }</style>\n</head>\n<body>\n<div id=\"app\"></div>\n</body>\n</html>".to_string()
}

#[derive(Debug, Serialize)]
pub struct TemplateSummary {
    pub id: i64,
    pub name: String,
    pub is_default: bool,
    pub updated_at: i64,
}

#[derive(Debug, Serialize)]
pub struct TemplateRecord {
    pub id: i64,
    pub name: String,
    pub is_default: bool,
    pub updated_at: i64,
    pub design_json: serde_json::Value,
    pub render_html: String,
}

static DB: LazyLock<Mutex<Connection>> =
    LazyLock::new(|| Mutex::new(open(&db_path()).expect("failed to open template database")));

fn db_path() -> PathBuf {
    Path::new(TEMPLATE_DIR).join(DB_FILE)
}

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

fn open(path: &Path) -> Result<Connection, CustomError> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS templates (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            name        TEXT NOT NULL,
            design_json TEXT NOT NULL,
            render_html TEXT NOT NULL,
            is_default  INTEGER NOT NULL DEFAULT 0,
            updated_at  INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS agent_settings (
            station TEXT PRIMARY KEY,
            printer TEXT,
            paper_width REAL,
            paper_height REAL
        );",
    )?;
    Ok(conn)
}

/// 工位级打印设置覆盖。字段为 None（NULL）时不覆盖：
/// 打印机回退代理本地配置，纸张宽高回退服务器 config.toml 的 [print] 段
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentSettings {
    pub printer: Option<String>,
    /// 自定义纸张宽度（cm）
    pub paper_width: Option<f64>,
    /// 自定义纸张高度（cm）
    pub paper_height: Option<f64>,
}

/// 保存指定工位的打印设置覆盖；同工位重复保存即更新
pub fn set_agent_settings(
    conn: &Connection,
    station: &str,
    settings: &AgentSettings,
) -> Result<(), CustomError> {
    conn.execute(
        "INSERT INTO agent_settings (station, printer, paper_width, paper_height)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(station) DO UPDATE SET
             printer = excluded.printer,
             paper_width = excluded.paper_width,
             paper_height = excluded.paper_height",
        params![
            station,
            settings.printer,
            settings.paper_width,
            settings.paper_height
        ],
    )?;
    Ok(())
}

/// 读取工位的打印设置覆盖；从未设置返回 None
pub fn get_agent_settings(
    conn: &Connection,
    station: &str,
) -> Result<Option<AgentSettings>, CustomError> {
    Ok(conn
        .query_row(
            "SELECT printer, paper_width, paper_height FROM agent_settings WHERE station = ?1",
            params![station],
            |r| {
                Ok(AgentSettings {
                    printer: r.get(0)?,
                    paper_width: r.get(1)?,
                    paper_height: r.get(2)?,
                })
            },
        )
        .optional()?)
}

/// 清除工位的打印设置覆盖
pub fn delete_agent_settings(conn: &Connection, station: &str) -> Result<(), CustomError> {
    conn.execute(
        "DELETE FROM agent_settings WHERE station = ?1",
        params![station],
    )?;
    Ok(())
}

fn lock_db() -> MutexGuard<'static, Connection> {
    DB.lock().unwrap_or_else(|e| e.into_inner())
}

/// 启动时初始化：空库播种默认模板，并把默认模板渲染 HTML 同步到 template.html
pub fn init() -> Result<(), CustomError> {
    let conn = lock_db();
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM templates", [], |r| r.get(0))?;
    if count == 0 {
        let design = fs::read_to_string(Path::new(TEMPLATE_DIR).join(SEED_JSON_FILE))?;
        let html = fs::read_to_string(Path::new(TEMPLATE_DIR).join(RENDER_HTML_FILE))?;
        seed_default(&conn, DEFAULT_TEMPLATE_NAME, &design, &html)?;
    }
    sync_render_file(&conn)
}

fn seed_default(
    conn: &Connection,
    name: &str,
    design_json: &str,
    render_html: &str,
) -> Result<(), CustomError> {
    // 播种前校验设计源 JSON 合法，避免把坏文件写进库
    serde_json::from_str::<serde_json::Value>(design_json)
        .map_err(|e| CustomError::OtherLibraryError(format!("种子模板 JSON 非法: {e}")))?;
    conn.execute(
        "INSERT INTO templates (name, design_json, render_html, is_default, updated_at) VALUES (?1, ?2, ?3, 1, ?4)",
        params![name, design_json, render_html, now_ts()],
    )?;
    Ok(())
}

/// 把默认模板的渲染 HTML 写入 templates/template.html（仅供本地查看，受 [template] save_html 控制）。
/// 只应由路由层/启动流程调用；单元测试用内存库，不得触碰真实文件。渲染以数据库为准
pub fn sync_render_file(conn: &Connection) -> Result<(), CustomError> {
    if !crate::config::CONFIG.template.save_html {
        return Ok(());
    }
    let html: String = conn.query_row(
        "SELECT render_html FROM templates WHERE is_default = 1",
        [],
        |r| r.get(0),
    )?;
    fs::write(Path::new(TEMPLATE_DIR).join(RENDER_HTML_FILE), html)?;
    Ok(())
}

/// 把指定渲染 HTML 落地到 templates/template.html（仅供本地查看，受 [template] save_html 控制）。
/// 渲染已走数据库，此文件只是查看用；保存模板时写入被保存模板的内容
pub fn write_render_file(html: &str) -> Result<(), CustomError> {
    if !crate::config::CONFIG.template.save_html {
        return Ok(());
    }
    fs::write(Path::new(TEMPLATE_DIR).join(RENDER_HTML_FILE), html)?;
    Ok(())
}

pub fn list(conn: &Connection) -> Result<Vec<TemplateSummary>, CustomError> {
    let mut stmt = conn.prepare(
        "SELECT id, name, is_default, updated_at FROM templates ORDER BY is_default DESC, updated_at DESC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(TemplateSummary {
                id: r.get(0)?,
                name: r.get(1)?,
                is_default: r.get::<_, i64>(2)? == 1,
                updated_at: r.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn record_by_id(conn: &Connection, id: i64) -> Result<TemplateRecord, CustomError> {
    let record = conn.query_row(
        "SELECT id, name, is_default, updated_at, design_json, render_html FROM templates WHERE id = ?1",
        params![id],
        |r| {
            let design: String = r.get(4)?;
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, design, r.get::<_, String>(5)?))
        },
    );
    match record {
        Ok((id, name, is_default, updated_at, design, html)) => Ok(TemplateRecord {
            id,
            name,
            is_default: is_default == 1,
            updated_at,
            design_json: serde_json::from_str(&design)?,
            render_html: html,
        }),
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            Err(CustomError::OtherLibraryError(format!("模板不存在: {id}")))
        }
        Err(e) => Err(e.into()),
    }
}

/// id 传 None 时返回默认模板
pub fn get(conn: &Connection, id: Option<i64>) -> Result<TemplateRecord, CustomError> {
    match id {
        Some(id) => record_by_id(conn, id),
        None => {
            let default_id: i64 =
                conn.query_row("SELECT id FROM templates WHERE is_default = 1", [], |r| {
                    r.get(0)
                })?;
            record_by_id(conn, default_id)
        }
    }
}

pub fn create(conn: &Connection, name: &str) -> Result<i64, CustomError> {
    if name.trim().is_empty() {
        return Err(CustomError::OtherLibraryError(
            "模板名称不能为空".to_string(),
        ));
    }
    conn.execute(
        "INSERT INTO templates (name, design_json, render_html, is_default, updated_at) VALUES (?1, ?2, ?3, 0, ?4)",
        params![name, BLANK_DESIGN_JSON, blank_render_html(), now_ts()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 复制模板：新名称为“原名 副本”
pub fn copy(conn: &Connection, id: i64) -> Result<i64, CustomError> {
    let record = record_by_id(conn, id)?;
    conn.execute(
        "INSERT INTO templates (name, design_json, render_html, is_default, updated_at) VALUES (?1, ?2, ?3, 0, ?4)",
        params![
            format!("{} 副本", record.name),
            serde_json::to_string(&record.design_json)?,
            record.render_html,
            now_ts()
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 保存设计（名称/设计 JSON/渲染 HTML 均可选）；若保存的是默认模板，同步渲染缓存文件
pub fn update(
    conn: &Connection,
    id: i64,
    name: Option<&str>,
    design_json: Option<&serde_json::Value>,
    render_html: Option<&str>,
) -> Result<(), CustomError> {
    let record = record_by_id(conn, id)?;
    let new_name = name.unwrap_or(&record.name);
    if new_name.trim().is_empty() {
        return Err(CustomError::OtherLibraryError(
            "模板名称不能为空".to_string(),
        ));
    }
    if let Some(json) = design_json {
        if !json.get("panels").map(|p| p.is_array()).unwrap_or(false) {
            return Err(CustomError::OtherLibraryError(
                "模板 JSON 缺少 panels 数组".to_string(),
            ));
        }
    }
    if let Some(html) = render_html {
        if !html.contains("id=\"app\"") {
            return Err(CustomError::OtherLibraryError(
                "模板 HTML 缺少 #app 容器".to_string(),
            ));
        }
    }
    let design = match design_json {
        Some(json) => serde_json::to_string(json)?,
        None => serde_json::to_string(&record.design_json)?,
    };
    let html = render_html.unwrap_or(&record.render_html);
    conn.execute(
        "UPDATE templates SET name = ?1, design_json = ?2, render_html = ?3, updated_at = ?4 WHERE id = ?5",
        params![new_name, design, html, now_ts(), id],
    )?;
    Ok(())
}

/// 设为默认模板（同时取消其模板的默认标记），并同步渲染缓存文件
pub fn set_default(conn: &Connection, id: i64) -> Result<(), CustomError> {
    record_by_id(conn, id)?;
    conn.execute("UPDATE templates SET is_default = 0", [])?;
    conn.execute(
        "UPDATE templates SET is_default = 1, updated_at = ?1 WHERE id = ?2",
        params![now_ts(), id],
    )?;
    Ok(())
}

/// 删除模板；默认模板不可删除
pub fn delete(conn: &Connection, id: i64) -> Result<(), CustomError> {
    let record = record_by_id(conn, id)?;
    if record.is_default {
        return Err(CustomError::OtherLibraryError(
            "默认模板不能删除".to_string(),
        ));
    }
    conn.execute("DELETE FROM templates WHERE id = ?1", params![id])?;
    Ok(())
}

/// 渲染用模板 HTML：selector 为空用默认模板；数字按 id、其它按名称（取最近更新的一条）
pub fn render_html_for(conn: &Connection, selector: Option<&str>) -> Result<String, CustomError> {
    let row = |sql: &str, p: Option<String>| -> Result<String, CustomError> {
        conn.query_row(sql, rusqlite::params_from_iter(p.iter()), |r| r.get(0))
            .map_err(CustomError::from)
    };
    match selector {
        None => row(
            "SELECT render_html FROM templates WHERE is_default = 1",
            None,
        ),
        Some(s) if s.parse::<i64>().is_ok() => row(
            "SELECT render_html FROM templates WHERE id = ?1",
            Some(s.to_string()),
        ),
        Some(s) => row(
            "SELECT render_html FROM templates WHERE name = ?1 ORDER BY updated_at DESC LIMIT 1",
            Some(s.to_string()),
        ),
    }
    .map_err(|e| match e {
        CustomError::DbError(_) => CustomError::OtherLibraryError(format!(
            "模板不存在: {}",
            selector.unwrap_or("<default>")
        )),
        other => other,
    })
}

/// 供路由层使用的全局连接
pub fn with_db<T>(f: impl FnOnce(&Connection) -> Result<T, CustomError>) -> Result<T, CustomError> {
    f(&lock_db())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(
            "CREATE TABLE templates (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                name        TEXT NOT NULL,
                design_json TEXT NOT NULL,
                render_html TEXT NOT NULL,
                is_default  INTEGER NOT NULL DEFAULT 0,
                updated_at  INTEGER NOT NULL
            );
            CREATE TABLE agent_settings (
                station TEXT PRIMARY KEY,
                printer TEXT,
                paper_width REAL,
                paper_height REAL
            );",
        )
        .expect("create table");
        conn
    }


    #[test]
    fn agent_settings_override_roundtrip() {
        let conn = test_conn();
        assert_eq!(get_agent_settings(&conn, "S1").unwrap(), None);
        let settings = AgentSettings {
            printer: Some("Zebra A".to_string()),
            paper_width: Some(10.0),
            paper_height: None, // 允许只覆盖部分字段
        };
        set_agent_settings(&conn, "S1", &settings).unwrap();
        assert_eq!(get_agent_settings(&conn, "S1").unwrap(), Some(settings));
        let updated = AgentSettings {
            printer: Some("Zebra B".to_string()),
            paper_width: None,
            paper_height: Some(7.0),
        };
        set_agent_settings(&conn, "S1", &updated).unwrap(); // 重复保存即整体更新
        assert_eq!(get_agent_settings(&conn, "S1").unwrap(), Some(updated));
        delete_agent_settings(&conn, "S1").unwrap();
        assert_eq!(get_agent_settings(&conn, "S1").unwrap(), None);
    }
    fn seed_test_default(conn: &Connection) {
        seed_default(
            conn,
            DEFAULT_TEMPLATE_NAME,
            r#"{"panels":[{"width":150,"height":70,"printElements":[]}]}"#,
            r#"<div id="app">{{ part_no }}</div>"#,
        )
        .expect("seed default");
    }

    #[test]
    fn crud_and_copy_roundtrip() {
        let conn = test_conn();
        seed_test_default(&conn);

        let templates = list(&conn).expect("list");
        assert_eq!(templates.len(), 1);
        assert!(templates[0].is_default);

        let default = get(&conn, None).expect("get default");
        assert_eq!(default.name, DEFAULT_TEMPLATE_NAME);

        let new_id = create(&conn, "新模板").expect("create");
        let copied_id = copy(&conn, new_id).expect("copy");
        let copied = get(&conn, Some(copied_id)).expect("get copied");
        assert_eq!(copied.name, "新模板 副本");
        assert!(!copied.is_default);

        update(
            &conn,
            copied_id,
            Some("改名"),
            Some(&serde_json::json!({"panels": []})),
            Some(r#"<div id="app">x</div>"#),
        )
        .expect("update");
        let updated = get(&conn, Some(copied_id)).expect("get updated");
        assert_eq!(updated.name, "改名");

        delete(&conn, copied_id).expect("delete");
        assert_eq!(list(&conn).expect("list").len(), 2);
    }
    #[test]
    fn render_html_for_selects_by_id_name_and_default() {
        let conn = test_conn();
        seed_test_default(&conn);
        let new_id = create(&conn, "新模板").expect("create");
        let default = get(&conn, None).expect("default");

        assert!(render_html_for(&conn, None)
            .expect("default")
            .contains("{{ part_no }}"));
        assert_eq!(
            render_html_for(&conn, Some(&default.id.to_string())).expect("by id"),
            render_html_for(&conn, None).expect("default")
        );
        assert!(render_html_for(&conn, Some("新模板"))
            .expect("by name")
            .contains("id=\"app\""));
        assert!(render_html_for(&conn, Some("9999")).is_err());
        assert!(render_html_for(&conn, Some("不存在")).is_err());
        let _ = new_id;
    }

    #[test]
    fn default_template_cannot_be_deleted() {
        let conn = test_conn();
        seed_test_default(&conn);
        let default = get(&conn, None).expect("get default");

        let err = delete(&conn, default.id).expect_err("delete default should fail");
        assert!(err.to_string().contains("默认模板不能删除"));
    }

    #[test]
    fn set_default_switches_flag() {
        let conn = test_conn();
        seed_test_default(&conn);
        let old_default = get(&conn, None).expect("get default");
        let new_id = create(&conn, "新模板").expect("create");

        set_default(&conn, new_id).expect("set default");
        let now_default = get(&conn, None).expect("get new default");
        assert_eq!(now_default.id, new_id);
        // 原默认降级为普通模板，可以删除
        delete(&conn, old_default.id).expect("old default now deletable");
    }

    #[test]
    fn update_rejects_invalid_payload() {
        let conn = test_conn();
        seed_test_default(&conn);
        let default = get(&conn, None).expect("get default");

        assert!(update(
            &conn,
            default.id,
            None,
            Some(&serde_json::json!({"foo": 1})),
            None
        )
        .is_err());
        assert!(update(&conn, default.id, None, None, Some("<div></div>")).is_err());
        assert!(update(&conn, default.id, Some("  "), None, None).is_err());
        assert!(get(&conn, Some(9999)).is_err());
    }
}
