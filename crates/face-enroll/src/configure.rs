use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use face_auth_core::config::Config;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, HighlightSpacing, Paragraph, Row, Table, TableState},
    Frame, Terminal,
};
use std::io::{self, Stdout};

const CONFIG_PATH: &str = "/etc/face-auth/config.toml";

// ---- Value types ----

#[derive(Clone, Debug)]
enum Val {
    Str(String),
    Bool(bool),
    F32(f32),
    U64(u64),
    U32(u32),
    I32(i32),
}

impl Val {
    fn display(&self) -> String {
        match self {
            Val::Str(s) => s.clone(),
            Val::Bool(b) => b.to_string(),
            Val::F32(f) => format!("{:.3}", f),
            Val::U64(n) => n.to_string(),
            Val::U32(n) => n.to_string(),
            Val::I32(n) => n.to_string(),
        }
    }

    fn is_bool(&self) -> bool {
        matches!(self, Val::Bool(_))
    }
}

// ---- Field definition ----

struct Field {
    section: &'static str,
    key: &'static str,
    desc: &'static str,
    val: Val,
    def: Val,
}

impl Field {
    fn modified(&self) -> bool {
        self.val.display() != self.def.display()
    }
}

// ---- App state ----

struct App {
    base_config: Config,
    fields: Vec<Field>,
    state: TableState,
    /// True when in inline edit mode
    edit: bool,
    /// Current edit buffer
    buf: String,
    /// Validation error for current edit
    err: Option<String>,
    /// Bottom-bar status message: (text, is_error)
    status: Option<(String, bool)>,
    dirty: bool,
}

impl App {
    fn new(config: Config) -> Self {
        let fields = to_fields(&config);
        let mut state = TableState::default();
        state.select(Some(0));
        Self {
            base_config: config,
            fields,
            state,
            edit: false,
            buf: String::new(),
            err: None,
            status: None,
            dirty: false,
        }
    }

    fn selected(&self) -> usize {
        self.state.selected().unwrap_or(0)
    }

    fn up(&mut self) {
        let i = self.selected();
        if i > 0 {
            self.state.select(Some(i - 1));
            self.status = None;
        }
    }

    fn down(&mut self) {
        let i = self.selected();
        if i + 1 < self.fields.len() {
            self.state.select(Some(i + 1));
            self.status = None;
        }
    }

    /// Enter edit mode, or toggle bool directly.
    fn activate(&mut self) {
        let i = self.selected();
        self.status = None;
        if self.fields[i].val.is_bool() {
            if let Val::Bool(b) = self.fields[i].val {
                self.fields[i].val = Val::Bool(!b);
                self.dirty = true;
            }
        } else {
            self.buf = self.fields[i].val.display();
            self.edit = true;
            self.err = None;
        }
    }

    fn cancel(&mut self) {
        self.edit = false;
        self.buf.clear();
        self.err = None;
    }

    fn confirm(&mut self) {
        let i = self.selected();
        let s = self.buf.trim().to_string();

        match parse_val(&self.fields[i].val, &s) {
            Err(e) => {
                self.err = Some(e);
            }
            Ok(v) => {
                if let Some(e) = validate(self.fields[i].key, &v) {
                    self.err = Some(e);
                } else {
                    self.fields[i].val = v;
                    self.dirty = true;
                    self.edit = false;
                    self.buf.clear();
                    self.err = None;
                }
            }
        }
    }

    fn reset_field(&mut self) {
        let i = self.selected();
        let def = self.fields[i].def.clone();
        self.fields[i].val = def;
        self.dirty = true;
        self.status = Some((format!("{} reset to default", self.fields[i].key), false));
    }

    fn save(&mut self) -> Result<(), String> {
        let config = to_config(&self.base_config, &self.fields);
        let toml_str =
            toml::to_string_pretty(&config).map_err(|e| format!("serialize error: {e}"))?;
        std::fs::create_dir_all("/etc/face-auth")
            .map_err(|e| format!("cannot create /etc/face-auth: {e}"))?;
        std::fs::write(CONFIG_PATH, &toml_str)
            .map_err(|e| format!("cannot write {CONFIG_PATH}: {e}"))?;
        self.dirty = false;

        // Notify running daemon to reload config
        let reload_msg = notify_daemon_reload();
        self.status = Some((
            format!("Saved — {reload_msg}"),
            false,
        ));
        Ok(())
    }
}

// ---- Parsing & validation ----

