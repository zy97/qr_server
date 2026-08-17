use serde::Deserialize;
use std::path::PathBuf;
use std::sync::LazyLock;
use tracing::warn;

/// 打印代理配置。查找顺序：工作目录 print-agent.toml → exe 同目录 print-agent.toml → 默认值。
/// 以服务方式运行时工作目录是 System32，所以 exe 同目录兜底是必要的
#[derive(Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub print: PrintConfig,
}

fn default_port() -> u16 {
    9195
}

fn default_station() -> String {
    // 工位标识默认取机器名（Windows: COMPUTERNAME；Linux: HOSTNAME）
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

#[derive(Deserialize)]
pub struct ServerConfig {
    /// 本机 HTTP 监听端口（工位浏览器调 http://127.0.0.1:9195/label）
    #[serde(default = "default_port")]
    pub port: u16,
    /// qr_service 的 WebSocket 接入地址，如 ws://192.168.1.10:9095/ws/agent
    #[serde(default)]
    pub url: String,
    /// 工位标识（显示在 qr_service /api/agents），缺省取机器名
    #[serde(default = "default_station")]
    pub station: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            url: String::new(),
            station: default_station(),
        }
    }
}

/// 以下默认值来自原 C# 打印服务的生产配置
fn default_printer_name() -> String {
    "ZDesigner ZT231-300dpi ZPL".to_string()
}


#[derive(Deserialize)]
pub struct PrintConfig {
    /// 目标打印机名（Windows 打印机列表中的名称）；请求可用 ?printer= 覆盖。
    /// 纸张/边距等由 qr_service 下发的打印脚本决定，不在本机配置
    #[serde(default = "default_printer_name")]
    pub printer_name: String,
}

impl Default for PrintConfig {
    fn default() -> Self {
        Self {
            printer_name: default_printer_name(),
        }
    }
}
/// exe 所在目录（服务模式下工作目录不可靠，配置/日志都以它为基准）
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn config_candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from("print-agent.toml"),
        exe_dir().join("print-agent.toml"),
    ]
}

pub static CONFIG: LazyLock<Config> = LazyLock::new(|| {
    for path in config_candidates() {
        match std::fs::read_to_string(&path) {
            Ok(content) => match toml::from_str(&content) {
                Ok(config) => {
                    tracing::info!(path = %path.display(), "已加载配置");
                    return config;
                }
                Err(err) => {
                    warn!(path = %path.display(), error = %err, "配置解析失败，尝试下一个位置");
                }
            },
            Err(_) => continue,
        }
    }
    warn!("未找到 print-agent.toml，使用默认配置");
    Config::default()
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_csharp_production() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.server.port, 9195);
        assert!(config.server.url.is_empty());
        assert_eq!(config.print.printer_name, "ZDesigner ZT231-300dpi ZPL");

    }

    #[test]
    fn parses_full_config() {
        let config: Config = toml::from_str(
            "[server]\nport = 8080\n[print]\nprinter_name = \"Test Printer\"\n",
        )
        .unwrap();
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.print.printer_name, "Test Printer");

    }
}