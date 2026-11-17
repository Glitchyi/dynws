//! OpenCode-style presentation primitives: theme tokens, the block-letter logo,
//! the prompt box, dialog frames with a dimmed backdrop, toasts and buttons.
//!
//! The palette and proportions follow OpenCode's default dark theme (v2.2.6):
//! a near-black page, raised panels for dialogs and the sidebar, an orange
//! interactive accent for focus, and purple category headings. Everything here
//! only draws; callers own state, input handling and hit regions.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

pub(super) const VERSION: &str = env!("CARGO_PKG_VERSION");

const BACKGROUND: Color = Color::Rgb(10, 10, 10);
const PANEL: Color = Color::Rgb(20, 20, 20);
const ELEMENT: Color = Color::Rgb(30, 30, 30);
const TEXT: Color = Color::Rgb(238, 238, 238);
const MUTED: Color = Color::Rgb(128, 128, 128);
const BORDER: Color = Color::Rgb(72, 72, 72);
const PRIMARY: Color = Color::Rgb(250, 178, 131);
const SECONDARY: Color = Color::Rgb(92, 156, 245);
const ACCENT: Color = Color::Rgb(157, 124, 216);
const ERROR: Color = Color::Rgb(224, 108, 117);
const SUCCESS: Color = Color::Rgb(127, 216, 143);
const WARNING: Color = Color::Rgb(245, 167, 66);
const INFO: Color = Color::Rgb(86, 182, 194);
const LOGO_DIM_SHADOW: Color = Color::Rgb(40, 40, 40);
const LOGO_BRIGHT_SHADOW: Color = Color::Rgb(67, 67, 67);

/// Semantic styles. `colors` is false under `NO_COLOR`, where every role falls
/// back to the terminal defaults and focus is carried by reverse video.
#[derive(Clone, Copy)]
pub(super) struct Theme {
    pub(super) colors: bool,
    pub(super) base: Style,
    pub(super) muted: Style,
    pub(super) surface: Style,
    pub(super) element: Style,
    pub(super) border: Style,
    pub(super) focus: Style,
    pub(super) key: Style,
    pub(super) title: Style,
    pub(super) primary: Style,
    pub(super) secondary: Style,
    pub(super) accent: Style,
    pub(super) error: Style,
    pub(super) success: Style,
    pub(super) warning: Style,
    pub(super) info: Style,
}

impl Theme {
    pub(super) fn new() -> Self {
        // https://no-color.org: only a present, non-empty value disables color.
        Self::for_colors(std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty()))
    }

    pub(super) fn for_colors(colors: bool) -> Self {
        let plain = Style::default();
        if !colors {
            let bold = plain.add_modifier(Modifier::BOLD);
            return Self {
                colors,
                base: plain,
                muted: plain,
                surface: plain,
                element: plain,
                border: plain,
                focus: plain.add_modifier(Modifier::REVERSED | Modifier::BOLD),
                key: bold,
                title: bold,
                primary: bold,
                secondary: bold,
                accent: bold,
                error: bold,
                success: plain,
                warning: bold,
                info: plain,
            };
        }
        Self {
            colors,
            base: plain.fg(TEXT).bg(BACKGROUND),
            muted: plain.fg(MUTED),
            surface: plain.bg(PANEL),
            element: plain.bg(ELEMENT),
            border: plain.fg(BORDER),
            focus: plain
                .fg(BACKGROUND)
                .bg(PRIMARY)
                .add_modifier(Modifier::BOLD),
            key: plain.fg(TEXT),
            title: plain.fg(TEXT).add_modifier(Modifier::BOLD),
            primary: plain.fg(PRIMARY),
            secondary: plain.fg(SECONDARY),
            accent: plain.fg(ACCENT).add_modifier(Modifier::BOLD),
            error: plain.fg(ERROR),
            success: plain.fg(SUCCESS),
            warning: plain.fg(WARNING),
            info: plain.fg(INFO),
        }
    }
}

// The logo reads "dynws" in OpenCode's block-letter face: the "dyn" half is
// muted and the "ws" half bright. Markup: `_` is a shadow-filled space, `^` an
// upper half block over shadow, and `~` an upper half block in the shadow color.
const LOGO: [(&str, &str); 4] = [
    ("   ▄          ", "          "),
    ("█▀▀█ █  █ █▀▀▄", "█ █ █ █▀▀▀"),
    ("█__█ █__█ █__█", "█_█_█ ^^^█"),
    ("▀▀▀▀ ▀▀▀█ ▀~~▀", "▀▀▀▀▀ ▀▀▀▀"),
];
pub(super) const LOGO_WIDTH: u16 = 25;
pub(super) const LOGO_HEIGHT: u16 = 4;

