use anyhow::{Context, Result};
use std::net::IpAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// 与 Dobot Dashboard 端口 (29999) 的 TCP 连接
///
/// 参考官方文档《TCP/IP远程控制接口文档 V3》:
/// - 发送: ASCII 命令字符串，不加右括号以外的多余内容
///   (C++ 源码 `tcpSend(szIp, 29999, szCmd, strlen(szCmd))`)
/// - 接收: 逐字节读取直到分号 `;`
/// - 响应格式: `ErrorID,{value1,...,valueN},指令名称(参数);`
pub struct DashboardConnection {
    reader: tokio::net::tcp::OwnedReadHalf,
    writer: tokio::net::tcp::OwnedWriteHalf,
}

impl DashboardConnection {
    pub async fn connect(ip: IpAddr, port: u16) -> Result<Self> {
        let addr = std::net::SocketAddr::new(ip, port);
        let stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
            .await.context("TCP 连接超时 (5s)")?
            .with_context(|| format!("无法连接到机械臂 Dashboard: {}", addr))?;
        let (rx, tx) = stream.into_split();
        Ok(Self { reader: rx, writer: tx })
    }

    /// 发送命令并等待响应
    ///
    /// 发送不含额外换行的原始命令。
    /// 响应逐字节读取直到 `;`，带 5 秒超时。
    pub async fn send_command(&mut self, cmd: &str) -> Result<String> {
        self.writer
            .write_all(cmd.as_bytes())
            .await
            .with_context(|| format!("发送命令失败: {}", cmd))?;
        self.writer.flush().await?;

        let resp = tokio::time::timeout(Duration::from_secs(5), self.read_until_semicolon())
            .await
            .with_context(|| format!("命令 '{}' 响应超时 (5s)", cmd))??;

        // 去除末尾分号及空白
        Ok(resp.trim_end_matches(';').trim().to_string())
    }

    async fn read_until_semicolon(&mut self) -> Result<String> {
        let mut buf = vec![0u8; 256];
        let mut offset = 0;
        loop {
            if offset >= buf.len() {
                buf.resize(buf.len() + 256, 0);
            }
            let n = self.reader.read(&mut buf[offset..offset + 1]).await?;
            if n == 0 { return Err(anyhow::anyhow!("连接关闭，未收到完整响应")); }
            offset += n;
            if buf[offset - 1] == b';' { break; }
        }
        Ok(String::from_utf8_lossy(&buf[..offset]).to_string())
    }

    // ── 状态查询 ──────────────────────────────────────────────

    /// 查询机器人模式
    ///
    /// 响应格式: `ErrorID,{Value},RobotMode();`
    /// 正确做法是提取第二个逗号分隔字段 `{Value}` 中的数值。
    pub async fn query_robot_mode(&mut self) -> Result<i32> {
        let resp = self.send_command("RobotMode()").await?;
        // resp 如 "0,{5},RobotMode()" → 取 {5} → 5
        let parts: Vec<&str> = resp.split(',').collect();
        if parts.len() >= 2 {
            let val = parts[1].trim().trim_start_matches('{').trim_end_matches('}');
            Ok(val.parse().unwrap_or(-1))
        } else {
            Ok(-1)
        }
    }

    /// 等待上电完成：每秒查询 RobotMode 直到不再返回"未上电"(code 3)
    ///
    /// 返回每次查询记录 `Vec<(秒数, 模式码)>`，超时 60 秒返回错误。
    pub async fn wait_power_on(&mut self, max_wait_secs: u64) -> Result<Vec<(u64, i32)>> {
        let deadline = std::time::Instant::now() + Duration::from_secs(max_wait_secs);
        let mut ticks = Vec::new();
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let mode = self.query_robot_mode().await.unwrap_or(-1);
            let elapsed = max_wait_secs.saturating_sub(
                deadline.saturating_duration_since(std::time::Instant::now()).as_secs(),
            );
            ticks.push((elapsed, mode));
            if mode != 3 {
                return Ok(ticks);
            }
            if std::time::Instant::now() >= deadline {
                return Err(anyhow::anyhow!("上电等待超时 ({}s)", max_wait_secs));
            }
        }
    }

    /// 查询错误 ID
    ///
    /// 响应格式: `ErrorID,{[[id,...],[id],...]},GetErrorID();`
    /// 无报警时返回 `0,{[[],[],[],[],[],[],[]]},GetErrorID()`
    /// 返回空字符串表示无错误，非空返回具体错误 ID 列表。
    pub async fn query_error_id(&mut self) -> Result<String> {
        let resp = self.send_command("GetErrorID()").await?;
        // 用花括号提取 body，不能用逗号 split（body 内有逗号分隔数组）
        let body = resp
            .splitn(2, '{')
            .nth(1)
            .unwrap_or("")
            .rsplitn(2, '}')
            .next()
            .unwrap_or("");
        // body 形如 [[],[12,34],[],...] — 提取所有非空错误 ID
        let errors: Vec<String> = body
            .split("],[")
            .flat_map(|seg| {
                seg.trim_matches(|c: char| c == '[' || c == ']')
                    .split(',')
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.trim().to_string())
                    .collect::<Vec<_>>()
            })
            .collect();
        if errors.is_empty() {
            Ok(String::new())
        } else {
            Ok(errors.join(", "))
        }
    }
}

// ── RobotMode 枚举 ─────────────────────────────────────────────

/// 机器人模式码 (RobotMode 返回值)
///
/// 参考《TCP/IP远程控制接口文档 V3》§2.3:
/// ```
/// 1 = ROBOT_MODE_INIT          初始化
/// 2 = ROBOT_MODE_BRAKE_OPEN    有任意关节抱闸松开
/// 3 = ROBOT_MODE_POWER_STATUS  本体未上电
/// 4 = ROBOT_MODE_DISABLED      未使能
/// 5 = ROBOT_MODE_ENABLE        使能且空闲
/// 6 = ROBOT_MODE_BACKDRIVE     拖拽模式
/// 7 = ROBOT_MODE_RUNNING       运行中
/// 8 = ROBOT_MODE_RECORDING     轨迹录制模式
/// 9 = ROBOT_MODE_ERROR         有未清除的报警 (优先级最高)
/// 10 = ROBOT_MODE_PAUSE        暂停状态
/// 11 = ROBOT_MODE_JOG          点动中
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RobotMode {
    Unknown = -1,
    Init = 1,
    BrakeOpen = 2,
    PowerOff = 3,
    Disabled = 4,
    Enabled = 5,
    Backdrive = 6,
    Running = 7,
    Recording = 8,
    Error = 9,
    Pause = 10,
    Jog = 11,
}

impl RobotMode {
    pub fn from_code(code: i32) -> Self {
        match code {
            1 => Self::Init,
            2 => Self::BrakeOpen,
            3 => Self::PowerOff,
            4 => Self::Disabled,
            5 => Self::Enabled,
            6 => Self::Backdrive,
            7 => Self::Running,
            8 => Self::Recording,
            9 => Self::Error,
            10 => Self::Pause,
            11 => Self::Jog,
            _ => Self::Unknown,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Unknown => "未知",
            Self::Init => "初始化",
            Self::BrakeOpen => "抱闸松开",
            Self::PowerOff => "未上电",
            Self::Disabled => "未使能",
            Self::Enabled => "使能就绪",
            Self::Backdrive => "拖拽中",
            Self::Running => "运行中",
            Self::Recording => "录制中",
            Self::Error => "⚠ 报警",
            Self::Pause => "暂停",
            Self::Jog => "点动中",
        }
    }
}
