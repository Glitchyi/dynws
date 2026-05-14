use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, Padding, Paragraph};

use crate::config::DynwsPaths;
use crate::discovery::{RepoCandidate, discover_repos, fuzzy_matches};
use crate::git;
use crate::session::{SessionMetadata, SessionStore};

pub fn run_interactive(paths: &DynwsPaths, cwd: &Path) -> Result<Option<SessionMetadata>> {
    let repos = discover_repos(cwd)?;
    if repos.is_empty() {
        println!("no subdirectories found in {}", cwd.display());
        return Ok(None);
    }

    let mut terminal = setup_terminal()?;
    let store = SessionStore::new(paths.clone());
    let mut app = App::new(repos, store);
    let outcome = run_app(&mut terminal, &mut app);
    restore_terminal(&mut terminal)?;
    outcome
}

pub fn run_session_picker(
    paths: &DynwsPaths,
    sessions: Vec<SessionMetadata>,
) -> Result<Option<SessionMetadata>> {
    if sessions.is_empty() {
        println!("no sessions found");
        return Ok(None);
    }

    let mut terminal = setup_terminal()?;
    let store = SessionStore::new(paths.clone());
    let mut app = SessionPickerApp::new(sessions, store);
    let outcome = run_session_picker_app(&mut terminal, &mut app);
    restore_terminal(&mut terminal)?;
    outcome
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<Option<SessionMetadata>> {
    loop {
        terminal.draw(|frame| app.draw(frame))?;
        if event::poll(Duration::from_millis(200))? {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match app.handle_key(key)? {
                AppSignal::Continue => {}
                AppSignal::Done(outcome) => return Ok(outcome),
            }
        }
    }
}

fn run_session_picker_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut SessionPickerApp,
) -> Result<Option<SessionMetadata>> {
    loop {
        terminal.draw(|frame| app.draw(frame))?;
        if event::poll(Duration::from_millis(200))? {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match app.handle_key(key) {
                SessionPickerSignal::Continue => {}
                SessionPickerSignal::Done(outcome) => return Ok(outcome),
            }
        }
    }
}

enum AppSignal {
    Continue,
    Done(Option<SessionMetadata>),
}

#[derive(Debug, Clone)]
enum Mode {
    Select,
    Search,
    Name,
    Duplicate { existing: SessionMetadata },
    Worktree { repo_idx: usize, input: String },
}

struct App {
    repos: Vec<RepoCandidate>,
    selected: BTreeSet<usize>,
    cursor: usize,
    filter: String,
    session_name: String,
    message: String,
    mode: Mode,
    store: SessionStore,
}

impl App {
    fn new(repos: Vec<RepoCandidate>, store: SessionStore) -> Self {
        Self {
            repos,
            selected: BTreeSet::new(),
            cursor: 0,
            filter: String::new(),
            session_name: String::new(),
            message: String::new(),
            mode: Mode::Select,
            store,
        }
    }