pub(super) fn logo_lines(theme: Theme) -> Vec<Line<'static>> {
    let half = |text: &str, letter: Style, shadow: Color| -> Vec<Span<'static>> {
        text.chars()
            .map(|glyph| {
                let (symbol, style) = match glyph {
                    '_' if theme.colors => (' ', letter.bg(shadow)),
                    '^' if theme.colors => ('▀', letter.bg(shadow)),
                    '~' if theme.colors => ('▀', Style::default().fg(shadow)),
                    '_' | '~' => (' ', letter),
                    '^' => ('▀', letter),
                    other => (other, letter),
                };
                Span::styled(symbol.to_string(), style)
            })
            .collect()
    };
    LOGO.iter()
        .map(|(dim, bright)| {
            let mut spans = half(dim, theme.muted, LOGO_DIM_SHADOW);
            spans.push(Span::raw(" "));
            spans.extend(half(bright, theme.title, LOGO_BRIGHT_SHADOW));
            Line::from(spans)
        })
        .collect()
}

/// The sidebar signature: "dyn" muted, "ws" bright, then the version.
pub(super) fn brand_line(theme: Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled("dyn", theme.muted),
        Span::styled("ws", theme.title),
        Span::styled(format!(" {VERSION}"), theme.muted),
    ])
}

/// Darkens everything outside `keep`, like OpenCode's dialog backdrop.
pub(super) fn dim(buffer: &mut Buffer, keep: Rect, theme: Theme) {
    let area = buffer.area;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if keep.contains(Position { x, y }) {
                continue;
            }
            if let Some(cell) = buffer.cell_mut((x, y)) {
                if theme.colors {
                    cell.fg = darken(cell.fg, TEXT);
                    cell.bg = darken(cell.bg, BACKGROUND);
                } else {
                    cell.modifier.insert(Modifier::DIM);
                }
            }
        }
    }
}

fn darken(color: Color, fallback: Color) -> Color {
    let Color::Rgb(r, g, b) = (match color {
        Color::Rgb(..) => color,
        _ => fallback,
    }) else {
        return color;
    };
    let scale = |value: u8| (u16::from(value) * 2 / 5) as u8;
    Color::Rgb(scale(r), scale(g), scale(b))
}

/// The geometry of a drawn dialog. `content` excludes the title row and padding;
/// `close` is the clickable "esc" label.
#[derive(Clone, Copy)]
pub(super) struct DialogFrame {
    pub(super) outer: Rect,
    pub(super) content: Rect,
    pub(super) close: Rect,
}

/// Rows a dialog spends on padding and its title, around `body_rows`.
pub(super) const DIALOG_CHROME: u16 = 4;

pub(super) fn dialog_width(screen: Rect, preferred: u16) -> u16 {
    preferred
        .min(screen.width.saturating_sub(4))
        .max(screen.width.min(20))
}

/// Draws a raised, centered dialog a quarter of the way down the screen, with a
/// bold title, a muted "esc" on the right and the rest of the screen dimmed.
pub(super) fn dialog(
    frame: &mut Frame<'_>,
    preferred_width: u16,
    body_rows: u16,
    title: &str,
    aside: &str,
    theme: Theme,
) -> DialogFrame {
    let screen = frame.area();
    let width = dialog_width(screen, preferred_width);
    let height = body_rows
        .saturating_add(DIALOG_CHROME)
        .min(screen.height.saturating_sub(2))
        .max(screen.height.min(DIALOG_CHROME + 1));
    let top = (screen.height / 4).min(screen.height.saturating_sub(height + 1));
    let outer = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + top,
        width,
        height,
    );
    dim(frame.buffer_mut(), outer, theme);
    frame.render_widget(Clear, outer);
    frame.render_widget(
        Block::default().style(theme.base.patch(theme.surface)),
        outer,
    );
    let pad = dialog_padding(width);
    let inner_width = width.saturating_sub(pad * 2);
    let close = Rect::new(
        (outer.right().saturating_sub(pad + 3)).max(outer.x),
        outer.y + 1,
        3.min(width),
        1,
    );
    let aside_width = Line::raw(aside).width() as u16;
    let title_width =
        inner_width.saturating_sub(if aside.is_empty() { 4 } else { aside_width + 6 });
    frame.render_widget(
        Paragraph::new(clip(title, title_width as usize)).style(theme.title),
        Rect::new(outer.x + pad, outer.y + 1, title_width, 1),
    );
    if !aside.is_empty() && close.x >= outer.x + pad + aside_width + 2 {
        frame.render_widget(
            Paragraph::new(aside).style(theme.muted),
            Rect::new(close.x - aside_width - 2, close.y, aside_width, 1),
        );
    }
    frame.render_widget(Paragraph::new("esc").style(theme.muted), close);
    DialogFrame {
        outer,
        content: Rect::new(
            outer.x + pad,
            outer.y + 3.min(height),
            inner_width,
            height.saturating_sub(DIALOG_CHROME),
        ),
        close,
    }
}

