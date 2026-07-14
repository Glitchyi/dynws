use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use std::process::{Child, Command, Stdio};
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
use crate::discovery::{RepoCandidate, discover_repos_excluding, fuzzy_matches};
use crate::git;
use crate::session::{RepoKind, RepoLink, RepoSelection, SessionMetadata, SessionStore};

pub fn run_interactive(paths: &DynwsPaths, cwd: &Path) -> Result<Option<SessionMetadata>> {
    let repos = discover_repos_excluding(cwd, Some(&paths.home))?;
    if repos.is_empty() {
        println!("no subdirectories found in {}", cwd.display());
        return Ok(None);
    }

    let mut terminal = TerminalSession::new()?;
    let store = SessionStore::new(paths.clone());
    let mut app = App::new(repos, store);
    run_app(terminal.terminal_mut(), &mut app)
}

pub fn run_session_manager(
    paths: &DynwsPaths,
    cwd: &Path,
    sessions: Vec<SessionMetadata>,
) -> Result<Vec<String>> {
    if sessions.is_empty() {
        println!("no sessions found");
        return Ok(Vec::new());
    }

    let mut terminal = TerminalSession::new()?;
    let store = SessionStore::new(paths.clone());
    let mut app = SessionManagerApp::new(sessions, store);
    run_session_manager_app(terminal.terminal_mut(), &mut app, cwd)
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl TerminalSession {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error.into());
        }
        match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let _ = disable_raw_mode();
                let mut stdout = io::stdout();
                let _ = execute!(stdout, LeaveAlternateScreen);
                Err(error.into())
            }
        }
    }

    fn terminal_mut(&mut self) -> &mut Terminal<CrosstermBackend<io::Stdout>> {
        &mut self.terminal
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<Option<SessionMetadata>> {
    loop {
        terminal.draw(|frame| app.draw(frame))?;
        let has_event = if matches!(app.mode, Mode::BranchLoading) {
            event::poll(Duration::from_millis(100))?
        } else {
            true
        };
        if has_event {
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

fn run_session_manager_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut SessionManagerApp,
    cwd: &Path,
) -> Result<Vec<String>> {
    loop {
        terminal.draw(|frame| app.draw(frame))?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match app.handle_key(key) {
            SessionManagerSignal::Continue => {}
            SessionManagerSignal::AddRepos {
                session_name,
                repo_cursor,
            } => run_add_repos_app(terminal, app, cwd, session_name, repo_cursor)?,
            SessionManagerSignal::Done => return Ok(app.changes.clone()),
        }
    }
}

fn run_add_repos_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    manager: &mut SessionManagerApp,
    cwd: &Path,
    session_name: String,
    repo_cursor: usize,
) -> Result<()> {
    let Some(session) = manager
        .sessions
        .iter()
        .find(|session| session.name == session_name)
        .cloned()
    else {
        manager.message = format!("session '{session_name}' was not found");
        manager.mode = SessionManagerMode::Browse;
        return Ok(());
    };

    let repos = match discover_repos_excluding(cwd, Some(&manager.store.paths().home)) {
        Ok(repos) => addable_repo_candidates(&session, repos),
        Err(error) => {
            manager.message = error.to_string();
            return Ok(());
        }
    };
    if repos.is_empty() {
        manager.message = format!("no new repositories found in {}", cwd.display());
        return Ok(());
    }

    let mut add_app = App::for_add(repos, manager.store.clone(), session_name.clone());
    match run_app(terminal, &mut add_app)? {
        Some(updated) => {
            manager.finish_repo_add(&session_name, updated, &add_app.added_repos);
        }
        None => {
            manager.mode = SessionManagerMode::Expanded {
                session_name,
                repo_cursor,
            };
            manager.message = "add repositories cancelled".to_string();
        }
    }
    Ok(())
}

enum AppSignal {
    Continue,
    Done(Option<SessionMetadata>),
}

#[derive(Debug, Clone)]
enum AppPurpose {
    CreateSession,
    AddRepos { session_name: String },
}

#[derive(Debug, Clone)]
enum Mode {
    Select,
    BranchLoading,
    Branch(BranchPicker),
    Name,
    Duplicate { existing: SessionMetadata },
}

#[derive(Debug, Clone)]
struct BranchPicker {
    repo_indices: Vec<usize>,
    current: usize,
    branches: Vec<git::OriginBranch>,
    filter: String,
}

#[derive(Debug, Clone)]
struct WorktreeChoice {
    repo_idx: usize,
    branch: git::OriginBranch,
}

struct MaterializedSelections {
    selections: Vec<RepoSelection>,
    created_worktrees: Vec<std::path::PathBuf>,
}

struct BranchFetch {
    repo_indices: Vec<usize>,
    current: usize,
    repo_name: String,
    task: GitBranchTask,
    frame: usize,
}

#[derive(Clone, Copy)]
enum GitTaskPhase {
    FetchPrune,
    FetchFallback,
    ListBranches,
}

struct GitBranchTask {
    repo_path: std::path::PathBuf,
    child: Option<Child>,
    phase: GitTaskPhase,
}

impl GitBranchTask {
    fn start(repo_path: std::path::PathBuf) -> Result<Self> {
        let child = spawn_git(&repo_path, &["fetch", "origin", "--prune"])?;
        Ok(Self {
            repo_path,
            child: Some(child),
            phase: GitTaskPhase::FetchPrune,
        })
    }

    fn poll(&mut self) -> Option<Result<Vec<git::OriginBranch>, String>> {
        let status = match self.child.as_mut()?.try_wait() {
            Ok(Some(status)) => status,
            Ok(None) => return None,
            Err(error) => return Some(Err(format!("failed to poll git: {error}"))),
        };
        let output = match self.child.take().expect("checked child").wait_with_output() {
            Ok(output) => output,
            Err(error) => return Some(Err(format!("failed to wait for git: {error}"))),
        };
        match self.phase {
            GitTaskPhase::FetchPrune if !status.success() => {
                match spawn_git(&self.repo_path, &["fetch", "origin"]) {
                    Ok(child) => {
                        self.child = Some(child);
                        self.phase = GitTaskPhase::FetchFallback;
                        None
                    }
                    Err(error) => Some(Err(error.to_string())),
                }
            }
            GitTaskPhase::FetchPrune | GitTaskPhase::FetchFallback if status.success() => {
                match spawn_git(
                    &self.repo_path,
                    &["branch", "-r", "--format=%(refname:short)"],
                ) {
                    Ok(child) => {
                        self.child = Some(child);
                        self.phase = GitTaskPhase::ListBranches;
                        None
                    }
                    Err(error) => Some(Err(error.to_string())),
                }
            }
            GitTaskPhase::ListBranches if status.success() => Some(Ok(
                git::parse_origin_branches(&String::from_utf8_lossy(&output.stdout)),
            )),
            GitTaskPhase::FetchPrune => unreachable!(),
            GitTaskPhase::FetchFallback => Some(Err(format!(
                "git fetch origin failed after prune fallback: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))),
            GitTaskPhase::ListBranches => Some(Err(format!(
                "git branch -r failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))),
        }
    }

    fn cancel(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for GitBranchTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn spawn_git(repo_path: &Path, args: &[&str]) -> Result<Child> {
    Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(Into::into)
}

struct App {
    repos: Vec<RepoCandidate>,
    selected: BTreeSet<usize>,
    worktree_selected: BTreeSet<usize>,
    worktree_choices: Vec<WorktreeChoice>,
    cursor: usize,
    filter: String,
    session_name: String,
    message: String,
    mode: Mode,
    branch_fetch: Option<BranchFetch>,
    store: SessionStore,
    purpose: AppPurpose,
    added_repos: Vec<RepoLink>,
}

impl App {
    fn new(repos: Vec<RepoCandidate>, store: SessionStore) -> Self {
        Self {
            repos,
            selected: BTreeSet::new(),
            worktree_selected: BTreeSet::new(),
            worktree_choices: Vec::new(),
            cursor: 0,
            filter: String::new(),
            session_name: String::new(),
            message: String::new(),
            mode: Mode::Select,
            branch_fetch: None,
            store,
            purpose: AppPurpose::CreateSession,
            added_repos: Vec::new(),
        }
    }

    fn for_add(repos: Vec<RepoCandidate>, store: SessionStore, session_name: String) -> Self {
        Self {
            purpose: AppPurpose::AddRepos { session_name },
            ..Self::new(repos, store)
        }
    }

    fn draw(&mut self, frame: &mut ratatui::Frame<'_>) {
        self.poll_branch_fetch();
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
            .block(
                panel_block(&self.panel_title(), self.mode_color()).padding(Padding::horizontal(1)),
            );
        frame.render_widget(header, chunks[0]);

        match &self.mode {
            Mode::BranchLoading => {
                let loading = Paragraph::new(self.branch_loading_lines())
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(theme::FG).bg(theme::BG))
                    .block(panel_block("fetching", theme::BLUE).padding(Padding::horizontal(1)));
                frame.render_widget(loading, chunks[1]);
            }
            Mode::Branch(picker) => {
                let branch_indices = self.filtered_branch_indices(picker);
                if self.cursor >= branch_indices.len() {
                    self.cursor = branch_indices.len().saturating_sub(1);
                }
                let items = branch_indices
                    .iter()
                    .enumerate()
                    .map(|(visible_idx, branch_idx)| {
                        self.branch_item(picker, *branch_idx, visible_idx == self.cursor)
                    })
                    .collect::<Vec<_>>();
                let repo = &self.repos[picker.repo_indices[picker.current]];
                let title = format!(
                    "branches for {} {}/{}",
                    repo.name,
                    picker.current + 1,
                    picker.repo_indices.len()
                );
                let list = List::new(items)
                    .highlight_symbol(">")
                    .block(panel_block(&title, theme::CYAN).padding(Padding::horizontal(1)));
                frame.render_widget(list, chunks[1]);
            }
            _ => {
                let indices = self.filtered_indices();
                if self.cursor >= indices.len() {
                    self.cursor = indices.len().saturating_sub(1);
                }
                let items = indices
                    .iter()
                    .enumerate()
                    .map(|(visible_idx, repo_idx)| {
                        self.repo_item(*repo_idx, visible_idx == self.cursor)
                    })
                    .collect::<Vec<_>>();
                let title = format!("repos {}/{}", indices.len(), self.repos.len());
                let list = List::new(items)
                    .highlight_symbol(">")
                    .block(panel_block(&title, theme::CYAN).padding(Padding::horizontal(1)));
                frame.render_widget(list, chunks[1]);
            }
        }

        let footer = Paragraph::new(self.footer_lines())
            .style(Style::default().fg(theme::FG).bg(theme::BG))
            .block(panel_block("prompt", self.mode_color()).padding(Padding::horizontal(1)));
        frame.render_widget(footer, chunks[2]);
    }

    fn header_line(&self) -> Line<'static> {
        let mode = self.mode_name();
        let heading = match &self.purpose {
            AppPurpose::CreateSession => "dynamic workspace manager".to_string(),
            AppPurpose::AddRepos { session_name } => {
                format!("add repositories to {session_name}")
            }
        };
        Line::from(vec![
            Span::styled(
                format!("{heading}  "),
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
            Span::styled("arrows", key_style()),
            Span::raw(" move  "),
            Span::styled("type", key_style()),
            Span::raw(" search  "),
            Span::styled("ctrl+s", key_style()),
            Span::raw(" select  "),
            Span::styled("ctrl+w", key_style()),
            Span::raw(" worktree  "),
            Span::styled("enter", key_style()),
            Span::raw(" next  "),
            Span::styled("ctrl+c", key_style()),
            Span::raw(if self.is_adding() { " cancel" } else { " quit" }),
        ])
    }

    fn panel_title(&self) -> String {
        match &self.purpose {
            AppPurpose::CreateSession => "dws".to_string(),
            AppPurpose::AddRepos { session_name } => format!("dws manage add · {session_name}"),
        }
    }

    fn is_adding(&self) -> bool {
        matches!(self.purpose, AppPurpose::AddRepos { .. })
    }

    fn repo_item(&self, repo_idx: usize, highlighted: bool) -> ListItem<'static> {
        let repo = &self.repos[repo_idx];
        let is_selected = self.selected.contains(&repo_idx);
        let is_worktree = self.worktree_selected.contains(&repo_idx);
        let marker = if is_selected {
            Span::styled(
                "[s]",
                Style::default()
                    .fg(theme::BG)
                    .bg(theme::GREEN)
                    .add_modifier(Modifier::BOLD),
            )
        } else if is_worktree {
            Span::styled(
                "[w]",
                Style::default()
                    .fg(theme::BG)
                    .bg(theme::PURPLE)
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
        } else if is_selected || is_worktree {
            let color = if is_worktree {
                theme::PURPLE
            } else {
                theme::GREEN
            };
            Style::default().fg(color).bg(theme::BG)
        } else {
            Style::default().fg(theme::FG_DIM).bg(theme::BG)
        };

        let mut spans = vec![
            marker,
            Span::raw(" "),
            Span::styled(
                repo.name.clone(),
                if is_selected || is_worktree {
                    let color = if is_worktree {
                        theme::PURPLE
                    } else {
                        theme::GREEN
                    };
                    Style::default().fg(color).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::FG)
                },
            ),
            Span::raw("  "),
        ];
        if is_worktree {
            spans.extend([
                Span::styled(" worktree ", pill_style(theme::PURPLE)),
                Span::raw(" "),
            ]);
        }
        spans.extend(git_spans);

        ListItem::new(Line::from(spans)).style(style)
    }

    fn branch_item(
        &self,
        picker: &BranchPicker,
        branch_idx: usize,
        highlighted: bool,
    ) -> ListItem<'static> {
        let branch = &picker.branches[branch_idx];
        let style = if highlighted {
            Style::default()
                .fg(theme::FG)
                .bg(theme::BG_HIGHLIGHT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::FG_DIM).bg(theme::BG)
        };
        let line = Line::from(vec![
            Span::styled(" branch ", pill_style(theme::BLUE)),
            Span::raw(" "),
            Span::styled(
                branch.name.clone(),
                Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                branch.remote_ref.clone(),
                Style::default().fg(theme::COMMENT),
            ),
        ]);
        ListItem::new(line).style(style)
    }

    fn footer_lines(&self) -> Vec<Line<'static>> {
        let primary = match &self.mode {
            Mode::Select => Line::from(vec![
                Span::styled("filter ", label_style(theme::CYAN)),
                Span::styled(empty_marker(&self.filter), value_style()),
                Span::raw("   "),
                Span::styled("selected ", label_style(theme::GREEN)),
                Span::styled(self.selected.len().to_string(), value_style()),
                Span::raw("   "),
                Span::styled("worktrees ", label_style(theme::PURPLE)),
                Span::styled(self.worktree_selected.len().to_string(), value_style()),
            ]),
            Mode::BranchLoading => {
                let repo_name = self
                    .branch_fetch
                    .as_ref()
                    .map(|fetch| fetch.repo_name.clone())
                    .unwrap_or_else(|| "repo".to_string());
                Line::from(vec![
                    Span::styled("fetching ", label_style(theme::BLUE)),
                    Span::styled(repo_name, value_style()),
                    Span::raw("   "),
                    Span::styled("command ", label_style(theme::CYAN)),
                    Span::styled(
                        "git fetch origin --prune",
                        Style::default().fg(theme::FG_DIM),
                    ),
                ])
            }
            Mode::Branch(picker) => Line::from(vec![
                Span::styled("branch filter ", label_style(theme::CYAN)),
                Span::styled(empty_marker(&picker.filter), value_style()),
                Span::raw("   "),
                Span::styled("repo ", label_style(theme::PURPLE)),
                Span::styled(
                    self.repos[picker.repo_indices[picker.current]].name.clone(),
                    value_style(),
                ),
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
        };

        vec![primary, self.message_line(), self.help_line()]
    }

    fn help_line(&self) -> Line<'static> {
        match self.mode {
            Mode::Select => Line::from(vec![
                Span::styled("ctrl+s", key_style()),
                Span::raw(" select  "),
                Span::styled("ctrl+w", key_style()),
                Span::raw(" worktree  "),
                Span::styled("backspace", key_style()),
                Span::raw(" edit filter  "),
                Span::styled("esc", key_style()),
                Span::raw(" clear filter"),
            ]),
            Mode::BranchLoading => Line::from(vec![
                Span::styled("esc", key_style()),
                Span::raw(" return to repositories"),
            ]),
            Mode::Branch(_) => Line::from(vec![
                Span::styled("ctrl+s", key_style()),
                Span::raw(" choose branch  "),
                Span::styled("backspace", key_style()),
                Span::raw(" edit filter  "),
                Span::styled("esc", key_style()),
                Span::raw(" return to repos"),
            ]),
            Mode::Name | Mode::Duplicate { .. } => Line::from(vec![
                Span::styled("esc", key_style()),
                Span::raw(" returns to selection or clears the current prompt"),
            ]),
        }
    }

    fn branch_loading_lines(&mut self) -> Vec<Line<'static>> {
        let (repo_name, frame) = match self.branch_fetch.as_mut() {
            Some(fetch) => {
                let frame = fetch.frame;
                fetch.frame = fetch.frame.wrapping_add(1);
                (fetch.repo_name.clone(), frame)
            }
            None => ("repo".to_string(), 0),
        };
        let spinner = FETCH_SPINNER[frame % FETCH_SPINNER.len()];
        let wave = FETCH_WAVE[frame % FETCH_WAVE.len()];

        vec![
            Line::from(""),
            Line::from(Span::styled(
                "        ________  ___  _________  ___  ___  ___  ___  ________",
                Style::default()
                    .fg(theme::BLUE)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "       |\\   ____\\|\\  \\|\\___   ___\\\\  \\|\\  \\|\\  \\|\\  \\|\\   __  \\",
                Style::default()
                    .fg(theme::BLUE)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "       \\ \\  \\___|\\ \\  \\|___ \\  \\_\\ \\  \\\\\\  \\ \\  \\\\\\  \\ \\  \\|\\ /_",
                Style::default()
                    .fg(theme::CYAN)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "        \\ \\  \\  __\\ \\  \\   \\ \\  \\ \\ \\   __  \\ \\  \\\\\\  \\ \\   __  \\",
                Style::default()
                    .fg(theme::CYAN)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "         \\ \\  \\|\\  \\ \\  \\   \\ \\  \\ \\ \\  \\ \\  \\ \\  \\\\\\  \\ \\  \\|\\  \\",
                Style::default()
                    .fg(theme::PURPLE)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "          \\ \\_______\\ \\__\\   \\ \\__\\ \\ \\__\\ \\__\\ \\_______\\ \\_______\\",
                Style::default()
                    .fg(theme::PURPLE)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "           \\|_______|\\|__|    \\|__|  \\|__|\\|__|\\|_______|\\|_______|",
                Style::default()
                    .fg(theme::PURPLE)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "             .------------------------------------------------.",
                Style::default().fg(theme::COMMENT),
            )),
            Line::from(Span::styled(
                format!("             |  {wave}  ORIGIN BRANCH RADAR  {wave}  |"),
                Style::default()
                    .fg(theme::YELLOW)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "             '------------------------------------------------'",
                Style::default().fg(theme::COMMENT),
            )),
            Line::from(vec![
                Span::styled(format!(" {spinner} "), pill_style(theme::YELLOW)),
                Span::raw(" scanning remote refs for "),
                Span::styled(repo_name, value_style()),
            ]),
            Line::from(Span::styled(
                "git fetch origin --prune  (falls back to git fetch origin on prune locks)",
                Style::default().fg(theme::COMMENT),
            )),
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
            Mode::BranchLoading => "fetch",
            Mode::Branch(_) => "branch",
            Mode::Name => "name",
            Mode::Duplicate { .. } => "duplicate",
        }
    }

    fn mode_color(&self) -> Color {
        match self.mode {
            Mode::Select => theme::CYAN,
            Mode::BranchLoading => theme::BLUE,
            Mode::Branch(_) => theme::BLUE,
            Mode::Name => theme::PURPLE,
            Mode::Duplicate { .. } => theme::YELLOW,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<AppSignal> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(AppSignal::Done(None));
        }

        match self.mode.clone() {
            Mode::Select => self.handle_select_key(key),
            Mode::BranchLoading => self.handle_branch_loading_key(key),
            Mode::Branch(picker) => self.handle_branch_key(key, picker),
            Mode::Name => self.handle_name_key(key),
            Mode::Duplicate { existing } => self.handle_duplicate_key(key, existing),
        }
    }

    fn handle_select_key(&mut self, key: KeyEvent) -> Result<AppSignal> {
        match key.code {
            KeyCode::Down => self.move_cursor(1),
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_current()
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_current_worktree()
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.cursor = 0;
            }
            KeyCode::Enter => {
                if self.selected.is_empty() && self.worktree_selected.is_empty() {
                    self.message = "select at least one repo".to_string();
                } else if !self.worktree_selected.is_empty() {
                    self.start_branch_selection();
                } else if self.is_adding() {
                    return self.try_add();
                } else {
                    self.mode = Mode::Name;
                    self.message.clear();
                }
            }
            KeyCode::Esc => {
                self.filter.clear();
                self.cursor = 0;
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.push(character);
                self.cursor = 0;
            }
            _ => {}
        }

        Ok(AppSignal::Continue)
    }

    fn handle_branch_loading_key(&mut self, key: KeyEvent) -> Result<AppSignal> {
        if key.code == KeyCode::Esc {
            self.branch_fetch = None;
            self.mode = Mode::Select;
            self.cursor = 0;
            self.message = "branch fetch cancelled".to_string();
        }
        Ok(AppSignal::Continue)
    }

    fn poll_branch_fetch(&mut self) {
        if !matches!(self.mode, Mode::BranchLoading) {
            return;
        }

        let Some(result) = self
            .branch_fetch
            .as_mut()
            .and_then(|fetch| fetch.task.poll())
        else {
            return;
        };

        let Some(fetch) = self.branch_fetch.take() else {
            return;
        };

        match result {
            Ok(branches) if branches.is_empty() => {
                self.mode = Mode::Select;
                self.cursor = 0;
                self.message = format!("{} has no origin branches", fetch.repo_name);
            }
            Ok(branches) => {
                self.mode = Mode::Branch(BranchPicker {
                    repo_indices: fetch.repo_indices,
                    current: fetch.current,
                    branches,
                    filter: String::new(),
                });
                self.cursor = 0;
                self.message.clear();
            }
            Err(error) => {
                self.mode = Mode::Select;
                self.cursor = 0;
                self.message = error;
            }
        }
    }

    fn handle_branch_key(&mut self, key: KeyEvent, mut picker: BranchPicker) -> Result<AppSignal> {
        match key.code {
            KeyCode::Down => self.move_branch_cursor(&picker, 1),
            KeyCode::Up => self.move_branch_cursor(&picker, -1),
            KeyCode::Esc => {
                self.mode = Mode::Select;
                self.cursor = 0;
                return Ok(AppSignal::Continue);
            }
            KeyCode::Backspace => {
                picker.filter.pop();
                self.cursor = 0;
                self.mode = Mode::Branch(picker);
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.select_current_branch(picker);
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                picker.filter.push(character);
                self.cursor = 0;
                self.mode = Mode::Branch(picker);
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

    fn move_branch_cursor(&mut self, picker: &BranchPicker, delta: isize) {
        let len = self.filtered_branch_indices(picker).len();
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
        } else {
            self.worktree_selected.remove(&repo_idx);
            self.worktree_choices
                .retain(|choice| choice.repo_idx != repo_idx);
        }
    }

    fn toggle_current_worktree(&mut self) {
        let Some(repo_idx) = self.current_repo_idx() else {
            return;
        };
        if self.repos[repo_idx].git.is_none() {
            self.message = "highlighted folder is not a git repository".to_string();
            return;
        }
        if !self.worktree_selected.insert(repo_idx) {
            self.worktree_selected.remove(&repo_idx);
            self.worktree_choices
                .retain(|choice| choice.repo_idx != repo_idx);
        } else {
            self.selected.remove(&repo_idx);
        }
        self.message.clear();
    }

    fn start_branch_selection(&mut self) {
        self.worktree_choices.clear();
        let repo_indices = self.worktree_selected.iter().copied().collect::<Vec<_>>();
        self.start_branch_fetch(repo_indices, 0);
    }

    fn start_branch_fetch(&mut self, repo_indices: Vec<usize>, current: usize) {
        let repo_idx = repo_indices[current];
        let repo_name = self.repos[repo_idx].name.clone();
        let repo_path = self.repos[repo_idx].path.clone();
        let task = match GitBranchTask::start(repo_path) {
            Ok(task) => task,
            Err(error) => {
                self.mode = Mode::Select;
                self.message = format!("failed to start origin fetch: {error:#}");
                return;
            }
        };

        self.branch_fetch = Some(BranchFetch {
            repo_indices,
            current,
            repo_name,
            task,
            frame: 0,
        });
        self.mode = Mode::BranchLoading;
        self.cursor = 0;
        self.message.clear();
    }

    fn select_current_branch(&mut self, picker: BranchPicker) -> Result<AppSignal> {
        let Some(branch_idx) = self.current_branch_idx(&picker) else {
            self.message = "no matching branch".to_string();
            self.mode = Mode::Branch(picker);
            return Ok(AppSignal::Continue);
        };
        let repo_idx = picker.repo_indices[picker.current];
        self.worktree_choices.push(WorktreeChoice {
            repo_idx,
            branch: picker.branches[branch_idx].clone(),
        });

        let next = picker.current + 1;
        if next >= picker.repo_indices.len() {
            if self.is_adding() {
                return self.try_add();
            }
            self.mode = Mode::Name;
            self.cursor = 0;
            self.message.clear();
            return Ok(AppSignal::Continue);
        }

        self.start_branch_fetch(picker.repo_indices, next);
        Ok(AppSignal::Continue)
    }

    fn try_create(&mut self, allow_duplicate: bool) -> Result<AppSignal> {
        if self.session_name.trim().is_empty() {
            self.message = "session name cannot be empty".to_string();
            return Ok(AppSignal::Continue);
        }

        let materialized = self.materialize_selections()?;
        let selected = &materialized.selections;
        if !allow_duplicate {
            let selected_paths = selected
                .iter()
                .map(|selection| selection.path.clone())
                .collect::<Vec<_>>();
            if let Some(existing) = self.store.find_duplicate(&selected_paths)? {
                let cleanup = rollback_created_worktrees(&materialized.created_worktrees);
                self.mode = Mode::Duplicate { existing };
                if let Some(cleanup) = cleanup {
                    self.message = cleanup;
                }
                return Ok(AppSignal::Continue);
            }
        }

        match self.store.create_session_with_links(
            &self.session_name,
            selected,
            None,
            allow_duplicate,
        ) {
            Ok(outcome) => Ok(AppSignal::Done(Some(outcome.into_metadata()))),
            Err(error) => {
                let cleanup = rollback_created_worktrees(&materialized.created_worktrees);
                self.message = error.to_string();
                if let Some(cleanup) = cleanup {
                    self.message = format!("{}; {cleanup}", self.message);
                }
                Ok(AppSignal::Continue)
            }
        }
    }

    fn try_add(&mut self) -> Result<AppSignal> {
        let AppPurpose::AddRepos { session_name } = &self.purpose else {
            return Ok(AppSignal::Continue);
        };
        let session_name = session_name.clone();
        let materialized = match self.materialize_selections() {
            Ok(materialized) => materialized,
            Err(error) => {
                self.mode = Mode::Select;
                self.worktree_choices.clear();
                self.message = error.to_string();
                return Ok(AppSignal::Continue);
            }
        };

        match self
            .store
            .add_repo_links(&session_name, &materialized.selections)
        {
            Ok(outcome) => {
                self.added_repos = outcome.repos;
                Ok(AppSignal::Done(Some(outcome.session)))
            }
            Err(error) => {
                let cleanup = rollback_created_worktrees(&materialized.created_worktrees);
                self.mode = Mode::Select;
                self.worktree_choices.clear();
                self.message = match cleanup {
                    Some(cleanup) => format!("{error:#}; {cleanup}"),
                    None => error.to_string(),
                };
                Ok(AppSignal::Continue)
            }
        }
    }

    fn materialize_selections(&self) -> Result<MaterializedSelections> {
        let mut selections = Vec::new();
        let mut created_worktrees = Vec::new();
        for idx in &self.selected {
            let repo = &self.repos[*idx];
            selections.push(RepoSelection::link(repo.name.clone(), repo.path.clone()));
        }
        for choice in &self.worktree_choices {
            let repo = &self.repos[choice.repo_idx];
            let outcome = match git::ensure_worktree_from_origin(
                &repo.path,
                &self.store.paths().worktrees_dir,
                &choice.branch,
            ) {
                Ok(outcome) => outcome,
                Err(error) => {
                    let cleanup = rollback_created_worktrees(&created_worktrees);
                    return match cleanup {
                        Some(cleanup) => Err(anyhow::anyhow!("{error:#}; {cleanup}")),
                        None => Err(error),
                    };
                }
            };
            if outcome.created {
                created_worktrees.push(outcome.path.clone());
            }
            selections.push(RepoSelection::worktree(
                repo.name.clone(),
                outcome.path,
                outcome.source_repo,
                outcome.remote_ref,
                outcome.local_branch,
            ));
        }
        Ok(MaterializedSelections {
            selections,
            created_worktrees,
        })
    }

    fn current_repo_idx(&self) -> Option<usize> {
        self.filtered_indices().get(self.cursor).copied()
    }

    fn current_branch_idx(&self, picker: &BranchPicker) -> Option<usize> {
        self.filtered_branch_indices(picker)
            .get(self.cursor)
            .copied()
    }

    fn filtered_indices(&self) -> Vec<usize> {
        self.repos
            .iter()
            .enumerate()
            .filter_map(|(idx, repo)| fuzzy_matches(&self.filter, repo).then_some(idx))
            .collect()
    }

    fn filtered_branch_indices(&self, picker: &BranchPicker) -> Vec<usize> {
        picker
            .branches
            .iter()
            .enumerate()
            .filter_map(|(idx, branch)| {
                fuzzy_text_matches(
                    &picker.filter,
                    &format!("{} {}", branch.name, branch.remote_ref),
                )
                .then_some(idx)
            })
            .collect()
    }
}

enum SessionManagerSignal {
    Continue,
    AddRepos {
        session_name: String,
        repo_cursor: usize,
    },
    Done,
}

#[derive(Debug, Clone)]
enum SessionManagerMode {
    Browse,
    Rename {
        session_name: String,
        input: String,
    },
    ConfirmRemoveSessions {
        session_names: Vec<String>,
    },
    Expanded {
        session_name: String,
        repo_cursor: usize,
    },
    ConfirmRemoveRepo {
        session_name: String,
        repo_name: String,
        repo_cursor: usize,
    },
}

struct SessionManagerApp {
    sessions: Vec<SessionMetadata>,
    store: SessionStore,
    selected: BTreeSet<String>,
    cursor: usize,
    filter: String,
    mode: SessionManagerMode,
    message: String,
    changes: Vec<String>,
}

impl SessionManagerApp {
    fn new(sessions: Vec<SessionMetadata>, store: SessionStore) -> Self {
        Self {
            sessions,
            store,
            selected: BTreeSet::new(),
            cursor: 0,
            filter: String::new(),
            mode: SessionManagerMode::Browse,
            message: String::new(),
            changes: Vec::new(),
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
                Constraint::Length(7),
            ])
            .split(area);

        let mode_color = self.mode_color();
        let header = Paragraph::new(self.header_line())
            .alignment(Alignment::Center)
            .style(Style::default().fg(theme::FG).bg(theme::BG))
            .block(panel_block("dws manage", mode_color).padding(Padding::horizontal(1)));
        frame.render_widget(header, chunks[0]);

        match self.expanded_session_name() {
            Some(session_name) => self.draw_repo_list(frame, chunks[1], &session_name),
            None => self.draw_session_list(frame, chunks[1]),
        }

        let footer = Paragraph::new(self.footer_lines())
            .style(Style::default().fg(theme::FG).bg(theme::BG))
            .block(panel_block("manage", mode_color).padding(Padding::horizontal(1)));
        frame.render_widget(footer, chunks[2]);
    }

    fn draw_session_list(&mut self, frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect) {
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
        let title = format!("sessions {}/{}", indices.len(), self.sessions.len());
        let list = List::new(items)
            .highlight_symbol(">")
            .block(panel_block(&title, theme::CYAN).padding(Padding::horizontal(1)));
        frame.render_widget(list, area);
    }

    fn draw_repo_list(
        &mut self,
        frame: &mut ratatui::Frame<'_>,
        area: ratatui::layout::Rect,
        session_name: &str,
    ) {
        let session = self
            .sessions
            .iter()
            .find(|session| session.name == session_name)
            .cloned();
        let repo_cursor = self.repo_cursor();
        let items = match session {
            Some(session) if !session.repos.is_empty() => session
                .repos
                .iter()
                .enumerate()
                .map(|(idx, repo)| self.repo_link_item(repo, idx == repo_cursor))
                .collect::<Vec<_>>(),
            Some(_) => vec![ListItem::new(Line::from(Span::styled(
                "no linked repos",
                Style::default().fg(theme::COMMENT),
            )))],
            None => vec![ListItem::new(Line::from(Span::styled(
                "session not found",
                Style::default().fg(theme::YELLOW),
            )))],
        };

        let repo_count = self
            .sessions
            .iter()
            .find(|session| session.name == session_name)
            .map(|session| session.repos.len())
            .unwrap_or(0);
        let title = format!("{session_name} repos {repo_count}");
        let list = List::new(items)
            .highlight_symbol(">")
            .block(panel_block(&title, theme::CYAN).padding(Padding::horizontal(1)));
        frame.render_widget(list, area);
    }

    fn header_line(&self) -> Line<'static> {
        Line::from(vec![
            Span::styled(
                "manage dynamic workspaces  ",
                Style::default()
                    .fg(theme::PURPLE)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("[{}]  ", self.mode_name()),
                Style::default()
                    .fg(theme::BG)
                    .bg(self.mode_color())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("arrows", key_style()),
            Span::raw(" move  "),
            Span::styled("type", key_style()),
            Span::raw(" search  "),
            Span::styled("ctrl+s", key_style()),
            Span::raw(" select  "),
            Span::styled("ctrl+e", key_style()),
            Span::raw(" edit  "),
            Span::styled("right", key_style()),
            Span::raw(" expand  "),
            Span::styled("ctrl+a", key_style()),
            Span::raw(" add  "),
            Span::styled("ctrl+d", key_style()),
            Span::raw(" remove  "),
            Span::styled("ctrl+c", key_style()),
            Span::raw(" quit"),
        ])
    }

    fn session_item(&self, session_idx: usize, highlighted: bool) -> ListItem<'static> {
        let session = &self.sessions[session_idx];
        let repo_count = session.repos.len();
        let is_selected = self.selected.contains(&session.name);
        let marker = if is_selected {
            Span::styled(
                "[s]",
                Style::default()
                    .fg(theme::BG)
                    .bg(theme::GREEN)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled("[ ]", Style::default().fg(theme::COMMENT))
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
            Span::styled(" ws ", pill_style(theme::BLUE)),
            Span::raw(" "),
            Span::styled(
                session.name.clone(),
                if is_selected {
                    Style::default()
                        .fg(theme::GREEN)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::FG).add_modifier(Modifier::BOLD)
                },
            ),
            Span::raw("  "),
            Span::styled(format!(" {repo_count} repos "), pill_style(theme::CYAN)),
            Span::raw("  "),
            Span::styled(">", Style::default().fg(theme::COMMENT)),
        ];
        if let Some(description) = &session.description {
            spans.extend([
                Span::raw("  "),
                Span::styled(description.clone(), Style::default().fg(theme::COMMENT)),
            ]);
        }

        ListItem::new(Line::from(spans)).style(style)
    }

    fn repo_link_item(&self, repo: &RepoLink, highlighted: bool) -> ListItem<'static> {
        let status = self.repo_link_status(repo);
        let status_span = match status {
            RepoLinkStatus::Normal => Span::styled(" repo ", pill_style(theme::GREEN)),
            RepoLinkStatus::DwsWorktree => Span::styled(" worktree ", pill_style(theme::PURPLE)),
            RepoLinkStatus::Missing => Span::styled(" missing ", pill_style(theme::YELLOW)),
        };
        let style = if highlighted {
            Style::default()
                .fg(theme::FG)
                .bg(theme::BG_HIGHLIGHT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::FG_DIM).bg(theme::BG)
        };
        ListItem::new(Line::from(vec![
            status_span,
            Span::raw(" "),
            Span::styled(
                repo.name.clone(),
                Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(repo.path.clone(), Style::default().fg(theme::COMMENT)),
        ]))
        .style(style)
    }

    fn footer_lines(&self) -> Vec<Line<'static>> {
        let display_session = self.display_session();
        let path = display_session
            .map(|session| {
                self.store
                    .workspace_path(&session.name)
                    .display()
                    .to_string()
            })
            .unwrap_or_else(|| "<none>".to_string());
        let repo_preview = display_session
            .map(session_repo_preview)
            .unwrap_or_else(|| "no matching session".to_string());
        let selected_count = self.selected.len().to_string();
        let prompt = match &self.mode {
            SessionManagerMode::Browse => Line::from(vec![
                Span::styled("filter ", label_style(theme::CYAN)),
                Span::styled(empty_marker(&self.filter), value_style()),
                Span::raw("   "),
                Span::styled("selected ", label_style(theme::GREEN)),
                Span::styled(selected_count, value_style()),
            ]),
            SessionManagerMode::Rename { input, .. } => Line::from(vec![
                Span::styled("new name ", label_style(theme::PURPLE)),
                Span::styled(empty_marker(input), value_style()),
            ]),
            SessionManagerMode::ConfirmRemoveSessions { session_names } => Line::from(vec![
                Span::styled("remove ", label_style(theme::YELLOW)),
                Span::styled(format!("{} session(s)", session_names.len()), value_style()),
                Span::raw("  "),
                Span::styled("y", key_style()),
                Span::raw(" confirm  "),
                Span::styled("n/esc", key_style()),
                Span::raw(" cancel"),
            ]),
            SessionManagerMode::Expanded { session_name, .. } => Line::from(vec![
                Span::styled("expanded ", label_style(theme::CYAN)),
                Span::styled(session_name.clone(), value_style()),
                Span::raw("   "),
                Span::styled("selected sessions ", label_style(theme::GREEN)),
                Span::styled(selected_count, value_style()),
            ]),
            SessionManagerMode::ConfirmRemoveRepo {
                session_name,
                repo_name,
                ..
            } => Line::from(vec![
                Span::styled("remove repo ", label_style(theme::YELLOW)),
                Span::styled(repo_name.clone(), value_style()),
                Span::raw(" from "),
                Span::styled(session_name.clone(), value_style()),
                Span::raw("?  "),
                Span::styled("y", key_style()),
                Span::raw(" confirm  "),
                Span::styled("n/esc", key_style()),
                Span::raw(" cancel"),
            ]),
        };

        vec![
            prompt,
            Line::from(vec![
                Span::styled("workspace ", label_style(theme::PURPLE)),
                Span::styled(path, Style::default().fg(theme::FG)),
            ]),
            Line::from(vec![
                Span::styled("repos ", label_style(theme::GREEN)),
                Span::styled(repo_preview, Style::default().fg(theme::FG_DIM)),
            ]),
            self.message_line(),
            self.help_line(),
        ]
    }

    fn message_line(&self) -> Line<'static> {
        if self.message.is_empty() {
            return Line::from(Span::styled("ready", Style::default().fg(theme::COMMENT)));
        }

        Line::from(Span::styled(
            self.message.clone(),
            Style::default()
                .fg(theme::YELLOW)
                .add_modifier(Modifier::BOLD),
        ))
    }

    fn help_line(&self) -> Line<'static> {
        match self.mode {
            SessionManagerMode::Browse => Line::from(vec![
                Span::styled("ctrl+s", key_style()),
                Span::raw(" select highlighted session  "),
                Span::styled("ctrl+e", key_style()),
                Span::raw(" rename  "),
                Span::styled("right", key_style()),
                Span::raw(" expand"),
            ]),
            SessionManagerMode::Expanded { .. } => Line::from(vec![
                Span::styled("left/esc", key_style()),
                Span::raw(" collapse  "),
                Span::styled("ctrl+a", key_style()),
                Span::raw(" add repos  "),
                Span::styled("ctrl+d", key_style()),
                Span::raw(" remove repo link"),
            ]),
            SessionManagerMode::Rename { .. } => Line::from(vec![
                Span::styled("enter", key_style()),
                Span::raw(" save rename  "),
                Span::styled("esc", key_style()),
                Span::raw(" cancel  "),
                Span::styled("backspace", key_style()),
                Span::raw(" edits name"),
            ]),
            SessionManagerMode::ConfirmRemoveSessions { .. } => Line::from(vec![
                Span::styled("y", key_style()),
                Span::raw(" deletes metadata and workspace folders; linked repos stay untouched"),
            ]),
            SessionManagerMode::ConfirmRemoveRepo { .. } => Line::from(vec![
                Span::styled("y", key_style()),
                Span::raw(" removes repo link and dws-created worktree when applicable"),
            ]),
        }
    }

    fn mode_name(&self) -> &'static str {
        match self.mode {
            SessionManagerMode::Browse => "browse",
            SessionManagerMode::Rename { .. } => "edit",
            SessionManagerMode::ConfirmRemoveSessions { .. } => "remove",
            SessionManagerMode::Expanded { .. } => "repos",
            SessionManagerMode::ConfirmRemoveRepo { .. } => "remove repo",
        }
    }

    fn mode_color(&self) -> Color {
        match self.mode {
            SessionManagerMode::Browse => theme::BLUE,
            SessionManagerMode::Rename { .. } => theme::PURPLE,
            SessionManagerMode::ConfirmRemoveSessions { .. } => theme::YELLOW,
            SessionManagerMode::Expanded { .. } => theme::CYAN,
            SessionManagerMode::ConfirmRemoveRepo { .. } => theme::YELLOW,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> SessionManagerSignal {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return SessionManagerSignal::Done;
        }

        match self.mode.clone() {
            SessionManagerMode::Browse => self.handle_browse_key(key),
            SessionManagerMode::Rename {
                session_name,
                input,
            } => self.handle_rename_key(key, session_name, input),
            SessionManagerMode::ConfirmRemoveSessions { session_names } => {
                self.handle_remove_sessions_key(key, session_names)
            }
            SessionManagerMode::Expanded {
                session_name,
                repo_cursor,
            } => self.handle_expanded_key(key, session_name, repo_cursor),
            SessionManagerMode::ConfirmRemoveRepo {
                session_name,
                repo_name,
                repo_cursor,
            } => self.handle_remove_repo_key(key, session_name, repo_name, repo_cursor),
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent) -> SessionManagerSignal {
        match key.code {
            KeyCode::Down => self.move_cursor(1),
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_current_session();
            }
            KeyCode::Enter | KeyCode::Right => self.expand_current_session(),
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.start_rename();
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.start_remove();
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.cursor = 0;
            }
            KeyCode::Esc => {
                self.filter.clear();
                self.cursor = 0;
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.push(character);
                self.cursor = 0;
            }
            _ => {}
        }
        SessionManagerSignal::Continue
    }

    fn handle_expanded_key(
        &mut self,
        key: KeyEvent,
        session_name: String,
        mut repo_cursor: usize,
    ) -> SessionManagerSignal {
        match key.code {
            KeyCode::Left | KeyCode::Esc => {
                self.mode = SessionManagerMode::Browse;
                self.message.clear();
            }
            KeyCode::Down => {
                repo_cursor = self.move_repo_cursor(&session_name, repo_cursor, 1);
                self.mode = SessionManagerMode::Expanded {
                    session_name,
                    repo_cursor,
                };
            }
            KeyCode::Up => {
                repo_cursor = self.move_repo_cursor(&session_name, repo_cursor, -1);
                self.mode = SessionManagerMode::Expanded {
                    session_name,
                    repo_cursor,
                };
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.start_remove_repo(session_name, repo_cursor);
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return SessionManagerSignal::AddRepos {
                    session_name,
                    repo_cursor,
                };
            }
            _ => {
                self.mode = SessionManagerMode::Expanded {
                    session_name,
                    repo_cursor,
                };
            }
        }
        SessionManagerSignal::Continue
    }

    fn handle_rename_key(
        &mut self,
        key: KeyEvent,
        session_name: String,
        mut input: String,
    ) -> SessionManagerSignal {
        match key.code {
            KeyCode::Esc => {
                self.mode = SessionManagerMode::Browse;
                self.message = "rename cancelled".to_string();
            }
            KeyCode::Backspace => {
                input.pop();
                self.mode = SessionManagerMode::Rename {
                    session_name,
                    input,
                };
            }
            KeyCode::Enter => {
                if input.trim().is_empty() {
                    self.message = "session name cannot be empty".to_string();
                    self.mode = SessionManagerMode::Rename {
                        session_name,
                        input,
                    };
                    return SessionManagerSignal::Continue;
                }

                match self.store.rename_session(&session_name, &input) {
                    Ok(metadata) => {
                        let new_name = metadata.name.clone();
                        self.replace_session(&session_name, metadata);
                        self.selected.remove(&session_name);
                        self.selected.insert(new_name.clone());
                        self.filter.clear();
                        self.set_cursor_to_name(&new_name);
                        self.message = format!("renamed {session_name} -> {new_name}");
                        self.changes
                            .push(format!("renamed session {session_name} -> {new_name}"));
                        self.mode = SessionManagerMode::Browse;
                    }
                    Err(error) => {
                        self.message = error.to_string();
                        self.mode = SessionManagerMode::Rename {
                            session_name,
                            input,
                        };
                    }
                }
            }
            KeyCode::Char(character) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL) {
                    input.push(character);
                    self.mode = SessionManagerMode::Rename {
                        session_name,
                        input,
                    };
                }
            }
            _ => {
                self.mode = SessionManagerMode::Rename {
                    session_name,
                    input,
                };
            }
        }
        SessionManagerSignal::Continue
    }

    fn handle_remove_sessions_key(
        &mut self,
        key: KeyEvent,
        session_names: Vec<String>,
    ) -> SessionManagerSignal {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n') => {
                self.mode = SessionManagerMode::Browse;
                self.message = "remove cancelled".to_string();
            }
            KeyCode::Char('y') => {
                let mut removed_count = 0;
                for session_name in &session_names {
                    match self.store.remove_session(session_name) {
                        Ok(removed) => {
                            removed_count += 1;
                            self.sessions.retain(|session| session.name != removed.name);
                            self.selected.remove(&removed.name);
                            self.changes
                                .push(format!("removed session {}", removed.name));
                        }
                        Err(error) => {
                            self.message = error.to_string();
                            self.mode = SessionManagerMode::Browse;
                            return SessionManagerSignal::Continue;
                        }
                    }
                }

                let len = self.filtered_indices().len();
                if self.cursor >= len {
                    self.cursor = len.saturating_sub(1);
                }
                self.message = format!("removed {removed_count} session(s)");
                self.mode = SessionManagerMode::Browse;
            }
            _ => {
                self.mode = SessionManagerMode::ConfirmRemoveSessions { session_names };
            }
        }
        SessionManagerSignal::Continue
    }

    fn handle_remove_repo_key(
        &mut self,
        key: KeyEvent,
        session_name: String,
        repo_name: String,
        repo_cursor: usize,
    ) -> SessionManagerSignal {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n') => {
                self.mode = SessionManagerMode::Expanded {
                    session_name,
                    repo_cursor,
                };
                self.message = "remove repo cancelled".to_string();
            }
            KeyCode::Char('y') => match self.store.remove_repo_link(&session_name, &repo_name) {
                Ok(outcome) => {
                    let updated_name = outcome.session.name.clone();
                    self.replace_session(&session_name, outcome.session);
                    let repo_count = self
                        .sessions
                        .iter()
                        .find(|session| session.name == updated_name)
                        .map(|session| session.repos.len())
                        .unwrap_or(0);
                    let next_cursor = repo_cursor.min(repo_count.saturating_sub(1));
                    let worktree_note = if outcome.removed_worktree {
                        " and dws worktree"
                    } else {
                        ""
                    };
                    self.message =
                        format!("removed repo link {}{}", outcome.repo.name, worktree_note);
                    if let Some(warning) = outcome.warning {
                        self.message = format!("{}; {warning}", self.message);
                    }
                    self.changes.push(format!(
                        "removed repo link {} from {}",
                        outcome.repo.name, updated_name
                    ));
                    self.mode = SessionManagerMode::Expanded {
                        session_name: updated_name,
                        repo_cursor: next_cursor,
                    };
                }
                Err(error) => {
                    self.message = error.to_string();
                    self.mode = SessionManagerMode::Expanded {
                        session_name,
                        repo_cursor,
                    };
                }
            },
            _ => {
                self.mode = SessionManagerMode::ConfirmRemoveRepo {
                    session_name,
                    repo_name,
                    repo_cursor,
                };
            }
        }
        SessionManagerSignal::Continue
    }

    fn start_rename(&mut self) {
        let session_names = self.target_session_names();
        if session_names.len() > 1 {
            self.message = "rename supports one session at a time".to_string();
            return;
        }
        let Some(session_name) = session_names.into_iter().next() else {
            self.message = "no matching session".to_string();
            return;
        };
        self.mode = SessionManagerMode::Rename {
            session_name: session_name.clone(),
            input: session_name,
        };
        self.message.clear();
    }

    fn start_remove(&mut self) {
        let session_names = self.target_session_names();
        if session_names.is_empty() {
            self.message = "no matching session".to_string();
            return;
        }
        self.mode = SessionManagerMode::ConfirmRemoveSessions { session_names };
        self.message.clear();
    }

    fn toggle_current_session(&mut self) {
        let Some(session) = self.current_session() else {
            self.message = "no matching session".to_string();
            return;
        };
        let session_name = session.name.clone();
        if self.selected.contains(&session_name) {
            self.selected.remove(&session_name);
        } else {
            self.selected.insert(session_name);
        }
        self.message.clear();
    }

    fn expand_current_session(&mut self) {
        let Some(session) = self.current_session() else {
            self.message = "no matching session".to_string();
            return;
        };
        self.mode = SessionManagerMode::Expanded {
            session_name: session.name.clone(),
            repo_cursor: 0,
        };
        self.message.clear();
    }

    fn start_remove_repo(&mut self, session_name: String, repo_cursor: usize) {
        let Some(repo) = self.current_repo_link(&session_name, repo_cursor) else {
            self.message = "no linked repo to remove".to_string();
            self.mode = SessionManagerMode::Expanded {
                session_name,
                repo_cursor,
            };
            return;
        };
        self.mode = SessionManagerMode::ConfirmRemoveRepo {
            session_name,
            repo_name: repo.name,
            repo_cursor,
        };
        self.message.clear();
    }

    fn finish_repo_add(
        &mut self,
        session_name: &str,
        updated: SessionMetadata,
        added: &[RepoLink],
    ) {
        let updated_name = updated.name.clone();
        let first_added_name = added.first().map(|repo| repo.name.clone());
        self.replace_session(session_name, updated);
        let repo_cursor = first_added_name
            .as_deref()
            .and_then(|name| {
                self.sessions
                    .iter()
                    .find(|session| session.name == updated_name)
                    .and_then(|session| session.repos.iter().position(|repo| repo.name == name))
            })
            .unwrap_or(0);
        let count = added.len();
        self.message = format!("added {count} repo link(s) to {updated_name}");
        self.changes.push(self.message.clone());
        self.mode = SessionManagerMode::Expanded {
            session_name: updated_name,
            repo_cursor,
        };
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

    fn active_session(&self) -> Option<&SessionMetadata> {
        match &self.mode {
            SessionManagerMode::Expanded { session_name, .. }
            | SessionManagerMode::ConfirmRemoveRepo { session_name, .. } => self
                .sessions
                .iter()
                .find(|session| session.name == *session_name),
            _ => None,
        }
    }

    fn display_session(&self) -> Option<&SessionMetadata> {
        self.active_session().or_else(|| self.current_session())
    }

    fn target_session_names(&self) -> Vec<String> {
        if !self.selected.is_empty() {
            return self
                .selected
                .iter()
                .filter(|name| self.sessions.iter().any(|session| session.name == **name))
                .cloned()
                .collect();
        }

        self.current_session()
            .map(|session| vec![session.name.clone()])
            .unwrap_or_default()
    }

    fn expanded_session_name(&self) -> Option<String> {
        match &self.mode {
            SessionManagerMode::Expanded { session_name, .. }
            | SessionManagerMode::ConfirmRemoveRepo { session_name, .. } => {
                Some(session_name.clone())
            }
            _ => None,
        }
    }

    fn repo_cursor(&self) -> usize {
        match self.mode {
            SessionManagerMode::Expanded { repo_cursor, .. }
            | SessionManagerMode::ConfirmRemoveRepo { repo_cursor, .. } => repo_cursor,
            _ => 0,
        }
    }

    fn move_repo_cursor(&self, session_name: &str, cursor: usize, delta: isize) -> usize {
        let len = self
            .sessions
            .iter()
            .find(|session| session.name == session_name)
            .map(|session| session.repos.len())
            .unwrap_or(0);
        if len == 0 {
            return 0;
        }

        if delta < 0 {
            cursor.saturating_sub(delta.unsigned_abs())
        } else {
            (cursor + delta as usize).min(len - 1)
        }
    }

    fn current_repo_link(&self, session_name: &str, repo_cursor: usize) -> Option<RepoLink> {
        self.sessions
            .iter()
            .find(|session| session.name == session_name)
            .and_then(|session| session.repos.get(repo_cursor))
            .cloned()
    }

    fn repo_link_status(&self, repo: &RepoLink) -> RepoLinkStatus {
        let path = Path::new(&repo.path);
        if !path.exists() {
            return RepoLinkStatus::Missing;
        }

        if repo.kind == RepoKind::Worktree {
            RepoLinkStatus::DwsWorktree
        } else {
            RepoLinkStatus::Normal
        }
    }

    fn replace_session(&mut self, previous_name: &str, metadata: SessionMetadata) {
        if let Some(existing) = self
            .sessions
            .iter_mut()
            .find(|session| session.name == previous_name)
        {
            *existing = metadata;
        } else {
            self.sessions.push(metadata);
        }
        self.sessions
            .sort_by(|left, right| left.name.cmp(&right.name));
    }

    fn set_cursor_to_name(&mut self, name: &str) {
        self.cursor = self
            .filtered_indices()
            .iter()
            .position(|idx| self.sessions[*idx].name == name)
            .unwrap_or(0);
    }

    fn filtered_indices(&self) -> Vec<usize> {
        self.sessions
            .iter()
            .enumerate()
            .filter_map(|(idx, session)| session_matches(&self.filter, session).then_some(idx))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepoLinkStatus {
    Normal,
    DwsWorktree,
    Missing,
}

fn addable_repo_candidates(
    session: &SessionMetadata,
    repos: Vec<RepoCandidate>,
) -> Vec<RepoCandidate> {
    let linked_names = session
        .repos
        .iter()
        .map(|repo| repo.name.clone())
        .collect::<BTreeSet<_>>();
    let linked_paths = session
        .repos
        .iter()
        .filter_map(|repo| Path::new(&repo.path).canonicalize().ok())
        .collect::<BTreeSet<_>>();

    repos
        .into_iter()
        .filter(|repo| {
            !linked_names.contains(&repo.name) && !linked_paths.contains(repo.path.as_path())
        })
        .collect()
}

fn rollback_created_worktrees(paths: &[std::path::PathBuf]) -> Option<String> {
    let failures = paths
        .iter()
        .rev()
        .filter_map(|path| {
            git::remove_worktree(path)
                .err()
                .map(|error| format!("{}: {error:#}", path.display()))
        })
        .collect::<Vec<_>>();
    (!failures.is_empty()).then(|| {
        format!(
            "failed to clean up created worktree(s): {}",
            failures.join("; ")
        )
    })
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

fn fuzzy_text_matches(query: &str, text: &str) -> bool {
    let query = query.trim();
    query.is_empty() || fuzzy_subsequence(&query.to_lowercase(), &text.to_lowercase())
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

const FETCH_SPINNER: &[&str] = &["|", "/", "-", "\\"];
const FETCH_WAVE: &[&str] = &[
    ">>----", "->>---", "-->>--", "--->>-", "---->>", "---<<-", "--<<--", "-<<---",
];

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::GitRepoStatus;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    fn manager_session(name: &str) -> SessionMetadata {
        SessionMetadata {
            name: name.to_string(),
            description: None,
            repos: Vec::new(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[cfg(unix)]
    fn make_repo_dir(root: &Path, name: &str) -> std::path::PathBuf {
        let path = root.join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[cfg(unix)]
    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    #[cfg(unix)]
    fn run_git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git command failed: {args:?}");
    }

    #[cfg(unix)]
    fn repo_with_origin(root: &Path, name: &str) -> (std::path::PathBuf, git::OriginBranch) {
        let origin = root.join(format!("{name}-origin.git"));
        let repo = root.join(name);
        assert!(
            Command::new("git")
                .args(["init", "--bare"])
                .arg(&origin)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("clone")
                .arg(&origin)
                .arg(&repo)
                .status()
                .unwrap()
                .success()
        );
        run_git(&repo, &["checkout", "-b", "main"]);
        fs::write(repo.join("README.md"), name).unwrap();
        run_git(&repo, &["add", "README.md"]);
        run_git(
            &repo,
            &[
                "-c",
                "user.name=dynws",
                "-c",
                "user.email=dynws@example.invalid",
                "commit",
                "-m",
                "init",
            ],
        );
        run_git(&repo, &["push", "-u", "origin", "main"]);
        (
            repo,
            git::OriginBranch {
                name: "main".to_string(),
                remote_ref: "origin/main".to_string(),
            },
        )
    }

    #[test]
    fn normal_and_worktree_selection_are_mutually_exclusive() {
        let temp = tempfile::tempdir().unwrap();
        let repos = vec![RepoCandidate {
            name: "repo".to_string(),
            path: temp.path().join("repo"),
            git: Some(GitRepoStatus {
                branch: "main".to_string(),
                dirty: false,
            }),
        }];
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut app = App::new(repos, store);

        app.toggle_current();
        assert!(app.selected.contains(&0));
        assert!(!app.worktree_selected.contains(&0));

        app.toggle_current_worktree();
        assert!(!app.selected.contains(&0));
        assert!(app.worktree_selected.contains(&0));

        app.toggle_current();
        assert!(app.selected.contains(&0));
        assert!(!app.worktree_selected.contains(&0));
    }

    #[test]
    fn add_picker_q_is_filter_input() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut app = App::for_add(Vec::new(), store, "alpha".to_string());

        let signal = app
            .handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE))
            .unwrap();

        assert!(matches!(signal, AppSignal::Continue));
        assert_eq!(app.filter, "q");
    }

    #[test]
    fn addable_candidates_exclude_linked_names_and_paths() {
        let temp = tempfile::tempdir().unwrap();
        let linked = temp.path().join("linked");
        let other = temp.path().join("other");
        let fresh = temp.path().join("fresh");
        fs::create_dir_all(&linked).unwrap();
        fs::create_dir_all(&other).unwrap();
        fs::create_dir_all(&fresh).unwrap();
        let session = SessionMetadata {
            repos: vec![RepoLink::link(
                "linked-name",
                linked.canonicalize().unwrap().display().to_string(),
            )],
            ..manager_session("alpha")
        };
        let candidates = vec![
            RepoCandidate {
                name: "linked-name".to_string(),
                path: other.canonicalize().unwrap(),
                git: None,
            },
            RepoCandidate {
                name: "same-path".to_string(),
                path: linked.canonicalize().unwrap(),
                git: None,
            },
            RepoCandidate {
                name: "fresh".to_string(),
                path: fresh.canonicalize().unwrap(),
                git: None,
            },
        ];

        let addable = addable_repo_candidates(&session, candidates);

        assert_eq!(
            addable
                .iter()
                .map(|repo| repo.name.as_str())
                .collect::<Vec<_>>(),
            vec!["fresh"]
        );
    }

    #[test]
    fn session_manager_ctrl_a_adds_to_the_expanded_session() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut app = SessionManagerApp::new(vec![manager_session("alpha")], store);
        app.mode = SessionManagerMode::Expanded {
            session_name: "alpha".to_string(),
            repo_cursor: 2,
        };

        let signal = app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));

        assert!(matches!(
            signal,
            SessionManagerSignal::AddRepos {
                ref session_name,
                repo_cursor: 2,
            } if session_name == "alpha"
        ));
    }

    #[test]
    fn session_manager_finishes_add_expanded_on_the_first_new_repo() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut app = SessionManagerApp::new(vec![manager_session("alpha")], store);
        let added = vec![RepoLink::link(
            "new-repo",
            temp.path().join("new-repo").display().to_string(),
        )];
        let updated = SessionMetadata {
            repos: vec![
                RepoLink::link(
                    "existing",
                    temp.path().join("existing").display().to_string(),
                ),
                added[0].clone(),
            ],
            ..manager_session("alpha")
        };

        app.finish_repo_add("alpha", updated, &added);

        assert!(matches!(
            app.mode,
            SessionManagerMode::Expanded {
                ref session_name,
                repo_cursor: 1,
            } if session_name == "alpha"
        ));
        assert_eq!(app.sessions[0].repos.len(), 2);
        assert_eq!(app.changes, vec!["added 1 repo link(s) to alpha"]);
    }

    #[cfg(unix)]
    #[test]
    fn add_picker_adds_normal_and_worktree_repositories_together() {
        if !git_available() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let base = make_repo_dir(temp.path(), "base");
        let normal = make_repo_dir(temp.path(), "normal");
        let (repo, branch) = repo_with_origin(temp.path(), "worktree-repo");
        let paths = DynwsPaths::from_home(&home);
        let store = SessionStore::new(paths.clone());
        store
            .create_session("demo", std::slice::from_ref(&base), None, false)
            .unwrap();
        let mut app = App::for_add(
            vec![
                RepoCandidate {
                    name: "normal".to_string(),
                    path: normal.clone(),
                    git: None,
                },
                RepoCandidate {
                    name: "worktree-repo".to_string(),
                    path: repo.clone(),
                    git: git::repo_status(&repo).unwrap(),
                },
            ],
            store.clone(),
            "demo".to_string(),
        );
        app.selected.insert(0);
        app.worktree_selected.insert(1);
        app.worktree_choices.push(WorktreeChoice {
            repo_idx: 1,
            branch: branch.clone(),
        });

        let signal = app.try_add().unwrap();

        assert!(matches!(signal, AppSignal::Done(Some(_))));
        assert_eq!(app.added_repos.len(), 2);
        assert_eq!(store.load_session("demo").unwrap().repos.len(), 3);
        assert_eq!(
            fs::read_link(home.join("workspaces/demo/normal")).unwrap(),
            normal.canonicalize().unwrap()
        );
        let worktree = git::worktree_target_path(&repo, &paths.worktrees_dir, &branch).unwrap();
        assert!(worktree.exists());
        assert_eq!(
            fs::read_link(home.join("workspaces/demo/worktree-repo")).unwrap(),
            worktree.canonicalize().unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn add_picker_rolls_back_a_created_worktree_when_another_fails() {
        if !git_available() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let base = make_repo_dir(temp.path(), "base");
        let (first_repo, first_branch) = repo_with_origin(temp.path(), "first");
        let (second_repo, _) = repo_with_origin(temp.path(), "second");
        let paths = DynwsPaths::from_home(&home);
        let store = SessionStore::new(paths.clone());
        store
            .create_session("demo", std::slice::from_ref(&base), None, false)
            .unwrap();
        let mut app = App::for_add(
            vec![
                RepoCandidate {
                    name: "first".to_string(),
                    path: first_repo.clone(),
                    git: git::repo_status(&first_repo).unwrap(),
                },
                RepoCandidate {
                    name: "second".to_string(),
                    path: second_repo.clone(),
                    git: git::repo_status(&second_repo).unwrap(),
                },
            ],
            store.clone(),
            "demo".to_string(),
        );
        app.worktree_choices.push(WorktreeChoice {
            repo_idx: 0,
            branch: first_branch.clone(),
        });
        app.worktree_choices.push(WorktreeChoice {
            repo_idx: 1,
            branch: git::OriginBranch {
                name: "missing".to_string(),
                remote_ref: "origin/missing".to_string(),
            },
        });

        let signal = app.try_add().unwrap();

        assert!(matches!(signal, AppSignal::Continue));
        let first_worktree =
            git::worktree_target_path(&first_repo, &paths.worktrees_dir, &first_branch).unwrap();
        assert!(!first_worktree.exists());
        assert_eq!(store.load_session("demo").unwrap().repos.len(), 1);
        assert!(!app.message.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn add_picker_does_not_remove_a_reused_worktree_after_session_failure() {
        if !git_available() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let base = make_repo_dir(temp.path(), "base");
        let (repo, branch) = repo_with_origin(temp.path(), "repo");
        let paths = DynwsPaths::from_home(&home);
        let store = SessionStore::new(paths.clone());
        store
            .create_session("demo", std::slice::from_ref(&base), None, false)
            .unwrap();
        let worktree =
            git::create_worktree_from_origin(&repo, &paths.worktrees_dir, &branch).unwrap();
        fs::write(home.join("workspaces/demo/repo"), "occupied").unwrap();
        let mut app = App::for_add(
            vec![RepoCandidate {
                name: "repo".to_string(),
                path: repo.clone(),
                git: git::repo_status(&repo).unwrap(),
            }],
            store.clone(),
            "demo".to_string(),
        );
        app.worktree_choices.push(WorktreeChoice {
            repo_idx: 0,
            branch,
        });

        let signal = app.try_add().unwrap();

        assert!(matches!(signal, AppSignal::Continue));
        assert!(worktree.exists());
        assert_eq!(store.load_session("demo").unwrap().repos.len(), 1);
        assert!(app.message.contains("already exists"));
    }

    #[test]
    fn session_manager_selects_highlighted_session() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = vec![
            SessionMetadata {
                name: "alpha".to_string(),
                description: None,
                repos: Vec::new(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
            },
            SessionMetadata {
                name: "beta".to_string(),
                description: None,
                repos: Vec::new(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
            },
        ];
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut app = SessionManagerApp::new(sessions, store);

        app.toggle_current_session();
        assert!(app.selected.contains("alpha"));

        app.move_cursor(1);
        app.toggle_current_session();
        assert!(app.selected.contains("alpha"));
        assert!(app.selected.contains("beta"));

        app.toggle_current_session();
        assert!(app.selected.contains("alpha"));
        assert!(!app.selected.contains("beta"));
    }

    #[test]
    fn session_manager_actions_fall_back_to_highlighted_session() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = vec![SessionMetadata {
            name: "alpha".to_string(),
            description: None,
            repos: Vec::new(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }];
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut app = SessionManagerApp::new(sessions, store);

        app.start_rename();

        assert!(matches!(
            app.mode,
            SessionManagerMode::Rename {
                ref session_name,
                ..
            } if session_name == "alpha"
        ));
    }

    #[test]
    fn session_manager_rename_rejects_multiple_selected_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = vec![
            SessionMetadata {
                name: "alpha".to_string(),
                description: None,
                repos: Vec::new(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
            },
            SessionMetadata {
                name: "beta".to_string(),
                description: None,
                repos: Vec::new(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
            },
        ];
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut app = SessionManagerApp::new(sessions, store);
        app.selected.insert("alpha".to_string());
        app.selected.insert("beta".to_string());

        app.start_rename();

        assert!(matches!(app.mode, SessionManagerMode::Browse));
        assert!(app.message.contains("one session"));
    }

    #[test]
    fn session_manager_right_arrow_expands_highlighted_session() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = vec![SessionMetadata {
            name: "alpha".to_string(),
            description: None,
            repos: vec![RepoLink::link(
                "repo",
                temp.path().join("repo").display().to_string(),
            )],
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }];
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut app = SessionManagerApp::new(sessions, store);

        app.expand_current_session();

        assert!(matches!(
            app.mode,
            SessionManagerMode::Expanded {
                ref session_name,
                repo_cursor: 0,
            } if session_name == "alpha"
        ));
    }
}