    fn draw(&mut self, frame: &mut ratatui::Frame<'_>) {
        let area = frame.area();
        frame.render_widget(
            Block::default().style(Style::default().fg(theme::FG).bg(theme::BG)),
            area,
        );
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),
                Constraint::Min(6),
                Constraint::Length(5),
            ])
            .split(area);

        let header = Paragraph::new(self.header_line())
            .alignment(Alignment::Center)
            .style(Style::default().fg(theme::FG).bg(theme::BG))
            .block(panel_block("dws", self.mode_color()).padding(Padding::horizontal(1)));
        frame.render_widget(header, chunks[0]);

        let indices = self.filtered_indices();
        if self.cursor >= indices.len() {
            self.cursor = indices.len().saturating_sub(1);
        }
        let items = indices
            .iter()
            .enumerate()
            .map(|(visible_idx, repo_idx)| self.repo_item(*repo_idx, visible_idx == self.cursor))
            .collect::<Vec<_>>();
        let title = format!("repos {}/{}", indices.len(), self.repos.len());
        let list = List::new(items)
            .highlight_symbol(">")
            .block(panel_block(&title, theme::CYAN).padding(Padding::horizontal(1)));
        frame.render_widget(list, chunks[1]);

        let footer = Paragraph::new(self.footer_lines())
            .style(Style::default().fg(theme::FG).bg(theme::BG))
            .block(panel_block("prompt", self.mode_color()).padding(Padding::horizontal(1)));
        frame.render_widget(footer, chunks[2]);
    }

    fn header_line(&self) -> Line<'static> {
        let mode = self.mode_name();
        Line::from(vec![
            Span::styled(
                "dynamic workspace manager  ",
                Style::default()
                    .fg(theme::PURPLE)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("[{mode}]  "),
                Style::default()
                    .fg(theme::BG)
                    .bg(self.mode_color())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("j/k", key_style()),
            Span::raw(" move  "),
            Span::styled("space", key_style()),
            Span::raw(" select  "),
            Span::styled("/", key_style()),
            Span::raw(" search  "),
            Span::styled("enter", key_style()),
            Span::raw(" create  "),
            Span::styled("w", key_style()),
            Span::raw(" worktree  "),
            Span::styled("q", key_style()),
            Span::raw(" quit"),
        ])
    }

    fn repo_item(&self, repo_idx: usize, highlighted: bool) -> ListItem<'static> {
        let repo = &self.repos[repo_idx];
        let is_selected = self.selected.contains(&repo_idx);
        let marker = if is_selected {
            Span::styled(
                "[x]",
                Style::default()
                    .fg(theme::BG)
                    .bg(theme::GREEN)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled("[ ]", Style::default().fg(theme::COMMENT))
        };
        let git_spans = match &repo.git {
            Some(status) => {
                if status.dirty {
                    vec![
                        Span::styled(" git ", pill_style(theme::YELLOW)),
                        Span::styled(
                            format!(" {}* ", status.branch),
                            Style::default().fg(theme::YELLOW),
                        ),
                    ]
                } else {
                    vec![
                        Span::styled(" git ", pill_style(theme::GREEN)),
                        Span::styled(
                            format!(" {} ", status.branch),
                            Style::default().fg(theme::GREEN),
                        ),
                    ]
                }
            }
            None => vec![
                Span::styled(" dir ", pill_style(theme::COMMENT)),
                Span::styled(" non-git ", Style::default().fg(theme::COMMENT)),
            ],
        };
        let style = if highlighted {
            Style::default()
                .fg(theme::FG)
                .bg(theme::BG_HIGHLIGHT)
                .add_modifier(Modifier::BOLD)
        } else if is_selected {
            Style::default().fg(theme::GREEN).bg(theme::BG)
        } else {
            Style::default().fg(theme::FG_DIM).bg(theme::BG)
        };

        let mut spans = vec![
            marker,
            Span::raw(" "),
            Span::styled(
                repo.name.clone(),
                if is_selected {
                    Style::default()
                        .fg(theme::GREEN)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::FG)
                },
            ),
            Span::raw("  "),
        ];
        spans.extend(git_spans);

        ListItem::new(Line::from(spans)).style(style)
    }

    fn footer_lines(&self) -> Vec<Line<'static>> {
        let primary = match &self.mode {
            Mode::Select => Line::from(vec![
                Span::styled("filter ", label_style(theme::CYAN)),
                Span::styled(empty_marker(&self.filter), value_style()),
                Span::raw("   "),
                Span::styled("selected ", label_style(theme::GREEN)),
                Span::styled(self.selected.len().to_string(), value_style()),
            ]),
            Mode::Search => Line::from(vec![
                Span::styled("search ", label_style(theme::CYAN)),
                Span::styled(empty_marker(&self.filter), value_style()),
            ]),
            Mode::Name => Line::from(vec![
                Span::styled("session name ", label_style(theme::PURPLE)),
                Span::styled(empty_marker(&self.session_name), value_style()),
            ]),
            Mode::Duplicate { existing } => Line::from(vec![
                Span::styled("duplicate ", label_style(theme::YELLOW)),
                Span::raw("matching session exists: "),
                Span::styled(existing.name.clone(), value_style()),
                Span::raw("   "),
                Span::styled("r", key_style()),
                Span::raw(" reuse  "),
                Span::styled("n", key_style()),
                Span::raw(" create duplicate  "),
                Span::styled("esc", key_style()),
                Span::raw(" rename"),
            ]),
            Mode::Worktree { repo_idx, input } => Line::from(vec![
                Span::styled("worktree ", label_style(theme::BLUE)),
                Span::raw("for "),
                Span::styled(self.repos[*repo_idx].name.clone(), value_style()),
                Span::raw(": "),
                Span::styled(empty_marker(input), value_style()),
            ]),
        };

        vec![
            primary,
            self.message_line(),
            Line::from(vec![
                Span::styled("esc", key_style()),
                Span::raw(" returns to selection or clears the current prompt"),
            ]),
        ]
    }

    fn message_line(&self) -> Line<'static> {
        if self.message.is_empty() {
            return Line::from(Span::styled("ready", Style::default().fg(theme::COMMENT)));
        }

        let color = if self.message.contains("created") {
            theme::GREEN
        } else {
            theme::YELLOW
        };
        Line::from(Span::styled(
            self.message.clone(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ))
    }

    fn mode_name(&self) -> &'static str {
        match self.mode {
            Mode::Select => "select",
            Mode::Search => "search",
            Mode::Name => "name",
            Mode::Duplicate { .. } => "duplicate",
            Mode::Worktree { .. } => "worktree",
        }
    }

    fn mode_color(&self) -> Color {
        match self.mode {
            Mode::Select => theme::CYAN,
            Mode::Search => theme::BLUE,
            Mode::Name => theme::PURPLE,
            Mode::Duplicate { .. } => theme::YELLOW,
            Mode::Worktree { .. } => theme::GREEN,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<AppSignal> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(AppSignal::Done(None));
        }

        match self.mode.clone() {
            Mode::Select => self.handle_select_key(key),
            Mode::Search => self.handle_search_key(key),
            Mode::Name => self.handle_name_key(key),
            Mode::Duplicate { existing } => self.handle_duplicate_key(key, existing),
            Mode::Worktree { repo_idx, input } => self.handle_worktree_key(key, repo_idx, input),
        }
    }

    fn handle_select_key(&mut self, key: KeyEvent) -> Result<AppSignal> {
        match key.code {
            KeyCode::Char('q') => return Ok(AppSignal::Done(None)),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Char(' ') => self.toggle_current(),
            KeyCode::Char('/') => self.mode = Mode::Search,
            KeyCode::Char('w') => self.start_worktree(),
            KeyCode::Enter => {
                if self.selected.is_empty() {
                    self.message = "select at least one repo".to_string();
                } else {
                    self.mode = Mode::Name;
                    self.message.clear();
                }
            }
            KeyCode::Esc => {
                self.filter.clear();
                self.cursor = 0;
            }
            _ => {}
        }

        Ok(AppSignal::Continue)
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> Result<AppSignal> {
        match key.code {
            KeyCode::Enter | KeyCode::Esc => self.mode = Mode::Select,
            KeyCode::Backspace => {
                self.filter.pop();
                self.cursor = 0;
            }
            KeyCode::Char(character) => {
                self.filter.push(character);
                self.cursor = 0;
            }
            _ => {}
        }
        Ok(AppSignal::Continue)
    }

    fn handle_name_key(&mut self, key: KeyEvent) -> Result<AppSignal> {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Select,
            KeyCode::Backspace => {
                self.session_name.pop();
            }
            KeyCode::Char(character) => self.session_name.push(character),
            KeyCode::Enter => return self.try_create(false),
            _ => {}
        }
        Ok(AppSignal::Continue)
    }

    fn handle_duplicate_key(
        &mut self,
        key: KeyEvent,
        existing: SessionMetadata,
    ) -> Result<AppSignal> {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Name,
            KeyCode::Char('r') => return Ok(AppSignal::Done(Some(existing))),
            KeyCode::Char('n') => return self.try_create(true),
            _ => {}
        }
        Ok(AppSignal::Continue)
    }

    fn handle_worktree_key(
        &mut self,
        key: KeyEvent,
        repo_idx: usize,
        mut input: String,
    ) -> Result<AppSignal> {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Select,
            KeyCode::Backspace => {
                input.pop();
                self.mode = Mode::Worktree { repo_idx, input };
            }
            KeyCode::Char(character) => {
                input.push(character);
                self.mode = Mode::Worktree { repo_idx, input };
            }
            KeyCode::Enter => {
                if input.trim().is_empty() {
                    self.message = "worktree name cannot be empty".to_string();
                    self.mode = Mode::Worktree { repo_idx, input };
                    return Ok(AppSignal::Continue);
                }

                match git::create_worktree(
                    &self.repos[repo_idx].path,
                    &self.store.paths().worktrees_dir,
                    &input,
                ) {
                    Ok(path) => {
                        self.message = format!("created worktree {}", path.display());
                        self.mode = Mode::Select;
                    }
                    Err(error) => {
                        self.message = error.to_string();
                        self.mode = Mode::Worktree { repo_idx, input };
                    }
                }
            }
            _ => self.mode = Mode::Worktree { repo_idx, input },
        }

        Ok(AppSignal::Continue)
    }

    fn move_cursor(&mut self, delta: isize) {
        let len = self.filtered_indices().len();
        if len == 0 {
            self.cursor = 0;
            return;
        }

        self.cursor = if delta < 0 {
            self.cursor.saturating_sub(delta.unsigned_abs())
        } else {
            (self.cursor + delta as usize).min(len - 1)
        };
    }

    fn toggle_current(&mut self) {
        let Some(repo_idx) = self.current_repo_idx() else {
            return;
        };
        if !self.selected.insert(repo_idx) {
            self.selected.remove(&repo_idx);
        }
    }

    fn start_worktree(&mut self) {
        let Some(repo_idx) = self.current_repo_idx() else {
            return;
        };
        if self.repos[repo_idx].git.is_none() {
            self.message = "highlighted folder is not a git repository".to_string();
            return;
        }
        self.mode = Mode::Worktree {
            repo_idx,
            input: String::new(),
        };
        self.message.clear();
    }

    fn try_create(&mut self, allow_duplicate: bool) -> Result<AppSignal> {
        if self.session_name.trim().is_empty() {
            self.message = "session name cannot be empty".to_string();
            return Ok(AppSignal::Continue);
        }

        let selected = self.selected_paths();
        if !allow_duplicate {
            if let Some(existing) = self.store.find_duplicate(&selected)? {
                self.mode = Mode::Duplicate { existing };
                return Ok(AppSignal::Continue);
            }
        }

        match self
            .store
            .create_session(&self.session_name, &selected, None, allow_duplicate)
        {
            Ok(outcome) => Ok(AppSignal::Done(Some(outcome.into_metadata()))),
            Err(error) => {
                self.message = error.to_string();
                Ok(AppSignal::Continue)
            }
        }
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        self.selected
            .iter()
            .map(|idx| self.repos[*idx].path.clone())
            .collect()
    }

    fn current_repo_idx(&self) -> Option<usize> {
        self.filtered_indices().get(self.cursor).copied()
    }

    fn filtered_indices(&self) -> Vec<usize> {
        self.repos
            .iter()
            .enumerate()
            .filter_map(|(idx, repo)| fuzzy_matches(&self.filter, repo).then_some(idx))
            .collect()
    }
}

