use crate::config::{ArmTarget, Config};
use crate::protocol::{DashboardConnection, RobotMode};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph, Wrap},
    Frame,
};
use std::{
    net::IpAddr,
    panic,
    sync::{Arc, Mutex},
    time::Duration,
};

// ── 显示状态 ────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ArmDisplay {
    name: String,
    ip: IpAddr,
    status: String,
    status_color: Color,
    connected: bool,
    robot_mode: i32,       // RobotMode() 返回值，-1 表示未查询
    error_info: String,    // GetErrorID() 响应
    last_query: String,    // 最后查询时间戳
}

struct AppState {
    left: ArmDisplay,
    right: ArmDisplay,
    log: Vec<String>,
    should_quit: bool,
    busy: bool,
}

impl AppState {
    fn new(cfg: &Config) -> Self {
        Self {
            left: ArmDisplay {
                name: cfg.arm_left.name.clone(),
                ip: cfg.arm_left.ip,
                status: "空闲".into(),
                status_color: Color::Gray,
                connected: false,
                robot_mode: -1,
                error_info: String::new(),
                last_query: String::new(),
            },
            right: ArmDisplay {
                name: cfg.arm_right.name.clone(),
                ip: cfg.arm_right.ip,
                status: "空闲".into(),
                status_color: Color::Gray,
                connected: false,
                robot_mode: -1,
                error_info: String::new(),
                last_query: String::new(),
            },
            log: vec!["[启动] Xtrainer Toolkit TUI 模式".into()],
            should_quit: false,
            busy: false,
        }
    }

    fn add_log(&mut self, msg: &str) {
        self.log.push(format!("[{}] {}", timestamp(), msg));
        if self.log.len() > 100 {
            self.log.remove(0);
        }
    }
}

fn timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "{:02}:{:02}:{:02}",
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60
    )
}

// ── TUI 入口 ────────────────────────────────────────────────

pub async fn run_tui(cfg: &Config) -> Result<()> {
    let state = Arc::new(Mutex::new(AppState::new(cfg)));
    let cfg = Arc::new(cfg.clone());

    // Panic hook 确保终端恢复
    let prev_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
        eprintln!("{}", info);
        prev_hook(info);
    }));

    // 启动终端
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    // ── 后台状态轮询: 每 2 秒查询双臂 RobotMode + GetErrorID ──
    let poll_state = state.clone();
    let poll_cfg = cfg.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if poll_state.lock().unwrap().should_quit {
                break;
            }
            if poll_state.lock().unwrap().busy { continue; }
            query_arm_state(&poll_state, &poll_cfg, ArmTarget::Left).await;
            query_arm_state(&poll_state, &poll_cfg, ArmTarget::Right).await;
        }
    });

    // ── 主循环 ──
    let result: Result<()> = async {
        let mut last_render = std::time::Instant::now();
        let frame_rate = Duration::from_millis(100);

        loop {
            if state.lock().unwrap().should_quit {
                break;
            }

            // 事件处理
            if event::poll(Duration::from_millis(10)).unwrap_or(false) {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        let code = key.code;
                        let modifiers = key.modifiers;

                        if (code == KeyCode::Char('c') || code == KeyCode::Char('q'))
                            && modifiers.contains(KeyModifiers::CONTROL)
                        {
                            state.lock().unwrap().add_log("Ctrl+C 退出");
                            break;
                        }
                        if code == KeyCode::Esc {
                            state.lock().unwrap().add_log("Esc 退出");
                            break;
                        }

                        let s = state.clone();
                        let c = cfg.clone();
                        tokio::spawn(async move {
                            dispatch_key(s, c, code, modifiers).await;
                        });
                    }
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }

            // 渲染
            let now = std::time::Instant::now();
            if now.duration_since(last_render) >= frame_rate {
                terminal.draw(|frame| render_ui(frame, &state)).ok();
                last_render = now;
            }

            tokio::task::yield_now().await;
        }
        Ok(())
    }
    .await;

    // 恢复终端
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

// ── 后台状态查询 ────────────────────────────────────────────

