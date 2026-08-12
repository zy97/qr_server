use actix_web::{get, post, web, HttpResponse, Responder};
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use crate::err::CustomError;

const STATIC_DIR: &str = "static";
const TEMPLATE_DIR: &str = "templates";
const TEMPLATE_JSON_FILE: &str = "label_template.json";
const TEMPLATE_HTML_FILE: &str = "template.html";

/// 模板保存串行化，避免并发请求写坏文件
static SAVE_LOCK: Mutex<()> = Mutex::new(());

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(designer_page)
        .service(designer_asset)
        .service(get_template)
        .service(save_template);
}

/// 标签模板设计器页面（hiprint 拖拽设计）
#[get("/designer")]
async fn designer_page() -> Result<impl Responder, CustomError> {
    let html = fs::read_to_string(static_path("designer.html"))?;
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

/// 读取 hiprint 模板 JSON（设计器打开时回显）
#[get("/api/template")]
async fn get_template() -> Result<impl Responder, CustomError> {
    let json = fs::read_to_string(template_path(TEMPLATE_JSON_FILE))?;
    Ok(HttpResponse::Ok()
        .content_type("application/json; charset=utf-8")
        .body(json))
}

#[derive(Deserialize)]
pub struct SaveTemplateRequest {
    /// hiprint 设计器导出的模板 JSON（设计源文件，供下次继续编辑）
    json: serde_json::Value,
    /// 由 hiprint getHtml(占位符数据) 生成的完整 HTML（Tera 渲染产物）
    html: String,
}

/// 保存模板：同时写入设计源 JSON 和渲染用 HTML
#[post("/api/template")]
async fn save_template(
    body: web::Json<SaveTemplateRequest>,
) -> Result<impl Responder, CustomError> {
    let _guard = SAVE_LOCK
        .lock()
        .map_err(|_| CustomError::OtherLibraryError("template save lock poisoned".to_string()))?;
    save_template_files(Path::new(TEMPLATE_DIR), &body)?;
    Ok(HttpResponse::Ok().finish())
}

fn save_template_files(dir: &Path, req: &SaveTemplateRequest) -> Result<(), CustomError> {
    if !req
        .json
        .get("panels")
        .map(|p| p.is_array())
        .unwrap_or(false)
    {
        return Err(CustomError::OtherLibraryError(
            "模板 JSON 缺少 panels 数组".to_string(),
        ));
    }
    if !req.html.contains("id=\"app\"") {
        return Err(CustomError::OtherLibraryError(
            "模板 HTML 缺少 #app 容器".to_string(),
        ));
    }
    let json = serde_json::to_string_pretty(&req.json)
        .map_err(|e| CustomError::OtherLibraryError(format!("模板 JSON 序列化失败: {e}")))?;
    write_atomic(&dir.join(TEMPLATE_JSON_FILE), json.as_bytes())?;
    write_atomic(&dir.join(TEMPLATE_HTML_FILE), req.html.as_bytes())?;
    Ok(())
}

/// 先写临时文件再重命名，避免渲染中途读到写了一半的模板
fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), CustomError> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)?;
    Ok(())
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

fn template_path(name: &str) -> PathBuf {
    Path::new(TEMPLATE_DIR).join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "qr_service_designer_test_{}_{}",
            std::process::id(),
            tag
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn sample_request() -> SaveTemplateRequest {
        SaveTemplateRequest {
            json: serde_json::json!({"panels": [{"width": 150, "height": 80, "printElements": []}]}),
            html: "<html><body><div id=\"app\">{{ part_no }}</div></body></html>".to_string(),
        }
    }

    #[test]
    fn saves_template_json_and_html_atomically() {
        let dir = temp_dir("save");
        save_template_files(&dir, &sample_request()).expect("save template files");

        let json = fs::read_to_string(dir.join(TEMPLATE_JSON_FILE)).expect("read json");
        assert!(json.contains("\"panels\""));
        let html = fs::read_to_string(dir.join(TEMPLATE_HTML_FILE)).expect("read html");
        assert!(html.contains("{{ part_no }}"));
        // 临时文件不应残留
        assert!(!dir.join("label_template.tmp").exists());
        assert!(!dir.join("template.tmp").exists());
    }

    #[test]
    fn rejects_template_without_panels() {
        let dir = temp_dir("no_panels");
        let mut req = sample_request();
        req.json = serde_json::json!({"foo": 1});

        let err = save_template_files(&dir, &req).expect_err("should reject");
        assert!(matches!(err, CustomError::OtherLibraryError(_)));
        assert!(!dir.join(TEMPLATE_HTML_FILE).exists());
    }

    #[test]
    fn rejects_html_without_app_container() {
        let dir = temp_dir("no_app");
        let mut req = sample_request();
        req.html = "<html><body></body></html>".to_string();

        let err = save_template_files(&dir, &req).expect_err("should reject");
        assert!(matches!(err, CustomError::OtherLibraryError(_)));
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