enum SessionPickerSignal {
    Continue,
    Done(Option<SessionMetadata>),
}

#[derive(Debug, Clone, Copy)]
enum SessionPickerMode {
    Browse,
    Search,
}

struct SessionPickerApp {
    sessions: Vec<SessionMetadata>,
    store: SessionStore,
    cursor: usize,
    filter: String,
    mode: SessionPickerMode,
}

impl SessionPickerApp {
    fn new(sessions: Vec<SessionMetadata>, store: SessionStore) -> Self {
        Self {
            sessions,
            store,
            cursor: 0,
            filter: String::new(),
            mode: SessionPickerMode::Browse,
        }
    }

    fn draw(&mut self, frame: &mut ratatui::Frame<'_>) {
        let area = frame.area();
        frame.render_widget(
            Block::default().style(Style::default().fg(theme::FG).bg(theme::BG)),
            area,
        );
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),
                Constraint::Min(7),
                Constraint::Length(6),
            ])
            .split(area);

        let header = Paragraph::new(self.header_line())
            .alignment(Alignment::Center)
            .style(Style::default().fg(theme::FG).bg(theme::BG))
            .block(panel_block("dws list", theme::BLUE).padding(Padding::horizontal(1)));
        frame.render_widget(header, chunks[0]);

        let indices = self.filtered_indices();
        if self.cursor >= indices.len() {
            self.cursor = indices.len().saturating_sub(1);
        }
        let items = indices
            .iter()
            .enumerate()
            .map(|(visible_idx, session_idx)| {
                self.session_item(*session_idx, visible_idx == self.cursor)
            })
            .collect::<Vec<_>>();
        let title = format!("workspaces {}/{}", indices.len(), self.sessions.len());
        let list = List::new(items)
            .highlight_symbol(">")
            .block(panel_block(&title, theme::CYAN).padding(Padding::horizontal(1)));
        frame.render_widget(list, chunks[1]);

        let footer = Paragraph::new(self.footer_lines())
            .style(Style::default().fg(theme::FG).bg(theme::BG))
            .block(panel_block("open", theme::PURPLE).padding(Padding::horizontal(1)));
        frame.render_widget(footer, chunks[2]);
    }

    fn header_line(&self) -> Line<'static> {
        let mode = match self.mode {
            SessionPickerMode::Browse => "browse",
            SessionPickerMode::Search => "search",
        };
        Line::from(vec![
            Span::styled(
                "existing dynamic workspaces  ",
                Style::default()
                    .fg(theme::PURPLE)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("[{mode}]  "),
                Style::default()
                    .fg(theme::BG)
                    .bg(theme::BLUE)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("j/k", key_style()),
            Span::raw(" move  "),
            Span::styled("/", key_style()),
            Span::raw(" search  "),
            Span::styled("enter", key_style()),
            Span::raw(" open  "),
            Span::styled("q", key_style()),
            Span::raw(" quit"),
        ])
    }

    fn session_item(&self, session_idx: usize, highlighted: bool) -> ListItem<'static> {
        let session = &self.sessions[session_idx];
        let repo_count = session.repos.len();
        let style = if highlighted {
            Style::default()
                .fg(theme::FG)
                .bg(theme::BG_HIGHLIGHT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::FG_DIM).bg(theme::BG)
        };
        let mut spans = vec![
            Span::styled(" ws ", pill_style(theme::BLUE)),
            Span::raw(" "),
            Span::styled(
                session.name.clone(),
                Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(format!(" {repo_count} repos "), pill_style(theme::CYAN)),
        ];
        if let Some(description) = &session.description {
            spans.extend([
                Span::raw("  "),
                Span::styled(description.clone(), Style::default().fg(theme::COMMENT)),
            ]);
        }

        ListItem::new(Line::from(spans)).style(style)
    }

    fn footer_lines(&self) -> Vec<Line<'static>> {
        let selected = self.current_session();
        let path = selected
            .map(|session| {
                self.store
                    .workspace_path(&session.name)
                    .display()
                    .to_string()
            })
            .unwrap_or_else(|| "<none>".to_string());
        let repo_preview = selected
            .map(session_repo_preview)
            .unwrap_or_else(|| "no matching workspace".to_string());

        vec![
            Line::from(vec![
                Span::styled("filter ", label_style(theme::CYAN)),
                Span::styled(empty_marker(&self.filter), value_style()),
            ]),
            Line::from(vec![
                Span::styled("workspace ", label_style(theme::PURPLE)),
                Span::styled(path, Style::default().fg(theme::FG)),
            ]),
            Line::from(vec![
                Span::styled("repos ", label_style(theme::GREEN)),
                Span::styled(repo_preview, Style::default().fg(theme::FG_DIM)),
            ]),
            Line::from(vec![
                Span::styled("enter", key_style()),
                Span::raw(" opens the highlighted workspace in the configured editor"),
            ]),
        ]
    }

    fn handle_key(&mut self, key: KeyEvent) -> SessionPickerSignal {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return SessionPickerSignal::Done(None);
        }

        match self.mode {
            SessionPickerMode::Browse => self.handle_browse_key(key),
            SessionPickerMode::Search => self.handle_search_key(key),
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent) -> SessionPickerSignal {
        match key.code {
            KeyCode::Char('q') => return SessionPickerSignal::Done(None),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Char('/') => self.mode = SessionPickerMode::Search,
            KeyCode::Enter => {
                return SessionPickerSignal::Done(self.current_session().cloned());
            }
            KeyCode::Esc => {
                self.filter.clear();
                self.cursor = 0;
            }
            _ => {}
        }
        SessionPickerSignal::Continue
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> SessionPickerSignal {
        match key.code {
            KeyCode::Enter | KeyCode::Esc => self.mode = SessionPickerMode::Browse,
            KeyCode::Backspace => {
                self.filter.pop();
                self.cursor = 0;
            }
            KeyCode::Char(character) => {
                self.filter.push(character);
                self.cursor = 0;
            }
            _ => {}
        }
        SessionPickerSignal::Continue
    }

    fn move_cursor(&mut self, delta: isize) {
        let len = self.filtered_indices().len();
        if len == 0 {
            self.cursor = 0;
            return;
        }

        self.cursor = if delta < 0 {
            self.cursor.saturating_sub(delta.unsigned_abs())
        } else {
            (self.cursor + delta as usize).min(len - 1)
        };
    }

    fn current_session(&self) -> Option<&SessionMetadata> {
        self.filtered_indices()
            .get(self.cursor)
            .and_then(|idx| self.sessions.get(*idx))
    }

    fn filtered_indices(&self) -> Vec<usize> {
        self.sessions
            .iter()
            .enumerate()
            .filter_map(|(idx, session)| session_matches(&self.filter, session).then_some(idx))
            .collect()
    }
}

