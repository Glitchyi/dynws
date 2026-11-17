use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListState, Paragraph, Wrap};

mod chrome;

use chrome::{
    Button, DIALOG_CHROME, LOGO_HEIGHT, LOGO_WIDTH, PromptBox, Theme, VERSION, age, brand_line,
    clip, clip_line, clip_start, dialog, dialog_padding, dialog_width, draw_buttons, draw_field,
    draw_prompt, draw_toast, logo_lines, prompt_height, wrap_lines,
};

use crate::config::DynwsPaths;
use crate::discovery::{RepoCandidate, discover_repos_excluding, fuzzy_matches};
use crate::git;
use crate::session::{RepoKind, RepoLink, RepoSelection, SessionMetadata, SessionStore};

pub fn run_interactive(paths: &DynwsPaths, cwd: &Path) -> Result<RepoFlowOutcome> {
    let mut terminal = TerminalSession::new()?;
    let mut app = App::new(Vec::new(), SessionStore::new(paths.clone()));
    app.message = "Loading repositories…".into();
    terminal.terminal_mut().draw(|frame| app.draw(frame))?;
    let repos = discover_repos_excluding(cwd, Some(&paths.home))?;
    if repos.is_empty() {
        drop(terminal);
        println!("no subdirectories found in {}", cwd.display());
        return Ok(RepoFlowOutcome::Cancelled);
    }
    app.repos = repos;
    app.message.clear();
    run_app(terminal.terminal_mut(), &mut app)
}

pub struct ManagerOutcome {
    pub open_session: Option<String>,
    pub changes: Vec<String>,
}

pub fn run_session_manager(
    paths: &DynwsPaths,
    cwd: &Path,
    sessions: Vec<SessionMetadata>,
) -> Result<ManagerOutcome> {
    let mut terminal = TerminalSession::new()?;
    let store = SessionStore::new(paths.clone());
    let mut app = SessionManagerApp::new(sessions, store);
    let open_session = run_session_manager_app(terminal.terminal_mut(), &mut app, cwd)?;
    Ok(ManagerOutcome {
        open_session,
        changes: app.changes,
    })
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl TerminalSession {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
            let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(error.into());
        }
        match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let _ = disable_raw_mode();
                let mut stdout = io::stdout();
                let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen);
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
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<RepoFlowOutcome> {
    loop {
        app.poll_branch_fetch();
        app.refresh_folder_tree();
        terminal.draw(|frame| app.draw(frame))?;
        if (matches!(app.mode, Mode::BranchLoading) || app.folder_tree.pending.is_some())
            && !event::poll(Duration::from_millis(100))?
        {
            continue;
        }
        if let Some(key) = app.input_key(event::read()?) {
            if let Some(message) = app.busy_hint(key) {
                let previous = std::mem::replace(&mut app.message, message.into());
                terminal.draw(|frame| app.draw(frame))?;
                app.message = previous;
            }
            match app.handle_key(key) {
                Ok(AppSignal::Done(outcome)) => return Ok(outcome),
                Ok(AppSignal::Continue) => {}
                Err(error) => app.message = format!("Error: {error:#}"),
            }
        }
    }
}

fn run_session_manager_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut SessionManagerApp,
    cwd: &Path,
) -> Result<Option<String>> {
    loop {
        terminal.draw(|frame| app.draw(frame))?;
        let Some(key) = app.input_key(event::read()?) else {
            continue;
        };
        if app.view.overlay.is_none()
            && !app.view.blocked()
            && matches!(
                (&app.mode, key.code),
                (SessionManagerMode::Rename { .. }, KeyCode::Enter)
                    | (
                        SessionManagerMode::ConfirmRemoveSessions { .. }
                            | SessionManagerMode::ConfirmRemoveRepo { .. },
                        KeyCode::Char('y')
                    )
            )
        {
            let previous =
                std::mem::replace(&mut app.message, "Working: saving session changes…".into());
            terminal.draw(|frame| app.draw(frame))?;
            app.message = previous;
        }
        match app.handle_key(key) {
            SessionManagerSignal::Continue => {}
            SessionManagerSignal::Open { session_name } => return Ok(Some(session_name)),
            SessionManagerSignal::New => {
                app.message = "Loading repositories…".into();
                terminal.draw(|frame| app.draw(frame))?;
                let repos = match discover_repos_excluding(cwd, Some(&app.store.paths().home)) {
                    Ok(repos) => repos,
                    Err(error) => {
                        app.message = format!("Error: {error:#}");
                        continue;
                    }
                };
                let mut picker = App::new(repos, app.store.clone());
                match run_app(terminal, &mut picker)? {
                    RepoFlowOutcome::Completed(session) => {
                        app.changes.push(format!(
                            "session ready: {} ({})",
                            session.name,
                            app.store.workspace_path(&session.name).display()
                        ));
                        return Ok(Some(session.name));
                    }
                    RepoFlowOutcome::Cancelled => app.message = "New session cancelled".into(),
                    RepoFlowOutcome::Quit => return Ok(None),
                }
            }
            SessionManagerSignal::AddRepos {
                session_name,
                repo_cursor,
            } => match run_add_repos_app(terminal, app, cwd, session_name, repo_cursor)? {
                RepoFlowOutcome::Quit => return Ok(None),
                RepoFlowOutcome::Completed(_) | RepoFlowOutcome::Cancelled => {}
            },
            SessionManagerSignal::Done => return Ok(None),
        }
    }
}

fn run_add_repos_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    manager: &mut SessionManagerApp,
    cwd: &Path,
    session_name: String,
    repo_cursor: usize,
) -> Result<RepoFlowOutcome> {
    manager.message = "Loading repositories…".into();
    terminal.draw(|frame| manager.draw(frame))?;
    let Some(session) = manager
        .sessions
        .iter()
        .find(|session| session.name == session_name)
        .cloned()
    else {
        manager.message = "Session no longer exists; Ctrl+R refreshes".into();
        return Ok(RepoFlowOutcome::Cancelled);
    };
    let repos = match discover_repos_excluding(cwd, Some(&manager.store.paths().home)) {
        Ok(repos) => addable_repo_candidates(&session, repos),
        Err(error) => {
            manager.message = format!("Error: {error:#}");
            return Ok(RepoFlowOutcome::Cancelled);
        }
    };
    let mut picker = App::for_add(repos, manager.store.clone(), session_name.clone());
    picker.expected = Some(session);
    match run_app(terminal, &mut picker)? {
        RepoFlowOutcome::Completed(updated) => {
            manager.finish_repo_add(&session_name, updated.clone(), &picker.added_repos);
            Ok(RepoFlowOutcome::Completed(updated))
        }
        RepoFlowOutcome::Cancelled => {
            manager.mode = SessionManagerMode::Expanded {
                session_name,
                repo_cursor,
            };
            manager.message = "Add repositories cancelled".into();
            Ok(RepoFlowOutcome::Cancelled)
        }
        RepoFlowOutcome::Quit => Ok(RepoFlowOutcome::Quit),
    }
}

pub enum RepoFlowOutcome {
    Completed(SessionMetadata),
    Cancelled,
    Quit,
}