pub(super) fn dialog_padding(width: u16) -> u16 {
    if width >= 44 { 4 } else { 2 }
}

/// What the prompt box shows. `bar` colors the left rail and the mode label.
pub(super) struct PromptBox<'a> {
    pub(super) value: &'a str,
    pub(super) placeholder: &'a str,
    pub(super) editable: bool,
    pub(super) mode: &'a str,
    pub(super) status: &'a str,
    pub(super) bar: Style,
}

pub(super) fn prompt_height(screen: Rect) -> u16 {
    if screen.height >= 24 { 5 } else { 3 }
}

/// OpenCode's prompt: a heavy left rail, an element-colored body, the input
/// line, a mode/status line and a half-block bottom edge.
pub(super) fn draw_prompt(frame: &mut Frame<'_>, area: Rect, prompt: PromptBox<'_>, theme: Theme) {
    if area.height < 3 || area.width < 8 {
        return;
    }
    let tall = area.height >= 5;
    let (input_row, meta_row) = if tall { (1, 3) } else { (0, 1) };
    let edge_row = area.height - 1;
    let rail = theme.base.patch(prompt.bar);
    for row in 0..edge_row {
        let y = area.y + row;
        frame.render_widget(Paragraph::new("┃").style(rail), Rect::new(area.x, y, 1, 1));
        frame.render_widget(
            Block::default().style(theme.base.patch(theme.element)),
            Rect::new(area.x + 1, y, area.width - 1, 1),
        );
    }
    let edge = if theme.colors {
        "▀".repeat(area.width as usize - 1)
    } else {
        String::new()
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("╹", rail),
            Span::styled(edge, theme.base.fg(element_color(theme))),
        ])),
        Rect::new(area.x, area.y + edge_row, area.width, 1),
    );
    let text_x = area.x + 3;
    let text_width = area.width.saturating_sub(5);
    draw_field(
        frame,
        Rect::new(text_x, area.y + input_row, text_width, 1),
        prompt.value,
        prompt.placeholder,
        prompt.editable,
        theme,
    );
    // The status carries counts the user acts on, so the mode label yields
    // first when both do not fit.
    let with_mode =
        Line::raw(format!("{} · {}", prompt.mode, prompt.status)).width() <= text_width as usize;
    let mut spans = Vec::new();
    if with_mode {
        spans.push(Span::styled(prompt.mode.to_owned(), prompt.bar));
    }
    for (index, part) in prompt
        .status
        .split(" · ")
        .filter(|part| !part.is_empty())
        .enumerate()
    {
        if with_mode || index > 0 {
            spans.push(Span::styled(" · ", theme.muted));
        }
        spans.push(Span::styled(
            part.to_owned(),
            if index == 0 { theme.key } else { theme.muted },
        ));
    }
    let meta = clip_line(Line::from(spans), text_width as usize);
    frame.render_widget(
        Paragraph::new(meta),
        Rect::new(text_x, area.y + meta_row, text_width, 1),
    );
}

fn element_color(theme: Theme) -> Color {
    theme.element.bg.unwrap_or(Color::Reset)
}

