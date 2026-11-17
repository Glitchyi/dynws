//! Slate presentation primitives: monochrome theme tokens, labelled fields,
//! bordered buttons, keycaps, dialogs over a dimmed backdrop, alerts, toasts,
//! the creation stepper and the animated constellation signature.
//!
//! Everything here only draws and reports where it drew; callers own state,
//! input handling and hit regions.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use std::sync::OnceLock;

pub(super) const VERSION: &str = env!("CARGO_PKG_VERSION");

const VOID: Color = Color::Rgb(9, 9, 11);
const CANVAS: Color = Color::Rgb(14, 14, 16);
const GRAPHITE: Color = Color::Rgb(21, 21, 23);
const ALLOY: Color = Color::Rgb(29, 29, 32);
const HAIRLINE: Color = Color::Rgb(48, 48, 52);
const CHALK: Color = Color::Rgb(242, 242, 243);
const ASH: Color = Color::Rgb(150, 150, 156);
const EMPHASIS: Color = Color::Rgb(232, 232, 234);
const DIM: Color = Color::Rgb(110, 110, 117);
const SOFT: Color = Color::Rgb(74, 74, 80);

/// Size-dependent measurements derived from the screen. Compact screens
/// (under 100 columns or 24 rows) keep the original fixed sizes; wider and
/// taller ones grow smoothly within clamps.
#[derive(Clone, Copy)]
pub(super) struct Scale {
    width: u32,
    height: u32,
}

/// `value` limited to `lo..=hi`; a `hi` below `lo` yields `lo`.
fn bounded(value: u32, lo: u32, hi: u32) -> u16 {
    value.max(lo).min(hi.max(lo)).min(u32::from(u16::MAX)) as u16
}

impl Scale {
    pub(super) fn new(screen: Rect) -> Self {
        Self {
            width: u32::from(screen.width),
            height: u32::from(screen.height),
        }
    }

    /// Horizontal page padding.
    pub(super) fn page_pad(self) -> u16 {
        if self.width < 60 {
            1
        } else {
            bounded(self.width / 40, 2, 6)
        }
    }

    pub(super) fn sidebar_width(self) -> u16 {
        bounded(self.width * 27 / 100, 40, 64)
    }

    pub(super) fn launcher_width(self) -> u16 {
        bounded(self.width * 55 / 100, 76, 120).min(self.width.saturating_sub(4) as u16)
    }

    /// Rows a list item may take: one, two or three.
    pub(super) fn item_rows(self) -> u16 {
        if self.height < 16 {
            1
        } else if self.height < 48 {
            2
        } else {
            3
        }
    }

    /// Width of a dialog that asks for `preferred` columns.
    pub(super) fn dialog_width(self, preferred: u16) -> u16 {
        bounded(self.width * 45 / 100, u32::from(preferred), 110)
            .min(self.width.saturating_sub(2) as u16)
            .max(self.width.min(24) as u16)
    }

    pub(super) fn creation_width(self) -> u16 {
        if self.width >= 64 {
            self.width.saturating_sub(4).min(160) as u16
        } else {
            self.width as u16
        }
    }

    /// The creation dialog's left column when it is split.
    pub(super) fn creation_left(self, content: u16) -> u16 {
        bounded(u32::from(content) * 40 / 100, 48, 72)
    }

    pub(super) fn toast_width(self, message_width: u16) -> u16 {
        let cap = bounded(self.width * 30 / 100, 64, 80);
        bounded(u32::from(message_width) + 8, 24, u32::from(cap))
            .min(self.width.saturating_sub(4) as u16)
    }

    pub(super) fn signature_width(self) -> u16 {
        bounded(self.width * 50 / 100, 60, 120)
    }

    pub(super) fn signature_height(self) -> u16 {
        if self.height >= 48 { 9 } else { 7 }
    }
}