async fn query_arm_state(state: &Arc<Mutex<AppState>>, cfg: &Config, target: ArmTarget) {
    let arm_cfgs = cfg.resolve_arms(target);
    for arm_cfg in arm_cfgs {
        let ip = arm_cfg.ip;
        let port = arm_cfg.port;
        match DashboardConnection::connect(ip, port).await {
            Ok(mut conn) => {
                // 标记连接成功
                {
                    let mut s = state.lock().unwrap();
                    if ip == cfg.arm_left.ip {
                        s.left.connected = true;
                        s.left.last_query = timestamp();
                    } else {
                        s.right.connected = true;
                        s.right.last_query = timestamp();
                    }
                }

                // 查询 RobotMode
                if let Ok(mode) = conn.query_robot_mode().await {
                    let mut s = state.lock().unwrap();
                    let arm = if ip == cfg.arm_left.ip { &mut s.left } else { &mut s.right };
                    arm.robot_mode = mode;
                    let rm = RobotMode::from_code(mode);
                    arm.status = rm.label().to_string();
                    arm.status_color = mode_color(mode);
                }

                // 查询 ErrorID
                if let Ok(err) = conn.query_error_id().await {
                    let mut s = state.lock().unwrap();
                    let arm = if ip == cfg.arm_left.ip { &mut s.left } else { &mut s.right };
                    if err.is_empty() {
                        arm.error_info.clear();
                    } else {
                        // 有错误时更新状态显示
                        arm.error_info = err.clone();
                        // 不覆盖正常状态显示，但在面板中展示错误
                    }
                }
            }
            Err(_e) => {
                let mut s = state.lock().unwrap();
                if ip == cfg.arm_left.ip {
                    s.left.connected = false;
                    s.left.robot_mode = -1;
                    s.left.status = "离线".into();
                    s.left.status_color = Color::Red;
                } else {
                    s.right.connected = false;
                    s.right.robot_mode = -1;
                    s.right.status = "离线".into();
                    s.right.status_color = Color::Red;
                }
            }
        }
    }
}

fn mode_color(mode: i32) -> Color {
    match RobotMode::from_code(mode) {
        RobotMode::Init => Color::Yellow,
        RobotMode::BrakeOpen => Color::Yellow,
        RobotMode::PowerOff => Color::Red,
        RobotMode::Disabled => Color::Red,
        RobotMode::Enabled => Color::Green,
        RobotMode::Backdrive => Color::Magenta,
        RobotMode::Running => Color::Cyan,
        RobotMode::Recording => Color::Blue,
        RobotMode::Error => Color::Red,
        RobotMode::Pause => Color::Yellow,
        RobotMode::Jog => Color::Yellow,
        RobotMode::Unknown => Color::Gray,
    }
}

// ── 带 busy 锁的命令包装 ───────────────────────────────────

/// 执行单个命令，完成后自动释放 busy 锁
async fn locked_cmd(
    state: Arc<Mutex<AppState>>,
    cfg: Arc<Config>,
    target: ArmTarget,
    label: &str,
    cmd: &str,
    desc: &str,
) {
    cmd_dispatch_inner(state.clone(), cfg, target, label, cmd, desc).await;
    state.lock().unwrap().busy = false;
}

// ── 按键分发 ────────────────────────────────────────────────