/// A single-line input: the value's tail stays visible and the caret follows it;
/// an empty value shows the muted placeholder.
pub(super) fn draw_field(
    frame: &mut Frame<'_>,
    area: Rect,
    value: &str,
    placeholder: &str,
    editable: bool,
    theme: Theme,
) {
    let available = (area.width as usize).saturating_sub(usize::from(editable));
    let (shown, width) = tail(value, available);
    let line = if value.is_empty() {
        Line::styled(clip(placeholder, available), theme.muted)
    } else {
        Line::styled(shown, theme.key)
    };
    frame.render_widget(Paragraph::new(line), area);
    if editable {
        frame.set_cursor_position((area.x + width as u16, area.y));
    }
}

fn tail(text: &str, available: usize) -> (String, usize) {
    let line = Line::raw(text);
    let graphemes = line.styled_graphemes(Style::default()).collect::<Vec<_>>();
    let mut width = 0;
    let mut first = graphemes.len();
    for (index, grapheme) in graphemes.iter().enumerate().rev() {
        let size = Line::raw(grapheme.symbol).width();
        if width + size > available {
            break;
        }
        width += size;
        first = index;
    }
    (graphemes[first..].iter().map(|g| g.symbol).collect(), width)
}

/// Kinds of toast, which pick the rail color.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Tone {
    Error,
    Warning,
    Info,
    Success,
}

pub(super) fn tone(message: &str) -> Tone {
    let lower = message.to_lowercase();
    if lower.starts_with("error") || lower.contains("error:") {
        Tone::Error
    } else if lower.contains("warning") || lower.contains("preserved") {
        Tone::Warning
    } else if lower.starts_with("fetching") || lower.ends_with("cancelled") {
        Tone::Info
    } else {
        Tone::Success
    }
}

/// Draws a toast in the top-right corner and returns its area. Long messages
/// are cut to three lines and end with a pointer to the details dialog.
pub(super) fn draw_toast(frame: &mut Frame<'_>, message: &str, theme: Theme) -> Option<Rect> {
    let screen = frame.area();
    if message.is_empty() || screen.width < 20 || screen.height < 6 {
        return None;
    }
    let longest = Line::raw(message).width() as u16 + 6;
    let width = longest.min(60).min(screen.width.saturating_sub(4));
    let text_width = width.saturating_sub(6) as usize;
    let mut lines = wrap_lines(&message.replace('\n', " "), text_width);
    if lines.len() > 3 {
        lines.truncate(3);
        let last = clip(&lines[2], text_width.saturating_sub(9));
        lines[2] = format!("{last} · f2 more");
    }
    let height = lines.len() as u16 + 2;
    let area = Rect::new(screen.right() - width - 2, screen.y + 1, width, height);
    let rail = match tone(message) {
        Tone::Error => theme.error,
        Tone::Warning => theme.warning,
        Tone::Info => theme.info,
        Tone::Success => theme.success,
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(theme.base.patch(theme.surface)),
        area,
    );
    for row in 0..height {
        let y = area.y + row;
        for x in [area.x, area.right() - 1] {
            frame.render_widget(
                Paragraph::new("┃").style(rail.patch(theme.surface)),
                Rect::new(x, y, 1, 1),
            );
        }
    }
    frame.render_widget(
        Paragraph::new(lines.into_iter().map(Line::raw).collect::<Vec<_>>()).style(theme.key),
        Rect::new(area.x + 3, area.y + 1, text_width as u16, height - 2),
    );
    Some(area)
}

/// One dialog button. The focused button is the Enter default.
pub(super) struct Button<'a> {
    pub(super) label: &'a str,
    pub(super) focused: bool,
    pub(super) danger: bool,
}

