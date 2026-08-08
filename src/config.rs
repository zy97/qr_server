use serde::Deserialize;
use std::sync::LazyLock;
use tracing::warn;

/// 服务配置，从工作目录下的 config.toml 读取；
/// 文件缺失或解析失败时使用默认值（打印关闭），不影响服务启动
#[derive(Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub print: PrintConfig,
}

/// 以下默认值来自原 C# 打印服务的生产配置
fn default_printer_name() -> String {
    "ZDesigner ZT231-300dpi ZPL".to_string()
}

fn default_paper_width() -> f64 {
    10.57
}

fn default_paper_height() -> f64 {
    29.70
}

#[derive(Deserialize)]
pub struct PrintConfig {
    /// 生成标签图片后是否同时发送到打印机
    #[serde(default)]
    pub enabled: bool,
    /// 目标打印机名（Windows 打印机列表中的名称）
    #[serde(default = "default_printer_name")]
    pub printer_name: String,
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
            enabled: false,
            printer_name: default_printer_name(),
            paper_width: default_paper_width(),
            paper_height: default_paper_height(),
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
    fn parse_print_config() {
        let config: Config = toml::from_str(
            "[print]\nenabled = true\nprinter_name = \"Test Printer\"\npaper_width = 15.0\npaper_height = 10.0",
        )
        .unwrap();
        assert!(config.print.enabled);
        assert_eq!(config.print.printer_name, "Test Printer");
        assert_eq!(config.print.paper_width, 15.0);
        assert_eq!(config.print.paper_height, 10.0);
    }

    #[test]
    fn missing_print_section_uses_csharp_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert!(!config.print.enabled);
        assert_eq!(config.print.printer_name, "ZDesigner ZT231-300dpi ZPL");
        assert_eq!(config.print.paper_width, 10.57);
        assert_eq!(config.print.paper_height, 29.70);
    }
}