enum AppSignal {
    Continue,
    Done(RepoFlowOutcome),
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
            GitTaskPhase::FetchPrune
                if !status.success()
                    && git::is_ref_lock_fetch_error(&String::from_utf8_lossy(&output.stderr)) =>
            {
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
            GitTaskPhase::ListBranches if status.success() => Some(Ok(git::parse_origin_branches(
                &String::from_utf8_lossy(&output.stdout),
            ))),
            GitTaskPhase::FetchPrune => Some(Err(format!(
                "git fetch origin failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))),
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

#[derive(Default)]
struct FolderTree {
    cache: BTreeMap<PathBuf, Vec<String>>,
    pending: Option<(PathBuf, Receiver<Vec<String>>)>,
}

impl FolderTree {
    fn refresh(&mut self, path: Option<&Path>) {
        if let Some((pending_path, receiver)) = &self.pending {
            match receiver.try_recv() {
                Ok(lines) => {
                    self.cache.insert(pending_path.clone(), lines);
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.cache.insert(
                        pending_path.clone(),
                        vec!["Folder preview unavailable".into()],
                    );
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let Some(path) = path.filter(|path| !self.cache.contains_key(*path)) else {
            return;
        };
        if self.pending.is_some() {
            return;
        }
        let path = path.to_path_buf();
        let worker_path = path.clone();
        let (sender, receiver) = mpsc::channel();
        match std::thread::Builder::new()
            .name("folder-preview".into())
            .spawn(move || {
                let _ = sender.send(folder_tree(&worker_path));
            }) {
            Ok(_) => self.pending = Some((path, receiver)),
            Err(error) => {
                self.cache
                    .insert(path, vec![format!("Cannot load folder tree: {error}")]);
            }
        }
    }
}

// Bounded, read-only discovery. Never follow symlinks or inspect file contents.
fn folder_tree(path: &Path) -> Vec<String> {
    fn visit(path: &Path, prefix: &str, depth: usize, lines: &mut Vec<String>) {
        if lines.len() >= 63 {
            return;
        }
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) => {
                lines.push(format!("{prefix}└─ Cannot read folder: {error}"));
                return;
            }
        };
        let mut truncated = false;
        let mut children = Vec::new();
        for (index, entry) in entries.take(129).enumerate() {
            if index == 128 {
                truncated = true;
                break;
            }
            match entry.and_then(|entry| {
                entry
                    .file_type()
                    .map(|kind| (entry.file_name(), entry.path(), kind))
            }) {
                Ok(child) => children.push(child),
                Err(error) => {
                    lines.push(format!("{prefix}… Cannot read entry: {error}"));
                    if lines.len() >= 63 {
                        return;
                    }
                }
            }
        }
        children.sort_by(|a, b| b.2.is_dir().cmp(&a.2.is_dir()).then_with(|| a.0.cmp(&b.0)));
        let count = children.len();
        for (index, (name, child, kind)) in children.into_iter().enumerate() {
            if lines.len() >= 63 {
                return;
            }
            let last = index + 1 == count && !truncated;
            let branch = if last { "└─" } else { "├─" };
            let name = name.to_string_lossy();
            let label: String = name
                .chars()
                .map(|c| if c.is_control() { '�' } else { c })
                .collect();
            let omitted = kind.is_dir()
                && (depth >= 1
                    || matches!(
                        name.as_ref(),
                        ".git" | ".dynws" | "node_modules" | "target" | ".venv"
                    ));
            let suffix = if kind.is_symlink() {
                "@"
            } else if omitted {
                "/ …"
            } else if kind.is_dir() {
                "/"
            } else {
                ""
            };
            lines.push(format!("{prefix}{branch} {label}{suffix}"));
            if kind.is_dir() && !omitted {
                visit(
                    &child,
                    &format!("{prefix}{}", if last { "   " } else { "│  " }),
                    depth + 1,
                    lines,
                );
            }
        }
        if truncated && lines.len() < 63 {
            lines.push(format!("{prefix}└─ … more entries (preview limit)"));
        }
    }
    let mut lines = Vec::new();
    visit(path, "", 0, &mut lines);
    if lines.len() >= 63 {
        lines.push("… folder tree truncated (preview limit)".into());
    }
    if lines.is_empty() {
        lines.push("(empty folder)".into());
    }
    lines
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
    expected: Option<SessionMetadata>,
    folder_tree: FolderTree,
    view: View,
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
            expected: None,
            folder_tree: FolderTree::default(),
            view: View::default(),
        }
    }

    fn for_add(repos: Vec<RepoCandidate>, store: SessionStore, session_name: String) -> Self {
        Self {
            purpose: AppPurpose::AddRepos { session_name },
            ..Self::new(repos, store)
        }
    }

    fn draw(&mut self, frame: &mut ratatui::Frame<'_>) {
        self.view.size = frame.area();
        let theme = Theme::new();
        let (title, mode) = match &self.purpose {
            AppPurpose::AddRepos { session_name } => {
                (format!("Add to {session_name}"), "Add repositories")
            }
            AppPurpose::CreateSession => ("New session".to_string(), "New session"),
        };
        let select_hint = if self.filter.is_empty() {
            "Enter Next  Ctrl+S Link  Ctrl+W Worktree  Esc Cancel"
        } else {
            "Enter Next  Ctrl+S Link  Ctrl+W Worktree  Esc Clear"
        };
        let (stage, mut input, hint) = match &self.mode {
            Mode::Select => (
                "Select repositories",
                Input::new("Type to filter repositories", &self.filter),
                select_hint,
            ),
            Mode::BranchLoading => (
                "Fetching branches",
                Input::new("Type to filter repositories", &self.filter),
                "Esc Cancel fetch",
            ),
            Mode::Branch(picker) => (
                "Choose branches",
                Input::new("Type to filter branches", &picker.filter),
                "Enter Choose branch  Esc Back",
            ),
            Mode::Name => (
                "Name session",
                Input::new("Name this session", &self.session_name),
                "Enter Create session  Esc Back",
            ),
            Mode::Duplicate { existing } => (
                "Matching session",
                Input::readonly("Matching session", &existing.name),
                "r Reuse  n Create duplicate  Esc Rename",
            ),
        };
        let indices = self.visible_repos();
        let hidden = self
            .selected
            .iter()
            .chain(&self.worktree_selected)
            .filter(|idx| !indices.contains(idx))
            .count();
        let fetching = self.branch_fetch.as_ref().map(|fetch| {
            format!(
                "Fetching origin for {}… ({}/{}) · esc cancels",
                fetch.repo_name,
                fetch.current + 1,
                fetch.repo_indices.len()
            )
        });
        let status = if let Some(fetching) = fetching {
            fetching
        } else if let Mode::Branch(picker) = &self.mode {
            format!(
                "{} / {} branches",
                self.filtered_branch_indices(picker).len(),
                picker.branches.len()
            )
        } else {
            format!(
                "{} / {} · {} selected · {hidden} hidden",
                indices.len(),
                self.repos.len(),
                self.selected.len() + self.worktree_selected.len()
            )
        };
        let message = &self.message;
        let location = display_path(&self.store.paths().collection_root);
        input.editable &= self.view.overlay.is_none() && self.view.palette.is_none();
        let Some(surface) = shell(
            frame,
            Screen {
                title: &title,
                subtitle: stage,
                mode,
                bar: theme.accent,
                input,
                status: &status,
                hints: hint,
                message,
                location: &location,
                home: None,
            },
            &mut self.view,
        ) else {
            return;
        };
        self.view.page = surface.list.height;
        if let Some(area) = surface.preview {
            let hint = self.item_details().draw(frame, area);
            self.view.hits.push(HitRegion {
                area: hint,
                target: HitTarget::Key(key_event(KeyCode::F(2), KeyModifiers::NONE)),
            });
        }
        let area = surface.list;
        match &self.mode {
            Mode::Branch(picker) => {
                let rows = self
                    .filtered_branch_indices(picker)
                    .iter()
                    .map(|idx| {
                        let branch = &picker.branches[*idx];
                        row(None, &branch.name, &branch.remote_ref, area.width)
                    })
                    .collect();
                draw_list(
                    frame,
                    area,
                    rows,
                    self.cursor,
                    &mut self.view.branches,
                    "No matching branches — esc returns",
                );
                self.view.list_hits(
                    area,
                    self.filtered_branch_indices(picker).len(),
                    true,
                    false,
                );
            }
            _ => {
                let rows = indices
                    .iter()
                    .map(|idx| {
                        let repo = &self.repos[*idx];
                        let marker = if self.selected.contains(idx) {
                            (SELECTED, theme.success)
                        } else if self.worktree_selected.contains(idx) {
                            (WORKTREE, theme.accent)
                        } else {
                            (UNSELECTED, theme.muted)
                        };
                        row(
                            Some(marker),
                            &repo.name,
                            &self.repo_summary(*idx),
                            area.width,
                        )
                    })
                    .collect();
                draw_list(
                    frame,
                    area,
                    rows,
                    self.cursor,
                    &mut self.view.list,
                    "No repositories match — esc clears the filter or returns",
                );
                if matches!(self.mode, Mode::Select | Mode::BranchLoading) {
                    self.view.list_hits(
                        area,
                        indices.len(),
                        false,
                        matches!(self.mode, Mode::Select),
                    );
                }
            }
        }
        self.view.draw_overlay(frame);
    }

    fn is_adding(&self) -> bool {
        matches!(self.purpose, AppPurpose::AddRepos { .. })
    }

    fn visible_repos(&self) -> Vec<usize> {
        if matches!(self.mode, Mode::Name | Mode::Duplicate { .. }) {
            (0..self.repos.len())
                .filter(|idx| self.selected.contains(idx) || self.worktree_selected.contains(idx))
                .collect()
        } else {
            self.filtered_indices()
        }
    }

    fn repo_summary(&self, idx: usize) -> String {
        if self.worktree_selected.contains(&idx) {
            return self
                .worktree_choices
                .iter()
                .find(|choice| choice.repo_idx == idx)
                .map(|choice| format!("worktree · {}", choice.branch.remote_ref))
                .unwrap_or_else(|| "worktree · choose branch".into());
        }
        self.repos[idx]
            .git
            .as_ref()
            .map(|git| format!("{}{}", git.branch, if git.dirty { " · dirty" } else { "" }))
            .unwrap_or_else(|| "directory".into())
    }

    fn active_repo_index(&self) -> Option<usize> {
        match &self.mode {
            Mode::Branch(picker) => picker.repo_indices.get(picker.current).copied(),
            Mode::Select | Mode::BranchLoading => self.current_repo_idx(),
            _ => None,
        }
    }

    fn refresh_folder_tree(&mut self) {
        let path = self
            .active_repo_index()
            .and_then(|idx| self.repos.get(idx))
            .map(|repo| repo.path.clone());
        let cached = path
            .as_ref()
            .is_some_and(|path| self.folder_tree.cache.contains_key(path));
        self.folder_tree.refresh(path.as_deref());
        if !cached
            && path
                .as_ref()
                .is_some_and(|path| self.folder_tree.cache.contains_key(path))
            && self
                .view
                .overlay
                .as_ref()
                .is_some_and(|(title, _)| title.starts_with("Details"))
        {
            self.view.show("Details", self.details());
        }
    }

    fn item_details(&self) -> Details {
        if matches!(self.mode, Mode::Name | Mode::Duplicate { .. }) {
            return Details {
                title: "Review session".into(),
                fields: vec![
                    ("Name", self.session_name.clone()),
                    (
                        "Workspace",
                        match crate::session::normalize_name(&self.session_name) {
                            Ok(name) => self.store.workspace_path(&name).display().to_string(),
                            Err(error) => format!("not valid yet: {error}"),
                        },
                    ),
                    (
                        "Next",
                        if self.is_adding() {
                            "Add selected repositories"
                        } else {
                            "Create session"
                        }
                        .into(),
                    ),
                ],
                items: self
                    .visible_repos()
                    .iter()
                    .map(|idx| {
                        format!(
                            "{} · {}\n{}",
                            self.repos[*idx].name,
                            self.repo_summary(*idx),
                            self.repos[*idx].path.display()
                        )
                    })
                    .collect(),
                items_title: "Selected",
                more_hint: "f2 all selections",
                tree: false,
            };
        }
        let idx = self.active_repo_index();
        let Some((idx, repo)) = idx.and_then(|idx| self.repos.get(idx).map(|repo| (idx, repo)))
        else {
            return Details {
                title: "No repository selected".into(),
                ..Details::default()
            };
        };
        let mut fields = vec![
            ("Path", repo.path.display().to_string()),
            ("Status", self.repo_summary(idx)),
        ];
        if let AppPurpose::AddRepos { session_name } = &self.purpose {
            fields.insert(0, ("Session", session_name.clone()));
        }
        if let Mode::Branch(picker) = &self.mode
            && let Some(branch) = self
                .current_branch_idx(picker)
                .map(|idx| &picker.branches[idx])
        {
            fields.push(("Branch", branch.name.clone()));
            fields.push(("Remote ref", branch.remote_ref.clone()));
        }
        Details {
            title: repo.name.clone(),
            fields,
            items: self
                .folder_tree
                .cache
                .get(&repo.path)
                .cloned()
                .unwrap_or_else(|| vec!["Loading folder tree…".into()]),
            items_title: "Files",
            tree: true,
            more_hint: "f2 folder tree",
        }
    }

    fn details(&self) -> String {
        format!("{}\n\n{}", self.item_details().text(), self.message)
    }

    fn input_key(&mut self, event: Event) -> Option<KeyEvent> {
        match self.view.input(event)? {
            PointerInput::Key(key) => Some(key),
            PointerInput::Row {
                index,
                toggle,
                activate,
            } => {
                self.cursor = index;
                if toggle && matches!(self.mode, Mode::Select) {
                    Some(key_event(KeyCode::Char('s'), KeyModifiers::CONTROL))
                } else if activate && matches!(self.mode, Mode::Branch(_)) {
                    Some(key_event(KeyCode::Enter, KeyModifiers::NONE))
                } else {
                    None
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<AppSignal> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(AppSignal::Done(RepoFlowOutcome::Quit));
        }

        if let Some(action) = self.view.palette_key(key) {
            return match action {
                Some(action) => self.handle_key(action),
                None => Ok(AppSignal::Continue),
            };
        }

        if self.view.overlay_key(key) {
            return Ok(AppSignal::Continue);
        }
        match key.code {
            KeyCode::F(1) => {
                self.view.show("Help", PICKER_HELP.into());
                return Ok(AppSignal::Continue);
            }
            KeyCode::F(2) => {
                self.view.show("Details", self.details());
                return Ok(AppSignal::Continue);
            }
            _ => {}
        }
        if self.view.blocked() {
            if key.code == KeyCode::Esc {
                return Ok(AppSignal::Done(RepoFlowOutcome::Cancelled));
            }
            return Ok(AppSignal::Continue);
        }
        let len = match &self.mode {
            Mode::Branch(picker) => self.filtered_branch_indices(picker).len(),
            _ => self.filtered_indices().len(),
        };
        if matches!(
            self.mode,
            Mode::Select | Mode::Branch(_) | Mode::BranchLoading
        ) && navigate(&mut self.cursor, key.code, len, self.view.page.max(1))
        {
            return Ok(AppSignal::Continue);
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
                if self.filter.is_empty() {
                    return Ok(AppSignal::Done(RepoFlowOutcome::Cancelled));
                }
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
        match key.code {
            KeyCode::Esc => {
                self.branch_fetch = None;
                self.mode = Mode::Select;
                self.cursor = 0;
                self.message = "branch fetch cancelled".to_string();
            }
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Down => self.move_cursor(1),
            KeyCode::Backspace => {
                self.filter.pop();
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

    fn busy_hint(&self, key: KeyEvent) -> Option<&'static str> {
        if self.view.overlay.is_some() || self.view.blocked() {
            return None;
        }
        match (&self.mode, key.code) {
            (Mode::Name, KeyCode::Enter) if !self.session_name.trim().is_empty() => {
                Some("Working: creating session…")
            }
            (Mode::Duplicate { .. }, KeyCode::Char('n')) => Some("Working: creating session…"),
            (Mode::Select, KeyCode::Enter) if !self.worktree_selected.is_empty() => {
                Some("Fetching origin branches…")
            }
            (Mode::Select, KeyCode::Enter) if self.is_adding() && !self.selected.is_empty() => {
                Some("Working: adding repositories…")
            }
            (Mode::Branch(picker), KeyCode::Enter | KeyCode::Char('s'))
                if self.current_branch_idx(picker).is_some()
                    && (key.code == KeyCode::Enter
                        || key.modifiers.contains(KeyModifiers::CONTROL)) =>
            {
                Some("Working: preparing worktrees…")
            }
            _ => None,
        }
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
                if picker.filter.is_empty() {
                    self.mode = Mode::Select;
                } else {
                    picker.filter.clear();
                    self.mode = Mode::Branch(picker);
                }
                self.cursor = 0;
                return Ok(AppSignal::Continue);
            }
            KeyCode::Backspace => {
                picker.filter.pop();
                self.cursor = 0;
                self.mode = Mode::Branch(picker);
            }
            KeyCode::Enter => return self.select_current_branch(picker),
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
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.session_name.push(character);
            }
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
            KeyCode::Char('r') => return Ok(AppSignal::Done(RepoFlowOutcome::Completed(existing))),
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

        let store = self.store.locked()?;
        let materialized = self.materialize_selections()?;
        let selected = &materialized.selections;
        if !allow_duplicate {
            let selected_paths = selected
                .iter()
                .map(|selection| selection.path.clone())
                .collect::<Vec<_>>();
            let duplicate = match store.find_duplicate(&selected_paths) {
                Ok(duplicate) => duplicate,
                Err(error) => {
                    let cleanup = rollback_created_worktrees(&materialized.created_worktrees);
                    self.message = format!(
                        "Error: {error:#}{}",
                        cleanup
                            .map(|warning| format!("; {warning}"))
                            .unwrap_or_default()
                    );
                    return Ok(AppSignal::Continue);
                }
            };
            if let Some(existing) = duplicate {
                let cleanup = rollback_created_worktrees(&materialized.created_worktrees);
                self.mode = Mode::Duplicate { existing };
                if let Some(cleanup) = cleanup {
                    self.message = cleanup;
                }
                return Ok(AppSignal::Continue);
            }
        }

        match store.create_session_with_links(&self.session_name, selected, None, allow_duplicate) {
            Ok(outcome) => Ok(AppSignal::Done(RepoFlowOutcome::Completed(
                outcome.into_metadata(),
            ))),
            Err(error) => {
                let cleanup = rollback_created_worktrees(&materialized.created_worktrees);
                self.message = format!("Error: {error:#}");
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
        let store = self.store.locked()?;
        if let Some(expected) = &self.expected {
            store.checked(std::slice::from_ref(expected), |_| Ok(()))?;
        }
        let materialized = match self.materialize_selections() {
            Ok(materialized) => materialized,
            Err(error) => {
                self.mode = Mode::Select;
                self.worktree_choices.clear();
                self.message = format!("Error: {error:#}");
                return Ok(AppSignal::Continue);
            }
        };

        match store.add_repo_links(&session_name, &materialized.selections) {
            Ok(outcome) => {
                self.added_repos = outcome.repos;
                Ok(AppSignal::Done(RepoFlowOutcome::Completed(outcome.session)))
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
    New,
    Open {
        session_name: String,
    },
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
    present_paths: BTreeSet<String>,
    view: View,
}

impl SessionManagerApp {
    fn new(sessions: Vec<SessionMetadata>, store: SessionStore) -> Self {
        let present_paths = present_paths(&sessions);
        Self {
            sessions,
            store,
            selected: BTreeSet::new(),
            cursor: 0,
            filter: String::new(),
            mode: SessionManagerMode::Browse,
            message: String::new(),
            changes: Vec::new(),
            present_paths,
            view: View::default(),
        }
    }

    fn draw(&mut self, frame: &mut ratatui::Frame<'_>) {
        self.view.size = frame.area();
        let theme = Theme::new();
        let visible = self.filtered_indices();
        let hidden = self
            .selected
            .iter()
            .filter(|name| {
                !visible
                    .iter()
                    .any(|idx| self.sessions[*idx].name.as_str() == name.as_str())
            })
            .count();
        let expanded = self.expanded_session_name();
        let title = expanded.clone().unwrap_or_else(|| "Sessions".into());
        let input = match &self.mode {
            SessionManagerMode::Expanded { .. } | SessionManagerMode::ConfirmRemoveRepo { .. } => {
                Input::readonly("esc returns to sessions", "")
            }
            mode => Input {
                placeholder: "Type to filter sessions",
                value: &self.filter,
                editable: matches!(mode, SessionManagerMode::Browse)
                    && self.view.overlay.is_none()
                    && self.view.palette.is_none(),
            },
        };
        let home = (self.sessions.is_empty() && matches!(self.mode, SessionManagerMode::Browse))
            .then_some("Ctrl+N to create your first session");
        let hint = match &self.mode {
            SessionManagerMode::Browse if home.is_some() => "Ctrl+N New  Ctrl+R Refresh",
            SessionManagerMode::Browse if self.filter.is_empty() => BROWSE_HINTS,
            SessionManagerMode::Browse => FILTERED_BROWSE_HINTS,
            SessionManagerMode::Expanded { .. } => {
                "Enter Open  Ctrl+A Add  Ctrl+D Remove link  Esc Back"
            }
            SessionManagerMode::Rename { .. } => "Enter Save  Esc Cancel",
            _ => "Enter Cancel  y Confirm",
        };
        let status = if home.is_some() {
            "none yet".to_string()
        } else if let Some(session) = self.active_session() {
            format!(
                "{} · {}",
                count_label(session.repos.len(), "repository", "repositories"),
                count_label(
                    session
                        .repos
                        .iter()
                        .filter(|repo| repo.kind == RepoKind::Worktree)
                        .count(),
                    "worktree",
                    "worktrees"
                )
            )
        } else {
            format!(
                "{} / {} · {} selected · {hidden} hidden",
                visible.len(),
                self.sessions.len(),
                self.selected.len()
            )
        };
        let location = display_path(&self.store.paths().collection_root);
        let Some(surface) = shell(
            frame,
            Screen {
                title: &title,
                subtitle: if expanded.is_some() {
                    "Repositories"
                } else {
                    ""
                },
                mode: if expanded.is_some() {
                    "Repositories"
                } else {
                    "Sessions"
                },
                bar: theme.secondary,
                input,
                status: &status,
                hints: hint,
                message: &self.message,
                location: &location,
                home,
            },
            &mut self.view,
        ) else {
            return;
        };
        self.view.page = surface.list.height;
        let confirming = matches!(
            self.mode,
            SessionManagerMode::ConfirmRemoveSessions { .. }
                | SessionManagerMode::ConfirmRemoveRepo { .. }
        );
        if home.is_none() {
            // A confirmation names its own targets; a focused-session preview
            // beside it could be mistaken for one of them.
            if let Some(area) = surface.preview.filter(|_| !confirming) {
                let hint = self.item_details().draw(frame, area);
                self.view.hits.push(HitRegion {
                    area: hint,
                    target: HitTarget::Key(key_event(KeyCode::F(2), KeyModifiers::NONE)),
                });
            }
            self.draw_rows(frame, surface.list, &visible, theme);
        }
        self.view.draw_toast(frame);
        let cancel = key_event(KeyCode::Esc, KeyModifiers::NONE);
        let confirm = key_event(KeyCode::Char('y'), KeyModifiers::NONE);
        match &self.mode {
            SessionManagerMode::ConfirmRemoveSessions { session_names } => {
                let text = self.removal_text(session_names);
                let title = if session_names.len() == 1 {
                    "Delete session"
                } else {
                    "Delete sessions"
                };
                self.view.text_dialog(
                    frame,
                    title,
                    &text,
                    TextStyle::Fields,
                    &[
                        Button {
                            label: "Cancel",
                            focused: true,
                            danger: false,
                        },
                        Button {
                            label: "Delete",
                            focused: false,
                            danger: true,
                        },
                    ],
                    &[cancel, confirm],
                    "y deletes · enter cancels",
                );
            }
            SessionManagerMode::ConfirmRemoveRepo {
                session_name,
                repo_name,
                ..
            } => {
                let text = self.repo_removal_text(session_name, repo_name);
                self.view.text_dialog(
                    frame,
                    "Remove repository",
                    &text,
                    TextStyle::Fields,
                    &[
                        Button {
                            label: "Cancel",
                            focused: true,
                            danger: false,
                        },
                        Button {
                            label: "Remove",
                            focused: false,
                            danger: true,
                        },
                    ],
                    &[cancel, confirm],
                    "y removes · enter cancels",
                );
            }
            SessionManagerMode::Rename { input, .. } => {
                let input = input.clone();
                if self.view.overlay.is_none() && self.view.palette.is_none() {
                    self.view
                        .prompt_dialog(frame, "Rename session", &input, "New session name");
                }
            }
            _ => {}
        }
        self.view.draw_overlay(frame);
    }

    fn draw_rows(
        &mut self,
        frame: &mut ratatui::Frame<'_>,
        area: Rect,
        visible: &[usize],
        theme: Theme,
    ) {
        if let Some(session) = self.active_session() {
            let repo_count = session.repos.len();
            let rows = session
                .repos
                .iter()
                .map(|repo| {
                    let kind = self.repo_kind(repo);
                    let marker = match kind {
                        "missing" => (SELECTED, theme.error),
                        "worktree" => (WORKTREE, theme.accent),
                        _ => (SELECTED, theme.success),
                    };
                    row(
                        Some(marker),
                        &repo.name,
                        &match repo.local_branch.as_deref() {
                            Some(branch) => format!("{kind} · {branch}"),
                            None => kind.to_string(),
                        },
                        area.width,
                    )
                })
                .collect();
            let cursor = self.repo_cursor();
            draw_list(
                frame,
                area,
                rows,
                cursor,
                &mut self.view.branches,
                "No linked repositories — ctrl+a adds repositories",
            );
            if matches!(self.mode, SessionManagerMode::Expanded { .. }) {
                self.view.list_hits(area, repo_count, true, false);
            }
            return;
        }
        let now = chrono::Utc::now();
        let rows = visible
            .iter()
            .map(|idx| {
                let session = &self.sessions[*idx];
                let marker = if self.selected.contains(&session.name) {
                    (SELECTED, theme.primary)
                } else {
                    (UNSELECTED, theme.muted)
                };
                let repos = count_label(session.repos.len(), "repo", "repos");
                let metadata = match age(&session.updated_at, now) {
                    Some(age) => format!("{repos} · {age}"),
                    None => repos,
                };
                row(Some(marker), &session.name, &metadata, area.width)
            })
            .collect();
        draw_list(
            frame,
            area,
            rows,
            self.cursor,
            &mut self.view.list,
            "No matching sessions — esc clears the filter",
        );
        if matches!(self.mode, SessionManagerMode::Browse) {
            self.view.list_hits(area, visible.len(), false, true);
        }
    }

    fn removal_text(&self, session_names: &[String]) -> String {
        let retained: Vec<String> = session_names
            .iter()
            .filter_map(|name| self.sessions.iter().find(|s| &s.name == name))
            .flat_map(|session| session.repos.iter())
            .filter(|repo| repo.kind == RepoKind::Worktree)
            .map(|repo| repo.path.clone())
            .collect();
        let keep = if retained.is_empty() {
            "Source repositories stay untouched.".to_string()
        } else {
            format!(
                "Source repositories stay untouched. These managed worktrees stay on disk and are not deleted:\n{}",
                retained.join("\n")
            )
        };
        format!(
            "Delete {}?\n\nThe session metadata and workspace links are deleted.\n{keep}\n\nTargets:\n{}",
            count_label(session_names.len(), "session", "sessions"),
            session_names.join("\n")
        )
    }

    fn repo_removal_text(&self, session_name: &str, repo_name: &str) -> String {
        let worktree = self
            .sessions
            .iter()
            .find(|s| s.name == session_name)
            .and_then(|s| s.repos.iter().find(|r| r.name == repo_name))
            .filter(|r| r.kind == RepoKind::Worktree)
            .map(|r| r.path.clone());
        let outcome = match worktree {
            Some(path) => format!(
                "The managed worktree at {path} is deleted only if it is clean, unshared and verified. Otherwise it is kept and the result says why."
            ),
            None => "The session link is removed. Source repositories stay untouched.".to_string(),
        };
        format!("Remove {repo_name} from {session_name}?\n\n{outcome}")
    }

    fn repo_kind(&self, repo: &RepoLink) -> &'static str {
        if !self.present_paths.contains(&repo.path) {
            "missing"
        } else if repo.kind == RepoKind::Worktree {
            "worktree"
        } else {
            "link"
        }
    }

    fn item_details(&self) -> Details {
        let Some(session) = self.display_session() else {
            return Details {
                title: if self.sessions.is_empty() {
                    "Your first session"
                } else {
                    "No matching session"
                }
                .into(),
                fields: vec![(
                    "Next",
                    if self.sessions.is_empty() {
                        "ctrl+n creates a focused workspace"
                    } else {
                        "esc clears the filter"
                    }
                    .into(),
                )],
                ..Details::default()
            };
        };
        if self.active_session().is_some()
            && let Some(repo) = session.repos.get(self.repo_cursor())
        {
            let mut fields = vec![
                ("Session", session.name.clone()),
                ("Kind", self.repo_kind(repo).into()),
                ("Path", repo.path.clone()),
            ];
            if let Some(source) = &repo.source_repo {
                fields.push(("Source", source.clone()));
            }
            if let Some(branch) = &repo.local_branch {
                fields.push(("Branch", branch.clone()));
            }
            if let Some(remote) = &repo.remote_ref {
                fields.push(("Remote ref", remote.clone()));
            }
            return Details {
                title: repo.name.clone(),
                fields,
                ..Details::default()
            };
        }
        Details {
            title: session.name.clone(),
            fields: vec![
                (
                    "Description",
                    session
                        .description
                        .clone()
                        .unwrap_or_else(|| "No description".into()),
                ),
                (
                    "Workspace",
                    self.store
                        .workspace_path(&session.name)
                        .display()
                        .to_string(),
                ),
                (
                    "Updated",
                    age(&session.updated_at, chrono::Utc::now())
                        .unwrap_or_else(|| session.updated_at.clone()),
                ),
            ],
            items_title: "Repositories",
            items: session
                .repos
                .iter()
                .map(|repo| {
                    format!(
                        "{} · {}{}\n{}",
                        repo.name,
                        self.repo_kind(repo),
                        repo.local_branch
                            .as_deref()
                            .map(|branch| format!(" · {branch}"))
                            .unwrap_or_default(),
                        repo.path
                    )
                })
                .collect(),
            more_hint: "→ all repositories",
            tree: false,
        }
    }

    fn details(&self) -> String {
        format!("{}\n\n{}", self.item_details().text(), self.message)
    }

    fn input_key(&mut self, event: Event) -> Option<KeyEvent> {
        match self.view.input(event)? {
            PointerInput::Key(key) => Some(key),
            PointerInput::Row {
                index,
                toggle,
                activate,
            } => {
                match &mut self.mode {
                    SessionManagerMode::Browse => self.cursor = index,
                    SessionManagerMode::Expanded { repo_cursor, .. } => *repo_cursor = index,
                    _ => return None,
                }
                if toggle && matches!(self.mode, SessionManagerMode::Browse) {
                    Some(key_event(KeyCode::Char('s'), KeyModifiers::CONTROL))
                } else if activate {
                    Some(key_event(KeyCode::Enter, KeyModifiers::NONE))
                } else {
                    None
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> SessionManagerSignal {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return SessionManagerSignal::Done;
        }

        if let Some(action) = self.view.palette_key(key) {
            return match action {
                Some(action) => self.handle_key(action),
                None => SessionManagerSignal::Continue,
            };
        }

        if self.view.overlay_key(key) {
            return SessionManagerSignal::Continue;
        }
        match key.code {
            KeyCode::F(1) => {
                self.view.show("Help", MANAGER_HELP.into());
                return SessionManagerSignal::Continue;
            }
            KeyCode::F(2) => {
                self.view.show("Details", self.details());
                return SessionManagerSignal::Continue;
            }
            _ => {}
        }
        if self.view.blocked() {
            if key.code == KeyCode::Esc {
                self.mode = SessionManagerMode::Browse;
            }
            return SessionManagerSignal::Continue;
        }
        if matches!(
            self.mode,
            SessionManagerMode::ConfirmRemoveSessions { .. }
                | SessionManagerMode::ConfirmRemoveRepo { .. }
        ) {
            match key.code {
                KeyCode::Up | KeyCode::PageUp => {
                    self.view.scroll = self.view.scroll.saturating_sub(1);
                    return SessionManagerSignal::Continue;
                }
                KeyCode::Down | KeyCode::PageDown => {
                    self.view.scroll = self.view.scroll.saturating_add(1);
                    return SessionManagerSignal::Continue;
                }
                _ => {}
            }
        }
        if matches!(self.mode, SessionManagerMode::Browse) {
            let len = self.filtered_indices().len();
            if navigate(&mut self.cursor, key.code, len, self.view.page.max(1)) {
                return SessionManagerSignal::Continue;
            }
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
            KeyCode::Enter => {
                if let Some(session) = self.current_session() {
                    return SessionManagerSignal::Open {
                        session_name: session.name.clone(),
                    };
                }
            }
            KeyCode::Right => self.expand_current_session(),
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return SessionManagerSignal::New;
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(session) = self.current_session() {
                    return SessionManagerSignal::AddRepos {
                        session_name: session.name.clone(),
                        repo_cursor: 0,
                    };
                }
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => self.refresh(),
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
        let len = self
            .sessions
            .iter()
            .find(|session| session.name == session_name)
            .map(|session| session.repos.len())
            .unwrap_or(0);
        if navigate(&mut repo_cursor, key.code, len, self.view.page.max(1)) {
            self.mode = SessionManagerMode::Expanded {
                session_name,
                repo_cursor,
            };
            return SessionManagerSignal::Continue;
        }
        match key.code {
            KeyCode::Enter => return SessionManagerSignal::Open { session_name },
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

                match self.mutate(std::slice::from_ref(&session_name), |store| {
                    store.rename_session(&session_name, &input)
                }) {
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
                        self.message = format!("Error: {error:#}");
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
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('n') => {
                self.mode = SessionManagerMode::Browse;
                self.message = "remove cancelled".to_string();
            }
            KeyCode::Char('y') => {
                let outcome = match self.mutate(&session_names, |store| {
                    store.remove_sessions(&session_names)
                }) {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        self.message = format!("Error: {error:#}");
                        self.mode = SessionManagerMode::Browse;
                        return SessionManagerSignal::Continue;
                    }
                };
                let removed_count = outcome.sessions.len();
                for removed in outcome.sessions {
                    self.sessions.retain(|session| session.name != removed.name);
                    self.selected.remove(&removed.name);
                    self.changes
                        .push(format!("removed session {}", removed.name));
                }
                let warnings = outcome.warnings.join("; ");
                let len = self.filtered_indices().len();
                if self.cursor >= len {
                    self.cursor = len.saturating_sub(1);
                }
                self.message = format!(
                    "removed {}{}",
                    count_label(removed_count, "session", "sessions"),
                    if warnings.is_empty() {
                        String::new()
                    } else {
                        format!("; Warning: {warnings}")
                    }
                );
                self.mode = SessionManagerMode::Browse;
            }
            _ => {
                self.view.scroll = 0;
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
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('n') => {
                self.mode = SessionManagerMode::Expanded {
                    session_name,
                    repo_cursor,
                };
                self.message = "remove repo cancelled".to_string();
            }
            KeyCode::Char('y') => match self.mutate(std::slice::from_ref(&session_name), |store| {
                store.remove_repo_link(&session_name, &repo_name)
            }) {
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
                    self.message = format!("Error: {error:#}");
                    self.mode = SessionManagerMode::Expanded {
                        session_name,
                        repo_cursor,
                    };
                }
            },
            _ => {
                self.view.scroll = 0;
                self.mode = SessionManagerMode::ConfirmRemoveRepo {
                    session_name,
                    repo_name,
                    repo_cursor,
                };
            }
        }
        SessionManagerSignal::Continue
    }

    fn mutate<T>(
        &self,
        names: &[String],
        operation: impl FnOnce(&SessionStore) -> Result<T>,
    ) -> Result<T> {
        let expected = names
            .iter()
            .map(|name| {
                self.sessions
                    .iter()
                    .find(|session| session.name == *name)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("session changed elsewhere; Ctrl+R refreshes"))
            })
            .collect::<Result<Vec<_>>>()?;
        self.store.checked(&expected, operation)
    }

    fn refresh(&mut self) {
        match self.store.load_sessions() {
            Ok(sessions) => {
                self.present_paths = present_paths(&sessions);
                self.sessions = sessions;
                self.selected
                    .retain(|name| self.sessions.iter().any(|session| session.name == *name));
                self.cursor = self
                    .cursor
                    .min(self.filtered_indices().len().saturating_sub(1));
                self.message = "Sessions refreshed".into();
            }
            Err(error) => self.message = format!("Error: {error:#}"),
        }
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
        self.present_paths = present_paths(&self.sessions);
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

#[derive(Clone, Copy)]
enum HitTarget {
    Key(KeyEvent),
    Command(KeyEvent),
    Row { index: usize, checkbox: bool },
}

struct HitRegion {
    area: Rect,
    target: HitTarget,
}

enum PointerInput {
    Key(KeyEvent),
    Row {
        index: usize,
        toggle: bool,
        activate: bool,
    },
}

fn key_event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

#[derive(Clone)]
struct PaletteCommand {
    label: String,
    shortcut: String,
    key: KeyEvent,
    category: &'static str,
}

enum PaletteRow {
    Blank,
    Heading(&'static str),
    Item(usize),
}

#[derive(Default)]
struct Palette {
    query: String,
    cursor: usize,
    list: ListState,
}

#[derive(Default)]
struct View {
    size: Rect,
    page: u16,
    list: ListState,
    branches: ListState,
    overlay: Option<(String, String)>,
    scroll: u16,
    hits: Vec<HitRegion>,
    scroll_area: Rect,
    last_click: Option<(Instant, Rect, usize)>,
    last_action: Option<Instant>,
    commands: Vec<PaletteCommand>,
    palette: Option<Palette>,
    toast: Option<String>,
}

impl View {
    fn input(&mut self, event: Event) -> Option<PointerInput> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                self.last_click = None;
                Some(PointerInput::Key(key))
            }
            Event::Resize(..) => {
                self.last_click = None;
                self.hits.clear();
                None
            }
            Event::Mouse(mouse) => self.mouse(mouse),
            _ => None,
        }
    }

    fn mouse(&mut self, mouse: MouseEvent) -> Option<PointerInput> {
        let point = (mouse.column, mouse.row).into();
        if matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            self.last_click = None;
            return self.scroll_area.contains(point).then(|| {
                PointerInput::Key(key_event(
                    if mouse.kind == MouseEventKind::ScrollUp {
                        KeyCode::Up
                    } else {
                        KeyCode::Down
                    },
                    KeyModifiers::NONE,
                ))
            });
        }
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return None;
        }
        let hit = self
            .hits
            .iter()
            .rev()
            .find(|hit| hit.area.contains(point))?;
        match hit.target {
            HitTarget::Key(key) | HitTarget::Command(key) => {
                let command = matches!(hit.target, HitTarget::Command(_));
                // Coalesce fast repeated action clicks, including across a dialog transition.
                self.last_click = None;
                let now = Instant::now();
                let navigation = matches!(key.code, KeyCode::Esc | KeyCode::F(1) | KeyCode::F(2))
                    || (key.code == KeyCode::Char('p')
                        && key.modifiers.contains(KeyModifiers::CONTROL));
                if !navigation
                    && self
                        .last_action
                        .is_some_and(|last| now.duration_since(last) < Duration::from_millis(350))
                {
                    return None;
                }
                self.last_action = if navigation { None } else { Some(now) };
                if command {
                    self.palette = None;
                }
                Some(PointerInput::Key(key))
            }
            HitTarget::Row { index, checkbox } => {
                let now = Instant::now();
                let activate = !checkbox
                    && self.last_click.is_some_and(|(last, area, row)| {
                        area == hit.area
                            && row == index
                            && now.duration_since(last) < Duration::from_millis(400)
                    });
                self.last_click = if activate || checkbox {
                    None
                } else {
                    Some((now, hit.area, index))
                };
                Some(PointerInput::Row {
                    index,
                    toggle: checkbox,
                    activate,
                })
            }
        }
    }

    fn list_hits(&mut self, area: Rect, len: usize, branches: bool, selectable: bool) {
        let offset = if branches {
            self.branches.offset()
        } else {
            self.list.offset()
        };
        self.scroll_area = area;
        for (line, index) in (offset..len).take(area.height as usize).enumerate() {
            let y = area.y + line as u16;
            self.hits.push(HitRegion {
                area: Rect::new(area.x, y, area.width, 1),
                target: HitTarget::Row {
                    index,
                    checkbox: false,
                },
            });
            if selectable {
                self.hits.push(HitRegion {
                    area: Rect::new(area.x + 1, y, 3, 1),
                    target: HitTarget::Row {
                        index,
                        checkbox: true,
                    },
                });
            }
        }
    }

    /// Adds a view's "Key Label" hints to the command palette under `category`.
    fn register(&mut self, hints: &str, category: &'static str) {
        for action in hints.split("  ").filter(|action| !action.is_empty()) {
            let (shortcut, label) = action.split_once(' ').unwrap_or((action, ""));
            if let Some(key) = action_key(shortcut)
                && key.code != KeyCode::Char('y')
                && !self.commands.iter().any(|command| command.key == key)
            {
                self.commands.push(PaletteCommand {
                    label: describe(label).to_owned(),
                    shortcut: shortcut.to_owned(),
                    key,
                    category,
                });
            }
        }
    }

    /// Under a modal dialog only the global quit shortcut stays clickable.
    fn block_click_through(&mut self) {
        self.hits.retain(|hit| {
            matches!(hit.target, HitTarget::Key(key)
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        });
        self.last_click = None;
    }

    fn draw_toast(&mut self, frame: &mut ratatui::Frame<'_>) {
        if let Some(message) = self.toast.take()
            && let Some(area) = draw_toast(frame, &message, Theme::new())
        {
            self.hits.push(HitRegion {
                area,
                target: HitTarget::Key(key_event(KeyCode::F(2), KeyModifiers::NONE)),
            });
        }
    }

    /// A scrollable text dialog. With buttons, the focused button is the Enter
    /// default and `hint` explains the other keys on the button row.
    #[allow(clippy::too_many_arguments)]
    fn text_dialog(
        &mut self,
        frame: &mut ratatui::Frame<'_>,
        title: &str,
        text: &str,
        style: TextStyle,
        buttons: &[Button<'_>],
        keys: &[KeyEvent],
        hint: &str,
    ) {
        let theme = Theme::new();
        let screen = frame.area();
        let preferred = if buttons.is_empty() { 76 } else { 60 };
        let width = dialog_width(screen, preferred);
        let lines = styled_text(
            text,
            style,
            width.saturating_sub(dialog_padding(width) * 2) as usize,
            theme,
        );
        let footer = if buttons.is_empty() { 0 } else { 2 };
        let body = (lines.len() as u16).saturating_add(footer);
        let text_rows = body
            .min(screen.height.saturating_sub(2 + DIALOG_CHROME))
            .saturating_sub(footer);
        let aside = if lines.len() as u16 > text_rows {
            "↑↓ scroll"
        } else {
            ""
        };
        let drawn = dialog(frame, preferred, body, title, aside, theme);
        let content = drawn.content;
        let text_area = Rect::new(
            content.x,
            content.y,
            content.width,
            content.height.saturating_sub(footer),
        );
        self.scroll = self
            .scroll
            .min((lines.len() as u16).saturating_sub(text_area.height));
        frame.render_widget(Paragraph::new(lines).scroll((self.scroll, 0)), text_area);
        self.block_click_through();
        self.scroll_area = text_area;
        self.hits.push(HitRegion {
            area: drawn.close,
            target: HitTarget::Key(key_event(KeyCode::Esc, KeyModifiers::NONE)),
        });
        if buttons.is_empty() || content.height == 0 {
            return;
        }
        let row = Rect::new(content.x, content.bottom() - 1, content.width, 1);
        let areas = draw_buttons(frame, row, buttons, theme);
        let first = areas
            .iter()
            .filter(|area| area.width > 0)
            .map(|area| area.x)
            .min()
            .unwrap_or(row.right());
        let room = first.saturating_sub(row.x + 2);
        if !hint.is_empty() && Line::raw(hint).width() as u16 <= room {
            frame.render_widget(
                Paragraph::new(hint).style(theme.muted),
                Rect::new(row.x, row.y, room, 1),
            );
        }
        for (area, key) in areas.into_iter().zip(keys) {
            if area.width > 0 {
                self.hits.push(HitRegion {
                    area,
                    target: HitTarget::Key(*key),
                });
            }
        }
    }

    /// A one-field dialog, such as renaming a session.
    fn prompt_dialog(
        &mut self,
        frame: &mut ratatui::Frame<'_>,
        title: &str,
        value: &str,
        placeholder: &str,
    ) {
        let theme = Theme::new();
        let drawn = dialog(frame, 60, 3, title, "", theme);
        let content = drawn.content;
        self.block_click_through();
        self.hits.push(HitRegion {
            area: drawn.close,
            target: HitTarget::Key(key_event(KeyCode::Esc, KeyModifiers::NONE)),
        });
        if content.height == 0 {
            return;
        }
        let field = Rect::new(content.x, content.y, content.width, 1);
        frame.render_widget(
            Block::default().style(theme.base.patch(theme.element)),
            field,
        );
        draw_field(
            frame,
            Rect::new(field.x + 1, field.y, field.width.saturating_sub(2), 1),
            value,
            placeholder,
            true,
            theme,
        );
        if content.height >= 3 {
            let label = Line::from(vec![
                Span::styled("enter", theme.key),
                Span::styled(" save", theme.muted),
            ]);
            let width = (label.width() as u16).min(content.width);
            let area = Rect::new(content.right() - width, content.y + 2, width, 1);
            frame.render_widget(Paragraph::new(label), area);
            self.hits.push(HitRegion {
                area,
                target: HitTarget::Key(key_event(KeyCode::Enter, KeyModifiers::NONE)),
            });
        }
    }

    /// OpenCode's command palette: a search field, purple category headings
    /// and rows with right-aligned shortcuts; focus is a full-width bar.
    fn draw_palette(&mut self, frame: &mut ratatui::Frame<'_>) {
        let Some(palette) = &mut self.palette else {
            return;
        };
        let theme = Theme::new();
        let commands = self
            .commands
            .iter()
            .filter(|command| {
                fuzzy_text_matches(
                    &palette.query,
                    &format!("{} {}", command.label, command.shortcut),
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        palette.cursor = palette.cursor.min(commands.len().saturating_sub(1));
        let mut rows = Vec::new();
        let mut category = "";
        for (index, command) in commands.iter().enumerate() {
            if command.category != category {
                if !rows.is_empty() {
                    rows.push(PaletteRow::Blank);
                }
                rows.push(PaletteRow::Heading(command.category));
                category = command.category;
            }
            rows.push(PaletteRow::Item(index));
        }
        let body = 2 + rows.len().max(1) as u16;
        let drawn = dialog(frame, 60, body, "Commands", "", theme);
        let content = drawn.content;
        self.hits.retain(|hit| {
            matches!(hit.target, HitTarget::Key(key)
                if matches!(key.code, KeyCode::Char('c') | KeyCode::Char('p'))
                    && key.modifiers.contains(KeyModifiers::CONTROL))
        });
        self.last_click = None;
        self.hits.push(HitRegion {
            area: drawn.close,
            target: HitTarget::Key(key_event(KeyCode::Esc, KeyModifiers::NONE)),
        });
        if content.height == 0 {
            return;
        }
        draw_field(
            frame,
            Rect::new(content.x, content.y, content.width, 1),
            &palette.query,
            "Search",
            true,
            theme,
        );
        let list = Rect::new(
            drawn.outer.x + 1,
            content.y + 2.min(content.height),
            drawn.outer.width.saturating_sub(2),
            content.height.saturating_sub(2),
        );
        self.scroll_area = list;
        let inset = dialog_padding(drawn.outer.width) - 1;
        let text_width = list.width.saturating_sub(inset * 2) as usize;
        if commands.is_empty() {
            if list.height > 0 {
                frame.render_widget(
                    Paragraph::new("No matching actions").style(theme.muted),
                    Rect::new(list.x + inset, list.y, text_width as u16, 1),
                );
            }
            return;
        }
        let focus = rows
            .iter()
            .position(|row| matches!(row, PaletteRow::Item(index) if *index == palette.cursor))
            .unwrap_or(0);
        let height = (list.height as usize).max(1);
        let mut offset = palette.list.offset().min(rows.len() - 1);
        if focus < offset {
            offset = focus.saturating_sub(usize::from(
                focus > 0 && matches!(rows[focus - 1], PaletteRow::Heading(_)),
            ));
        }
        if focus >= offset + height {
            offset = focus + 1 - height;
        }
        *palette.list.offset_mut() = offset;
        for (line, entry) in rows
            .iter()
            .skip(offset)
            .take(list.height as usize)
            .enumerate()
        {
            let area = Rect::new(list.x, list.y + line as u16, list.width, 1);
            match entry {
                PaletteRow::Blank => {}
                PaletteRow::Heading(name) => frame.render_widget(
                    Paragraph::new(*name).style(theme.accent),
                    Rect::new(list.x + inset, area.y, text_width as u16, 1),
                ),
                PaletteRow::Item(index) => {
                    let command = &commands[*index];
                    let shortcut = display_key(&command.shortcut);
                    let shortcut_width = Line::raw(&shortcut).width();
                    let label = clip(
                        &command.label,
                        text_width.saturating_sub(shortcut_width + 2),
                    );
                    let gap = text_width.saturating_sub(Line::raw(&label).width() + shortcut_width);
                    let (label_style, key_style) = if *index == palette.cursor {
                        frame.render_widget(Block::default().style(theme.focus), area);
                        (theme.focus, theme.focus.remove_modifier(Modifier::BOLD))
                    } else {
                        (theme.key, theme.muted)
                    };
                    frame.render_widget(
                        Paragraph::new(Line::from(vec![
                            Span::raw(" ".repeat(inset as usize)),
                            Span::styled(label, label_style),
                            Span::raw(" ".repeat(gap)),
                            Span::styled(shortcut, key_style),
                        ])),
                        area,
                    );
                    self.hits.push(HitRegion {
                        area,
                        target: HitTarget::Command(command.key),
                    });
                }
            }
        }
    }

    fn palette_key(&mut self, key: KeyEvent) -> Option<Option<KeyEvent>> {
        if key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if !self.blocked() && self.overlay.is_none() {
                self.palette = if self.palette.is_some() {
                    None
                } else {
                    Some(Palette::default())
                };
            }
            return Some(None);
        }
        self.palette.as_ref()?;
        if key.code == KeyCode::Esc {
            self.palette = None;
            return Some(None);
        }
        if self.blocked() {
            return Some(None);
        }
        let palette = self.palette.as_mut().unwrap();
        let commands = self
            .commands
            .iter()
            .filter(|command| {
                fuzzy_text_matches(
                    &palette.query,
                    &format!("{} {}", command.label, command.shortcut),
                )
            })
            .collect::<Vec<_>>();
        palette.cursor = palette.cursor.min(commands.len().saturating_sub(1));
        match key.code {
            KeyCode::Enter => {
                let action = commands.get(palette.cursor).map(|command| command.key);
                if action.is_some() {
                    self.palette = None;
                }
                return Some(action);
            }
            KeyCode::Up => palette.cursor = palette.cursor.saturating_sub(1),
            KeyCode::Down => {
                palette.cursor = (palette.cursor + 1).min(commands.len().saturating_sub(1))
            }
            KeyCode::Backspace => {
                palette.query.pop();
                palette.cursor = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                palette.query.push(c);
                palette.cursor = 0;
            }
            key => {
                navigate(
                    &mut palette.cursor,
                    key,
                    commands.len(),
                    self.size.height.saturating_sub(8),
                );
            }
        }
        Some(None)
    }

    fn blocked(&self) -> bool {
        self.size.width > 0 && (self.size.width < 40 || self.size.height < 12)
    }

    fn overlay_key(&mut self, key: KeyEvent) -> bool {
        if self.overlay.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Esc | KeyCode::F(1) | KeyCode::F(2) => {
                self.overlay = None;
                self.scroll = 0;
            }
            KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
            KeyCode::PageUp => {
                self.scroll = self
                    .scroll
                    .saturating_sub(self.size.height.saturating_sub(7))
            }
            KeyCode::PageDown => {
                self.scroll = self
                    .scroll
                    .saturating_add(self.size.height.saturating_sub(7))
            }
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = u16::MAX,
            _ => {}
        }
        true
    }

    fn show(&mut self, title: &str, text: String) {
        self.overlay = Some((title.into(), text));
        self.scroll = 0;
    }
    fn draw_overlay(&mut self, frame: &mut ratatui::Frame<'_>) {
        self.draw_toast(frame);
        if let Some((title, text)) = self.overlay.clone() {
            let style = if title.starts_with("Help") {
                TextStyle::Keybinds
            } else {
                TextStyle::Fields
            };
            self.text_dialog(frame, &title, &text, style, &[], &[], "");
        }
        self.draw_palette(frame);
    }
}

fn present_paths(sessions: &[SessionMetadata]) -> BTreeSet<String> {
    sessions
        .iter()
        .flat_map(|session| &session.repos)
        .filter(|repo| Path::new(&repo.path).exists())
        .map(|repo| repo.path.clone())
        .collect()
}

struct Input<'a> {
    placeholder: &'a str,
    value: &'a str,
    editable: bool,
}

impl<'a> Input<'a> {
    fn new(placeholder: &'a str, value: &'a str) -> Self {
        Self {
            placeholder,
            value,
            editable: true,
        }
    }

    fn readonly(placeholder: &'a str, value: &'a str) -> Self {
        Self {
            placeholder,
            value,
            editable: false,
        }
    }
}

#[cfg(test)]
fn draw_input(frame: &mut ratatui::Frame<'_>, area: Rect, input: Input<'_>, theme: Theme) {
    draw_field(
        frame,
        area,
        input.value,
        input.placeholder,
        input.editable,
        theme,
    );
}

/// What one view asks the shared frame to draw.
struct Screen<'a> {
    title: &'a str,
    subtitle: &'a str,
    mode: &'a str,
    bar: Style,
    input: Input<'a>,
    status: &'a str,
    hints: &'a str,
    message: &'a str,
    location: &'a str,
    /// Shows the logo home with this hint-formatted tip instead of a list.
    home: Option<&'a str>,
}

struct Surface {
    list: Rect,
    preview: Option<Rect>,
}

/// Actions every view offers through the command palette.
const GLOBAL_ACTIONS: &str = "Ctrl+C Quit  F1 Help  F2 Details";
const SIDEBAR_WIDTH: u16 = 42;
const UNSELECTED: &str = "○";
const SELECTED: &str = "●";
const WORKTREE: &str = "◆";

/// Draws the OpenCode-style frame: a header, the content column, the prompt box
/// with its mode/status line, the hint line and, when wide, a raised sidebar.
fn shell(frame: &mut ratatui::Frame<'_>, screen: Screen<'_>, view: &mut View) -> Option<Surface> {
    let area = frame.area();
    let theme = Theme::new();
    view.hits.clear();
    view.commands.clear();
    view.scroll_area = Rect::default();
    view.toast = (!screen.message.is_empty()).then(|| screen.message.to_owned());
    frame.render_widget(Block::default().style(theme.base), area);
    if area.width < 40 || area.height < 12 {
        view.toast = None;
        draw_too_small(frame, area, theme, view);
        return None;
    }
    view.register(screen.hints, "Suggested");
    view.register(GLOBAL_ACTIONS, "System");
    if let Some(tip) = screen.home {
        draw_home(frame, area, &screen, tip, theme, view);
        return Some(Surface {
            list: Rect::default(),
            preview: None,
        });
    }
    let sidebar = (area.width >= 100 && area.height >= 20).then(|| {
        Rect::new(
            area.right() - SIDEBAR_WIDTH,
            area.y,
            SIDEBAR_WIDTH,
            area.height,
        )
    });
    let main_width = area.width - sidebar.map_or(0, |sidebar| sidebar.width);
    let pad = if main_width >= 60 { 2 } else { 1 };
    let column = Rect::new(area.x + pad, area.y, main_width - pad * 2, area.height);
    let gap = u16::from(area.height >= 16);
    let rows = Layout::vertical([
        Constraint::Length(u16::from(area.height >= 20)),
        Constraint::Length(1),
        Constraint::Length(gap),
        Constraint::Min(1),
        Constraint::Length(gap),
        Constraint::Length(prompt_height(area)),
        Constraint::Length(1),
    ])
    .split(column);
    let mut header = vec![Span::styled(screen.title.to_owned(), theme.title)];
    if !screen.subtitle.is_empty() {
        header.push(Span::styled(format!(" · {}", screen.subtitle), theme.muted));
    }
    frame.render_widget(
        Paragraph::new(clip_line(Line::from(header), column.width as usize)),
        rows[1],
    );
    draw_prompt(
        frame,
        rows[5],
        PromptBox {
            value: screen.input.value,
            placeholder: screen.input.placeholder,
            editable: screen.input.editable,
            mode: screen.mode,
            status: screen.status,
            bar: screen.bar,
        },
        theme,
    );
    draw_hint_line(frame, rows[6], screen.location, screen.hints, theme, view);
    if let Some(sidebar) = sidebar {
        frame.render_widget(
            Block::default().style(theme.base.patch(theme.surface)),
            sidebar,
        );
    }
    Some(Surface {
        list: rows[3],
        preview: sidebar,
    })
}

/// The empty-state home: centered logo, a tip, the prompt and the hint line,
/// with the version in the bottom-right corner.
fn draw_home(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    screen: &Screen<'_>,
    tip: &str,
    theme: Theme,
    view: &mut View,
) {
    let width = 75.min(area.width.saturating_sub(4));
    let x = area.x + (area.width - width) / 2;
    let prompt_rows = prompt_height(area);
    let with_logo = area.height >= LOGO_HEIGHT + prompt_rows + 8;
    let block = if with_logo { LOGO_HEIGHT + 1 } else { 0 } + 2 + prompt_rows + 1;
    let mut y = area.y + area.height.saturating_sub(block) / 2;
    if with_logo {
        frame.render_widget(
            Paragraph::new(logo_lines(theme)),
            Rect::new(
                area.x + area.width.saturating_sub(LOGO_WIDTH) / 2,
                y,
                LOGO_WIDTH.min(area.width),
                LOGO_HEIGHT,
            ),
        );
        y += LOGO_HEIGHT + 1;
    }
    let (key, rest) = tip.split_once(' ').unwrap_or((tip, ""));
    let line = Line::from(vec![
        Span::styled(display_key(key), theme.primary),
        Span::styled(format!(" {rest}"), theme.muted),
    ]);
    let tip_width = (line.width() as u16).min(area.width);
    let tip_area = Rect::new(area.x + (area.width - tip_width) / 2, y, tip_width, 1);
    frame.render_widget(Paragraph::new(line), tip_area);
    if let Some(event) = action_key(key) {
        view.hits.push(HitRegion {
            area: tip_area,
            target: HitTarget::Key(event),
        });
    }
    y += 2;
    draw_prompt(
        frame,
        Rect::new(x, y, width, prompt_rows),
        PromptBox {
            value: screen.input.value,
            placeholder: screen.input.placeholder,
            editable: screen.input.editable,
            mode: screen.mode,
            status: screen.status,
            bar: screen.bar,
        },
        theme,
    );
    draw_hint_line(
        frame,
        Rect::new(x, y + prompt_rows, width, 1),
        screen.location,
        screen.hints,
        theme,
        view,
    );
    let version = Line::raw(VERSION).width() as u16;
    if area.width > version + 2 {
        frame.render_widget(
            Paragraph::new(VERSION).style(theme.muted),
            Rect::new(area.right() - version - 2, area.bottom() - 1, version, 1),
        );
    }
}

fn draw_too_small(frame: &mut ratatui::Frame<'_>, area: Rect, theme: Theme, view: &mut View) {
    let actions = [
        (
            "ctrl+c",
            " quit",
            key_event(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ),
        (
            "esc",
            " cancel",
            key_event(KeyCode::Esc, KeyModifiers::NONE),
        ),
    ];
    let mut spans = Vec::new();
    let mut x = area.x;
    for (key, label, event) in actions {
        let width = (key.len() + label.len()) as u16;
        if x + width <= area.right() && area.height >= 2 {
            view.hits.push(HitRegion {
                area: Rect::new(x, area.y + 1, width, 1),
                target: HitTarget::Key(event),
            });
        }
        spans.push(Span::styled(key, theme.key));
        spans.push(Span::styled(format!("{label}  "), theme.muted));
        x += width + 2;
    }
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                format!("Resize to 40×12 · now {}×{}", area.width, area.height),
                theme.key,
            ),
            Line::from(spans),
        ]),
        area,
    );
}

/// The palette's descriptive name for a short hint label.
fn describe(label: &str) -> &str {
    match label {
        "Open" => "Open session",
        "New" => "New session",
        "Add" => "Add repositories",
        "Rename" => "Rename session",
        "Delete" => "Delete sessions",
        "Repos" => "Show repositories",
        "Select" => "Toggle selection",
        "Refresh" => "Refresh sessions",
        "Clear" => "Clear filter",
        "Back" => "Go back",
        "Remove link" => "Remove repository",
        "Next" => "Continue",
        "Link" => "Link repository",
        "Worktree" => "Add as worktree",
        "Cancel fetch" => "Cancel branch fetch",
        "Reuse" => "Reuse matching session",
        "Create duplicate" => "Create a duplicate session",
        "Save" => "Save name",
        "Quit" => "Quit dws",
        "Help" => "Keyboard help",
        "Details" => "Full details",
        other => other,
    }
}

/// How a shortcut is shown on screen, in OpenCode's lowercase key style.
fn display_key(key: &str) -> String {
    match key {
        "Right" => "→".into(),
        "Up/Down" => "↑↓".into(),
        other => other.to_lowercase(),
    }
}

/// The line under the prompt: the collection path on the left, then as many
/// of the view's shortcuts as fit, always ending with the command palette.
fn draw_hint_line(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    location: &str,
    hints: &str,
    theme: Theme,
    view: &mut View,
) {
    let palette = (
        "ctrl+p".to_string(),
        "commands".to_string(),
        Some(key_event(KeyCode::Char('p'), KeyModifiers::CONTROL)),
    );
    let width_of = |(key, label, _): &(String, String, Option<KeyEvent>)| {
        Line::raw(key).width() + 1 + Line::raw(label).width()
    };
    // Narrow screens keep key hints and let the path take what is left.
    let reserve = if location.is_empty() || area.width < 70 {
        0
    } else {
        Line::raw(location).width().min(16) + 2
    };
    let mut budget = (area.width as usize).saturating_sub(width_of(&palette) + reserve);
    let mut chosen = Vec::new();
    for action in hints.split("  ").filter(|action| !action.is_empty()) {
        let (key, label) = action.split_once(' ').unwrap_or((action, ""));
        let item = (display_key(key), label.to_lowercase(), action_key(key));
        let size = width_of(&item) + 2;
        if size > budget {
            break;
        }
        budget -= size;
        chosen.push(item);
    }
    chosen.push(palette);
    let total = chosen.iter().map(width_of).sum::<usize>() + 2 * (chosen.len() - 1);
    let mut x = area.right().saturating_sub(total as u16).max(area.x);
    let room = x.saturating_sub(area.x + 2) as usize;
    if room > 0 {
        frame.render_widget(
            Paragraph::new(clip_start(location, room)).style(theme.muted),
            Rect::new(area.x, area.y, room as u16, 1),
        );
    }
    for item in &chosen {
        let width = width_of(item) as u16;
        if x + width > area.right() {
            break;
        }
        let rect = Rect::new(x, area.y, width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(item.0.clone(), theme.key),
                Span::styled(format!(" {}", item.1), theme.muted),
            ])),
            rect,
        );
        if let Some(event) = item.2 {
            view.hits.push(HitRegion {
                area: rect,
                target: HitTarget::Key(event),
            });
        }
        x += width + 2;
    }
}

/// Shortens the user's home directory to `~`.
fn display_path(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && !home.as_os_str().is_empty()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~/{}", rest.display())
        };
    }
    path.display().to_string()
}

fn action_key(label: &str) -> Option<KeyEvent> {
    let (code, modifiers) = match label {
        "Enter" => (KeyCode::Enter, KeyModifiers::NONE),
        "Esc" => (KeyCode::Esc, KeyModifiers::NONE),
        "Right" => (KeyCode::Right, KeyModifiers::NONE),
        "F1" => (KeyCode::F(1), KeyModifiers::NONE),
        "F2" => (KeyCode::F(2), KeyModifiers::NONE),
        key if key.starts_with("Ctrl+") => (
            KeyCode::Char(key.chars().last()?.to_ascii_lowercase()),
            KeyModifiers::CONTROL,
        ),
        key if key.len() == 1 => (KeyCode::Char(key.chars().next()?), KeyModifiers::NONE),
        _ => return None,
    };
    Some(key_event(code, modifiers))
}

fn count_label(count: usize, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}

/// One list row: two columns of padding, an optional marker, the name and
/// right-aligned muted metadata. Focus paints the whole row.
fn row(
    marker: Option<(&'static str, Style)>,
    name: &str,
    metadata: &str,
    width: u16,
) -> Line<'static> {
    let theme = Theme::new();
    let lead = if marker.is_some() { 4 } else { 2 };
    let space = (width as usize).saturating_sub(lead + 2);
    let meta_width = if space >= 26 {
        Line::raw(metadata).width().min(space / 2)
    } else {
        0
    };
    let name_width = space.saturating_sub(meta_width + usize::from(meta_width > 0) * 2);
    let name = clip(name, name_width);
    let gap = name_width.saturating_sub(Line::raw(&name).width());
    let mut spans = vec![Span::raw("  ")];
    if let Some((glyph, style)) = marker {
        spans.push(Span::styled(glyph, style));
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled(name, theme.key));
    if meta_width > 0 {
        spans.push(Span::raw(" ".repeat(gap + 2)));
        spans.push(Span::styled(clip(metadata, meta_width), theme.muted));
    }
    Line::from(spans)
}

fn draw_list(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    rows: Vec<Line<'static>>,
    cursor: usize,
    state: &mut ListState,
    empty: &str,
) {
    let theme = Theme::new();
    if rows.is_empty() {
        state.select(None);
        frame.render_widget(
            Paragraph::new(empty)
                .style(theme.muted)
                .wrap(Wrap { trim: false }),
            Rect::new(
                area.x + 2.min(area.width),
                area.y,
                area.width.saturating_sub(2),
                area.height,
            ),
        );
        return;
    }
    state.select(Some(cursor.min(rows.len() - 1)));
    frame.render_stateful_widget(
        List::new(rows)
            .highlight_style(theme.focus)
            .scroll_padding(0),
        area,
        state,
    );
}

#[derive(Default)]
struct Details {
    title: String,
    fields: Vec<(&'static str, String)>,
    items_title: &'static str,
    items: Vec<String>,
    more_hint: &'static str,
    tree: bool,
}

impl Details {
    fn text(&self) -> String {
        let mut text = self.title.clone();
        for (label, value) in &self.fields {
            text.push_str(&format!("\n\n{label}: {value}"));
        }
        if !self.items_title.is_empty() && !self.items.is_empty() {
            text.push_str(&format!("\n\n{} ({}):", self.items_title, self.items.len()));
        }
        for item in &self.items {
            text.push_str(&format!("{}{item}", if self.tree { "\n" } else { "\n\n" }));
        }
        text
    }

    /// Draws the raised sidebar and returns the clickable "f2 details" hint.
    fn draw(&self, frame: &mut ratatui::Frame<'_>, area: Rect) -> Rect {
        let theme = Theme::new();
        frame.render_widget(
            Block::default().style(theme.base.patch(theme.surface)),
            area,
        );
        if !theme.colors {
            for y in area.top()..area.bottom() {
                frame.render_widget(
                    Paragraph::new("│").style(theme.border),
                    Rect::new(area.x, y, 1, 1),
                );
            }
        }
        let inner = Rect::new(
            area.x + 2.min(area.width),
            area.y + 1.min(area.height),
            area.width.saturating_sub(4),
            area.height.saturating_sub(2),
        );
        if inner.height < 4 || inner.width == 0 {
            return Rect::default();
        }
        let width = inner.width as usize;
        let capacity = inner.height.saturating_sub(3) as usize;
        let mut lines = wrap_lines(&self.title, width)
            .into_iter()
            .take(2)
            .map(|line| Line::styled(line, theme.title))
            .collect::<Vec<_>>();
        let field_limit = if self.items.is_empty() {
            capacity
        } else {
            capacity.saturating_sub(4)
        };
        for (label, value) in &self.fields {
            if lines.len() + 2 > field_limit {
                break;
            }
            lines.push(Line::raw(""));
            lines.push(Line::styled(*label, theme.title));
            for line in wrap_lines(value, width) {
                if lines.len() >= field_limit {
                    break;
                }
                lines.push(Line::styled(line, theme.muted));
            }
        }
        if !self.items.is_empty() && lines.len() + 2 < capacity {
            lines.push(Line::raw(""));
            if !self.items_title.is_empty() {
                lines.push(Line::styled(
                    if self.tree {
                        self.items_title.to_string()
                    } else {
                        format!("{} ({})", self.items_title, self.items.len())
                    },
                    theme.title,
                ));
            }
            let room = capacity.saturating_sub(lines.len());
            let shown = if self.items.len() <= room {
                self.items.len()
            } else {
                room.saturating_sub(1)
            };
            for item in self.items.iter().take(shown) {
                let summary = item.lines().next().unwrap_or_default();
                lines.push(if self.tree {
                    Line::styled(clip(summary, width), theme.muted)
                } else {
                    clip_line(
                        Line::from(vec![
                            Span::styled("• ", item_tone(summary, theme)),
                            Span::styled(summary.to_owned(), theme.key),
                        ]),
                        width,
                    )
                });
            }
            if shown < self.items.len() {
                lines.push(Line::styled(
                    clip(
                        &format!("+{} more · {}", self.items.len() - shown, self.more_hint),
                        width,
                    ),
                    theme.muted,
                ));
            }
        }
        frame.render_widget(
            Paragraph::new(lines),
            Rect::new(inner.x, inner.y, inner.width, capacity as u16),
        );
        let hint = Rect::new(inner.x, inner.bottom() - 2, 10.min(inner.width), 1);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("f2", theme.key),
                Span::styled(" details", theme.muted),
            ])),
            hint,
        );
        frame.render_widget(
            Paragraph::new(brand_line(theme)),
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        );
        hint
    }
}

/// Bullet color for a repository summary: links green, worktrees purple,
/// missing paths red.
fn item_tone(summary: &str, theme: Theme) -> Style {
    let parts = summary.split(" · ").collect::<Vec<_>>();
    if parts.contains(&"missing") {
        theme.error
    } else if parts.iter().any(|part| part.starts_with("worktree")) {
        theme.accent
    } else if parts.contains(&"link") || parts.len() > 1 {
        theme.success
    } else {
        theme.muted
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TextStyle {
    /// Help: "Key: description" lines become description-left, key-right rows.
    Keybinds,
    /// Details and confirmations: a bold first line, then "Label: value" lines.
    Fields,
}

fn styled_text(text: &str, style: TextStyle, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (index, source) in text.split('\n').enumerate() {
        if source.is_empty() {
            lines.push(Line::raw(""));
            continue;
        }
        if let Some(heading) = source
            .strip_prefix("# ")
            .filter(|_| style == TextStyle::Keybinds)
        {
            lines.push(Line::styled(clip(heading, width), theme.accent));
            continue;
        }
        if index == 0 && style == TextStyle::Fields {
            lines.extend(
                wrap_lines(source, width)
                    .into_iter()
                    .map(|line| Line::styled(line, theme.title)),
            );
            continue;
        }
        let pair = source
            .split_once(": ")
            .filter(|(key, _)| Line::raw(*key).width() <= 36 && !key.contains(". "));
        match (style, pair) {
            (TextStyle::Keybinds, Some((key, description))) => {
                let key = key.to_lowercase();
                let key_width = Line::raw(&key).width();
                let text_width = width.saturating_sub(key_width + 2).max(width / 2);
                for (part_index, part) in
                    wrap_lines(description, text_width).into_iter().enumerate()
                {
                    let part_width = Line::raw(&part).width();
                    if part_index == 0 && part_width + 2 + key_width <= width {
                        lines.push(Line::from(vec![
                            Span::styled(part, theme.key),
                            Span::raw(" ".repeat(width - part_width - key_width)),
                            Span::styled(key.clone(), theme.muted),
                        ]));
                    } else {
                        lines.push(Line::styled(part, theme.key));
                    }
                }
            }
            (TextStyle::Fields, Some((label, _))) => {
                let prefix = format!("{label}: ");
                for (part_index, part) in wrap_lines(source, width).into_iter().enumerate() {
                    match part.strip_prefix(&prefix).filter(|_| part_index == 0) {
                        Some(rest) => lines.push(Line::from(vec![
                            Span::styled(prefix.clone(), theme.muted),
                            Span::styled(rest.to_owned(), theme.key),
                        ])),
                        None => lines.push(Line::styled(part, theme.key)),
                    }
                }
            }
            (TextStyle::Keybinds, None) => lines.extend(
                wrap_lines(source, width)
                    .into_iter()
                    .map(|line| Line::styled(line, theme.muted)),
            ),
            (TextStyle::Fields, None) => lines.extend(
                wrap_lines(source, width)
                    .into_iter()
                    .map(|line| Line::styled(line, theme.key)),
            ),
        }
    }
    lines
}

fn navigate(cursor: &mut usize, key: KeyCode, len: usize, page: u16) -> bool {
    *cursor = match key {
        KeyCode::Home => 0,
        KeyCode::End => len.saturating_sub(1),
        KeyCode::PageUp => cursor.saturating_sub(page.max(1) as usize),
        KeyCode::PageDown => cursor
            .saturating_add(page.max(1) as usize)
            .min(len.saturating_sub(1)),
        _ => return false,
    };
    true
}

const BROWSE_HINTS: &str = "Enter Open  Ctrl+N New  Ctrl+A Add  Ctrl+E Rename  Ctrl+D Delete  Right Repos  Ctrl+S Select  Ctrl+R Refresh";
const FILTERED_BROWSE_HINTS: &str = "Enter Open  Esc Clear  Ctrl+N New  Ctrl+A Add  Ctrl+E Rename  Ctrl+D Delete  Right Repos  Ctrl+S Select  Ctrl+R Refresh";
const PICKER_HELP: &str = "# Choose repositories
Ctrl+P: Search commands
Enter: Continue, choose a branch or create the session
Ctrl+S: Link the repository as it is
Ctrl+W: Add the repository as a managed worktree
Esc: Clear the filter, go back or cancel

# Navigation
Type: Filter; every printable key is search text
↑↓: Move
Home/End: First or last
PgUp/PgDn: Page
F2: Full details and errors
Ctrl+C: Quit

# Mouse
Click: Focus a row; click its marker to link it
Double-click: Choose a branch
Wheel: Scroll

Choosing a worktree fetches origin first; Esc cancels the fetch.
Source repositories are never deleted.";
const MANAGER_HELP: &str = "# Sessions
Ctrl+P: Search commands
Enter: Open the highlighted session
Ctrl+N: New session
Ctrl+A: Add repositories to the highlighted session
Ctrl+E: Rename the selected or highlighted session
Ctrl+D: Delete the selected or highlighted sessions
Ctrl+S: Toggle selection; it survives filtering
Right: Show repositories; Left or Esc returns
Ctrl+R: Refresh after changes elsewhere

# Navigation
Type: Filter sessions; Esc clears the filter
↑↓: Move
Home/End: First or last
PgUp/PgDn: Page
F2: Full details and errors
Ctrl+C: Quit

# Mouse
Click: Focus a row; click its marker to select it
Double-click: Open the highlighted session
Wheel: Scroll

Deleting confirms the exact targets; Enter, Esc or n cancels.
Deleting a session keeps source repositories and managed worktrees.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::GitRepoStatus;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn palette_search_preserves_session_state_and_dispatches_existing_actions() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut manager = SessionManagerApp::new(
            vec![manager_session("alpha"), manager_session("beta")],
            store,
        );
        manager.filter = "alpha".into();
        manager.selected.insert("beta".into());
        for (w, h) in [(120, 40), (100, 20), (80, 24), (60, 16), (40, 12)] {
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            terminal.draw(|frame| manager.draw(frame)).unwrap();
            manager.handle_key(key_event(KeyCode::Char('p'), KeyModifiers::CONTROL));
            terminal.draw(|frame| manager.draw(frame)).unwrap();
            capture(&terminal, &format!("palette-all-{w}x{h}"));
            for c in "new".chars() {
                manager.handle_key(key_event(KeyCode::Char(c), KeyModifiers::NONE));
            }
            terminal.draw(|frame| manager.draw(frame)).unwrap();
            let text = buffer_text(&terminal);
            assert!(text.contains("Commands") && text.contains("new"));
            assert!(buffer_text(&terminal).contains("New"));
            assert!(matches!(
                manager.handle_key(key_event(KeyCode::Enter, KeyModifiers::NONE)),
                SessionManagerSignal::New
            ));
            assert!(manager.view.palette.is_none());
            assert_eq!(manager.filter, "alpha");
            assert!(manager.selected.contains("beta"));
            capture(&terminal, &format!("palette-{w}x{h}"));
        }
    }

    #[test]
    fn palette_mouse_scrolling_and_confirmation_cannot_bypass_safety() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut manager = SessionManagerApp::new(vec![manager_session("alpha")], store);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(40, 12)).unwrap();
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        let key = manager
            .input_key(click(action_area(&manager.view, KeyCode::Char('p'))))
            .unwrap();
        manager.handle_key(key);
        manager.handle_key(key_event(KeyCode::End, KeyModifiers::NONE));
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        assert!(buffer_text(&terminal).contains("Full details"));
        for c in "quit".chars() {
            manager.handle_key(key_event(KeyCode::Char(c), KeyModifiers::NONE));
        }
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        let area = manager
            .view
            .hits
            .iter()
            .find_map(|hit| match hit.target {
                HitTarget::Command(key) if key.code == KeyCode::Char('c') => Some(hit.area),
                _ => None,
            })
            .unwrap();
        let key = manager.input_key(click(area)).unwrap();
        assert!(matches!(
            manager.handle_key(key),
            SessionManagerSignal::Done
        ));
        manager.start_remove();
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        manager.handle_key(key_event(KeyCode::Char('p'), KeyModifiers::CONTROL));
        for c in "confirm".chars() {
            manager.handle_key(key_event(KeyCode::Char(c), KeyModifiers::NONE));
        }
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        assert!(buffer_text(&terminal).contains("No matching actions"));
        manager.handle_key(key_event(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            manager.mode,
            SessionManagerMode::ConfirmRemoveSessions { .. }
        ));
        manager.handle_key(key_event(KeyCode::Esc, KeyModifiers::NONE));
        manager.handle_key(key_event(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(manager.mode, SessionManagerMode::Browse));
        assert_eq!(manager.sessions.len(), 1);
    }

    #[test]
    fn palette_keeps_onboarding_text_and_selections_and_handles_empty_and_resize() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut app = App::new(vec![], store);
        app.mode = Mode::Name;
        app.session_name = "my-workspace".into();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(60, 16)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        app.handle_key(key_event(KeyCode::Char('p'), KeyModifiers::CONTROL))
            .unwrap();
        for c in "no-such-action".chars() {
            app.handle_key(key_event(KeyCode::Char(c), KeyModifiers::NONE))
                .unwrap();
        }
        terminal.draw(|frame| app.draw(frame)).unwrap();
        app.handle_key(key_event(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert!(app.view.palette.is_some());
        terminal.resize(Rect::new(0, 0, 30, 8)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        app.handle_key(key_event(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        app.handle_key(key_event(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(app.view.palette.is_none());
        assert!(matches!(app.mode, Mode::Name));
        assert_eq!(app.session_name, "my-workspace");
        terminal.resize(Rect::new(0, 0, 60, 16)).unwrap();
        app.mode = Mode::BranchLoading;
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert!(
            !app.view
                .commands
                .iter()
                .any(|command| command.key.code == KeyCode::Char('w'))
        );
        app.handle_key(key_event(KeyCode::Char('p'), KeyModifiers::CONTROL))
            .unwrap();
        app.view.palette.as_mut().unwrap().cursor = usize::MAX;
        terminal.draw(|frame| app.draw(frame)).unwrap();
        app.handle_key(key_event(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert!(app.view.palette.is_none());
        assert!(app.view.overlay.is_some());
    }

    #[test]
    fn folder_tree_is_bounded_sorted_and_does_not_follow_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("src/nested")).unwrap();
        fs::write(temp.path().join("src/main.rs"), "").unwrap();
        fs::write(temp.path().join("src/nested/deeper.txt"), "").unwrap();
        fs::create_dir_all(temp.path().join("node_modules/package")).unwrap();
        fs::write(temp.path().join("node_modules/package/never-shown.txt"), "").unwrap();
        fs::write(temp.path().join("z.txt"), "").unwrap();
        std::os::unix::fs::symlink(temp.path(), temp.path().join("cycle")).unwrap();
        let lines = folder_tree(temp.path());
        let text = lines.join("\n");
        assert!(text.contains("src/") && text.contains("main.rs"));
        assert!(text.contains("nested/ …"));
        assert!(text.contains("node_modules/ …") && !text.contains("never-shown"));
        assert!(text.contains("cycle@") && !text.contains("deeper.txt"));
        assert!(text.find("src/").unwrap() < text.find("z.txt").unwrap());
        for i in 0..200 {
            fs::write(temp.path().join(format!("entry-{i:03}")), "").unwrap();
        }
        let lines = folder_tree(temp.path());
        assert!(lines.len() <= 64);
        assert!(lines.last().unwrap().contains("preview limit"));
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(folder_tree(empty.path()), ["(empty folder)"]);
        assert!(
            folder_tree(&empty.path().join("missing"))
                .join("")
                .contains("Cannot read folder")
        );
    }

    #[test]
    fn active_folder_preview_loads_off_render_and_survives_resize_and_removal() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir_all(first.join("src")).unwrap();
        fs::create_dir(&second).unwrap();
        fs::write(first.join("src/first.rs"), "").unwrap();
        fs::write(second.join("second.rs"), "").unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut app = App::for_add(
            vec![
                RepoCandidate {
                    name: "first".into(),
                    path: first.clone(),
                    git: None,
                },
                RepoCandidate {
                    name: "second".into(),
                    path: second.clone(),
                    git: None,
                },
            ],
            store,
            "existing".into(),
        );
        app.selected.insert(1);
        assert!(app.item_details().text().contains("Loading folder tree"));
        assert!(app.folder_tree.pending.is_none());
        app.handle_key(key_event(KeyCode::F(2), KeyModifiers::NONE))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !app.folder_tree.cache.contains_key(&first) {
            app.refresh_folder_tree();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(app.view.overlay.as_ref().unwrap().1.contains("first.rs"));
        assert!(!app.item_details().text().contains("second.rs"));
        app.view.overlay = None;
        fs::remove_dir_all(&first).unwrap();
        for (w, h) in [(120, 40), (100, 20), (60, 16), (40, 12)] {
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            assert!(app.item_details().text().contains("first.rs"));
            if w >= 100 {
                assert!(buffer_text(&terminal).contains("first.rs"));
            }
            capture(&terminal, &format!("folder-tree-{w}x{h}"));
        }
        app.cursor = 1;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !app.folder_tree.cache.contains_key(&second) {
            app.refresh_folder_tree();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(app.item_details().text().contains("second.rs"));
        assert!(!app.item_details().text().contains("first.rs"));
        assert!(app.selected.contains(&1));
    }

    fn click(area: Rect) -> Event {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn action_area(view: &View, code: KeyCode) -> Rect {
        view.hits
            .iter()
            .find_map(|hit| match hit.target {
                HitTarget::Key(key) if key.code == code => Some(hit.area),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing clickable {code:?}"))
    }

    fn row_area(view: &View, index: usize, checkbox: bool) -> Rect {
        view.hits
            .iter()
            .find_map(|hit| match hit.target {
                HitTarget::Row {
                    index: row,
                    checkbox: check,
                } if row == index && check == checkbox => Some(hit.area),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn mouse_focus_selection_double_click_and_resize_use_visible_rows() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut manager = SessionManagerApp::new(
            (0..120)
                .map(|i| manager_session(&format!("session-{i:03}")))
                .collect(),
            store,
        );
        manager.cursor = 119;
        manager.selected.insert("session-000".into());
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        let index = manager.view.list.offset();
        assert!(index > 0);
        assert!(
            manager
                .input_key(click(row_area(&manager.view, index, false)))
                .is_none()
        );
        assert_eq!(manager.cursor, index);
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        assert_eq!(manager.view.list.offset(), index);
        let key = manager
            .input_key(click(row_area(&manager.view, index, true)))
            .unwrap();
        manager.handle_key(key);
        assert_eq!(manager.selected.len(), 2);
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        let area = row_area(&manager.view, index, false);
        assert!(manager.input_key(click(area)).is_none());
        let key = manager.input_key(click(area)).unwrap();
        assert!(
            matches!(manager.handle_key(key), SessionManagerSignal::Open { session_name } if session_name == format!("session-{index:03}"))
        );
        assert_eq!(manager.selected.len(), 2);
        manager.input_key(Event::Resize(40, 12));
        assert!(manager.view.hits.is_empty());
        terminal.resize(Rect::new(0, 0, 40, 12)).unwrap();
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        let event = Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: manager.view.scroll_area.x,
            row: manager.view.scroll_area.y,
            modifiers: KeyModifiers::NONE,
        });
        let key = manager.input_key(event).unwrap();
        manager.handle_key(key);
        assert_eq!(manager.cursor, index + 1);
        assert_eq!(manager.selected.len(), 2);
    }

    #[test]
    fn mouse_actions_fit_every_layout_and_ignore_release_drag_and_right_click() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut manager = SessionManagerApp::new(vec![manager_session("one")], store);
        for (width, height) in [(120, 40), (100, 20), (80, 24), (60, 16), (40, 12)] {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| manager.draw(frame)).unwrap();
            for code in [
                KeyCode::Enter,
                KeyCode::Char('n'),
                KeyCode::Char('a'),
                KeyCode::Char('e'),
                KeyCode::Char('d'),
                KeyCode::Right,
                KeyCode::Char('s'),
                KeyCode::Char('c'),
                KeyCode::F(1),
                KeyCode::F(2),
            ] {
                assert!(
                    manager
                        .view
                        .commands
                        .iter()
                        .any(|command| command.key.code == code),
                    "{code:?} missing from the palette at {width}x{height}"
                );
            }
            let palette = action_area(&manager.view, KeyCode::Char('p'));
            assert!(
                palette.right() <= width && palette.bottom() <= height,
                "{width}x{height}"
            );
            for kind in [
                MouseEventKind::Up(MouseButton::Left),
                MouseEventKind::Drag(MouseButton::Left),
                MouseEventKind::Down(MouseButton::Right),
            ] {
                assert!(
                    manager
                        .input_key(Event::Mouse(MouseEvent {
                            kind,
                            column: 2,
                            row: 2,
                            modifiers: KeyModifiers::NONE
                        }))
                        .is_none()
                );
            }
        }
    }

    #[test]
    fn mouse_confirmation_blocks_click_through_and_requires_a_separate_click() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        store.create_session("one", &[repo], None, true).unwrap();
        let mut manager = SessionManagerApp::new(store.load_sessions().unwrap(), store.clone());
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        manager.handle_key(key_event(KeyCode::Char('p'), KeyModifiers::CONTROL));
        for c in "delete".chars() {
            manager.handle_key(key_event(KeyCode::Char(c), KeyModifiers::NONE));
        }
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        let delete = manager
            .view
            .hits
            .iter()
            .find_map(|hit| match hit.target {
                HitTarget::Command(key) if key.code == KeyCode::Char('d') => Some(hit.area),
                _ => None,
            })
            .unwrap();
        let key = manager.input_key(click(delete)).unwrap();
        manager.handle_key(key);
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        assert!(
            !manager
                .view
                .hits
                .iter()
                .any(|hit| matches!(hit.target, HitTarget::Row { .. }))
        );
        let confirm = click(action_area(&manager.view, KeyCode::Char('y')));
        assert!(manager.input_key(confirm).is_none());
        assert_eq!(store.load_sessions().unwrap().len(), 1);
        manager.view.last_action = None;
        let key = manager
            .input_key(click(action_area(&manager.view, KeyCode::Esc)))
            .unwrap();
        manager.handle_key(key);
        assert!(matches!(manager.mode, SessionManagerMode::Browse));
        assert_eq!(store.load_sessions().unwrap().len(), 1);
        manager.start_remove();
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        manager.view.last_action = None;
        let key = manager
            .input_key(click(action_area(&manager.view, KeyCode::Char('y'))))
            .unwrap();
        manager.handle_key(key);
        assert!(store.load_sessions().unwrap().is_empty());
    }

    #[test]
    fn mouse_picker_selection_branch_acceptance_and_overlay_close_share_keys() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut picker = App::new(
            vec![RepoCandidate {
                name: "api".into(),
                path: temp.path().join("api"),
                git: None,
            }],
            store,
        );
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(60, 16)).unwrap();
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        let key = picker
            .input_key(click(row_area(&picker.view, 0, true)))
            .unwrap();
        picker.handle_key(key).unwrap();
        assert!(picker.selected.contains(&0));
        picker.mode = Mode::Branch(BranchPicker {
            repo_indices: vec![0],
            current: 0,
            branches: vec![git::OriginBranch {
                name: "main".into(),
                remote_ref: "origin/main".into(),
            }],
            filter: String::new(),
        });
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        let area = row_area(&picker.view, 0, false);
        assert!(picker.input_key(click(area)).is_none());
        let key = picker.input_key(click(area)).unwrap();
        picker.handle_key(key).unwrap();
        assert!(matches!(picker.mode, Mode::Name));
        picker
            .handle_key(key_event(KeyCode::F(2), KeyModifiers::NONE))
            .unwrap();
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        let key = picker
            .input_key(click(action_area(&picker.view, KeyCode::Esc)))
            .unwrap();
        picker.handle_key(key).unwrap();
        assert!(picker.view.overlay.is_none());
        assert!(matches!(picker.mode, Mode::Name));
    }

    #[test]
    fn preview_reports_overflow_and_details_retain_every_item() {
        let details = Details {
            title: "Many repositories".into(),
            fields: vec![("Description", "A long description ".repeat(100))],
            items_title: "Repositories",
            items: (0..200).map(|i| format!("entry-{i:03}")).collect(),
            more_hint: "Right All repositories",
            tree: false,
        };
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(44, 15)).unwrap();
        terminal
            .draw(|frame| {
                details.draw(frame, frame.area());
            })
            .unwrap();
        let text = buffer_text(&terminal);
        let shown = text.matches("entry-").count();
        assert!(shown > 0 && shown < 200);
        assert!(text.contains(&format!("+{} more", 200 - shown)));
        assert!(text.contains("Right All repositories"));
        assert!(details.text().contains("entry-199"));
        assert_eq!(
            wrap_lines("background worker", 12),
            ["background", "worker"]
        );
    }

    #[test]
    fn resize_preserves_focus_selections_filter_and_feedback() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut manager = SessionManagerApp::new(
            (0..120)
                .map(|i| manager_session(&format!("session-{i:03}")))
                .collect(),
            store,
        );
        manager.filter = "session".into();
        manager.cursor = 119;
        manager.selected.insert("session-000".into());
        manager.message = "Error: changed elsewhere; Ctrl+R refreshes".into();
        for width in [39, 40, 41, 59, 60, 61, 80, 99, 100, 101, 120] {
            for height in [11, 12, 15, 16, 19, 20, 24, 40] {
                let mut terminal =
                    Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| manager.draw(frame)).unwrap();
                let text = buffer_text(&terminal);
                if width >= 40 && height >= 12 {
                    assert!(text.contains("session-119"), "{width}x{height}");
                    assert!(text.contains("1 selected"));
                    assert!(text.contains("Error:"));
                    assert!(text.contains("ctrl+p commands"));
                    assert_eq!(
                        text.contains(&format!("dynws {VERSION}")),
                        width >= 100 && height >= 20
                    );
                }
                assert_eq!(manager.cursor, 119);
                assert_eq!(manager.filter, "session");
                assert!(manager.selected.contains("session-000"));
            }
        }
    }

    #[test]
    fn details_follow_focus_instead_of_bulk_selection_and_expanded_parent() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut alpha = manager_session("alpha");
        alpha.description = Some("Focused description".into());
        alpha.repos = vec![
            RepoLink::link("first", "/first"),
            RepoLink::link("second", "/目录/é/🧑‍💻/second"),
        ];
        let mut manager = SessionManagerApp::new(vec![alpha, manager_session("beta")], store);
        manager.selected.insert("beta".into());
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        assert!(buffer_text(&terminal).contains("Focused description"));
        manager.mode = SessionManagerMode::Expanded {
            session_name: "alpha".into(),
            repo_cursor: 1,
        };
        let details = manager.item_details();
        assert_eq!(details.title, "second");
        assert!(details.text().contains("/目录/é/🧑‍💻/second"));
        assert!(!details.text().contains("/first"));
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        assert!(buffer_text(&terminal).contains("2 repositories"));
    }

    #[test]
    fn unicode_clipping_and_long_input_keep_the_tail_and_caret_visible() {
        assert_eq!(clip("étail", 2), "é…");
        assert_eq!(clip("🧑‍💻tail", 3), "🧑‍💻…");
        assert_eq!(clip("目录", 3), "目…");
        assert_eq!(wrap_lines("é🧑‍💻目录", 2), ["é", "🧑‍💻", "目", "录"]);
        let input = format!("{}é🧑‍💻尾部END", "long-".repeat(100));
        for width in [40, 60, 80, 120] {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, 12)).unwrap();
            terminal
                .draw(|frame| {
                    draw_input(
                        frame,
                        Rect::new(0, 1, width, 1),
                        Input::new("Filter:", &input),
                        Theme::for_colors(true),
                    )
                })
                .unwrap();
            assert!(buffer_text(&terminal).replace(' ', "").contains("尾部END"));
            let cursor = terminal.get_cursor_position().unwrap();
            assert!(cursor.x < width && cursor.y == 1);
        }
        let plain = Theme::for_colors(false);
        assert!(plain.focus.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(plain.base.fg, None);
        assert_eq!(plain.surface.bg, None);
    }

    #[test]
    fn fetching_allows_filtering_and_navigation_but_guards_selection() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let repos = (0..120)
            .map(|i| RepoCandidate {
                name: format!("repo-{i:03}"),
                path: temp.path().join(format!("repo-{i:03}")),
                git: None,
            })
            .collect();
        let mut app = App::new(repos, store);
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        app.mode = Mode::BranchLoading;
        app.branch_fetch = Some(BranchFetch {
            repo_indices: vec![0],
            current: 0,
            repo_name: "repo-000".into(),
            task: GitBranchTask {
                repo_path: temp.path().into(),
                child: Some(child),
                phase: GitTaskPhase::FetchPrune,
            },
        });
        app.selected.insert(0);
        for _ in 0..2 {
            app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
                .unwrap();
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
                .unwrap();
        }
        assert_eq!(app.selected.len(), 1);
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.cursor, 1);
        for character in "missing".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))
                .unwrap();
        }
        assert_eq!(app.filter, "missing");
        assert!(app.filtered_indices().is_empty());
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert!(buffer_text(&terminal).contains("No repositories match"));
        assert!(buffer_text(&terminal).contains("Fetching origin"));
        for _ in 0..7 {
            app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))
                .unwrap();
        }
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.cursor, 1);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(app.branch_fetch.is_none());
        assert!(matches!(app.mode, Mode::Select));
    }

    #[test]
    fn repeated_delete_keys_cannot_delete_another_session() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        store
            .create_session("one", std::slice::from_ref(&repo), None, true)
            .unwrap();
        store.create_session("two", &[repo], None, true).unwrap();
        let mut manager = SessionManagerApp::new(store.load_sessions().unwrap(), store.clone());
        for _ in 0..2 {
            manager.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        }
        for _ in 0..2 {
            manager.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        }
        assert!(store.load_session("one").is_err());
        assert!(store.load_session("two").is_ok());
        assert_eq!(manager.changes.len(), 1);
    }

    #[test]
    fn capture_realistic_responsive_screens_and_all_workflow_states() {
        let temp = tempfile::tempdir().unwrap();
        let mut paths = DynwsPaths::from_home(temp.path().join(".dynws"));
        paths.workspaces_dir = std::path::PathBuf::from("/code/.dynws/workspaces");
        let store = SessionStore::new(paths);
        let mut sessions = Vec::new();
        for name in [
            "api-review",
            "billing-refactor",
            "incident-482",
            "release-check",
            "search-index",
            "worker-retry",
        ] {
            let mut session = manager_session(name);
            session.description = Some(format!(
                "{} across the API and background worker",
                name.replace('-', " ")
            ));
            session.repos = vec![
                RepoLink::link("api", "/code/services/api"),
                RepoLink {
                    name: "worker".into(),
                    path: "/code/.dynws/worktrees/worker/fix-retry".into(),
                    kind: RepoKind::Worktree,
                    source_repo: Some("/code/services/worker".into()),
                    remote_ref: Some("origin/fix-retry".into()),
                    local_branch: Some("fix-retry".into()),
                },
            ];
            sessions.push(session);
        }
        let mut manager = SessionManagerApp::new(sessions, store.clone());
        manager.present_paths.extend(
            manager
                .sessions
                .iter()
                .flat_map(|s| s.repos.iter().map(|r| r.path.clone())),
        );
        manager.selected.insert("incident-482".into());
        for (width, height) in [
            (120, 40),
            (100, 20),
            (99, 20),
            (100, 19),
            (80, 24),
            (60, 16),
            (40, 12),
        ] {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| manager.draw(frame)).unwrap();
            capture(&terminal, &format!("polish-manager-{width}x{height}"));
        }
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        manager.mode = SessionManagerMode::Expanded {
            session_name: "api-review".into(),
            repo_cursor: 1,
        };
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        capture(&terminal, "polish-expanded");
        manager.mode = SessionManagerMode::Rename {
            session_name: "api-review".into(),
            input: "api-review-updated".into(),
        };
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        capture(&terminal, "polish-rename");
        manager.start_remove();
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        capture(&terminal, "polish-confirm");
        assert!(buffer_text(&terminal).contains("Delete session"));
        assert!(!buffer_text(&terminal).contains("api review across"));
        manager.mode = SessionManagerMode::Browse;
        manager.message = "Error: session changed elsewhere; Ctrl+R refreshes".into();
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        capture(&terminal, "polish-error");
        manager.view.show("Help", MANAGER_HELP.into());
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        capture(&terminal, "polish-help");
        manager.view.overlay = None;
        manager.message.clear();
        manager.sessions.clear();
        manager.selected.clear();
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        capture(&terminal, "polish-empty");
        let repos = ["api", "dashboard", "worker"]
            .iter()
            .map(|name| RepoCandidate {
                name: (*name).into(),
                path: std::path::PathBuf::from(format!("/code/services/{name}")),
                git: Some(GitRepoStatus {
                    branch: "main".into(),
                    dirty: *name == "api",
                }),
            })
            .collect();
        let mut picker = App::new(repos, store);
        picker.selected.insert(0);
        picker.worktree_selected.insert(2);
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        capture(&terminal, "polish-new");
        picker.purpose = AppPurpose::AddRepos {
            session_name: "api-review".into(),
        };
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        capture(&terminal, "polish-add");
        picker.mode = Mode::Branch(BranchPicker {
            repo_indices: vec![2],
            current: 0,
            branches: vec![
                git::OriginBranch {
                    name: "fix-retry".into(),
                    remote_ref: "origin/fix-retry".into(),
                },
                git::OriginBranch {
                    name: "main".into(),
                    remote_ref: "origin/main".into(),
                },
            ],
            filter: String::new(),
        });
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        capture(&terminal, "polish-branch");
        picker.purpose = AppPurpose::CreateSession;
        picker.mode = Mode::Name;
        picker.session_name = "worker-investigation".into();
        picker.worktree_choices.push(WorktreeChoice {
            repo_idx: 2,
            branch: git::OriginBranch {
                name: "fix-retry".into(),
                remote_ref: "origin/fix-retry".into(),
            },
        });
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        capture(&terminal, "polish-name");
        picker.mode = Mode::Duplicate {
            existing: manager_session("existing-review"),
        };
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        capture(&terminal, "polish-duplicate");
        picker.mode = Mode::BranchLoading;
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        picker.branch_fetch = Some(BranchFetch {
            repo_indices: vec![2],
            current: 0,
            repo_name: "worker".into(),
            task: GitBranchTask {
                repo_path: temp.path().into(),
                child: Some(child),
                phase: GitTaskPhase::FetchPrune,
            },
        });
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        capture(&terminal, "polish-loading");
        picker.branch_fetch = None;
    }

    #[test]
    fn branch_enter_accepts_and_fetch_cancellation_reaps_its_child() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let repos = vec![RepoCandidate {
            name: "api".into(),
            path: temp.path().join("api"),
            git: None,
        }];
        let mut picker = App::new(repos, store);
        picker.mode = Mode::Branch(BranchPicker {
            repo_indices: vec![0],
            current: 0,
            branches: vec![git::OriginBranch {
                name: "main".into(),
                remote_ref: "origin/main".into(),
            }],
            filter: String::new(),
        });
        picker
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert!(matches!(picker.mode, Mode::Name));
        assert_eq!(picker.worktree_choices[0].branch.name, "main");
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let mut task = GitBranchTask {
            repo_path: temp.path().into(),
            child: Some(child),
            phase: GitTaskPhase::FetchPrune,
        };
        task.cancel();
        assert!(task.child.is_none());
        assert!(
            !Command::new("sh")
                .args(["-c", &format!("kill -0 {pid}")])
                .output()
                .unwrap()
                .status
                .success()
        );
    }

    #[test]
    fn complete_errors_and_hidden_targets_remain_reachable_through_help_details() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut manager = SessionManagerApp::new(Vec::new(), store);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(60, 16)).unwrap();
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        capture(&terminal, "empty-60x16");
        assert!(buffer_text(&terminal).contains("to create your first session"));
        manager.message = format!("Error: {} THE_END", "long path /目录/ ".repeat(100));
        manager.handle_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
        for _ in 0..200 {
            manager.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        }
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        capture(&terminal, "details-60x16");
        assert!(buffer_text(&terminal).contains("THE_END"));
        manager.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(manager.view.overlay.is_none());
        manager.handle_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        capture(&terminal, "help-60x16");
        assert!(buffer_text(&terminal).contains("ctrl+n"));
        manager.view.overlay = None;
        manager.sessions = vec![manager_session("one"), manager_session("two")];
        manager.selected.extend(["one".into(), "two".into()]);
        manager.start_remove();
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        capture(&terminal, "confirm-60x16");
    }

    fn capture(terminal: &Terminal<ratatui::backend::TestBackend>, name: &str) {
        if let Ok(dir) = std::env::var("DWS_CAPTURE_DIR") {
            std::fs::create_dir_all(&dir).unwrap();
            let size = terminal.size().unwrap();
            let cells = terminal.backend().buffer().content.iter().map(|cell| serde_json::json!({
                "symbol": cell.symbol(), "fg": format!("{:?}", cell.fg), "bg": format!("{:?}", cell.bg),
                "reverse": cell.modifier.contains(Modifier::REVERSED), "bold": cell.modifier.contains(Modifier::BOLD),
            })).collect::<Vec<_>>();
            std::fs::write(
                format!("{dir}/{name}.json"),
                serde_json::to_vec(
                    &serde_json::json!({ "width":size.width,"height":size.height,"cells":cells }),
                )
                .unwrap(),
            )
            .unwrap();
        }
    }

    fn buffer_text(terminal: &Terminal<ratatui::backend::TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content
            .chunks(terminal.size().unwrap().width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn every_list_keeps_the_last_of_many_items_visible_and_can_resize() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let repos = (0..100)
            .map(|i| RepoCandidate {
                name: format!("repo-{i:03}"),
                path: temp.path().join(format!("repo-{i:03}")),
                git: None,
            })
            .collect();
        let mut picker = App::new(repos, store.clone());
        picker.cursor = 99;
        let sessions = (0..100)
            .map(|i| manager_session(&format!("session-{i:03}")))
            .collect();
        let mut manager = SessionManagerApp::new(sessions, store);
        manager.cursor = 99;
        for (width, height) in [
            (120, 40),
            (100, 28),
            (80, 24),
            (70, 20),
            (60, 16),
            (50, 14),
            (40, 12),
        ] {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| picker.draw(frame)).unwrap();
            assert!(buffer_text(&terminal).contains("repo-099"));
            assert!(buffer_text(&terminal).contains("100 / 100 · 0 selected · 0 hidden"));
            assert!(picker.view.list.offset() > 0);
            terminal.draw(|frame| manager.draw(frame)).unwrap();
            let text = buffer_text(&terminal);
            capture(&terminal, &format!("manager-{width}x{height}"));
            assert!(text.contains("session-099"));
            assert!(text.contains("ctrl+p commands"));
            if let Ok(dir) = std::env::var("DWS_CAPTURE_DIR") {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(format!("{dir}/manager-{width}x{height}.txt"), &text).unwrap();
                std::fs::write(
                    format!("{dir}/manager-{width}x{height}.buffer"),
                    format!("{:?}", terminal.backend().buffer()),
                )
                .unwrap();
            }
        }
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(30, 8)).unwrap();
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        capture(&terminal, "resize-30x8");
        assert!(buffer_text(&terminal).contains("Resize to 40"));
        let signal = manager.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert!(matches!(signal, SessionManagerSignal::Continue));
        assert!(matches!(manager.mode, SessionManagerMode::Browse));
        picker.mode = Mode::Branch(BranchPicker {
            repo_indices: vec![0],
            current: 0,
            branches: (0..100)
                .map(|i| git::OriginBranch {
                    name: format!("branch-{i:03}"),
                    remote_ref: format!("origin/branch-{i:03}"),
                })
                .collect(),
            filter: String::new(),
        });
        picker.cursor = 99;
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(60, 16)).unwrap();
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        capture(&terminal, "branch-60x16");
        assert!(buffer_text(&terminal).contains("branch-099"));
        assert!(buffer_text(&terminal).contains("100 / 100 branches"));
        if let Mode::Branch(branches) = &mut picker.mode {
            branches.filter = "missing".into();
        }
        picker.cursor = 0;
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        assert!(buffer_text(&terminal).contains("0 / 100 branches"));
        manager.sessions[99].repos = (0..100)
            .map(|i| RepoLink::link(format!("linked-{i:03}"), format!("/repo-{i:03}")))
            .collect();
        manager.mode = SessionManagerMode::Expanded {
            session_name: "session-099".into(),
            repo_cursor: 99,
        };
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        assert!(buffer_text(&terminal).contains("linked-099"));
    }

    #[test]
    fn manager_home_and_picker_have_distinct_open_cancel_and_quit_actions() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut manager = SessionManagerApp::new(
            vec![manager_session("one"), manager_session("two")],
            store.clone(),
        );
        manager.selected.insert("two".into());
        assert!(
            matches!(manager.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), SessionManagerSignal::Open { session_name } if session_name == "one")
        );
        assert!(matches!(
            manager.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            SessionManagerSignal::New
        ));
        assert!(
            matches!(manager.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)), SessionManagerSignal::AddRepos { session_name, .. } if session_name == "one")
        );
        let mut picker = App::for_add(Vec::new(), store, "one".into());
        assert!(matches!(
            picker
                .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
                .unwrap(),
            AppSignal::Done(RepoFlowOutcome::Cancelled)
        ));
        assert!(matches!(
            picker
                .handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
                .unwrap(),
            AppSignal::Done(RepoFlowOutcome::Quit)
        ));
    }

    #[test]
    fn confirmations_show_hidden_targets_and_enter_cancels() {
        let temp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut manager =
            SessionManagerApp::new(vec![manager_session("one"), manager_session("two")], store);
        manager.selected.extend(["one".into(), "two".into()]);
        manager.filter = "one".into();
        manager.start_remove();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        let text = buffer_text(&terminal);
        capture(&terminal, "confirm-80x24");
        assert!(text.contains("one") && text.contains("two") && text.contains("1 hidden"));
        manager.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(manager.mode, SessionManagerMode::Browse));
        assert_eq!(manager.sessions.len(), 2);
    }

    #[test]
    fn drawing_uses_cached_filesystem_state_and_never_polls_fetch() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let store = SessionStore::new(DynwsPaths::from_home(temp.path().join(".dynws")));
        let mut session = manager_session("one");
        session
            .repos
            .push(RepoLink::link("repo", repo.display().to_string()));
        let mut manager = SessionManagerApp::new(vec![session], store.clone());
        manager.mode = SessionManagerMode::Expanded {
            session_name: "one".into(),
            repo_cursor: 0,
        };
        std::fs::remove_dir(&repo).unwrap();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| manager.draw(frame)).unwrap();
        assert!(!buffer_text(&terminal).contains("missing"));
        let child = Command::new("sh")
            .args(["-c", "exit 0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut picker = App::new(Vec::new(), store);
        picker.mode = Mode::BranchLoading;
        picker.branch_fetch = Some(BranchFetch {
            repo_indices: vec![0],
            current: 0,
            repo_name: "repo".into(),
            task: GitBranchTask {
                repo_path: repo,
                child: Some(child),
                phase: GitTaskPhase::FetchPrune,
            },
        });
        terminal.draw(|frame| picker.draw(frame)).unwrap();
        assert!(matches!(
            picker.branch_fetch.as_ref().unwrap().task.phase,
            GitTaskPhase::FetchPrune
        ));
    }

    fn manager_session(name: &str) -> SessionMetadata {
        SessionMetadata {
            creation: None,
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
            creation: None,
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
            creation: None,
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

        assert!(matches!(
            signal,
            AppSignal::Done(RepoFlowOutcome::Completed(_))
        ));
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
                creation: None,
                name: "alpha".to_string(),
                description: None,
                repos: Vec::new(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
            },
            SessionMetadata {
                creation: None,
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
            creation: None,
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
                creation: None,
                name: "alpha".to_string(),
                description: None,
                repos: Vec::new(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
            },
            SessionMetadata {
                creation: None,
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
            creation: None,
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
