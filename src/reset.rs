//! Reset adapted from X-Trainer-XVLA smooth_reset / make_joint_command.
use crate::{
    config::{ArmTarget, Config},
    protocol::DashboardConnection,
};
use anyhow::{bail, ensure, Result};
use std::time::Duration;

pub const LEFT: [f64; 6] = [-1.57, 0.0, -1.57, 0.0, 1.57, 1.57];
pub const RIGHT: [f64; 6] = [1.57, 0.0, 1.57, 0.0, -1.57, -1.57];

fn payload(response: &str) -> Result<&str> {
    let (code, rest) = response
        .split_once(',')
        .ok_or_else(|| anyhow::anyhow!("无效响应: {response}"))?;
    ensure!(code.trim() == "0", "控制器拒绝指令: {response}");
    let begin = rest
        .find('{')
        .ok_or_else(|| anyhow::anyhow!("缺少响应内容"))?;
    let end = rest
        .rfind('}')
        .ok_or_else(|| anyhow::anyhow!("响应不完整"))?;
    ensure!(end > begin, "响应不完整");
    Ok(&rest[begin + 1..end])
}

fn joints(response: &str) -> Result<[f64; 6]> {
    let values = payload(response)?
        .split(',')
        .map(|v| v.trim().parse::<f64>())
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        values.len() == 6 && values.iter().all(|v| v.is_finite()),
        "关节角响应无效"
    );
    Ok(std::array::from_fn(|i| values[i].to_radians()))
}

fn command(values: [f64; 6]) -> String {
    let args = values
        .iter()
        .map(|v| format!("{:.6}", v.to_degrees()))
        .collect::<Vec<_>>()
        .join(",");
    format!("ServoJ({args},0.030000,gain=500)\n")
}

pub async fn run(cfg: &Config, target: ArmTarget, dry_run: bool) -> Result<()> {
    let arms = cfg.resolve_arms(target);
    let mut connections = Vec::new();
    let mut grippers = Vec::new();
    for arm in &arms {
        ensure!(
            arm.gripper.is_some(),
            "{} 缺少 gripper 配置（port、motor_id）",
            arm.label()
        );
    }
    // Preflight every selected arm before sending any motion.
    for arm in &arms {
        let goal = if std::ptr::eq(*arm, &cfg.arm_left) {
            LEFT
        } else {
            RIGHT
        };
        if dry_run {
            println!(
                "{} {}:{} reset(rad)={goal:?}; gripper={:?}, open=3052",
                arm.label(),
                arm.ip,
                arm.motion_port,
                arm.gripper
            );
            continue;
        }
        let mut dashboard = DashboardConnection::connect(arm.ip, arm.port).await?;
        let response = dashboard.send_command("RobotMode()").await?;
        ensure!(
            payload(&response)?.trim() == "5",
            "{} 必须先使能并处于空闲状态: {response}",
            arm.label()
        );
        let start = joints(&dashboard.send_command("GetAngle()").await?)?;
        let motion = DashboardConnection::connect(arm.ip, arm.motion_port).await?;
        let mut gripper = crate::gripper::Gripper::connect(arm.gripper.as_ref().unwrap())?;
        let position = gripper.position()?;
        ensure!(
            (2048..=3052).contains(&position),
            "夹爪当前位置不在参考标定范围内"
        );
        grippers.push((gripper, position));
        connections.push((dashboard, motion, start, goal));
    }
    if dry_run {
        return Ok(());
    }
    let joint_delta = connections
        .iter()
        .flat_map(|(_, _, start, goal)| (0..6).map(move |i| (goal[i] - start[i]).abs()))
        .fold(0.0_f64, f64::max);
    let delta = grippers
        .iter()
        .map(|(_, start)| (3052 - start) as f64 / 1004.0)
        .fold(joint_delta, f64::max);
    // Match reference ramp step and maximum interpolation count.
    let steps = ((delta / 0.01).ceil() as usize).clamp(1, 100);
    for step in 0..steps {
        let alpha = if steps == 1 {
            1.0
        } else {
            step as f64 / (steps - 1) as f64
        };
        for (_, motion, start, goal) in &mut connections {
            let values = std::array::from_fn(|i| start[i] + alpha * (goal[i] - start[i]));
            payload(&motion.send_command(&command(values)).await?)?;
        }
        for (gripper, start) in &mut grippers {
            gripper.write((*start as f64 + alpha * (3052 - *start) as f64).round() as u16)?;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    // An acknowledgement means accepted, not arrived: verify actual joint feedback.
    for (dashboard, _, _, goal) in &mut connections {
        let mut reached = false;
        for _ in 0..100 {
            let current = joints(&dashboard.send_command("GetAngle()").await?)?;
            if (0..6).all(|i| (current[i] - goal[i]).abs() <= 0.02) {
                reached = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if !reached {
            bail!("复位指令已发送，但未在等待时间内到达目标位置");
        }
    }
    for (gripper, _) in &mut grippers {
        let mut reached = false;
        for _ in 0..50 {
            if gripper.position()?.abs_diff(3052) <= 20 {
                reached = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        ensure!(reached, "夹爪未到达张开位置");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_feedback_and_rejects_errors() {
        let q = joints("0,{180,0,-90,0,90,0},GetAngle()").unwrap();
        assert!((q[0] - std::f64::consts::PI).abs() < 1e-9);
        for bad in [
            "-1,{0},GetAngle()",
            "0,{NaN,0,0,0,0,0},GetAngle()",
            "0,{1,2},GetAngle()",
            "broken",
        ] {
            assert!(joints(bad).is_err());
        }
    }
    #[test]
    fn formats_reference_motion_protocol() {
        assert_eq!(
            command([0.0; 6]),
            "ServoJ(0.000000,0.000000,0.000000,0.000000,0.000000,0.000000,0.030000,gain=500)\n"
        );
    }
    #[tokio::test]
    async fn reads_fragmented_controller_response() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 10];
            socket.read_exact(&mut request).await.unwrap();
            assert_eq!(&request, b"GetAngle()");
            socket.write_all(b"0,{180,0,").await.unwrap();
            tokio::task::yield_now().await;
            socket.write_all(b"-90,0,90,0},GetAngle();").await.unwrap();
        });
        let mut conn = DashboardConnection::connect(addr.ip(), addr.port())
            .await
            .unwrap();
        let q = joints(&conn.send_command("GetAngle()").await.unwrap()).unwrap();
        assert!((q[2] + std::f64::consts::FRAC_PI_2).abs() < 1e-9);
        server.await.unwrap();
    }
}