/// Semantic styles. `colors` is false under `NO_COLOR`, where every role falls
/// back to terminal defaults and focus is carried by reverse video and weight.
#[derive(Clone, Copy)]
pub(super) struct Theme {
    pub(super) colors: bool,
    pub(super) base: Style,
    pub(super) muted: Style,
    pub(super) dim: Style,
    pub(super) soft: Style,
    pub(super) surface: Style,
    pub(super) canvas: Style,
    pub(super) element: Style,
    pub(super) border: Style,
    pub(super) ring: Style,
    pub(super) focus: Style,
    pub(super) focus_soft: Style,
    pub(super) key: Style,
    pub(super) title: Style,
    pub(super) heading: Style,
}

impl Theme {
    pub(super) fn new() -> Self {
        // https://no-color.org: only a present, non-empty value disables color.
        static COLORS: OnceLock<bool> = OnceLock::new();
        Self::for_colors(
            *COLORS
                .get_or_init(|| std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())),
        )
    }

    pub(super) fn for_colors(colors: bool) -> Self {
        let plain = Style::default();
        let bold = plain.add_modifier(Modifier::BOLD);
        if !colors {
            return Self {
                colors,
                base: plain,
                muted: plain,
                dim: plain,
                soft: plain,
                surface: plain,
                canvas: plain,
                element: plain.add_modifier(Modifier::REVERSED),
                border: plain,
                ring: bold,
                focus: plain.add_modifier(Modifier::REVERSED | Modifier::BOLD),
                focus_soft: plain.add_modifier(Modifier::REVERSED),
                key: bold,
                title: bold,
                heading: bold,
            };
        }
        Self {
            colors,
            base: plain.fg(CHALK).bg(VOID),
            muted: plain.fg(ASH),
            dim: plain.fg(DIM),
            soft: plain.fg(SOFT),
            surface: plain.bg(GRAPHITE),
            canvas: plain.bg(CANVAS),
            element: plain.fg(CHALK).bg(ALLOY),
            border: plain.fg(HAIRLINE),
            ring: plain.fg(EMPHASIS),
            focus: plain.fg(VOID).bg(EMPHASIS).add_modifier(Modifier::BOLD),
            focus_soft: plain.fg(SOFT).bg(EMPHASIS),
            key: plain.fg(CHALK),
            title: plain.fg(CHALK).add_modifier(Modifier::BOLD),
            heading: plain.fg(ASH).add_modifier(Modifier::BOLD),
        }
    }
}

/// A rounded hairline box. `fill` paints only the interior, so the corners
/// keep whatever is behind them.
pub(super) fn rounded(frame: &mut Frame<'_>, area: Rect, border: Style, fill: Style) {
    if area.width < 2 || area.height < 2 {
        return;
    }
    frame.render_widget(Block::default().style(fill), inner(area));
    frame.render_widget(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(border),
        area,
    );
}

fn inner(area: Rect) -> Rect {
    Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

/// The sidebar signature: "dyn" dim, "ws" bright, then the version.
pub(super) fn brand_line(theme: Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled("dyn", theme.dim),
        Span::styled("ws", theme.title),
        Span::styled(format!(" {VERSION}"), theme.muted),
    ])
}

/// A key shown as a keycap, such as ` ctrl+n `.
pub(super) fn keycap(key: &str, theme: Theme) -> Span<'static> {
    Span::styled(format!(" {key} "), theme.element)
}

/// An uppercase section heading.
pub(super) fn heading(text: &str, theme: Theme) -> Span<'static> {
    Span::styled(text.to_uppercase(), theme.heading)
}