async fn dispatch_key(
    state: Arc<Mutex<AppState>>,
    cfg: Arc<Config>,
    code: KeyCode,
    _modifiers: KeyModifiers,
) {
    // 退出键始终响应
    if code == KeyCode::Char('q') {
        state.lock().unwrap().add_log("q 退出");
        state.lock().unwrap().should_quit = true;
        return;
    }

    // busy 状态检查 — 命令执行中拒绝新操作
    {
        let mut s = state.lock().unwrap();
        if s.busy {
            s.add_log("⛔ 正在执行命令中，请稍候...");
            return;
        }
        // 标记 busy
        s.busy = true;
    }

    match code {
        KeyCode::Char('r') => {
            state.lock().unwrap().add_log("双臂关节复位并张开夹爪中...");
            let result = crate::reset::run(&cfg, ArmTarget::Both, false).await;
            let mut s = state.lock().unwrap();
            match result {
                Ok(()) => s.add_log("双臂关节复位完成"),
                Err(e) => s.add_log(&format!("复位失败: {e}")),
            }
            s.busy = false;
        }
        KeyCode::Char('h') => {
            let mut s = state.lock().unwrap();
            s.add_log("帮助: 快捷键见界面底部");
            s.busy = false;
        }
        // 左臂
        KeyCode::Char('1') => locked_cmd(state, cfg, ArmTarget::Left, "PowerOn", "PowerOn()", "上电").await,
        KeyCode::Char('2') => locked_cmd(state, cfg, ArmTarget::Left, "EnableRobot", "EnableRobot()", "解锁").await,
        KeyCode::Char('3') => locked_cmd(state, cfg, ArmTarget::Left, "ClearError", "ClearError()", "清除错误").await,
        KeyCode::Char('4') => locked_cmd(state, cfg, ArmTarget::Left, "StartDrag", "StartDrag()", "开启示教").await,
        KeyCode::Char('5') => locked_cmd(state, cfg, ArmTarget::Left, "StopDrag", "StopDrag()", "停止示教").await,
        KeyCode::Char('6') => locked_cmd(state, cfg, ArmTarget::Left, "DisableRobot", "DisableRobot()", "去使能").await,
        // 右臂
        KeyCode::Char('!') => locked_cmd(state, cfg, ArmTarget::Right, "PowerOn", "PowerOn()", "上电").await,
        KeyCode::Char('@') => locked_cmd(state, cfg, ArmTarget::Right, "EnableRobot", "EnableRobot()", "解锁").await,
        KeyCode::Char('#') => locked_cmd(state, cfg, ArmTarget::Right, "ClearError", "ClearError()", "清除错误").await,
        KeyCode::Char('$') => locked_cmd(state, cfg, ArmTarget::Right, "StartDrag", "StartDrag()", "开启示教").await,
        KeyCode::Char('%') => locked_cmd(state, cfg, ArmTarget::Right, "StopDrag", "StopDrag()", "停止示教").await,
        KeyCode::Char('^') => locked_cmd(state, cfg, ArmTarget::Right, "DisableRobot", "DisableRobot()", "去使能").await,
        // 全局 — quick_start/quick_stop 内部自己管理 busy
        KeyCode::Char('s') => quick_start(state, cfg).await,
        KeyCode::Char('x') => quick_stop(state, cfg).await,
        _ => {
            state.lock().unwrap().busy = false;
        }
    }
}

// ── 命令执行 (内部，不管 busy 锁) ──────────────────────────

async fn cmd_dispatch_inner(
    state: Arc<Mutex<AppState>>,
    cfg: Arc<Config>,
    target: ArmTarget,
    label: &str,
    cmd: &str,
    desc: &str,
) {
    state.lock().unwrap().add_log(&format!("→ {} {}...", target_name(target), desc));
    set_arm_status(&state, &cfg, target, desc, Color::Yellow);

    let arm_cfgs = cfg.resolve_arms(target);
    for arm_cfg in arm_cfgs {
        match DashboardConnection::connect(arm_cfg.ip, arm_cfg.port).await {
            Ok(mut conn) => {
                set_connected(&state, &cfg, arm_cfg.ip);
                if label == "PowerOn" {
                    // PowerOn 特殊处理：轮询直到上电完成
                    match conn.send_command(cmd).await {
                        Ok(resp) => {
                            let msg = format!("{} {} 已下发 ✓ {}", arm_cfg.label(), desc, resp);
                            state.lock().unwrap().add_log(&msg);
                            state.lock().unwrap().add_log(&format!("{} 等待上电完成 (最长60秒)...", arm_cfg.label()));
                            match conn.wait_power_on(60).await {
                                Ok(ticks) => {
                                    for (sec, mode) in &ticks {
                                        let rm = RobotMode::from_code(*mode);
                                        state.lock().unwrap().add_log(
                                            &format!("  {} {}秒 {}", arm_cfg.label(), sec, rm.label())
                                        );
                                    }
                                    state.lock().unwrap().add_log(&format!("{} ✅ 上电完成", arm_cfg.label()));
                                    set_arm_status(&state, &cfg, target, "上电完成", Color::Green);
                                }
                                Err(e) => {
                                    state.lock().unwrap().add_log(&format!("{} ❌ {}", arm_cfg.label(), e));
                                    set_arm_status(&state, &cfg, target, "上电超时", Color::Red);
                                }
                            }
                        }
                        Err(e) => {
                            let msg = format!("{} {} ✗ {}", arm_cfg.label(), desc, e);
                            state.lock().unwrap().add_log(&msg);
                            set_arm_status(&state, &cfg, target, "命令失败", Color::Red);
                        }
                    }
                } else {
                    match conn.send_command(cmd).await {
                        Ok(resp) => {
                            let msg = format!("{} {} ✓ {}", arm_cfg.label(), desc, resp);
                            state.lock().unwrap().add_log(&msg);
                            set_arm_status(&state, &cfg, target, &format!("{}完成", desc), make_color(label));
                        }
                        Err(e) => {
                            let msg = format!("{} {} ✗ {}", arm_cfg.label(), desc, e);
                            state.lock().unwrap().add_log(&msg);
                            set_arm_status(&state, &cfg, target, "命令失败", Color::Red);
                        }
                    }
                }
            }
            Err(e) => {
                let msg = format!("{} {} ✗ 连接失败: {}", arm_cfg.label(), desc, e);
                state.lock().unwrap().add_log(&msg);
                set_connected_false(&state, &cfg, arm_cfg.ip);
                set_arm_status(&state, &cfg, target, "离线", Color::Red);
            }
        }
    }
}

