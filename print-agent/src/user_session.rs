//! 把打印进程投递到“当前登录用户的会话”执行（仅 Windows）。
//!
//! 背景：print-agent 以 Windows 服务运行时为 SYSTEM/Session 0，而 WSD 等
//! 通过网络发现添加的打印机是“按用户安装”的——Session 0 里任务进队列后
//! 会被静默丢弃（脚本看到出队误报成功）。CreateProcessAsUser 让打印进程
//! 跑在登录用户的会话里，打印机视图与手工命令行执行完全一致。

use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::FromRawHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use base64::{engine::general_purpose, Engine};
use tracing::warn;
use windows_sys::Win32::Foundation::{CloseHandle, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, WAIT_OBJECT_0};
use windows_sys::Win32::Security::{DuplicateTokenEx, SecurityImpersonation, TokenPrimary, TOKEN_ALL_ACCESS, SECURITY_ATTRIBUTES};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken};
use windows_sys::Win32::System::Threading::{
    CreateProcessAsUserW, GetExitCodeProcess, TerminateProcess, WaitForSingleObject,
    CREATE_NO_WINDOW, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW,
};

const POWERSHELL: &str = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";

fn widen(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn close(handle: HANDLE) {
    if !handle.is_null() {
        unsafe { CloseHandle(handle) };
    }
}

/// 用户会话里的打印进程：stdin 可写、stderr 可读、进程句柄可等待
pub struct UserSessionProcess {
    process: HANDLE,
    thread: HANDLE,
    stdin: Option<File>,
    stderr: Option<File>,
}

impl UserSessionProcess {
    /// 写入图像数据并关闭 stdin（PowerShell 读到 EOF 后开始执行脚本）
    pub fn finish_stdin(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        let mut stdin = self.stdin.take().context("stdin 已关闭")?;
        stdin.write_all(bytes)?;
        drop(stdin);
        Ok(())
    }

    /// 等待进程退出，返回 (退出码, stderr)；超时杀进程并返回错误。
    /// stderr 由后台线程持续引流：若子进程写满管道缓冲区（默认 4KB），
    /// 没人读会把子进程堵死，造成“进程活着但永远不出来”的假挂起
    pub fn wait_with_timeout(&mut self, timeout: Duration) -> anyhow::Result<(Option<i32>, Vec<u8>)> {
        let mut stderr = self.stderr.take().context("stderr 已被取走")?;
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf);
            buf
        });
        let deadline = Instant::now() + timeout;
        loop {
            let result = unsafe { WaitForSingleObject(self.process, 200) };
            if result == WAIT_OBJECT_0 {
                let mut code = 0u32;
                unsafe { GetExitCodeProcess(self.process, &mut code) };
                let stderr = reader.join().unwrap_or_default();
                return Ok((Some(code as i32), stderr));
            }
            if Instant::now() >= deadline {
                unsafe {
                    TerminateProcess(self.process, 1);
                    WaitForSingleObject(self.process, 5000);
                }
                warn!(secs = timeout.as_secs(), "用户会话打印进程超时，已强制终止");
                let stderr = reader.join().unwrap_or_default();
                let detail = String::from_utf8_lossy(&stderr).trim().to_string();
                return Err(anyhow!(
                    "打印进程超过 {} 秒未结束，已终止。通常是打印机无响应，或驱动弹出对话框\
                    （如 Microsoft Print to PDF 的“另存为”窗口）。子进程输出: {}",
                    timeout.as_secs(),
                    detail
                ));
            }
        }
    }
}

impl Drop for UserSessionProcess {
    fn drop(&mut self) {
        close(self.process);
        close(self.thread);
    }
}