/// Darkens everything outside `keep`, the backdrop behind a dialog.
pub(super) fn dim(buffer: &mut Buffer, keep: Rect, theme: Theme) {
    let area = buffer.area;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if keep.contains(Position { x, y }) {
                continue;
            }
            if let Some(cell) = buffer.cell_mut((x, y)) {
                if theme.colors {
                    cell.fg = darken(cell.fg, CHALK);
                    cell.bg = darken(cell.bg, VOID);
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

/// What a labelled input field shows.
pub(super) struct Field<'a> {
    pub(super) label: &'a str,
    pub(super) aside: &'a str,
    pub(super) value: &'a str,
    pub(super) placeholder: &'a str,
    pub(super) editable: bool,
}

/// Draws a labelled, rounded input in four rows, only the box in three rows,
/// or only the input line in fewer. Returns the input line's area.
pub(super) fn draw_field(
    frame: &mut Frame<'_>,
    area: Rect,
    field: &Field<'_>,
    theme: Theme,
) -> Rect {
    if area.height == 0 || area.width < 6 {
        return Rect::default();
    }
    if area.height < 3 {
        let line = Rect::new(area.x, area.y, area.width, 1);
        draw_input(
            frame,
            line,
            field.value,
            field.placeholder,
            field.editable,
            theme,
        );
        return line;
    }
    let labelled = area.height >= 4;
    if labelled {
        let row = Rect::new(area.x, area.y, area.width, 1);
        let label_width = Line::raw(field.label).width() as u16;
        let aside = clip_start(
            field.aside,
            area.width.saturating_sub(label_width + 2) as usize,
        );
        let aside_width = Line::raw(&aside).width() as u16;
        frame.render_widget(
            Paragraph::new(clip(
                field.label,
                area.width.saturating_sub(aside_width + 2) as usize,
            ))
            .style(theme.title),
            row,
        );
        if !aside.is_empty() && aside_width < area.width {
            frame.render_widget(
                Paragraph::new(aside).style(theme.muted),
                Rect::new(area.right() - aside_width, row.y, aside_width, 1),
            );
        }
    }
    let boxed = Rect::new(area.x, area.y + u16::from(labelled), area.width, 3);
    rounded(
        frame,
        boxed,
        if field.editable {
            theme.ring
        } else {
            theme.border
        },
        Style::default(),
    );
    let inner = Rect::new(boxed.x + 2, boxed.y + 1, boxed.width.saturating_sub(4), 1);
    frame.render_widget(
        Paragraph::new("›").style(theme.muted),
        Rect::new(inner.x, inner.y, 1, 1),
    );
    let input = Rect::new(inner.x + 2, inner.y, inner.width.saturating_sub(2), 1);
    draw_input(
        frame,
        input,
        field.value,
        field.placeholder,
        field.editable,
        theme,
    );
    input
}

/// A single-line input: the value's tail stays visible and the caret follows
/// it; an empty value shows the muted placeholder after the caret.
pub(super) fn draw_input(
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
        Line::from(vec![
            Span::raw(if editable { " " } else { "" }),
            Span::styled(clip(placeholder, available), theme.muted),
        ])
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

/// A bordered action button with an optional icon and key.
#[derive(Clone, Copy)]
pub(super) struct Button<'a> {
    pub(super) icon: &'a str,
    pub(super) label: &'a str,
    pub(super) key: &'a str,
    /// The Enter default: a bright border and a filled face.
    pub(super) strong: bool,
}

pub(super) fn button_width(button: &Button<'_>, tall: bool) -> u16 {
    let icon = if button.icon.is_empty() {
        0
    } else {
        Line::raw(button.icon).width() + 1
    };
    let key = if button.key.is_empty() {
        0
    } else {
        Line::raw(button.key).width() + 2
    };
    (icon + Line::raw(button.label).width() + key) as u16 + if tall { 4 } else { 2 }
}

/// Draws a button: three rows with a rounded border when `area` is that tall,
/// otherwise a one-row chip.
pub(super) fn draw_button(frame: &mut Frame<'_>, area: Rect, button: &Button<'_>, theme: Theme) {
    let tall = area.height >= 3;
    let face = if button.strong || !tall {
        theme.element
    } else {
        Style::default()
    };
    let row = if tall {
        rounded(
            frame,
            area,
            if button.strong {
                theme.ring
            } else {
                theme.border
            },
            face,
        );
        Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(4), 1)
    } else {
        frame.render_widget(Block::default().style(face), area);
        Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), 1)
    };
    let mut spans = Vec::new();
    if !button.icon.is_empty() {
        spans.push(Span::styled(
            format!("{} ", button.icon),
            theme.key.patch(face),
        ));
    }
    let label = theme.key.patch(face);
    spans.push(Span::styled(
        button.label.to_owned(),
        if button.strong {
            label.add_modifier(Modifier::BOLD)
        } else {
            label
        },
    ));
    if !button.key.is_empty() {
        spans.push(Span::styled(
            format!("  {}", button.key),
            theme.muted.patch(face),
        ));
    }
    frame.render_widget(
        Paragraph::new(clip_line(Line::from(spans), row.width as usize)),
        row,
    );
}