fn target_name(t: ArmTarget) -> &'static str {
    match t {
        ArmTarget::Left => "Arm1", ArmTarget::Right => "Arm2", ArmTarget::Both => "双臂",
    }
}

fn make_color(label: &str) -> Color {
    match label {
        "PowerOn" | "EnableRobot" | "ClearError" | "StopDrag" => Color::Green,
        "StartDrag" => Color::Magenta,
        "DisableRobot" => Color::Red,
        _ => Color::Gray,
    }
}

fn set_arm_status(state: &Arc<Mutex<AppState>>, cfg: &Config, target: ArmTarget, status: &str, color: Color) {
    let mut s = state.lock().unwrap();
    for arm_cfg in cfg.resolve_arms(target) {
        if arm_cfg.ip == cfg.arm_left.ip {
            s.left.status = status.to_string();
            s.left.status_color = color;
        } else {
            s.right.status = status.to_string();
            s.right.status_color = color;
        }
    }
}

fn set_connected(state: &Arc<Mutex<AppState>>, cfg: &Config, ip: IpAddr) {
    let mut s = state.lock().unwrap();
    if ip == cfg.arm_left.ip { s.left.connected = true; } else { s.right.connected = true; }
}

fn set_connected_false(state: &Arc<Mutex<AppState>>, cfg: &Config, ip: IpAddr) {
    let mut s = state.lock().unwrap();
    if ip == cfg.arm_left.ip { s.left.connected = false; } else { s.right.connected = false; }
}

// ── 一键启停 (自己管理 busy 锁，每步间隔 1 秒) ────────────

