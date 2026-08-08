use actix_web::{post, web, HttpResponse, Responder};
use std::{
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::{watch, Mutex, OnceCell},
    time::timeout,
};
use tracing::{error, info};

use crate::err::CustomError;
use crate::requests::dtos::create_lable_dto::LabelInfo;

const TEMPLATE_FILE: &str = "main.typ";
const DATA_FILE: &str = "data.json";
const OUTPUT_FILE: &str = "main.png";
const COMPILE_TIMEOUT: Duration = Duration::from_secs(10);

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(create_label);
}

/// typst watch 常驻进程：监控 main.typ 及其依赖（含 data.json）变化后自动重编译，
/// 避免每次请求都重新启动进程、扫描字体和加载 tiaoma 包（热编译约 50-90ms）。
struct TypstWatcher {
    /// 串行化渲染请求：watch 进程同一时间只编译一份 data.json
    render_lock: Mutex<()>,
    /// 编译完成通知，值为递增序号 + 编译结果
    compiled_rx: watch::Receiver<(u64, Result<(), String>)>,
    /// 持有句柄保证进程存活；测试结束时用于主动 kill，避免运行时退出悬挂
    #[allow(dead_code)]
    child: Mutex<Child>,
}

static WATCHER: OnceCell<TypstWatcher> = OnceCell::const_new();

async fn watcher() -> Result<&'static TypstWatcher, CustomError> {
    WATCHER
        .get_or_try_init(|| async {
            let mut child = Command::new("typst.exe")
                .arg("watch")
                .arg(TEMPLATE_FILE)
                .arg(OUTPUT_FILE)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()?;

            let (tx, rx) = watch::channel((0u64, Ok(())));
            let stderr = child.stderr.take().expect("stderr is piped");
            // watch 的编译状态输出在 stderr，逐行解析并广播编译结果；
            // 模板诊断信息紧跟在 "compiled with errors" 之后，以 error 级别输出便于排查
            tokio::spawn(async move {
                let mut seq = 0u64;
                let mut in_error_block = false;
                let mut lines = BufReader::new(stderr).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) if line.contains("compiled successfully") => {
                            in_error_block = false;
                            seq += 1;
                            let _ = tx.send((seq, Ok(())));
                        }
                        Ok(Some(line)) if line.contains("compiled with errors") => {
                            in_error_block = true;
                            seq += 1;
                            let _ = tx.send((seq, Err("typst 模板编译失败".to_string())));
                        }
                        Ok(Some(line))
                            if line.starts_with("watching") || line.contains("compiling") =>
                        {
                            in_error_block = false;
                        }
                        Ok(Some(line)) if in_error_block => {
                            error!(target: "typst", "{line}");
                        }
                        Ok(Some(_)) => {}
                        Ok(None) => {
                            error!("typst watch process exited unexpectedly");
                            let _ = tx.send((seq, Err("typst watch 进程已退出".to_string())));
                            break;
                        }
                        Err(err) => {
                            error!(error = %err, "failed to read typst watch output");
                            break;
                        }
                    }
                }
            });

            Ok::<_, CustomError>(TypstWatcher {
                render_lock: Mutex::new(()),
                compiled_rx: rx,
                child: Mutex::new(child),
            })
        })
        .await
}

#[post("/label")]
async fn create_label(labels: web::Json<Vec<LabelInfo>>) -> Result<impl Responder, CustomError> {
    let request_started = Instant::now();
    let labels = labels.into_inner();
    let label_count = labels.len();

    let mut result_image = None;
    for label in labels {
        let image = render_label(&label).await?;
        if crate::config::CONFIG.print.enabled {
            crate::print::print_label_png(&image)?;
        }
        result_image = Some(image);
    }

    // Command::new("powershell")
    //     .args([
    //         "-Command",
    //         "-NoProfile",
    //         "-WindowStyle",
    //         "Hidden",
    //         "Add-Type -AssemblyName System.Drawing;
    //      $pd = New-Object System.Drawing.Printing.PrintDocument;
    //      $pd.PrinterSettings.PrinterName = 'NPIFD3D7B (HP LaserJet MFP M233sdw)';
    //      $pd.add_PrintPage({
    //          param($s, $e)
    //          $e.Graphics.DrawString(
    //              'Hello World',
    //              (New-Object Drawing.Font('Arial', 20)),
    //              [Drawing.Brushes]::Black,
    //              100, 100
    //          )
    //      });
    //      $pd.Print();",
    //     ])
    //     .spawn()
    //     .unwrap();

    let result_image = result_image
        .ok_or_else(|| CustomError::OtherLibraryError("no label data provided".to_string()))?;
    info!(
        elapsed_ms = request_started.elapsed().as_millis(),
        label_count, "typst label response finished"
    );

    Ok(HttpResponse::Ok()
        .content_type("image/png")
        .body(result_image))
}

/// 写入 data.json 触发 watch 重编译，等待本次编译完成后读取输出 PNG。
async fn render_label(label: &LabelInfo) -> Result<Vec<u8>, CustomError> {
    let watcher = watcher().await?;
    let _permit = watcher.render_lock.lock().await;

    let render_started = Instant::now();
    let json = serde_json::to_string(label)?;
    let mut compiled_rx = watcher.compiled_rx.clone();
    let last_seq = compiled_rx.borrow().0;

    tokio::fs::write(DATA_FILE, json).await?;

    // 等待序号前进，确保读到的是本次写入触发的编译结果
    let compiled = timeout(
        COMPILE_TIMEOUT,
        compiled_rx.wait_for(|(seq, _)| *seq > last_seq),
    )
    .await
    .map_err(|_| CustomError::OtherLibraryError("typst 编译超时".to_string()))?
    .map_err(|_| CustomError::OtherLibraryError("typst watch 通道已关闭".to_string()))?;
    compiled.1.clone().map_err(CustomError::OtherLibraryError)?;

    let png = tokio::fs::read(OUTPUT_FILE).await?;
    info!(
        elapsed_ms = render_started.elapsed().as_millis(),
        "typst label rendered"
    );
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_label() -> LabelInfo {
        LabelInfo {
            kind: 0,
            customer_name: "测试客户".to_string(),
            part_no: "P-001".to_string(),
            material_name: "测试物料".to_string(),
            qr_string: "M001|S001|O001|10|x|2026-08-08|BOX-1".to_string(),
            is_return: false,
        }
    }

    #[actix_web::test]
    async fn render_label_via_watcher() {
        // 正常渲染：完整走 watch 流程
        let png = render_label(&sample_label())
            .await
            .expect("typst render should succeed");
        const PNG_MAGIC: [u8; 4] = [0x89, b'P', b'N', b'G'];
        assert!(png.starts_with(&PNG_MAGIC), "expected PNG bytes");

        // 编译失败应传回错误：qr_string 分段不足触发模板越界
        let mut label = sample_label();
        label.qr_string = "too-short".to_string();
        let err = render_label(&label)
            .await
            .expect_err("invalid label should fail");
        assert!(matches!(err, CustomError::OtherLibraryError(_)));

        // 清理：结束 watch 进程，避免运行时退出时悬挂
        if let Some(watcher) = WATCHER.get() {
            watcher
                .child
                .lock()
                .await
                .kill()
                .await
                .expect("failed to kill typst watch");
        }
    }
}
