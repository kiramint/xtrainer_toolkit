use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;

/// 默认仪表板端口
pub const DEFAULT_DASHBOARD_PORT: u16 = 29999;

/// 单个机械臂配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArmConfig {
    /// 显示名称
    pub name: String,
    /// IP 地址
    pub ip: IpAddr,
    /// 仪表板端口 (默认 29999)
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_motion_port")]
    pub motion_port: u16,
    #[serde(default)]
    pub gripper: Option<GripperConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GripperConfig {
    pub port: String,
    pub motor_id: u8,
}

fn default_motion_port() -> u16 { 30003 }

fn default_port() -> u16 {
    DEFAULT_DASHBOARD_PORT
}

/// 应用配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 左臂配置
    pub arm_left: ArmConfig,
    /// 右臂配置
    pub arm_right: ArmConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            arm_left: ArmConfig {
                name: "Arm1 (Left)".into(),
                ip: "192.168.5.1".parse().unwrap(),
                port: DEFAULT_DASHBOARD_PORT,
                motion_port: 30003,
                gripper: None,
            },
            arm_right: ArmConfig {
                name: "Arm2 (Right)".into(),
                ip: "192.168.5.2".parse().unwrap(),
                port: DEFAULT_DASHBOARD_PORT,
                motion_port: 30003,
                gripper: None,
            },
        }
    }
}

/// 目标机械臂选择
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmTarget {
    Left,
    Right,
    Both,
}

impl ArmTarget {
    pub fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "left" | "l" => Ok(Self::Left),
            "right" | "r" => Ok(Self::Right),
            "both" | "b" | "all" => Ok(Self::Both),
            other => Err(anyhow::anyhow!(
                "无效的机械臂目标 '{}'，可选: left, right, both",
                other
            )),
        }
    }
}

impl Config {
    /// 根据目标返回要控制的机械臂列表
    pub fn resolve_arms(&self, target: ArmTarget) -> Vec<&ArmConfig> {
        match target {
            ArmTarget::Left => vec![&self.arm_left],
            ArmTarget::Right => vec![&self.arm_right],
            ArmTarget::Both => vec![&self.arm_left, &self.arm_right],
        }
    }
}

impl ArmConfig {
    pub fn label(&self) -> &str {
        &self.name
    }
}

/// 加载配置：优先从指定路径加载，其次从默认路径，最后使用内置默认值
pub fn load_config(path: Option<&str>) -> Result<Config> {
    if let Some(p) = path {
        let content = std::fs::read_to_string(p)
            .with_context(|| format!("无法读取配置文件: {}", p))?;
        serde_json::from_str(&content)
            .with_context(|| format!("配置文件 JSON 格式错误: {}", p))
    } else {
        match default_config_path() {
            Some(p) if p.exists() => {
                let content = std::fs::read_to_string(&p)
                    .with_context(|| format!("无法读取配置文件: {}", p.display()))?;
                serde_json::from_str(&content)
                    .with_context(|| format!("配置文件 JSON 格式错误: {}", p.display()))
            }
            _ => {
                // 生成默认配置并写入默认路径
                let cfg = Config::default();
                if let Some(p) = default_config_path() {
                    if let Some(parent) = p.parent() {
                        std::fs::create_dir_all(parent).ok();
                    }
                    let json = serde_json::to_string_pretty(&cfg).unwrap();
                    std::fs::write(&p, &json).ok();
                    eprintln!("已生成默认配置文件: {}", p.display());
                }
                Ok(cfg)
            }
        }
    }
}

fn default_config_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| d.join("xtk").join("config.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_config_remains_compatible() {
        let cfg: Config = serde_json::from_str(r#"{"arm_left":{"name":"L","ip":"192.168.5.1"},"arm_right":{"name":"R","ip":"192.168.5.2"}}"#).unwrap();
        assert_eq!(cfg.arm_left.motion_port, 30003);
        assert_eq!(cfg.arm_right.port, 29999);
        assert!(cfg.arm_left.gripper.is_none());
    }
    #[test]
    fn example_has_gripper_settings() {
        let cfg: Config = serde_json::from_str(include_str!("../config.example.json")).unwrap();
        assert_eq!(cfg.arm_left.gripper.unwrap().motor_id, 21);
        assert_eq!(cfg.arm_right.gripper.unwrap().motor_id, 22);
    }
}