/// 在当前活动控制台用户的会话里启动 PowerShell 执行打印脚本。
/// 没有人登录（无活动会话）时返回 Ok(None)，调用方回退服务会话打印
pub fn spawn_in_active_user_session(script: &str) -> anyhow::Result<Option<UserSessionProcess>> {
    unsafe {
        let session_id = WTSGetActiveConsoleSessionId();
        if session_id == u32::MAX {
            return Ok(None);
        }
        let mut token = HANDLE::default();
        if WTSQueryUserToken(session_id, &mut token) == 0 {
            // 拿不到用户令牌视同没有可用会话（例如锁屏/无人登录边界）
            return Ok(None);
        }
        let mut user_token = HANDLE::default();
        let duplicated = DuplicateTokenEx(
            token,
            TOKEN_ALL_ACCESS,
            std::ptr::null(),
            SecurityImpersonation,
            TokenPrimary,
            &mut user_token,
        );
        close(token);
        if duplicated == 0 {
            close(user_token);
            return Err(anyhow!("DuplicateTokenEx 失败（错误码 {}）", std::io::Error::last_os_error()));
        }

        // 管道：stdin（我们写/子进程读）、stderr（子进程写/我们读）
        let attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        let (mut stdin_read, mut stdin_write) = (HANDLE::default(), HANDLE::default());
        let (mut stderr_read, mut stderr_write) = (HANDLE::default(), HANDLE::default());
        let result = (|| -> anyhow::Result<UserSessionProcess> {
            if CreatePipe(&mut stdin_read, &mut stdin_write, &attrs, 0) == 0 {
                return Err(anyhow!("CreatePipe(stdin) 失败"));
            }
            if CreatePipe(&mut stderr_read, &mut stderr_write, &attrs, 0) == 0 {
                return Err(anyhow!("CreatePipe(stderr) 失败"));
            }
            // 保留在我们手里的端点不能被子进程继承
            SetHandleInformation(stdin_write, HANDLE_FLAG_INHERIT, 0);
            SetHandleInformation(stderr_read, HANDLE_FLAG_INHERIT, 0);

            let desktop = widen("winsta0\\default");
            let startup = STARTUPINFOW {
                cb: std::mem::size_of::<STARTUPINFOW>() as u32,
                lpDesktop: desktop.as_ptr() as _,
                dwFlags: STARTF_USESTDHANDLES,
                hStdInput: stdin_read,
                hStdOutput: stderr_write,
                hStdError: stderr_write,
                ..Default::default()
            };
            let app = widen(POWERSHELL);
            // 用 -EncodedCommand 传脚本：PowerShell 5.1 解析 -Command 命令行时
            // 非 ASCII 字符（如脚本里的中文）会按 ANSI 代码页损坏并吃掉引号，
            // Base64(UTF-16LE) 彻底绕开命令行编码与引号转义问题
            let script_utf16le: Vec<u8> = script
                .encode_utf16()
                .flat_map(|unit| unit.to_le_bytes())
                .collect();
            let encoded_command = general_purpose::STANDARD.encode(script_utf16le);
            let mut command_line = widen(&format!(
                "powershell -NoProfile -WindowStyle Hidden -EncodedCommand {encoded_command}"
            ));
            let mut info = PROCESS_INFORMATION::default();
            if CreateProcessAsUserW(
                user_token,
                app.as_ptr(),
                command_line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                CREATE_NO_WINDOW,
                std::ptr::null(),
                std::ptr::null(),
                &startup,
                &mut info,
            ) == 0
            {
                return Err(anyhow!(
                    "CreateProcessAsUserW 失败（错误码 {}）",
                    std::io::Error::last_os_error()
                ));
            }
            // 子进程已继承到管道端点，关闭我们手里的副本
            close(stdin_read);
            close(stderr_write);
            Ok(UserSessionProcess {
                process: info.hProcess,
                thread: info.hThread,
                stdin: Some(File::from_raw_handle(stdin_write)),
                stderr: Some(File::from_raw_handle(stderr_read)),
            })
        })();
        close(user_token);
        if result.is_err() {
            close(stdin_read);
            close(stdin_write);
            close(stderr_read);
            close(stderr_write);
        }
        result.map(Some)
    }
}