/// The geometry of a drawn dialog. `content` excludes the border, title and
/// description; `close` is the clickable "esc close" label.
#[derive(Clone, Copy)]
pub(super) struct DialogFrame {
    pub(super) outer: Rect,
    pub(super) content: Rect,
    pub(super) close: Rect,
}

/// Rows a dialog spends around its body: borders, the title row and a gap.
pub(super) fn dialog_chrome(described: bool) -> u16 {
    4 + u16::from(described)
}

pub(super) fn dialog_width(screen: Rect, preferred: u16) -> u16 {
    Scale::new(screen).dialog_width(preferred)
}

/// Draws a rounded dialog a third of the way down the screen with the rest of
/// the screen dimmed: a bold title, an "esc close" keycap and an optional
/// one-line description of the consequence.
pub(super) fn dialog(
    frame: &mut Frame<'_>,
    preferred_width: u16,
    body_rows: u16,
    title: &str,
    description: &str,
    theme: Theme,
) -> DialogFrame {
    let width = dialog_width(frame.area(), preferred_width);
    dialog_sized(frame, width, body_rows, title, description, theme)
}

/// Like [`dialog`] with an exact `width`, limited only to the screen.
pub(super) fn dialog_sized(
    frame: &mut Frame<'_>,
    width: u16,
    body_rows: u16,
    title: &str,
    description: &str,
    theme: Theme,
) -> DialogFrame {
    let screen = frame.area();
    let width = width
        .min(screen.width.saturating_sub(2))
        .max(screen.width.min(24));
    let described = !description.is_empty();
    let height = body_rows
        .saturating_add(dialog_chrome(described))
        .min(screen.height)
        .max(screen.height.min(dialog_chrome(described) + 1));
    let top = screen.height.saturating_sub(height) / 3;
    let outer = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + top,
        width,
        height,
    );
    dim(frame.buffer_mut(), inner(outer), theme);
    frame.render_widget(Clear, inner(outer));
    rounded(frame, outer, theme.border, theme.base.patch(theme.surface));
    let pad = if width >= 40 { 3 } else { 2 };
    let close_label = Line::from(vec![
        keycap("esc", theme),
        Span::styled(" close", theme.muted),
    ]);
    let close_width = (close_label.width() as u16).min(width.saturating_sub(2));
    let close = Rect::new(
        outer
            .right()
            .saturating_sub(pad + close_width)
            .max(outer.x + 1),
        outer.y + 1,
        close_width,
        1,
    );
    frame.render_widget(Paragraph::new(close_label), close);
    let title_width = close.x.saturating_sub(outer.x + pad + 2);
    frame.render_widget(
        Paragraph::new(clip(title, title_width as usize)).style(theme.title),
        Rect::new(outer.x + pad, outer.y + 1, title_width, 1),
    );
    if described && height > 4 {
        frame.render_widget(
            Paragraph::new(clip(description, width.saturating_sub(pad * 2) as usize))
                .style(theme.muted),
            Rect::new(outer.x + pad, outer.y + 2, width.saturating_sub(pad * 2), 1),
        );
    }
    let content_top = (outer.y + 3 + u16::from(described)).min(outer.bottom());
    DialogFrame {
        outer,
        content: Rect::new(
            outer.x + pad,
            content_top,
            width.saturating_sub(pad * 2),
            outer.bottom().saturating_sub(content_top + 1),
        ),
        close,
    }
}

