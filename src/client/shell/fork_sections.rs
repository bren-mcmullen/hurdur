//! hurdur fork: extra sidebar sections fed by local scripts.
//!
//! Sections are configured in `sidebar-sections.toml` next to `config.toml`. Only this client
//! reads that file, so servers that don't know about it never report unknown keys. Each
//! section's command runs on its own interval off the render path, and its JSON Lines output
//! becomes rows below the Agents panel. Clicking a row opens its URL in the browser.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;
use tracing::warn;

use super::render::{display_width, put_text};
use super::{ClientShellConfig, ClientShellState, ShellHitMap};
use crate::app::state::Palette;

const FILE_NAME: &str = "sidebar-sections.toml";
const MAX_SECTIONS: usize = 8;
const MAX_ITEMS: usize = 50;
const MAX_LINES_PER_ITEM: usize = 3;
const MAX_OUTPUT_BYTES: u64 = 256 * 1024;
const MIN_INTERVAL_SECONDS: u64 = 5;
const MAX_TIMEOUT_SECONDS: u64 = 300;
/// Divider row plus title row above each section's items.
const SECTION_HEADER_ROWS: u16 = 2;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SectionsFile {
    sections: Vec<SectionConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct SectionConfig {
    title: String,
    command: String,
    #[serde(default = "default_interval_seconds")]
    interval_seconds: u64,
    #[serde(default = "default_timeout_seconds")]
    timeout_seconds: u64,
    #[serde(default = "default_max_items")]
    max_items: usize,
}

fn default_interval_seconds() -> u64 {
    60
}

fn default_timeout_seconds() -> u64 {
    30
}

fn default_max_items() -> usize {
    5
}

impl SectionConfig {
    fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_seconds.max(MIN_INTERVAL_SECONDS))
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_seconds.clamp(1, MAX_TIMEOUT_SECONDS))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpanColor {
    Text,
    Subtext,
    Dim,
    Accent,
    Red,
    Green,
    Yellow,
    Blue,
    Mauve,
    Teal,
    Peach,
}

