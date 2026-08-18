use serde::Deserialize;
use std::sync::LazyLock;
use tracing::warn;

/// 服务配置，从工作目录下的 config.toml 读取；
/// 文件缺失或解析失败时使用默认值（打印关闭），不影响服务启动
#[derive(Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub print: PrintConfig,
    #[serde(default)]
    pub template: TemplateConfig,
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    9095
}

/// HTTP 监听地址。服务器部署需要被其他机器访问，默认 0.0.0.0；
/// 仅本机使用可改为 127.0.0.1
#[derive(Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// 以下默认值来自原 C# 打印服务的生产配置
fn default_paper_width() -> f64 {
    10.57
}

fn default_paper_height() -> f64 {
    29.70
}

/// 打印纸张配置：打印脚本由 qr_service 生成（见 print_script），经 WS 下发给
/// print-agent 执行，所以纸张边距等业务参数集中在服务器侧维护
#[derive(Deserialize)]
pub struct PrintConfig {
    /// 自定义纸张宽度（cm）
    #[serde(default = "default_paper_width")]
    pub paper_width: f64,
    /// 自定义纸张高度（cm）
    #[serde(default = "default_paper_height")]
    pub paper_height: f64,
}

impl Default for PrintConfig {
    fn default() -> Self {
        Self {
            paper_width: default_paper_width(),
            paper_height: default_paper_height(),
        }
    }
}

#[derive(Deserialize)]
pub struct TemplateConfig {
    /// 保存模板时是否把渲染 HTML 落地到 templates/template.html（仅供本地查看，渲染以数据库为准）
    #[serde(default = "default_true")]
    pub save_html: bool,
}

impl Default for TemplateConfig {
    fn default() -> Self {
        Self {
            save_html: default_true(),
        }
    }
}

pub static CONFIG: LazyLock<Config> =
    LazyLock::new(|| match std::fs::read_to_string("config.toml") {
        Ok(content) => match toml::from_str(&content) {
            Ok(config) => config,
            Err(err) => {
                warn!(error = %err, "config.toml 解析失败，使用默认配置");
                Config::default()
            }
        },
        Err(_) => Config::default(),
    });

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn server_defaults_lan_reachable_and_parses() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.server.port, 9095);
        let config: Config = toml::from_str("[server]\nhost = \"127.0.0.1\"\nport = 8080").unwrap();
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 8080);
    }

    #[test]
    fn print_paper_defaults_match_csharp_production() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.print.paper_width, 10.57);
        assert_eq!(config.print.paper_height, 29.70);
        let config: Config =
            toml::from_str("[print]\npaper_width = 15.0\npaper_height = 10.0").unwrap();
        assert_eq!(config.print.paper_width, 15.0);
        assert_eq!(config.print.paper_height, 10.0);
    }

    #[test]
    fn template_save_html_defaults_true_and_parses() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.template.save_html);
        let config: Config = toml::from_str("[template]\nsave_html = false").unwrap();
        assert!(!config.template.save_html);
    }
}