/// An inline alert box: an icon and title, one line of explanation and an
/// optional keycap action on the right. Returns the action's area.
pub(super) fn alert(
    frame: &mut Frame<'_>,
    area: Rect,
    icon: &str,
    title: &str,
    body: &str,
    action: Option<(&str, &str)>,
    theme: Theme,
) -> Option<Rect> {
    if area.height < 3 || area.width < 12 {
        return None;
    }
    rounded(frame, area, theme.dim, Style::default());
    let inner = Rect::new(
        area.x + 2,
        area.y + 1,
        area.width.saturating_sub(4),
        area.height - 2,
    );
    let mut action_area = None;
    let mut room = inner.width;
    if let Some((key, label)) = action {
        let line = Line::from(vec![
            keycap(key, theme),
            Span::styled(format!(" {label}"), theme.muted),
        ]);
        let width = line.width() as u16;
        if width + 12 < inner.width {
            let rect = Rect::new(inner.right() - width, inner.y, width, 1);
            frame.render_widget(Paragraph::new(line), rect);
            action_area = Some(rect);
            room = inner.width - width - 2;
        }
    }
    frame.render_widget(
        Paragraph::new(clip_line(
            Line::from(vec![
                Span::styled(format!("{icon} "), theme.title),
                Span::styled(title.to_owned(), theme.title),
            ]),
            room as usize,
        )),
        Rect::new(inner.x, inner.y, room, 1),
    );
    if inner.height >= 2 {
        frame.render_widget(
            Paragraph::new(clip(body, inner.width.saturating_sub(2) as usize)).style(theme.muted),
            Rect::new(inner.x + 2, inner.y + 1, inner.width.saturating_sub(2), 1),
        );
    }
    action_area
}

/// Kinds of feedback, which pick the toast's icon.
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
    } else if lower.starts_with("fetching")
        || lower.starts_with("loading")
        || lower.starts_with("working")
        || lower.ends_with("cancelled")
    {
        Tone::Info
    } else {
        Tone::Success
    }
}

/// Draws feedback as a bordered toast at the right edge, starting at row
/// `top`, and returns its area. Long messages are cut to three lines that
/// point at the details view.
pub(super) fn draw_toast(
    frame: &mut Frame<'_>,
    message: &str,
    top: u16,
    theme: Theme,
) -> Option<Rect> {
    let screen = frame.area();
    if message.is_empty() || screen.width < 24 || screen.height < 6 {
        return None;
    }
    let icon = match tone(message) {
        Tone::Error | Tone::Warning => "!",
        Tone::Info => "·",
        Tone::Success => "✓",
    };
    let width = Scale::new(screen).toast_width(Line::raw(message).width() as u16);
    let text_width = width.saturating_sub(8) as usize;
    let mut lines = wrap_lines(&message.replace('\n', " "), text_width);
    if lines.len() > 3 {
        lines.truncate(3);
        let last = clip(&lines[2], text_width.saturating_sub(9));
        lines[2] = format!("{last} · f2 more");
    }
    let height = lines.len() as u16 + 2;
    let y = (screen.y + top).min(screen.bottom().saturating_sub(height));
    let area = Rect::new(screen.right() - width - 2, y, width, height);
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(theme.base), area);
    rounded(frame, area, theme.border, theme.base.patch(theme.surface));
    frame.render_widget(
        Paragraph::new(icon).style(theme.title),
        Rect::new(area.x + 2, area.y + 1, 1, 1),
    );
    frame.render_widget(
        Paragraph::new(lines.into_iter().map(Line::raw).collect::<Vec<_>>()).style(theme.key),
        Rect::new(area.x + 4, area.y + 1, text_width as u16, height - 2),
    );
    Some(area)
}