impl SpanColor {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "text" => Self::Text,
            "subtext" => Self::Subtext,
            "dim" => Self::Dim,
            "accent" => Self::Accent,
            "red" => Self::Red,
            "green" => Self::Green,
            "yellow" => Self::Yellow,
            "blue" => Self::Blue,
            "mauve" => Self::Mauve,
            "teal" => Self::Teal,
            "peach" => Self::Peach,
            _ => return None,
        })
    }

    fn resolve(self, palette: &Palette) -> Color {
        match self {
            Self::Text => palette.text,
            Self::Subtext => palette.subtext0,
            Self::Dim => palette.overlay0,
            Self::Accent => palette.accent,
            Self::Red => palette.red,
            Self::Green => palette.green,
            Self::Yellow => palette.yellow,
            Self::Blue => palette.blue,
            Self::Mauve => palette.mauve,
            Self::Teal => palette.teal,
            Self::Peach => palette.peach,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Span {
    text: String,
    color: Option<SpanColor>,
    bold: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Item {
    url: Option<String>,
    lines: Vec<Vec<Span>>,
}

impl Item {
    fn height(&self) -> u16 {
        self.lines.len() as u16
    }
}

#[derive(Deserialize)]
struct RawItem {
    #[serde(default)]
    url: Option<String>,
    lines: Vec<Vec<RawSpan>>,
}

#[derive(Deserialize)]
struct RawSpan {
    text: String,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    bold: bool,
}

fn sanitize(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_control()).collect()
}

/// Parses a successful run's JSON Lines output. Empty output is a valid empty list; output with
/// content but no valid item is treated as a failure so the previous rows stay visible.
fn parse_output(output: &[u8]) -> Result<Vec<Item>, String> {
    let text = String::from_utf8_lossy(output);
    let mut items = Vec::new();
    let mut content_lines = 0usize;
    let mut invalid_lines = 0usize;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        content_lines += 1;
        let Ok(raw) = serde_json::from_str::<RawItem>(line) else {
            invalid_lines += 1;
            continue;
        };
        let lines = raw
            .lines
            .into_iter()
            .take(MAX_LINES_PER_ITEM)
            .map(|spans| {
                spans
                    .into_iter()
                    .map(|span| Span {
                        text: sanitize(&span.text),
                        color: span.color.as_deref().and_then(SpanColor::parse),
                        bold: span.bold,
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        if lines.is_empty() {
            invalid_lines += 1;
            continue;
        }
        let url = raw
            .url
            .filter(|url| crate::app::actions::safe_web_url(url).is_some());
        items.push(Item { url, lines });
        if items.len() == MAX_ITEMS {
            break;
        }
    }
    if invalid_lines > 0 {
        warn!(invalid_lines, "sidebar section output had invalid lines");
    }
    if content_lines > 0 && items.is_empty() {
        return Err("output had no valid items".into());
    }
    Ok(items)
}

struct Section {
    config: SectionConfig,
    items: Vec<Item>,
    /// `None` means the section has never run and should run now.
    next_run_at: Option<Instant>,
    running: Option<tokio::task::AbortHandle>,
}

impl Section {
    fn new(config: SectionConfig, items: Vec<Item>) -> Self {
        Self {
            config,
            items,
            next_run_at: None,
            running: None,
        }
    }
}

impl Drop for Section {
    fn drop(&mut self) {
        // Dropping the task drops its process-group guard, which kills the command.
        if let Some(task) = self.running.take() {
            task.abort();
        }
    }
}

struct RunResult {
    generation: u64,
    index: usize,
    output: Result<Vec<u8>, String>,
}

pub(super) struct ForkSections {
    path: Option<PathBuf>,
    sections: Vec<Section>,
    /// Bumped whenever the section list is rebuilt so results from old runs are ignored.
    generation: u64,
    results_tx: mpsc::Sender<RunResult>,
    results: mpsc::Receiver<RunResult>,
}

impl ForkSections {
    pub(super) fn load() -> Self {
        // Tests build client configs too; keep them independent of the user's file.
        let path = (!cfg!(test)).then(|| {
            crate::config::config_path()
                .parent()
                .map_or_else(|| PathBuf::from(FILE_NAME), |dir| dir.join(FILE_NAME))
        });
        let mut sections = Self::empty(path);
        sections.reload();
        sections
    }

    fn empty(path: Option<PathBuf>) -> Self {
        let (results_tx, results) = mpsc::channel();
        Self {
            path,
            sections: Vec::new(),
            generation: 0,
            results_tx,
            results,
        }
    }

    pub(super) fn reload(&mut self) {
        let Some(path) = self.path.as_deref() else {
            return;
        };
        let configs = match read_configs(path) {
            Ok(configs) => configs,
            Err(error) => {
                warn!(path = %path.display(), %error, "keeping previous sidebar sections");
                return;
            }
        };
        self.apply_configs(configs);
    }

    fn apply_configs(&mut self, configs: Vec<SectionConfig>) {
        if self
            .sections
            .iter()
            .map(|section| &section.config)
            .eq(configs.iter())
        {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        let mut previous = std::mem::take(&mut self.sections);
        self.sections = configs
            .into_iter()
            .map(|config| {
                let items = previous
                    .iter_mut()
                    .find(|old| {
                        old.config.title == config.title && old.config.command == config.command
                    })
                    .map(|old| std::mem::take(&mut old.items))
                    .unwrap_or_default();
                Section::new(config, items)
            })
            .collect();
    }

    /// Applies finished runs and starts due ones. Returns whether visible rows changed.
    pub(super) fn tick(&mut self, now: Instant) -> bool {
        let mut repaint = false;
        while let Ok(result) = self.results.try_recv() {
            if result.generation != self.generation {
                continue;
            }
            let Some(section) = self.sections.get_mut(result.index) else {
                continue;
            };
            section.running = None;
            match result.output.and_then(|output| parse_output(&output)) {
                Ok(items) => {
                    if section.items != items {
                        section.items = items;
                        repaint = true;
                    }
                }
                Err(error) => warn!(
                    section = %section.config.title,
                    %error,
                    "sidebar section command failed; keeping previous rows"
                ),
            }
        }

        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return repaint;
        };
        for (index, section) in self.sections.iter_mut().enumerate() {
            if section.running.is_some() || section.next_run_at.is_some_and(|at| now < at) {
                continue;
            }
            section.next_run_at = Some(now + section.config.interval());
            let results_tx = self.results_tx.clone();
            let generation = self.generation;
            let command = section.config.command.clone();
            let timeout = section.config.timeout();
            let task = runtime.spawn(async move {
                let output = run_command(&command, timeout).await;
                let _ = results_tx.send(RunResult {
                    generation,
                    index,
                    output,
                });
            });
            section.running = Some(task.abort_handle());
        }
        repaint
    }

    /// Picks which sections and how many of their items fit in `budget` rows. Earlier sections
    /// win, so overflow drops items from the bottom of the last section first.
    fn plan(&self, budget: u16) -> Vec<(usize, usize)> {
        let mut remaining = budget;
        let mut plan = Vec::new();
        for (index, section) in self.sections.iter().enumerate() {
            let mut shown = 0;
            let mut height = SECTION_HEADER_ROWS;
            for item in section.items.iter().take(section.config.max_items) {
                if height.saturating_add(item.height()) > remaining {
                    break;
                }
                height += item.height();
                shown += 1;
            }
            if shown == 0 {
                continue;
            }
            remaining -= height;
            plan.push((index, shown));
        }
        plan
    }

    fn plan_height(&self, plan: &[(usize, usize)]) -> u16 {
        plan.iter()
            .map(|(index, shown)| {
                SECTION_HEADER_ROWS
                    + self.sections[*index]
                        .items
                        .iter()
                        .take(*shown)
                        .map(Item::height)
                        .sum::<u16>()
            })
            .sum()
    }
}

fn read_configs(path: &std::path::Path) -> Result<Vec<SectionConfig>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let file = toml::from_str::<SectionsFile>(&text).map_err(|error| error.to_string())?;
    Ok(file.sections.into_iter().take(MAX_SECTIONS).collect())
}

async fn run_command(command: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;

    let mut process = crate::platform::detached_custom_command_process(command);
    process
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::platform::configure_status_command(&mut process);
    let mut process = tokio::process::Command::from(process);
    process.kill_on_drop(true);
    let mut child = process.spawn().map_err(|error| error.to_string())?;
    // Kills the whole process group (the script's children too) when this future ends or is
    // dropped.
    let _process_group =
        crate::platform::StatusCommandGuard::new(&child).map_err(|error| error.to_string())?;
    let stdout = child.stdout.take();
    let operation = async {
        let read_output = async {
            let mut output = Vec::new();
            if let Some(stdout) = stdout {
                stdout
                    .take(MAX_OUTPUT_BYTES)
                    .read_to_end(&mut output)
                    .await?;
            }
            std::io::Result::Ok(output)
        };
        let (status, output) = tokio::join!(child.wait(), read_output);
        let status = status.map_err(|error| error.to_string())?;
        let output = output.map_err(|error| error.to_string())?;
        if status.success() {
            Ok(output)
        } else {
            Err(format!("exited with {status}"))
        }
    };
    match tokio::time::timeout(timeout, operation).await {
        Ok(result) => result,
        Err(_) => Err(format!("timed out after {}s", timeout.as_secs())),
    }
}

#[derive(Default)]
pub(super) struct ForkSectionHits {
    items: Vec<(Rect, String)>,
    split_area: Option<Rect>,
}

pub(super) struct ForkSectionsLayout {
    area: Rect,
    plan: Vec<(usize, usize)>,
}

/// Takes rows for the fork sections off the bottom of the expanded sidebar. Returns the area
/// left for Spaces and Agents, plus the layout to pass to [`render`].
pub(super) fn split_sidebar(
    area: Rect,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) -> (Rect, ForkSectionsLayout) {
    let sections = &config.fork_sections;
    let plan = sections.plan(area.height / 2);
    let height = sections.plan_height(&plan);
    if height == 0 {
        return (
            area,
            ForkSectionsLayout {
                area: Rect::default(),
                plan,
            },
        );
    }
    let split = Rect::new(area.x, area.y, area.width, area.height - height);
    hits.fork_sections.split_area = Some(split);
    // The last column is the sidebar's separator.
    let fork_area = Rect::new(area.x, split.bottom(), area.width.saturating_sub(1), height);
    (
        split,
        ForkSectionsLayout {
            area: fork_area,
            plan,
        },
    )
}

pub(super) fn render(
    buffer: &mut Buffer,
    layout: &ForkSectionsLayout,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) {
    let area = layout.area;
    if area.is_empty() {
        return;
    }
    let palette = &config.palette;
    // Keep the last column free for the sidebar collapse toggle.
    let text_width = area.width.saturating_sub(1);
    let mut y = area.y;
    for (index, shown) in &layout.plan {
        let section = &config.fork_sections.sections[*index];
        put_text(
            buffer,
            area.x,
            y,
            area.width,
            &"─".repeat(area.width as usize),
            Style::default().fg(palette.surface_dim),
        );
        put_text(
            buffer,
            area.x,
            y + 1,
            text_width,
            &format!(" {}", sanitize(&section.config.title)),
            Style::default()
                .fg(palette.overlay0)
                .add_modifier(Modifier::BOLD),
        );
        y += SECTION_HEADER_ROWS;
        for item in section.items.iter().take(*shown) {
            for (offset, line) in item.lines.iter().enumerate() {
                render_line(buffer, area.x, y + offset as u16, text_width, line, palette);
            }
            if let Some(url) = &item.url {
                hits.fork_sections
                    .items
                    .push((Rect::new(area.x, y, area.width, item.height()), url.clone()));
            }
            y += item.height();
        }
    }
}

fn render_line(buffer: &mut Buffer, x: u16, y: u16, width: u16, spans: &[Span], palette: &Palette) {
    let mut column = 0u16;
    for span in spans {
        let remaining = width.saturating_sub(column);
        if remaining == 0 {
            break;
        }
        let text = crate::ui::truncate_end(&span.text, remaining as usize);
        let mut style = Style::default().fg(span
            .color
            .map_or(palette.subtext0, |color| color.resolve(palette)));
        if span.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        put_text(buffer, x + column, y, remaining, &text, style);
        column = column.saturating_add(display_width(&text));
    }
}

pub(super) fn clicked_url(hits: &ShellHitMap, point: (u16, u16)) -> Option<String> {
    hits.fork_sections
        .items
        .iter()
        .find(|(rect, _)| super::contains(*rect, point))
        .map(|(_, url)| url.clone())
}

/// The Spaces/Agents area the section divider drag should be measured against, when fork
/// sections have taken part of the sidebar.
pub(super) fn split_drag_area(hits: &ShellHitMap) -> Option<Rect> {
    hits.fork_sections.split_area
}

impl ClientShellState {
    pub(crate) fn tick_fork_sections(&mut self, now: Instant) -> bool {
        self.config.fork_sections.tick(now)
    }
}

#[cfg(test)]
impl ForkSections {
    /// Sections titled as given, each holding the items parsed from its JSON Lines output.
    pub(super) fn from_jsonl_for_test(sections: &[(&str, &str)]) -> Self {
        let mut fork_sections = Self::empty(None);
        fork_sections.sections = sections
            .iter()
            .map(|(title, output)| {
                Section::new(
                    test_section_config(title),
                    parse_output(output.as_bytes()).expect("valid test output"),
                )
            })
            .collect();
        fork_sections
    }
}

#[cfg(test)]
fn test_section_config(title: &str) -> SectionConfig {
    SectionConfig {
        title: title.into(),
        command: format!("{title}.sh"),
        interval_seconds: default_interval_seconds(),
        timeout_seconds: default_timeout_seconds(),
        max_items: default_max_items(),
    }
}

#[cfg(test)]
mod tests;
