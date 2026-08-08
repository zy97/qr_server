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

/// 缺省打印机名（来自打印原型脚本）
fn default_printer_name() -> String {
    "NPIFD3D7B (HP LaserJet MFP M233sdw)".to_string()
}

#[derive(Deserialize)]
pub struct PrintConfig {
    /// 生成标签图片后是否同时发送到打印机
    #[serde(default)]
    pub enabled: bool,
    /// 目标打印机名（Windows 打印机列表中的名称）
    #[serde(default = "default_printer_name")]
    pub printer_name: String,
}

impl Default for PrintConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            printer_name: default_printer_name(),
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
        let config: Config =
            toml::from_str("[print]\nenabled = true\nprinter_name = \"Test Printer\"").unwrap();
        assert!(config.print.enabled);
        assert_eq!(config.print.printer_name, "Test Printer");
    }

    #[test]
    fn missing_print_section_defaults_to_disabled() {
        let config: Config = toml::from_str("").unwrap();
        assert!(!config.print.enabled);
        assert_eq!(
            config.print.printer_name,
            "NPIFD3D7B (HP LaserJet MFP M233sdw)"
        );
    }
}