/// The creation stepper: finished steps are checked, the current step is a
/// reverse chip and later steps are muted. When the full stepper is wider
/// than `width`, only the current step is shown with the step count.
pub(super) fn stepper(names: &[&str], current: usize, width: usize, theme: Theme) -> Line<'static> {
    let full = names
        .iter()
        .enumerate()
        .map(|(index, name)| Line::raw(*name).width() + if index == current { 4 } else { 2 })
        .sum::<usize>()
        + 6 * names.len().saturating_sub(1);
    if full > width {
        return Line::from(vec![
            Span::styled(
                format!(
                    " {} {} ",
                    current + 1,
                    names.get(current).copied().unwrap_or("")
                ),
                theme.focus,
            ),
            Span::styled(format!(" of {}", names.len()), theme.muted),
        ]);
    }
    let mut spans = Vec::new();
    for (index, name) in names.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" ──── ", theme.border));
        }
        spans.push(match index.cmp(&current) {
            std::cmp::Ordering::Less => Span::styled(format!("✓ {name}"), theme.key),
            std::cmp::Ordering::Equal => {
                Span::styled(format!(" {} {name} ", index + 1), theme.focus)
            }
            std::cmp::Ordering::Greater => {
                Span::styled(format!("{} {name}", index + 1), theme.muted)
            }
        });
    }
    Line::from(spans)
}

/// One session in the constellation: its name and the repositories it links.
pub(super) struct Group {
    pub(super) name: String,
    pub(super) repos: Vec<String>,
}

/// The narrowest signature, and the screen width a launcher needs to show it.
pub(super) const SIGNATURE_WIDTH: u16 = 60;
/// Constellation sessions and repository stars drawn at most.
pub(super) const MAX_GROUPS: usize = 12;
const STAR_SLOTS: usize = 12;
/// Longest repository name beside a star.
const LABEL_WIDTH: usize = 14;
const FRAMES_PER_GROUP: u64 = 64;

/// The star positions inside a signature `width` by `height`: one slot per
/// column band, rows cycling so neighbours never share a row and labels never
/// overlap.
fn star_slots(width: u16, height: u16) -> Vec<(u16, u16)> {
    let span = u32::from(width.saturating_sub(16));
    let rows = u32::from(height.max(1));
    (0..STAR_SLOTS as u32)
        .map(|index| {
            let x = 2 + index * span / STAR_SLOTS as u32;
            let y = (index * 4 + 1) % rows;
            (x as u16, y as u16)
        })
        .collect()
}

