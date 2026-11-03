mod config;
mod reset;
mod gripper;
mod protocol;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// Dobot Xtrainer 机械臂控制工具
///
/// 在不依赖ROS的情况下直接通过TCP/Dashboard协议控制双臂。
/// 支持命令与TUI两种交互模式。
#[derive(Parser)]
#[command(name = "xtk", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// 配置文件路径 (默认: ~/.config/xtk/config.json)
    #[arg(short, long, default_value = None)]
    config: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// 返回 XVLA 默认关节姿态并张开夹爪
    Reset {
        #[arg(short, long, default_value = "both")]
        arm: String,
        /// 仅显示目标，不连接机械臂
        #[arg(long)]
        dry_run: bool,
    },
    /// 机械臂上电
    PowerOn {
        /// 目标机械臂: left, right, both
        #[arg(short, long, default_value = "both")]
        arm: String,
    },
    /// 机械臂解锁/使能
    Enable {
        /// 目标机械臂: left, right, both
        #[arg(short, long, default_value = "both")]
        arm: String,
    },
    /// 机械臂去使能/锁定
    Disable {
        /// 目标机械臂: left, right, both
        #[arg(short, long, default_value = "both")]
        arm: String,
    },
    /// 清除错误状态
    ClearError {
        /// 目标机械臂: left, right, both
        #[arg(short, long, default_value = "both")]
        arm: String,
    },
    /// 开启示教模式 (Drag)
    StartDrag {
        /// 目标机械臂: left, right, both
        #[arg(short, long, default_value = "both")]
        arm: String,
    },
    /// 停止示教模式 (Stop Drag)
    StopDrag {
        /// 目标机械臂: left, right, both
        #[arg(short, long, default_value = "both")]
        arm: String,
    },
    /// 一键启动: 上电 + 解锁 + 清除错误 + 示教
    QuickStart {
        /// 目标机械臂: left, right, both
        #[arg(short, long, default_value = "both")]
        arm: String,
    },
    /// 一键停止: 停止示教 + 去使能
    QuickStop {
        /// 目标机械臂: left, right, both
        #[arg(short, long, default_value = "both")]
        arm: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let cfg = config::load_config(cli.config.as_deref())?;

    match cli.command {
        Some(cmd) => run_command(cmd, &cfg).await,
        None => tui::run_tui(&cfg).await,
    }
}

async fn run_command(cmd: Commands, cfg: &config::Config) -> Result<()> {
    use config::ArmTarget;
    use Commands::*;

    let arm = match &cmd {
        PowerOn { arm } | Enable { arm } | Disable { arm }
        | ClearError { arm } | StartDrag { arm } | StopDrag { arm }
        | QuickStart { arm } | QuickStop { arm } | Reset { arm, .. } => {
            ArmTarget::from_str(arm)?
        }
    };

    let arms = cfg.resolve_arms(arm);

    match cmd {
        Reset { dry_run, .. } => {
            reset::run(cfg, arm, dry_run).await?;
            if !dry_run { println!("复位完成"); }
        }
        PowerOn { .. } => {
            for a in &arms {
                let mut conn = protocol::DashboardConnection::connect(a.ip, a.port).await?;
                let resp = conn.send_command("PowerOn()").await?;
                println!("[{}] PowerOn 已下发: {}", a.label(), resp);
                println!("[{}] 等待上电完成 (最长 60 秒)...", a.label());
                match conn.wait_power_on(60).await {
                    Ok(ticks) => {
                        for (sec, mode) in &ticks {
                            println!("[{}]   {}秒 → RobotMode({})", a.label(), sec, mode);
                        }
                        println!("[{}] ✅ 上电完成", a.label());
                    }
                    Err(e) => {
                        println!("[{}] ❌ {}", a.label(), e);
                    }
                }
            }
        }
        Enable { .. } => {
            for a in &arms {
                let mut conn = protocol::DashboardConnection::connect(a.ip, a.port).await?;
                let resp = conn.send_command("EnableRobot()").await?;
                println!("[{}] EnableRobot: {}", a.label(), resp);
            }
        }
        Disable { .. } => {
            for a in &arms {
                let mut conn = protocol::DashboardConnection::connect(a.ip, a.port).await?;
                let resp = conn.send_command("DisableRobot()").await?;
                println!("[{}] DisableRobot: {}", a.label(), resp);
            }
        }
        ClearError { .. } => {
            for a in &arms {
                let mut conn = protocol::DashboardConnection::connect(a.ip, a.port).await?;
                let resp = conn.send_command("ClearError()").await?;
                println!("[{}] ClearError: {}", a.label(), resp);
            }
        }
        StartDrag { .. } => {
            for a in &arms {
                let mut conn = protocol::DashboardConnection::connect(a.ip, a.port).await?;
                let resp = conn.send_command("StartDrag()").await?;
                println!("[{}] StartDrag: {}", a.label(), resp);
            }
        }
        StopDrag { .. } => {
            for a in &arms {
                let mut conn = protocol::DashboardConnection::connect(a.ip, a.port).await?;
                let resp = conn.send_command("StopDrag()").await?;
                println!("[{}] StopDrag: {}", a.label(), resp);
            }
        }
        QuickStart { .. } => {
            for a in &arms {
                let mut conn = protocol::DashboardConnection::connect(a.ip, a.port).await?;
                let r1 = conn.send_command("DisableRobot()").await?;
                println!("[{}] DisableRobot (锁定): {}", a.label(), r1);
                let r2 = conn.send_command("ClearError()").await?;
                println!("[{}] ClearError: {}", a.label(), r2);
                let r3 = conn.send_command("PowerOn()").await?;
                println!("[{}] PowerOn: {}", a.label(), r3);
                let r4 = conn.send_command("EnableRobot()").await?;
                println!("[{}] EnableRobot (解锁): {}", a.label(), r4);
            }
        }
        QuickStop { .. } => {
            for a in &arms {
                let mut conn = protocol::DashboardConnection::connect(a.ip, a.port).await?;
                let r1 = conn.send_command("StopDrag()").await?;
                println!("[{}] StopDrag: {}", a.label(), r1);
                let r2 = conn.send_command("DisableRobot()").await?;
                println!("[{}] DisableRobot: {}", a.label(), r2);
            }
        }
    }

    Ok(())
}
