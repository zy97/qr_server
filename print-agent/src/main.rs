mod config;
mod print;
mod server;
#[cfg(windows)]
mod user_session;
#[cfg(windows)]
mod win_service;
mod ws_client;

use std::process::Command;

use tracing_subscriber::{fmt::Layer, layer::SubscriberExt, util::SubscriberInitExt};

const SERVICE_NAME: &str = "print-agent";

/// 日志同时输出到控制台和 exe 同目录的 logs/（服务模式没有控制台，主要靠文件）；
/// 返回的 guard 必须活到进程结束，否则文件日志可能丢失
fn init_logging() -> tracing_appender::non_blocking::WorkerGuard {
    let log_dir = config::exe_dir().join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    // 文件名形如 print-agent.2026-08-19.log（日期在中间，后缀统一 .log）
    let file_appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("print-agent")
        .filename_suffix("log")
        .build(&log_dir)
        .expect("创建日志文件失败");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    // 默认计时是 UTC，运维看日志要换算，改成本地时间
    let timer = tracing_subscriber::fmt::time::LocalTime::rfc_3339();
    tracing_subscriber::registry()
        .with(Layer::new().with_writer(std::io::stdout).with_timer(timer.clone()))
        .with(
            Layer::new()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_timer(timer),
        )
        .with(tracing::level_filters::LevelFilter::INFO)
        .init();
    guard
}

fn main() {
    let _guard = init_logging();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "print-agent 启动");
    match std::env::args().nth(1).as_deref() {
        Some("install") => service_install(),
        Some("uninstall") => service_uninstall(),
        Some("run") => run_console(),
        _ => {
            #[cfg(windows)]
            {
                // SCM 启动时进入服务分发；控制台直接启动会返回 1063 错误，回退控制台模式
                match win_service::run() {
                    Ok(()) => return,
                    Err(err) => {
                        tracing::info!(error = %err, "非服务方式启动，进入控制台模式（安装服务: print-agent install）");
                    }
                }
            }
            run_console();
        }
    }
}

fn run_console() {
    server::run(async {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("收到 Ctrl+C，正在停止");
    });
}

/// 注册为开机自启的 Windows 服务（需要管理员权限）。
/// 服务崩溃后自动重启：5s / 10s / 30s 递延，86400s 重置计数
#[cfg(windows)]
fn service_install() {
    let exe = std::env::current_exe().expect("获取 exe 路径失败");
    let exe = exe.display().to_string();
    // sc.exe 参数要求等号后带空格（拆成两个 argv 即可）
    let created = Command::new("sc.exe")
        .args([
            "create",
            SERVICE_NAME,
            "binPath=",
            &format!("\"{exe}\""),
            "start=",
            "auto",
            "DisplayName=",
            "标签打印代理",
        ])
        .status();
    match created {
        Ok(status) if status.success() => {
            let _ = Command::new("sc.exe")
                .args([
                    "description",
                    SERVICE_NAME,
                    "接收标签 PNG 并发送到本机打印机（qr_server 打印代理）",
                ])
                .status();
            let _ = Command::new("sc.exe")
                .args([
                    "failure",
                    SERVICE_NAME,
                    "reset=",
                    "86400",
                    "actions=",
                    "restart/5000/restart/10000/restart/30000",
                ])
                .status();
            println!("服务已安装（开机自启，崩溃自动重启）。启动命令: sc start {SERVICE_NAME}");
        }
        Ok(status) => eprintln!("安装失败（退出码 {status}），请用管理员权限运行"),
        Err(err) => eprintln!("安装失败: {err}"),
    }
}

#[cfg(windows)]
fn service_uninstall() {
    let _ = Command::new("sc.exe").args(["stop", SERVICE_NAME]).status();
    match Command::new("sc.exe")
        .args(["delete", SERVICE_NAME])
        .status()
    {
        Ok(status) if status.success() => println!("服务已卸载"),
        Ok(status) => eprintln!("卸载失败（退出码 {status}），请用管理员权限运行"),
        Err(err) => eprintln!("卸载失败: {err}"),
    }
}

#[cfg(not(windows))]
fn service_install() {
    eprintln!("install/uninstall 仅支持 Windows；其他平台请直接运行 print-agent run");
}

#[cfg(not(windows))]
fn service_uninstall() {
    service_install();
}
