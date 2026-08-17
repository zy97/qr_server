//! 打印执行器：只负责执行 qr_service 经 WS 下发的打印脚本。
//! 脚本内容（纸张、边距、任务监听规则）由服务器统一生成维护——
//! 调整打印行为升级 qr_service 即可，工位 agent 不用动。

use std::io::Write;
use std::process::{Command, Stdio};

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

    // 写完即关闭标准输入，PowerShell 读到 EOF 后开始打印
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin.write_all(encoded.as_bytes())?;
    drop(stdin);

    let output = child.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        print_failure_message(output.status.code(), &output.stderr)
    );
    Ok(())
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