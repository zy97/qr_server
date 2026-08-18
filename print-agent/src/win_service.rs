//! Windows 服务集成：由服务控制管理器（SCM）启动时走这里。
//! 停止/关机信号经 mpsc 通道转成 HTTP 服务的优雅停机

use std::ffi::OsString;
use std::sync::mpsc;
use std::time::Duration;

use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};

define_windows_service!(ffi_service_main, service_main);

/// 进入 SCM 服务分发；从控制台直接运行时会立即返回
/// ERROR_FAILED_SERVICE_CONTROLLER_CONNECT(1063) 错误，由调用方回退到控制台模式
pub fn run() -> windows_service::Result<()> {
    service_dispatcher::start(crate::SERVICE_NAME, ffi_service_main)
}

fn service_status(state: ServiceState, controls: ServiceControlAccept) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: controls,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::ZERO,
        process_id: None,
    }
}

fn service_main(_args: Vec<OsString>) {
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let status_handle = match service_control_handler::register(crate::SERVICE_NAME, move |event| {
        if matches!(event, ServiceControl::Stop | ServiceControl::Shutdown) {
            let _ = stop_tx.send(());
        }
        ServiceControlHandlerResult::NoError
    }) {
        Ok(handle) => handle,
        Err(_) => return,
    };

    let _ = status_handle.set_service_status(service_status(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
    ));

    crate::server::run(async move {
        // recv 是阻塞调用，放进阻塞线程池等待停止信号
        let _ = tokio::task::spawn_blocking(move || stop_rx.recv()).await;
    });

    let _ = status_handle.set_service_status(service_status(
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
    ));
}
