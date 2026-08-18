//! 模板设计器与管理页面的 HTTP 接口：
//! - 页面：GET /designer（拖拽设计）、GET /templates（模板管理）
//! - 静态资源：GET /designer/vendor/{file}（vendored hiprint 及依赖）
//! - 模板 API：/api/templates 增删查改、复制、设默认；默认模板渲染 HTML
//!   由 template_store 同步到 templates/template.html，供渲染分支按文件加载
use actix_web::{delete, get, post, put, web, HttpResponse, Responder};
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::err::CustomError;
use crate::template_store as store;

const STATIC_DIR: &str = "static";

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(designer_page)
        .service(manager_page)
        .service(agents_page)
        .service(designer_asset)
        .service(list_templates)
        .service(create_template)
        .service(get_template)
        .service(update_template)
        .service(delete_template)
        .service(copy_template)
        .service(set_default_template);
}

/// 标签模板设计器页面（hiprint 拖拽设计），?id=N 编辑指定模板，缺省编辑默认模板
#[get("/designer")]
async fn designer_page() -> Result<impl Responder, CustomError> {
    serve_static_page("designer.html")
}

/// 模板管理页面（列表/新建/复制/设默认/删除）
#[get("/templates")]
async fn manager_page() -> Result<impl Responder, CustomError> {
    serve_static_page("manager.html")
}

/// 打印代理在线管理页面
#[get("/agents")]
async fn agents_page() -> Result<impl Responder, CustomError> {
    serve_static_page("agents.html")
}
fn serve_static_page(name: &str) -> Result<HttpResponse, CustomError> {
    let html = fs::read_to_string(Path::new(STATIC_DIR).join(name))?;
    Ok(HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(html))
}

/// 设计器依赖的静态资源（vendored 的 hiprint 及其依赖，文件名白名单校验）
#[get("/designer/vendor/{filename}")]
async fn designer_asset(path: web::Path<String>) -> Result<impl Responder, CustomError> {
    let filename = path.into_inner();
    if !is_safe_asset_name(&filename) {
        return Err(CustomError::OtherLibraryError(format!(
            "invalid asset name: {filename}"
        )));
    }
    let bytes = fs::read(static_path("vendor").join(&filename))?;
    Ok(HttpResponse::Ok()
        .content_type(asset_content_type(&filename))
        .body(bytes))
}

/// 模板列表
#[get("/api/templates")]
async fn list_templates() -> Result<impl Responder, CustomError> {
    let list = store::with_db(|conn| store::list(conn))?;
    Ok(HttpResponse::Ok().json(list))
}

#[derive(Deserialize)]
pub struct CreateTemplateRequest {
    name: String,
}

/// 新建空白模板
#[post("/api/templates")]
async fn create_template(
    body: web::Json<CreateTemplateRequest>,
) -> Result<impl Responder, CustomError> {
    let id = store::with_db(|conn| store::create(conn, &body.name))?;
    Ok(HttpResponse::Ok().json(serde_json::json!({ "id": id })))
}

/// 读取模板（id 为 "default" 时返回默认模板，设计器回显用）
#[get("/api/templates/{id}")]
async fn get_template(path: web::Path<String>) -> Result<impl Responder, CustomError> {
    let id = parse_template_id(&path.into_inner())?;
    let record = store::with_db(|conn| store::get(conn, id))?;
    Ok(HttpResponse::Ok().json(record))
}

#[derive(Deserialize)]
pub struct UpdateTemplateRequest {
    /// 模板名称（可选）
    name: Option<String>,
    /// hiprint 设计器导出的模板 JSON（设计源文件，供下次继续编辑）
    json: Option<serde_json::Value>,
    /// 由 hiprint getHtml(占位符数据) 生成的完整 HTML（Tera 渲染产物）
    html: Option<String>,
}

/// 保存模板（改名 / 保存设计）
#[put("/api/templates/{id}")]
async fn update_template(
    path: web::Path<String>,
    body: web::Json<UpdateTemplateRequest>,
) -> Result<impl Responder, CustomError> {
    // "default" 解析为当前默认模板的实际 id
    let id = match parse_template_id(&path.into_inner())? {
        Some(id) => id,
        None => store::with_db(|conn| store::get(conn, None))?.id,
    };
    store::with_db(|conn| {
        store::update(
            conn,
            id,
            body.name.as_deref(),
            body.json.as_ref(),
            body.html.as_deref(),
        )
    })?;
    // 落地文件仅供本地查看：写被保存模板自己的渲染 HTML（渲染以数据库为准）
    if let Some(html) = body.html.as_deref() {
        store::write_render_file(html)?;
    }
    Ok(HttpResponse::Ok().finish())
}

/// 删除模板；默认模板不可删除
#[delete("/api/templates/{id}")]
async fn delete_template(path: web::Path<i64>) -> Result<impl Responder, CustomError> {
    store::with_db(|conn| store::delete(conn, path.into_inner()))?;
    Ok(HttpResponse::Ok().finish())
}

/// 一键复制模板
#[post("/api/templates/{id}/copy")]
async fn copy_template(path: web::Path<i64>) -> Result<impl Responder, CustomError> {
    let id = store::with_db(|conn| store::copy(conn, path.into_inner()))?;
    Ok(HttpResponse::Ok().json(serde_json::json!({ "id": id })))
}

/// 设为默认模板（/label 渲染用）
#[post("/api/templates/{id}/default")]
async fn set_default_template(path: web::Path<i64>) -> Result<impl Responder, CustomError> {
    store::with_db(|conn| store::set_default(conn, path.into_inner()))?;
    store::with_db(|conn| store::sync_render_file(conn))?;
    Ok(HttpResponse::Ok().finish())
}

/// "default" -> None（默认模板），否则解析数字 id
fn parse_template_id(raw: &str) -> Result<Option<i64>, CustomError> {
    if raw == "default" {
        return Ok(None);
    }
    raw.parse::<i64>()
        .map(Some)
        .map_err(|_| CustomError::OtherLibraryError(format!("invalid template id: {raw}")))
}

fn is_safe_asset_name(filename: &str) -> bool {
    !filename.is_empty()
        && filename
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

fn asset_content_type(filename: &str) -> &'static str {
    match Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
    {
        "js" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "png" => "image/png",
        _ => "application/octet-stream",
    }
}

fn static_path(name: &str) -> PathBuf {
    Path::new(STATIC_DIR).join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_template_id() {
        assert_eq!(parse_template_id("default").expect("default"), None);
        assert_eq!(parse_template_id("42").expect("numeric"), Some(42));
        assert!(parse_template_id("abc").is_err());
    }

    #[test]
    fn rejects_unsafe_asset_names() {
        assert!(!is_safe_asset_name("../Cargo.toml"));
        assert!(!is_safe_asset_name("a/b.js"));
        assert!(!is_safe_asset_name(""));
        assert!(is_safe_asset_name("vue-plugin-hiprint.js"));
        assert!(is_safe_asset_name("print-lock.css"));
    }
}
