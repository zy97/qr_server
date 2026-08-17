use tracing::info;

use crate::config::CONFIG;
use crate::err::CustomError;

/// 把标签 PNG POST 给 Windows 打印代理（print-agent），由它送到本机打印机。
/// 集中部署后服务器（可为 Linux）只负责渲染，打印动作落在接打印机的 Windows 机器上。
/// 同步等待代理返回真实打印结果（print-agent 会监听打印任务直到出队/失败/超时），
/// 失败时错误透传给 /label 调用方
pub async fn print_label_png(png: &[u8]) -> Result<(), CustomError> {
    let agent_url = &CONFIG.print.agent_url;
    if agent_url.is_empty() {
        return Err(CustomError::OtherLibraryError(
            "打印已启用但未配置 print.agent_url".to_string(),
        ));
    }
    post_to_agent(agent_url, png).await
}

async fn post_to_agent(agent_url: &str, png: &[u8]) -> Result<(), CustomError> {
    let url = format!("{}/print", agent_url.trim_end_matches('/'));
    let response = reqwest::Client::new()
        .post(&url)
        .body(png.to_vec())
        .send()
        .await
        .map_err(|err| CustomError::OtherLibraryError(format!("调用打印代理失败: {err}")))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(CustomError::OtherLibraryError(format!(
            "打印代理返回 {status}: {body}"
        )));
    }
    info!(url, "label sent to print agent");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// 一次性 mock 打印代理：读完整请求（请求头 + Content-Length 指示的 body），
    /// 返回给定状态码，返回收到的原始字节供断言
    fn mock_agent(status_line: &str) -> (String, std::thread::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock agent");
        let addr = listener.local_addr().expect("local addr");
        let status_line = status_line.to_string();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            let header_end = loop {
                let n = stream.read(&mut chunk).expect("read");
                buf.extend_from_slice(&chunk[..n]);
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
            let content_length: usize = headers
                .split("\r\n")
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse().ok())
                })
                .expect("content-length header");
            while buf.len() < header_end + content_length {
                let n = stream.read(&mut chunk).expect("read body");
                buf.extend_from_slice(&chunk[..n]);
            }
            let response = format!("{status_line}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            stream.write_all(response.as_bytes()).expect("write response");
            buf
        });
        (format!("http://{addr}"), handle)
    }

    #[actix_web::test]
    async fn posts_png_bytes_to_agent_print_endpoint() {
        let (url, server) = mock_agent("HTTP/1.1 200 OK");
        post_to_agent(&url, b"fake-png").await.expect("post should succeed");
        let received = server.join().expect("mock agent thread");
        let text = String::from_utf8_lossy(&received);
        assert!(text.starts_with("POST /print "), "请求行: {}", text.lines().next().unwrap_or(""));
        assert!(received.ends_with(b"fake-png"));
    }

    #[actix_web::test]
    async fn agent_error_status_becomes_error() {
        let (url, server) = mock_agent("HTTP/1.1 500 Internal Server Error");
        let err = post_to_agent(&url, b"fake-png").await.expect_err("500 should fail");
        server.join().expect("mock agent thread");
        assert!(err.to_string().contains("500"), "错误信息: {err}");
    }
}