fn empty_marker(value: &str) -> String {
    if value.is_empty() {
        "<empty>".to_string()
    } else {
        value.to_string()
    }
}

fn session_repo_preview(session: &SessionMetadata) -> String {
    if session.repos.is_empty() {
        return "no linked repos".to_string();
    }

    let mut names = session
        .repos
        .iter()
        .take(4)
        .map(|repo| repo.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if session.repos.len() > 4 {
        names.push_str(&format!(" +{}", session.repos.len() - 4));
    }
    names
}

fn session_matches(query: &str, session: &SessionMetadata) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }

    let mut haystack = session.name.clone();
    if let Some(description) = &session.description {
        haystack.push(' ');
        haystack.push_str(description);
    }
    for repo in &session.repos {
        haystack.push(' ');
        haystack.push_str(&repo.name);
        haystack.push(' ');
        haystack.push_str(&repo.path);
    }

    fuzzy_subsequence(&query.to_lowercase(), &haystack.to_lowercase())
}

fn fuzzy_subsequence(needle: &str, haystack: &str) -> bool {
    let mut haystack = haystack.chars();
    needle
        .chars()
        .all(|needle_char| haystack.any(|hay_char| hay_char == needle_char))
}

fn panel_block(title: &str, color: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(Style::default().fg(theme::FG).bg(theme::BG))
        .border_style(Style::default().fg(color))
        .title(Line::from(Span::styled(
            format!(" {title} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )))
}

fn key_style() -> Style {
    Style::default()
        .fg(theme::BG)
        .bg(theme::BLUE)
        .add_modifier(Modifier::BOLD)
}

fn pill_style(color: Color) -> Style {
    Style::default()
        .fg(theme::BG)
        .bg(color)
        .add_modifier(Modifier::BOLD)
}

fn label_style(color: Color) -> Style {
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn value_style() -> Style {
    Style::default().fg(theme::FG).add_modifier(Modifier::BOLD)
}

mod theme {
    use ratatui::style::Color;

    pub const BG: Color = Color::Rgb(26, 27, 38);
    pub const BG_HIGHLIGHT: Color = Color::Rgb(41, 46, 66);
    pub const FG: Color = Color::Rgb(192, 202, 245);
    pub const FG_DIM: Color = Color::Rgb(169, 177, 214);
    pub const COMMENT: Color = Color::Rgb(86, 95, 137);
    pub const BLUE: Color = Color::Rgb(122, 162, 247);
    pub const CYAN: Color = Color::Rgb(125, 207, 255);
    pub const GREEN: Color = Color::Rgb(158, 206, 106);
    pub const PURPLE: Color = Color::Rgb(187, 154, 247);
    pub const YELLOW: Color = Color::Rgb(224, 175, 104);
}