async fn quick_start(state: Arc<Mutex<AppState>>, cfg: Arc<Config>) {
    let steps: &[(&str, &str, &str)] = &[
        ("DisableRobot", "DisableRobot()", "锁定"),
        ("ClearError", "ClearError()", "清除错误"),
        ("PowerOn", "PowerOn()", "上电"),
        ("EnableRobot", "EnableRobot()", "解锁"),
    ];

    state.lock().unwrap().add_log("══════ 双臂一键启动 ══════");
    for target in [ArmTarget::Left, ArmTarget::Right] {
        for (label, cmd, desc) in steps {
            cmd_dispatch_inner(state.clone(), cfg.clone(), target, label, cmd, desc).await;
            state.lock().unwrap().add_log("  等待 1 秒...");
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    state.lock().unwrap().add_log("══════ 一键启动完成 ══════");
    state.lock().unwrap().busy = false;
}

async fn quick_stop(state: Arc<Mutex<AppState>>, cfg: Arc<Config>) {
    let steps: &[(&str, &str, &str)] = &[
        ("StopDrag", "StopDrag()", "停止示教"),
        ("DisableRobot", "DisableRobot()", "去使能"),
    ];

    state.lock().unwrap().add_log("══════ 双臂一键停止 ══════");
    for target in [ArmTarget::Left, ArmTarget::Right] {
        for (label, cmd, desc) in steps {
            cmd_dispatch_inner(state.clone(), cfg.clone(), target, label, cmd, desc).await;
            state.lock().unwrap().add_log("  等待 1 秒...");
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    state.lock().unwrap().add_log("══════ 一键停止完成 ══════");
    state.lock().unwrap().busy = false;
}

// ── 渲染 ────────────────────────────────────────────────────

fn render_ui(frame: &mut Frame, state: &Arc<Mutex<AppState>>) {
    let s = state.lock().unwrap();

    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3),  // 标题
            Constraint::Length(14), // 双臂状态
            Constraint::Length(3),  // 快捷键
            Constraint::Min(4),     // 日志
        ])
        .split(frame.area());

    // 标题
    let title = Paragraph::new(
        Text::from(vec![
            Line::from(Span::styled(
                " XTK - Xtrainer Toolkit",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                " Ctrl+C/Esc/q 退出 | 状态每2秒自动刷新",
                Style::default().fg(Color::DarkGray),
            )),
        ]),
    )
    .block(Block::default().borders(Borders::ALL).border_style(Color::Cyan));
    frame.render_widget(title, main_layout[0]);

    // 双臂面板
    let arms_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(main_layout[1]);
    render_arm_panel(frame, arms_layout[0], &s.left);
    render_arm_panel(frame, arms_layout[1], &s.right);

    // 快捷键
    let shortcuts_text = vec![
        Line::from(vec![
            span_key("1"), Span::raw("上电  "),
            span_key("2"), Span::raw("解锁  "),
            span_key("3"), Span::raw("清错  "),
            span_key("4"), Span::raw("示教  "),
            span_key("5"), Span::raw("停教  "),
            span_key("6"), Span::raw("锁定  "),
            Span::raw(" 右臂(按住Shift) "),
            span_key("!"), Span::raw("@#$%^"),
        ]),
        Line::from(vec![
            span_key("s"), Span::raw("一键启动  "),
            span_key("x"), Span::raw("一键停止  "),
            span_key("r"), Span::raw("双臂复位  "),
            Span::raw("  Ctrl+C 强制退出"),
        ]),
    ];
    let shortcuts = Paragraph::new(Text::from(shortcuts_text))
        .block(Block::default().borders(Borders::TOP).border_style(Color::DarkGray));
    frame.render_widget(shortcuts, main_layout[2]);

    // 日志
    let log_items: Vec<ListItem> = s
        .log.iter().rev().take(20)
        .map(|msg| ListItem::new(Line::from(Span::raw(msg.clone()))))
        .collect();
    let log_widget = List::new(log_items)
        .block(Block::default().borders(Borders::ALL).title(" 日志 ").border_style(Color::DarkGray))
        .style(Style::default().fg(Color::Gray));
    frame.render_widget(log_widget, main_layout[3]);
}

fn render_arm_panel(frame: &mut Frame, area: Rect, arm: &ArmDisplay) {
    let conn_str = if arm.connected { "✓ 已连接" } else { "✗ 未连接" };
    let conn_color = if arm.connected { Color::Green } else { Color::Red };

    let mode_label = if arm.connected && arm.robot_mode >= 0 {
        format!("{} (code: {})", RobotMode::from_code(arm.robot_mode).label(), arm.robot_mode)
    } else {
        "—".into()
    };
    let mode_color = if arm.connected && arm.robot_mode >= 0 {
        mode_color(arm.robot_mode)
    } else {
        Color::Gray
    };

    let error_line = if arm.error_info.is_empty() {
        Line::from(Span::styled("错误: 无", Style::default().fg(Color::DarkGray)))
    } else {
        Line::from(vec![
            Span::styled("错误: ", Style::default().fg(Color::Red)),
            Span::styled(&arm.error_info, Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        ])
    };

    let content = vec![
        Line::from(vec![
            Span::raw("IP: "),
            Span::styled(arm.ip.to_string(), Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![
            Span::raw("连接: "),
            Span::styled(conn_str, Style::default().fg(conn_color)),
        ]),
        Line::from(vec![
            Span::raw("模式: "),
            Span::styled(&mode_label, Style::default().fg(mode_color).add_modifier(Modifier::BOLD)),
        ]),
        error_line,
        Line::from(vec![
            Span::raw("状态: "),
            Span::styled(&arm.status, Style::default().fg(arm.status_color).add_modifier(Modifier::BOLD)),
        ]),
        if arm.connected {
            Line::from(Span::styled(
                format!("最后查询: {}", arm.last_query),
                Style::default().fg(Color::DarkGray),
            ))
        } else {
            Line::from(Span::raw(""))
        },
    ];

    let border_style = if arm.connected {
        match RobotMode::from_code(arm.robot_mode) {
            RobotMode::PowerOff | RobotMode::Disabled | RobotMode::Error => Color::Red,
            RobotMode::Enabled => Color::Green,
            RobotMode::Backdrive => Color::Magenta,
            RobotMode::Running => Color::Cyan,
            _ => Color::DarkGray,
        }
    } else {
        Color::DarkGray
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", arm.name))
        .title_alignment(Alignment::Center)
        .border_type(BorderType::Rounded)
        .border_style(border_style);

    let panel = Paragraph::new(Text::from(content))
        .block(block)
        .wrap(Wrap { trim: true });
    frame.render_widget(panel, area);
}

fn span_key(key: &str) -> Span<'_> {
    Span::styled(
        key,
        Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
    )
}