/// Draws buttons right-aligned in `row` and returns each button's area.
pub(super) fn draw_buttons(
    frame: &mut Frame<'_>,
    row: Rect,
    buttons: &[Button<'_>],
    theme: Theme,
) -> Vec<Rect> {
    let mut x = row.right();
    let mut areas = vec![Rect::default(); buttons.len()];
    for (index, button) in buttons.iter().enumerate().rev() {
        let label = format!(" {} ", button.label);
        let width = Line::raw(&label).width() as u16;
        if x < row.x + width {
            break;
        }
        x -= width;
        let style = if button.focused {
            theme.focus
        } else if button.danger {
            theme.error
        } else {
            theme.muted
        };
        let area = Rect::new(x, row.y, width, 1);
        frame.render_widget(Paragraph::new(label).style(style), area);
        areas[index] = area;
        x = x.saturating_sub(1);
    }
    areas
}

/// Truncates to `width` columns, ending with an ellipsis when shortened.
/// Ratatui's grapheme iterator preserves combining marks and wide glyphs.
pub(super) fn clip(text: &str, width: usize) -> String {
    let line = Line::raw(text);
    if line.width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for grapheme in line.styled_graphemes(Style::default()) {
        let size = Line::raw(grapheme.symbol).width();
        if used + size > width - 1 {
            break;
        }
        result.push_str(grapheme.symbol);
        used += size;
    }
    result.push('…');
    result
}

/// Truncates from the left, keeping the end of a path visible.
pub(super) fn clip_start(text: &str, width: usize) -> String {
    let line = Line::raw(text);
    if line.width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let (kept, _) = tail(text, width - 1);
    format!("…{kept}")
}

/// Clips a styled line to `width` columns, preserving span styles.
pub(super) fn clip_line(line: Line<'static>, width: usize) -> Line<'static> {
    let mut used = 0;
    let mut spans = Vec::new();
    for span in line.spans {
        let size = Line::raw(span.content.as_ref()).width();
        if used + size <= width {
            used += size;
            spans.push(span);
            continue;
        }
        let rest = width.saturating_sub(used);
        if rest > 0 {
            spans.push(Span::styled(clip(&span.content, rest), span.style));
        }
        break;
    }
    Line::from(spans)
}

/// Word-wraps on spaces, breaking long words by grapheme.
pub(super) fn wrap_lines(text: &str, width: usize) -> Vec<String> {
    let mut result = Vec::new();
    for line in text.split('\n') {
        let line = Line::raw(line);
        let mut wrapped = String::new();
        let mut used = 0;
        for grapheme in line.styled_graphemes(Style::default()) {
            let size = Line::raw(grapheme.symbol).width();
            if used + size > width.max(1) {
                if let Some(space) = wrapped.rfind(' ').filter(|idx| *idx > 0) {
                    let remainder = wrapped.split_off(space + 1);
                    wrapped.pop();
                    result.push(std::mem::replace(&mut wrapped, remainder));
                    used = Line::raw(&wrapped).width();
                } else {
                    result.push(std::mem::take(&mut wrapped));
                    used = 0;
                }
            }
            wrapped.push_str(grapheme.symbol);
            used += size;
        }
        result.push(wrapped);
    }
    result
}

/// Formats an RFC 3339 timestamp as a compact age such as "3d ago".
pub(super) fn age(timestamp: &str, now: chrono::DateTime<chrono::Utc>) -> Option<String> {
    let then = chrono::DateTime::parse_from_rfc3339(timestamp).ok()?;
    let seconds = (now - then.with_timezone(&chrono::Utc))
        .num_seconds()
        .max(0);
    Some(match seconds {
        0..60 => "just now".into(),
        60..3_600 => format!("{}m ago", seconds / 60),
        3_600..86_400 => format!("{}h ago", seconds / 3_600),
        86_400..2_592_000 => format!("{}d ago", seconds / 86_400),
        2_592_000..31_536_000 => format!("{}mo ago", seconds / 2_592_000),
        _ => format!("{}y ago", seconds / 31_536_000),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_rows_share_one_width_and_toasts_pick_tones() {
        for (dim, bright) in LOGO {
            assert_eq!(
                Line::raw(dim).width() + 1 + Line::raw(bright).width(),
                LOGO_WIDTH as usize
            );
        }
        assert_eq!(tone("Error: changed elsewhere"), Tone::Error);
        assert_eq!(tone("removed 1 session; Warning: kept"), Tone::Warning);
        assert_eq!(tone("remove cancelled"), Tone::Info);
        assert_eq!(tone("renamed a -> b"), Tone::Success);
        assert_eq!(clip_start("/a/very/long/path", 8), "…ng/path");
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-17T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(age("2026-11-14T12:00:00Z", now).as_deref(), Some("3d ago"));
        assert_eq!(age("not a time", now), None);
    }

    #[test]
    fn backdrop_darkens_outside_the_dialog_only() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 1));
        for x in 0..4 {
            buffer.cell_mut((x, 0)).unwrap().bg = Color::Rgb(100, 100, 100);
        }
        dim(&mut buffer, Rect::new(2, 0, 2, 1), Theme::for_colors(true));
        assert_eq!(buffer.cell((0, 0)).unwrap().bg, Color::Rgb(40, 40, 40));
        assert_eq!(buffer.cell((3, 0)).unwrap().bg, Color::Rgb(100, 100, 100));
    }
}