fn parse_val(template: &Val, s: &str) -> Result<Val, String> {
    match template {
        Val::Str(_) => Ok(Val::Str(s.to_string())),
        Val::Bool(_) => match s {
            "true" | "yes" | "1" | "on" => Ok(Val::Bool(true)),
            "false" | "no" | "0" | "off" => Ok(Val::Bool(false)),
            _ => Err("enter true or false".into()),
        },
        Val::F32(_) => s
            .parse::<f32>()
            .map(Val::F32)
            .map_err(|_| "expected decimal number (e.g. 0.70)".into()),
        Val::U64(_) => s
            .parse::<u64>()
            .map(Val::U64)
            .map_err(|_| "expected non-negative integer".into()),
        Val::U32(_) => s
            .parse::<u32>()
            .map(Val::U32)
            .map_err(|_| "expected non-negative integer".into()),
        Val::I32(_) => s
            .parse::<i32>()
            .map(Val::I32)
            .map_err(|_| "expected integer".into()),
    }
}

fn validate(key: &str, val: &Val) -> Option<String> {
    match (key, val) {
        ("threshold", Val::F32(v)) if !(*v > 0.0 && *v < 1.0) => {
            Some("must be between 0.0 and 1.0".into())
        }
        ("frames_required", Val::U32(0)) => Some("must be at least 1".into()),
        ("session_timeout_s", Val::U64(0)) => Some("must be at least 1".into()),
        ("level", Val::Str(v)) => {
            if ["error", "warn", "info", "debug", "trace"].contains(&v.as_str()) {
                None
            } else {
                Some("must be: error, warn, info, debug, trace".into())
            }
        }
        ("execution_provider", Val::Str(v)) => {
            if ["cpu", "rocm", "cuda", "xdna"].contains(&v.as_str()) {
                None
            } else {
                Some("must be: cpu, rocm, cuda, xdna".into())
            }
        }
        _ => None,
    }
}

// ---- Config ↔ Fields conversion ----

fn to_fields(c: &Config) -> Vec<Field> {
    let d = Config::default();
    macro_rules! f {
        ($sec:literal, $key:literal, $desc:literal, $val:expr, $def:expr) => {
            Field {
                section: $sec,
                key: $key,
                desc: $desc,
                val: $val,
                def: $def,
            }
        };
    }
    vec![
        f!(
            "daemon", "session_timeout_s", "Max seconds per auth attempt",
            Val::U64(c.daemon.session_timeout_s), Val::U64(d.daemon.session_timeout_s)
        ),
        f!(
            "daemon", "idle_unload_s", "Unload models after N idle seconds (0 = keep loaded)",
            Val::U64(c.daemon.idle_unload_s), Val::U64(d.daemon.idle_unload_s)
        ),
        f!(
            "daemon", "execution_provider", "Inference backend: cpu / rocm / cuda / xdna",
            Val::Str(c.daemon.execution_provider.clone()), Val::Str(d.daemon.execution_provider.clone())
        ),
        f!(
            "camera", "device_path", "Camera device path (empty = auto-detect IR camera)",
            Val::Str(c.camera.device_path.clone()), Val::Str(d.camera.device_path.clone())
        ),
        f!(
            "camera", "flush_frames", "Frames to discard on open (0 = none)",
            Val::U32(c.camera.flush_frames), Val::U32(d.camera.flush_frames)
        ),
        f!(
            "recognition", "threshold", "Cosine similarity threshold 0.0–1.0 (default 0.70)",
            Val::F32(c.recognition.threshold), Val::F32(d.recognition.threshold)
        ),
        f!(
            "recognition", "frames_required", "Consecutive matching frames needed to accept",
            Val::U32(c.recognition.frames_required), Val::U32(d.recognition.frames_required)
        ),
        f!(
            "recognition", "max_enrollment", "Maximum face samples stored per user",
            Val::U32(c.recognition.max_enrollment), Val::U32(d.recognition.max_enrollment)
        ),
        f!(
            "liveness", "enabled", "Enable IR texture liveness detection (anti-spoof)",
            Val::Bool(c.liveness.enabled), Val::Bool(d.liveness.enabled)
        ),
        f!(
            "liveness", "lbp_entropy_min", "Min LBP entropy — real skin ~6.0, flat screens <5.0",
            Val::F32(c.liveness.lbp_entropy_min), Val::F32(d.liveness.lbp_entropy_min)
        ),
        f!(
            "liveness", "contrast_cv_min", "Min local contrast CV — real face ~0.28",
            Val::F32(c.liveness.local_contrast_cv_min), Val::F32(d.liveness.local_contrast_cv_min)
        ),
        f!(
            "liveness", "contrast_cv_max", "Max local contrast CV — screens/photos often >0.80",
            Val::F32(c.liveness.local_contrast_cv_max), Val::F32(d.liveness.local_contrast_cv_max)
        ),
        f!(
            "geometry", "distance_min", "Min face-width ratio — reports TooFar if below",
            Val::F32(c.geometry.distance_min), Val::F32(d.geometry.distance_min)
        ),
        f!(
            "geometry", "distance_max", "Max face-width ratio — reports TooClose if above",
            Val::F32(c.geometry.distance_max), Val::F32(d.geometry.distance_max)
        ),
        f!(
            "geometry", "yaw_max_deg", "Max horizontal turn allowed for auth (degrees)",
            Val::F32(c.geometry.yaw_max_deg), Val::F32(d.geometry.yaw_max_deg)
        ),
        f!(
            "geometry", "pitch_max_deg", "Max vertical tilt allowed for auth (degrees)",
            Val::F32(c.geometry.pitch_max_deg), Val::F32(d.geometry.pitch_max_deg)
        ),
        f!(
            "geometry", "roll_max_deg", "Max head roll allowed for auth (degrees)",
            Val::F32(c.geometry.roll_max_deg), Val::F32(d.geometry.roll_max_deg)
        ),
        f!(
            "logging", "level", "Log level: error / warn / info / debug / trace",
            Val::Str(c.logging.level.clone()), Val::Str(d.logging.level.clone())
        ),
        f!(
            "notify", "enabled", "Send desktop notification on successful auth",
            Val::Bool(c.notify.enabled), Val::Bool(d.notify.enabled)
        ),
        f!(
            "notify", "timeout_ms", "Notification display duration in milliseconds",
            Val::I32(c.notify.timeout_ms), Val::I32(d.notify.timeout_ms)
        ),
    ]
}