/// The launcher signature: repositories as stars, each session drawn as a
/// constellation joining its repositories, cycling through sessions. `tick`
/// is `None` for the still frame used under reduced motion. The signature is
/// as tall as `area` (at most 9 rows) and scales with its width.
pub(super) fn constellation(
    frame: &mut Frame<'_>,
    area: Rect,
    groups: &[Group],
    tick: Option<u64>,
    theme: Theme,
) {
    let width = Scale::new(area).signature_width().min(area.width);
    let height = area.height.min(9);
    if width < SIGNATURE_WIDTH || height < 7 {
        return;
    }
    let ox = area.x + (area.width - width) / 2;
    let oy = area.y;
    let f = tick.unwrap_or(40);
    let buffer = frame.buffer_mut();
    let stars = (width / 3).clamp(20, 60);
    let scatter_width = f64::from(width.saturating_sub(16));
    for star in 0..u64::from(stars) {
        let x = ox + 1 + (scatter(star, 1) * scatter_width) as u16;
        let y = oy + (scatter(star, 2) * f64::from(height)) as u16;
        let twinkle = (f as f64 * 0.15 + star as f64 * 1.7).sin();
        if let Some(cell) = buffer.cell_mut((x, y)) {
            cell.set_symbol(if twinkle > 0.6 { "+" } else { "·" });
            cell.set_style(if twinkle > 0.2 {
                theme.muted
            } else {
                theme.soft
            });
        }
    }
    let slots = star_slots(width, height);
    let mut names: Vec<&str> = Vec::new();
    for group in groups {
        for repo in &group.repos {
            if names.len() < slots.len() && !names.contains(&repo.as_str()) {
                names.push(repo);
            }
        }
    }
    let slot = |index: usize| (ox + slots[index].0, oy + slots[index].1);
    let current =
        (!groups.is_empty()).then(|| &groups[((f / FRAMES_PER_GROUP) as usize) % groups.len()]);
    if let Some(group) = current {
        let members: Vec<(u16, u16)> = group
            .repos
            .iter()
            .filter_map(|repo| names.iter().position(|name| *name == repo).map(slot))
            .collect();
        let progress =
            ((f % FRAMES_PER_GROUP) as f64 / 26.0).min(1.0) * members.len().max(1) as f64;
        for (index, pair) in members.windows(2).enumerate() {
            let segment = (progress - index as f64).clamp(0.0, 1.0);
            let (ax, ay) = (f64::from(pair[0].0), f64::from(pair[0].1));
            let (bx, by) = (f64::from(pair[1].0), f64::from(pair[1].1));
            let steps = (bx - ax).abs().max((by - ay).abs() * 2.0).max(1.0);
            let mut step = 1.0;
            while step < steps * segment {
                let x = (ax + (bx - ax) * step / steps).round() as u16;
                let y = (ay + (by - ay) * step / steps).round() as u16;
                if let Some(cell) = buffer.cell_mut((x, y)) {
                    cell.set_symbol("·");
                    cell.set_style(theme.muted);
                }
                step += 1.0;
            }
        }
    }
    // The "dws" mark and the session name sit on rows 3 and 4 at the right.
    let anchor = ox + width - 12;
    for (index, name) in names.iter().enumerate() {
        let on = current.is_some_and(|group| group.repos.iter().any(|repo| repo == name));
        let (x, y) = slot(index);
        buffer.set_string(
            x,
            y,
            if on { "●" } else { "○" },
            if on { theme.key } else { theme.soft },
        );
        let room = if y == oy + 3 || y == oy + 4 {
            usize::from(anchor.saturating_sub(x + 3)).min(LABEL_WIDTH)
        } else {
            LABEL_WIDTH
        };
        buffer.set_string(
            x + 2,
            y,
            clip(name, room),
            if on { theme.title } else { theme.dim },
        );
    }
    buffer.set_string(anchor, oy + 3, "dws", theme.title);
    if let Some(group) = current {
        buffer.set_string(anchor, oy + 4, clip(&group.name, 12), theme.muted);
    }
}

/// A deterministic pseudo-random value in [0, 1) for star placement.
fn scatter(a: u64, b: u64) -> f64 {
    let mut h = a
        .wrapping_mul(374_761_393)
        .wrapping_add(b.wrapping_mul(668_265_263));
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    ((h ^ (h >> 16)) % 10_000) as f64 / 10_000.0
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
    fn feedback_tones_paths_ages_and_button_sizes() {
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
        let button = Button {
            icon: "+",
            label: "New session",
            key: "ctrl+n",
            strong: false,
        };
        assert_eq!(button_width(&button, true), 25);
        assert_eq!(button_width(&button, false), 23);
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

    #[test]
    fn constellation_draws_the_current_session_and_holds_still() {
        let backend = ratatui::backend::TestBackend::new(64, 9);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let groups = vec![
            Group {
                name: "api-review".into(),
                repos: vec!["api".into(), "worker".into()],
            },
            Group {
                name: "docs".into(),
                repos: vec!["docs".into()],
            },
        ];
        terminal
            .draw(|frame| {
                constellation(frame, frame.area(), &groups, None, Theme::for_colors(true));
            })
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("dws") && text.contains("api-review") && text.contains("worker"));
    }
}
