//! 打印执行器：只负责执行 qr_service 经 WS 下发的打印脚本。
//! 脚本内容（纸张、边距、任务监听规则）由服务器统一生成维护——
//! 调整打印行为升级 qr_service 即可，工位 agent 不用动。

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::{engine::general_purpose, Engine};
use tracing::info;

/// 执行打印脚本：PNG 以 Base64 经标准输入传给 PowerShell 进程，全程在内存中传递，不落盘。
/// 同步等待脚本出结果（脚本内含打印队列监听；通常 1~2 秒，打印机异常时最坏 60 秒超时），
/// 失败时把 stderr 里的原因返回给调用方
pub fn print_with_script(script: &str, png: &[u8]) -> anyhow::Result<()> {
    let encoded = general_purpose::STANDARD.encode(png);
    run_print_process(script, &encoded)?;
    info!(bytes = png.len(), "label printed");
    Ok(())
}

fn run_print_process(script: &str, encoded: &str) -> anyhow::Result<()> {
    let mut child = Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", script])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    info!(pid = child.id(), "打印进程已启动");

    // 写完即关闭标准输入，PowerShell 读到 EOF 后开始打印
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin.write_all(encoded.as_bytes())?;
    drop(stdin);
    info!(bytes = encoded.len(), "图像数据已写入打印进程，等待打印结束");

    // 硬超时兜底：脚本自身的队列监听有 60s 上限，但驱动弹窗（如 Microsoft Print to PDF
    // 的“另存为”对话框）会让 $pd.Print() 永远阻塞，无限等待会挂死后台任务
    let output = wait_with_timeout(&mut child, Duration::from_secs(120))?;
    info!(
        status = ?output.status.code(),
        stderr = %String::from_utf8_lossy(&output.stderr).trim(),
        "打印进程已退出"
    );
    anyhow::ensure!(
        output.status.success(),
        "{}",
        print_failure_message(output.status.code(), &output.stderr)
    );
    Ok(())
}

/// 等待子进程退出，最多 timeout；超时杀掉进程并返回带原因的错误
fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> anyhow::Result<std::process::Output> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            // 进程已退出：管道随之 EOF，直接读完残留输出
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            if let Some(mut out) = child.stdout.take() {
                let _ = out.read_to_end(&mut stdout);
            }
            if let Some(mut err) = child.stderr.take() {
                let _ = err.read_to_end(&mut stderr);
            }
            return Ok(std::process::Output {
                status,
                stdout,
                stderr,
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            tracing::warn!(secs = timeout.as_secs(), "打印进程超时，已强制终止");
            anyhow::bail!(
                "打印进程超过 {} 秒未结束，已终止。通常是打印机无响应，或驱动弹出对话框\
                （如 Microsoft Print to PDF 的“另存为”窗口在服务会话中不可见）",
                timeout.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// 脚本报错写在 stderr；为空时（如 powershell 自身异常）给兜底描述
fn print_failure_message(exit_code: Option<i32>, stderr: &[u8]) -> String {
    let detail = String::from_utf8_lossy(stderr).trim().to_string();
    if detail.is_empty() {
        format!("powershell 打印进程异常退出（退出码 {:?}）", exit_code)
    } else {
        detail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_message_prefers_stderr_detail() {
        assert_eq!(
            print_failure_message(Some(1), "打印任务失败: Offline".as_bytes()),
            "打印任务失败: Offline"
        );
        assert!(print_failure_message(Some(1), b"").contains("退出码"));
    }

    /// 需要本机有 powershell，仅在 Windows 上运行
    #[cfg(windows)]
    #[test]
    fn wait_with_timeout_kills_hung_process() {
        let mut child = Command::new("powershell")
            .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 60"])
            .spawn()
            .expect("failed to run powershell");
        let started = Instant::now();
        let err = wait_with_timeout(&mut child, Duration::from_secs(1))
            .expect_err("hang process should time out");
        assert!(err.to_string().contains("未结束"));
        assert!(started.elapsed() < Duration::from_secs(30));
    }
    /// 需要本机有 powershell，仅在 Windows 上运行
    #[cfg(windows)]
    #[test]
    fn powershell_roundtrips_png_bytes_from_stdin() {
        let png: &[u8] = b"\x89PNG\r\n\x1a\nfake-label-image";
        let encoded = general_purpose::STANDARD.encode(png);

        let mut child = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "$bytes = [Convert]::FromBase64String([Console]::In.ReadToEnd()); [Convert]::ToBase64String($bytes)",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("failed to run powershell");
        child
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(encoded.as_bytes())
            .expect("failed to write stdin");

        let output = child.wait_with_output().expect("failed to wait");
        assert!(output.status.success());
        let roundtripped = String::from_utf8(output.stdout).expect("stdout should be utf8");
        assert_eq!(roundtripped.trim(), encoded);
    }
}