fn to_config(base: &Config, fields: &[Field]) -> Config {
    let mut c = base.clone(); // preserve platform + any unexposed fields
    for f in fields {
        match (f.section, f.key, &f.val) {
            ("daemon", "session_timeout_s", Val::U64(v)) => c.daemon.session_timeout_s = *v,
            ("daemon", "idle_unload_s", Val::U64(v)) => c.daemon.idle_unload_s = *v,
            ("daemon", "execution_provider", Val::Str(v)) => {
                c.daemon.execution_provider = v.clone()
            }
            ("camera", "device_path", Val::Str(v)) => c.camera.device_path = v.clone(),
            ("camera", "flush_frames", Val::U32(v)) => c.camera.flush_frames = *v,
            ("recognition", "threshold", Val::F32(v)) => c.recognition.threshold = *v,
            ("recognition", "frames_required", Val::U32(v)) => {
                c.recognition.frames_required = *v
            }
            ("recognition", "max_enrollment", Val::U32(v)) => c.recognition.max_enrollment = *v,
            ("liveness", "enabled", Val::Bool(v)) => c.liveness.enabled = *v,
            ("liveness", "lbp_entropy_min", Val::F32(v)) => c.liveness.lbp_entropy_min = *v,
            ("liveness", "contrast_cv_min", Val::F32(v)) => {
                c.liveness.local_contrast_cv_min = *v
            }
            ("liveness", "contrast_cv_max", Val::F32(v)) => {
                c.liveness.local_contrast_cv_max = *v
            }
            ("geometry", "distance_min", Val::F32(v)) => c.geometry.distance_min = *v,
            ("geometry", "distance_max", Val::F32(v)) => c.geometry.distance_max = *v,
            ("geometry", "yaw_max_deg", Val::F32(v)) => c.geometry.yaw_max_deg = *v,
            ("geometry", "pitch_max_deg", Val::F32(v)) => c.geometry.pitch_max_deg = *v,
            ("geometry", "roll_max_deg", Val::F32(v)) => c.geometry.roll_max_deg = *v,
            ("logging", "level", Val::Str(v)) => c.logging.level = v.clone(),
            ("notify", "enabled", Val::Bool(v)) => c.notify.enabled = *v,
            ("notify", "timeout_ms", Val::I32(v)) => c.notify.timeout_ms = *v,
            _ => {}
        }
    }
    c
}

// ---- Daemon reload ----

/// Send SIGHUP to face-authd via systemctl so the daemon hot-reloads config.
/// Returns a short status string describing what happened.
fn notify_daemon_reload() -> &'static str {
    let result = std::process::Command::new("systemctl")
        .args(["kill", "--signal=SIGHUP", "face-authd"])
        .output();

    match result {
        Ok(out) if out.status.success() => "daemon reloaded",
        Ok(_) => "saved (daemon not running — start with: systemctl start face-authd)",
        Err(_) => "saved (systemctl unavailable — restart daemon manually)",
    }
}

// ---- TUI entry point ----

pub fn run_configure() {
    let config = Config::load_system().unwrap_or_else(|_| Config::default());
    let can_save = unsafe { libc::geteuid() } == 0;

    let mut app = App::new(config);

    if !can_save {
        app.status = Some((
            "Not root — save will fail. Re-run with: sudo face-enroll --configure".into(),
            true,
        ));
    }

    match run_tui(&mut app) {
        Ok(()) => {}
        Err(e) => eprintln!("TUI error: {e}"),
    }

    if app.dirty {
        eprintln!("Note: unsaved changes discarded.");
    }
}

