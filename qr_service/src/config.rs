use serde::Deserialize;
use std::sync::LazyLock;
use tracing::warn;

/// 服务配置，从工作目录下的 config.toml 读取；
/// 文件缺失或解析失败时使用默认值（打印关闭），不影响服务启动
#[derive(Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub print: PrintConfig,
    #[serde(default)]
    pub template: TemplateConfig,
}

fn default_true() -> bool {
    true
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

/// 集中部署后服务器（可为 Linux）只负责渲染，打印动作由 Windows 机器上的
/// print-agent 完成；打印机名/纸张等设置都在 print-agent 侧
#[derive(Deserialize, Default)]
pub struct PrintConfig {
    /// 生成标签图片后是否同时发送到打印代理
    #[serde(default)]
    pub enabled: bool,
    /// 打印代理地址，如 http://192.168.1.100:9195
    #[serde(default)]
    pub agent_url: String,
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
    fn template_save_html_defaults_true_and_parses() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.template.save_html);
        let config: Config = toml::from_str("[template]\nsave_html = false").unwrap();
        assert!(!config.template.save_html);
    }

    #[test]
    fn parse_print_config() {
        let config: Config =
            toml::from_str("[print]\nenabled = true\nagent_url = \"http://192.168.1.100:9195\"")
                .unwrap();
        assert!(config.print.enabled);
        assert_eq!(config.print.agent_url, "http://192.168.1.100:9195");
    }

    #[test]
    fn missing_print_section_defaults_disabled() {
        let config: Config = toml::from_str("").unwrap();
        assert!(!config.print.enabled);
        assert!(config.print.agent_url.is_empty());
    }
}