fn run_tui(app: &mut App) -> io::Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, app);

    // Always restore terminal, even on error
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();

    result
}

// ---- Event loop ----

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> io::Result<()> {
    loop {
        terminal.draw(|f| draw(f, app))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            if app.edit {
                match key.code {
                    KeyCode::Esc => app.cancel(),
                    KeyCode::Enter => app.confirm(),
                    KeyCode::Char(c) => {
                        app.buf.push(c);
                        app.err = None; // clear error on input
                    }
                    KeyCode::Backspace => {
                        app.buf.pop();
                        app.err = None;
                    }
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('s') => match app.save() {
                        Ok(()) => {}
                        Err(e) => app.status = Some((e, true)),
                    },
                    KeyCode::Char('r') => app.reset_field(),
                    KeyCode::Up | KeyCode::Char('k') => app.up(),
                    KeyCode::Down | KeyCode::Char('j') => app.down(),
                    KeyCode::Enter | KeyCode::Char('e') => app.activate(),
                    _ => {}
                }
            }
        }
    }
}

// ---- Rendering ----

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // title
            Constraint::Min(5),    // table
            Constraint::Length(3), // bottom bar
        ])
        .split(area);

    draw_title(f, app, chunks[0]);
    draw_table(f, app, chunks[1]);
    draw_bottom(f, app, chunks[2]);
}

fn draw_title(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let (label, style) = if app.dirty {
        (
            format!(" Face Auth Configuration — {CONFIG_PATH}  [unsaved] "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (
            format!(" Face Auth Configuration — {CONFIG_PATH} "),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )
    };
    let title = Paragraph::new(label)
        .style(style)
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, area);
}

fn draw_table(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let header = Row::new(vec![
        Cell::from("Section").style(Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Cell::from("Setting").style(Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Cell::from("Value").style(Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Cell::from("Default").style(Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Cell::from("Description").style(Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
    ])
    .height(1)
    .bottom_margin(1);

    let selected = app.selected();

    let rows: Vec<Row> = app
        .fields
        .iter()
        .enumerate()
        .map(|(i, field)| {
            // Show section label only on first row of each section group
            let section_str = if i == 0 || app.fields[i - 1].section != field.section {
                field.section
            } else {
                ""
            };

            let val_str = field.val.display();
            let def_str = field.def.display();
            let is_sel = i == selected;

            let val_style = if field.modified() {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Green)
            };

            // Bool values get a visual indicator
            let val_display = match &field.val {
                Val::Bool(true) => "true  ✓".to_string(),
                Val::Bool(false) => "false ✗".to_string(),
                _ => val_str,
            };

            Row::new(vec![
                Cell::from(section_str).style(Style::default().fg(Color::Cyan)),
                Cell::from(field.key),
                Cell::from(val_display).style(val_style),
                Cell::from(def_str).style(Style::default().fg(Color::DarkGray)),
                Cell::from(field.desc).style(Style::default().fg(Color::Gray)),
            ])
            .height(1)
            // Per-row selection style is applied via highlight_style below;
            // we only need the Cell styles for non-selected appearance.
            .style(if is_sel {
                Style::default()
            } else {
                Style::default()
            })
        })
        .collect();

    let widths = [
        Constraint::Length(12),  // section
        Constraint::Length(20),  // key
        Constraint::Length(13),  // value
        Constraint::Length(9),   // default
        Constraint::Min(20),     // description
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(" Settings "))
        .highlight_spacing(HighlightSpacing::Always)
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

    f.render_stateful_widget(table, area, &mut app.state);
}

fn draw_bottom(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let widget = if app.edit {
        let field = &app.fields[app.selected()];
        let err_suffix = app
            .err
            .as_deref()
            .map(|e| format!("  ! {e}"))
            .unwrap_or_default();
        let text = format!(" {} > [{}]{}  Esc=cancel  Enter=confirm", field.key, app.buf, err_suffix);
        let style = if app.err.is_some() {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Yellow)
        };
        Paragraph::new(text)
            .style(style)
            .block(Block::default().borders(Borders::ALL))
    } else if let Some((ref msg, is_err)) = app.status {
        let style = if is_err {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Green)
        };
        Paragraph::new(format!(" {msg}"))
            .style(style)
            .block(Block::default().borders(Borders::ALL))
    } else {
        let hint = if app.dirty {
            " ↑↓/jk  Navigate    Enter/e  Edit    r  Reset field    s  Save*    q  Quit"
        } else {
            " ↑↓/jk  Navigate    Enter/e  Edit    r  Reset field    s  Save     q  Quit"
        };
        Paragraph::new(hint)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL))
    };

    f.render_widget(widget, area);
}
