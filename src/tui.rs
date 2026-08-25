use std::{
    collections::HashSet,
    fs,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    time::Duration,
};

use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};

use crate::{
    batch::derive_item_seed, preview, ApplicationService, DatabenderError, FilterSpec, JobQueue,
    MediaFormat, MediaPreview, MediaProbe, PluginCompatibility, PluginRegistry, QueueJob,
    QueueState, Result, FILTER_NAMES,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TuiTheme {
    #[default]
    Standard,
    HighContrast,
    Monochrome,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalCapabilities {
    pub interactive: bool,
    pub color: bool,
    pub size: Option<(u16, u16)>,
}

impl TerminalCapabilities {
    pub fn detect() -> Self {
        let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
        let term = std::env::var("TERM").ok();
        let no_color = std::env::var_os("NO_COLOR").is_some();
        Self::from_signals(
            interactive,
            term.as_deref(),
            no_color,
            interactive
                .then(crossterm::terminal::size)
                .and_then(|result| result.ok()),
        )
    }

    fn from_signals(
        interactive: bool,
        term: Option<&str>,
        no_color: bool,
        size: Option<(u16, u16)>,
    ) -> Self {
        Self {
            interactive,
            color: interactive && !no_color && term != Some("dumb"),
            size: interactive.then_some(size).flatten(),
        }
    }
}

#[derive(Clone, Debug)]
struct MediaEntry {
    path: PathBuf,
    probe: std::result::Result<MediaProbe, String>,
}

#[derive(Clone, Debug)]
pub struct TuiApp {
    entries: Vec<MediaEntry>,
    selected: usize,
    output_directory: String,
    editing_output: bool,
    filters: Vec<FilterSpec>,
    filter_specifications: Vec<String>,
    selected_filter: usize,
    filter_input: String,
    editing_filter: bool,
    editing_filter_index: Option<usize>,
    pipeline_error: Option<String>,
    plugins: PluginRegistry,
    selected_plugin: usize,
    seed: u64,
    theme: TuiTheme,
    preview: MediaPreview,
    preview_error: Option<String>,
    queue: JobQueue,
    queue_error: Option<String>,
    show_help: bool,
    show_catalog: bool,
    show_preview: bool,
}

impl TuiApp {
    pub fn load(inputs: &[PathBuf], output_directory: PathBuf, no_color: bool) -> Result<Self> {
        let theme = if no_color {
            TuiTheme::Monochrome
        } else {
            TuiTheme::Standard
        };
        Self::load_with_pipeline(inputs, output_directory, theme, Vec::new(), 0)
    }

    pub fn load_with_pipeline(
        inputs: &[PathBuf],
        output_directory: PathBuf,
        theme: TuiTheme,
        filters: Vec<FilterSpec>,
        seed: u64,
    ) -> Result<Self> {
        Self::load_with_plugins(
            inputs,
            output_directory,
            theme,
            filters,
            seed,
            PluginRegistry::default(),
        )
    }

    pub fn load_with_plugins(
        inputs: &[PathBuf],
        output_directory: PathBuf,
        theme: TuiTheme,
        filters: Vec<FilterSpec>,
        seed: u64,
        plugins: PluginRegistry,
    ) -> Result<Self> {
        let filter_specifications = filters.iter().map(FilterSpec::specification).collect();
        Self::load_with_plugin_specifications(
            inputs,
            output_directory,
            theme,
            filters,
            filter_specifications,
            seed,
            plugins,
        )
    }

    fn load_with_plugin_specifications(
        inputs: &[PathBuf],
        output_directory: PathBuf,
        theme: TuiTheme,
        filters: Vec<FilterSpec>,
        filter_specifications: Vec<String>,
        seed: u64,
        plugins: PluginRegistry,
    ) -> Result<Self> {
        let inputs = if inputs.is_empty() {
            vec![
                std::env::current_dir().map_err(|source| DatabenderError::Io {
                    path: PathBuf::from("."),
                    source,
                })?,
            ]
        } else {
            inputs.to_vec()
        };
        let mut paths = Vec::new();
        for input in inputs {
            collect_files(&input, &mut paths)?;
        }
        paths.sort();
        paths.dedup();
        let service = ApplicationService;
        let entries = paths
            .into_iter()
            .map(|path| MediaEntry {
                probe: service.probe(&path).map_err(|error| error.to_string()),
                path,
            })
            .collect();
        let mut app = Self {
            entries,
            selected: 0,
            output_directory: output_directory.to_string_lossy().into_owned(),
            editing_output: false,
            filters,
            filter_specifications,
            selected_filter: 0,
            filter_input: String::new(),
            editing_filter: false,
            editing_filter_index: None,
            pipeline_error: None,
            plugins,
            selected_plugin: 0,
            seed,
            theme,
            preview: MediaPreview::Unavailable,
            preview_error: None,
            queue: JobQueue::new(100)?,
            queue_error: None,
            show_help: false,
            show_catalog: false,
            show_preview: false,
        };
        app.validate_pipeline();
        app.refresh_preview();
        Ok(app)
    }

    pub fn selected_output(&self) -> Option<PathBuf> {
        self.entries
            .get(self.selected)
            .and_then(|entry| entry.path.file_name())
            .map(|name| Path::new(&self.output_directory).join(name))
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind != KeyEventKind::Press {
            return false;
        }
        if self.show_preview {
            if matches!(
                key.code,
                KeyCode::Char('v') | KeyCode::Char('q') | KeyCode::Esc
            ) {
                self.show_preview = false;
            }
            return false;
        }
        if self.show_help {
            if matches!(
                key.code,
                KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Esc
            ) {
                self.show_help = false;
            }
            return false;
        }
        if self.show_catalog {
            if matches!(
                key.code,
                KeyCode::Char('f') | KeyCode::Char('q') | KeyCode::Esc
            ) {
                self.show_catalog = false;
            }
            return false;
        }
        if self.editing_filter {
            match key.code {
                KeyCode::Esc => {
                    self.filter_input.clear();
                    self.editing_filter = false;
                    self.editing_filter_index = None;
                }
                KeyCode::Enter => self.commit_filter(),
                KeyCode::Backspace => {
                    self.filter_input.pop();
                }
                KeyCode::Char(character) => self.filter_input.push(character),
                _ => {}
            }
            return false;
        }
        if self.editing_output {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.editing_output = false,
                KeyCode::Backspace => {
                    self.output_directory.pop();
                }
                KeyCode::Char(character) => self.output_directory.push(character),
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => true,
            KeyCode::Char('?') => {
                self.show_help = true;
                false
            }
            KeyCode::Char('f') => {
                self.show_catalog = true;
                false
            }
            KeyCode::Char('v') => {
                self.show_preview = true;
                false
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.entries.is_empty() {
                    self.selected = (self.selected + 1).min(self.entries.len() - 1);
                    self.validate_pipeline();
                    self.refresh_preview();
                }
                false
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                self.validate_pipeline();
                self.refresh_preview();
                false
            }
            KeyCode::Home => {
                self.selected = 0;
                self.validate_pipeline();
                self.refresh_preview();
                false
            }
            KeyCode::End => {
                self.selected = self.entries.len().saturating_sub(1);
                self.validate_pipeline();
                self.refresh_preview();
                false
            }
            KeyCode::Char('o') => {
                self.editing_output = true;
                false
            }
            KeyCode::Char('p') => {
                self.filter_input.clear();
                self.editing_filter = true;
                self.editing_filter_index = None;
                self.pipeline_error = None;
                false
            }
            KeyCode::Char('d') => {
                if !self.filters.is_empty() {
                    self.filters.remove(self.selected_filter);
                    if self.selected_filter < self.filter_specifications.len() {
                        self.filter_specifications.remove(self.selected_filter);
                    }
                    self.selected_filter = self
                        .selected_filter
                        .min(self.filters.len().saturating_sub(1));
                }
                self.validate_pipeline();
                false
            }
            KeyCode::Left => {
                self.selected_filter = self.selected_filter.saturating_sub(1);
                false
            }
            KeyCode::Right => {
                self.selected_filter =
                    (self.selected_filter + 1).min(self.filters.len().saturating_sub(1));
                false
            }
            KeyCode::Char('[') => {
                self.selected_plugin = self.selected_plugin.saturating_sub(1);
                false
            }
            KeyCode::Char(']') => {
                self.selected_plugin =
                    (self.selected_plugin + 1).min(self.plugins.plugins().len().saturating_sub(1));
                false
            }
            KeyCode::Char('e') => {
                if let Some(filter) = self.filters.get(self.selected_filter) {
                    self.filter_input = self
                        .filter_specifications
                        .get(self.selected_filter)
                        .cloned()
                        .unwrap_or_else(|| filter.specification());
                    self.editing_filter = true;
                    self.editing_filter_index = Some(self.selected_filter);
                    self.pipeline_error = None;
                }
                false
            }
            KeyCode::Char('t') => {
                if !self.plugins.plugins().is_empty() {
                    if let Err(error) = self.plugins.toggle(self.selected_plugin) {
                        self.pipeline_error = Some(error.to_string());
                    }
                }
                false
            }
            KeyCode::Char('s') => {
                if let Err(error) = self.start_queue() {
                    self.queue_error = Some(error.to_string());
                }
                false
            }
            KeyCode::Char(' ') => {
                let result = match self.queue.snapshot().state {
                    QueueState::Running => self.queue.pause(),
                    QueueState::Paused => self.queue.resume(),
                    _ => Ok(()),
                };
                if let Err(error) = result {
                    self.queue_error = Some(error.to_string());
                }
                false
            }
            KeyCode::Char('c') => {
                if let Err(error) = self.queue.cancel() {
                    self.queue_error = Some(error.to_string());
                }
                false
            }
            KeyCode::Char('r') => {
                let result = match self.queue.snapshot().state {
                    QueueState::Cancelled => self.queue.resume(),
                    QueueState::Finished => self.queue.retry_failed(),
                    _ => Ok(()),
                };
                if let Err(error) = result {
                    self.queue_error = Some(error.to_string());
                }
                false
            }
            _ => false,
        }
    }

    fn start_queue(&mut self) -> Result<()> {
        if self.filters.is_empty() {
            return Err(invalid_tui(
                "add at least one pipeline filter before starting",
            ));
        }
        if let Some(error) = &self.pipeline_error {
            return Err(invalid_tui(format!("pipeline preflight failed: {error}")));
        }
        let output_directory = PathBuf::from(&self.output_directory);
        let mut outputs = HashSet::new();
        let mut jobs = Vec::new();
        for entry in &self.entries {
            if entry.probe.is_err() {
                continue;
            }
            let Some(file_name) = entry.path.file_name() else {
                continue;
            };
            let output = output_directory.join(file_name);
            if !outputs.insert(output.clone()) {
                return Err(invalid_tui(format!(
                    "multiple inputs resolve to output {}",
                    output.display()
                )));
            }
            jobs.push(QueueJob {
                input: entry.path.clone(),
                output,
                filters: self.filters.clone(),
                seed: derive_item_seed(self.seed, &entry.path),
                protect_output: false,
            });
        }
        if jobs.is_empty() {
            return Err(invalid_tui("no supported media inputs are queued"));
        }
        self.queue_error = None;
        self.queue.start(jobs)
    }

    fn commit_filter(&mut self) {
        let specification = self.filter_input.clone();
        let filter = match FilterSpec::parse(&self.filter_input) {
            Ok(filter) => filter,
            Err(error) => {
                self.pipeline_error = Some(error.to_string());
                return;
            }
        };
        if let Some(index) = self.editing_filter_index.take() {
            while self.filter_specifications.len() < self.filters.len() {
                let missing = self.filter_specifications.len();
                self.filter_specifications
                    .push(self.filters[missing].specification());
            }
            self.filters[index] = filter;
            self.filter_specifications[index] = specification;
            self.selected_filter = index;
        } else {
            self.filters.push(filter);
            self.filter_specifications.push(specification);
            self.selected_filter = self.filters.len() - 1;
        }
        self.filter_input.clear();
        self.editing_filter = false;
        self.validate_pipeline();
    }

    fn validate_pipeline(&mut self) {
        let Some(Ok(probe)) = self.entries.get(self.selected).map(|entry| &entry.probe) else {
            return;
        };
        self.pipeline_error = if self.filters.is_empty() {
            None
        } else {
            ApplicationService
                .build_plan(probe.format, self.filters.clone(), self.seed)
                .err()
                .map(|error| error.to_string())
        };
    }

    fn refresh_preview(&mut self) {
        let Some(entry) = self.entries.get(self.selected) else {
            self.preview = MediaPreview::Unavailable;
            self.preview_error = None;
            return;
        };
        let Ok(probe) = &entry.probe else {
            self.preview = MediaPreview::Unavailable;
            self.preview_error = None;
            return;
        };
        match preview::generate(&entry.path, probe.format) {
            Ok(preview) => {
                self.preview = preview;
                self.preview_error = None;
            }
            Err(error) => {
                self.preview = MediaPreview::Unavailable;
                self.preview_error = Some(error.to_string());
            }
        }
    }

    fn draw(&self, frame: &mut Frame<'_>) {
        let regions =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(frame.area());
        let body = regions[0];
        if self.show_preview {
            self.draw_preview(frame, body);
            self.draw_controls_hint(frame, regions[1]);
            return;
        }
        let vertical = body.width < 72;
        let direction = if vertical {
            Direction::Vertical
        } else {
            Direction::Horizontal
        };
        let constraints = if vertical {
            [Constraint::Length(body.height.min(5)), Constraint::Min(1)]
        } else {
            [Constraint::Percentage(45), Constraint::Percentage(55)]
        };
        let panes = Layout::default()
            .direction(direction)
            .constraints(constraints)
            .split(body);
        self.draw_inputs(frame, panes[0]);
        self.draw_details(frame, panes[1]);
        self.draw_controls_hint(frame, regions[1]);
        if self.show_help {
            self.draw_controls_guide(frame);
        }
        if self.show_catalog {
            self.draw_catalog(frame);
        }
    }

    fn draw_controls_hint(&self, frame: &mut Frame<'_>, area: Rect) {
        let text = if self.show_preview {
            "Expanded preview | v, q, or Esc close"
        } else if area.width >= 100 {
            "? Help | f Filters/formats | v Preview | p Add | Left/Right Select | e Edit | d Remove | s Start | q Quit"
        } else if area.width >= 60 {
            "? Help | f List | v Preview | p Add | e Edit | q Quit"
        } else {
            "? Help  f List  v Preview  q Quit"
        };
        frame.render_widget(Paragraph::new(text).style(self.accent()), area);
    }

    fn draw_controls_guide(&self, frame: &mut Frame<'_>) {
        let area = centered_rect(frame.area(), 76, 17);
        let lines = vec![
            Line::from(vec![
                Span::styled("Navigation  ", self.accent()),
                Span::raw("Up/Down or j/k select  Home/End first/last"),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Pipeline    ", self.accent()),
                Span::raw("p add  Left/Right select  e edit  d remove"),
            ]),
            Line::from(vec![
                Span::styled("Catalog     ", self.accent()),
                Span::raw("f list formats and filters"),
            ]),
            Line::from(vec![
                Span::styled("Preview     ", self.accent()),
                Span::raw("v expand  v, q, or Esc close"),
            ]),
            Line::from(vec![
                Span::styled("Output      ", self.accent()),
                Span::raw("o edit directory  Enter or Esc finish"),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Queue       ", self.accent()),
                Span::raw("s start  Space pause/resume  c cancel  r resume/retry"),
            ]),
            Line::from(vec![
                Span::styled("Plugins     ", self.accent()),
                Span::raw("[ / ] select  t enable/disable"),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Guide       ", self.accent()),
                Span::raw("? toggle  q or Esc close"),
            ]),
            Line::from(vec![
                Span::styled("Application ", self.accent()),
                Span::raw("q or Esc quit when guide is closed"),
            ]),
        ];
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(lines)
                .block(Block::default().title(" Controls ").borders(Borders::ALL))
                .wrap(Wrap { trim: true }),
            area,
        );
    }

    fn draw_catalog(&self, frame: &mut Frame<'_>) {
        let area = centered_rect(frame.area(), 82, 17);
        let formats = MediaFormat::ALL
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("  ");
        let mut lines = vec![
            Line::from(Span::styled("Formats", self.accent())),
            Line::from(formats),
            Line::from(""),
            Line::from(Span::styled("Filters", self.accent())),
        ];
        lines.extend(FILTER_NAMES.chunks(3).map(|filters| {
            Line::from(
                filters
                    .iter()
                    .map(|filter| format!("{filter:<24}"))
                    .collect::<String>(),
            )
        }));
        lines.push(Line::from(""));
        lines.push(Line::from("f, q, or Esc closes"));
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .title(" Formats and filters ")
                    .borders(Borders::ALL),
            ),
            area,
        );
    }

    fn draw_inputs(&self, frame: &mut Frame<'_>, area: Rect) {
        let items = self.entries.iter().enumerate().map(|(index, entry)| {
            let name = entry
                .path
                .file_name()
                .unwrap_or_else(|| entry.path.as_os_str())
                .to_string_lossy();
            let style = if index == self.selected {
                self.accent().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(name).style(style)
        });
        let title = format!(" Inputs ({}) ", self.entries.len());
        frame.render_widget(
            List::new(items).block(Block::default().title(title).borders(Borders::ALL)),
            area,
        );
    }

    fn draw_details(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.height >= 29 {
            let chunks = Layout::vertical([
                Constraint::Length(7),
                Constraint::Min(5),
                Constraint::Length(7),
                Constraint::Length(7),
                Constraint::Length(3),
            ])
            .split(area);
            self.draw_media(frame, chunks[0]);
            self.draw_preview(frame, chunks[1]);
            self.draw_pipeline(frame, chunks[2]);
            self.draw_queue(frame, chunks[3]);
            self.draw_output(frame, chunks[4]);
            return;
        }
        if area.height >= 23 {
            let chunks = Layout::vertical([
                Constraint::Length(7),
                Constraint::Min(5),
                Constraint::Length(7),
                Constraint::Length(3),
            ])
            .split(area);
            self.draw_media(frame, chunks[0]);
            self.draw_preview(frame, chunks[1]);
            self.draw_pipeline(frame, chunks[2]);
            self.draw_output(frame, chunks[3]);
            return;
        }
        let chunks = Layout::vertical([
            Constraint::Min(5),
            Constraint::Length(7),
            Constraint::Length(3),
        ])
        .split(area);
        self.draw_media(frame, chunks[0]);
        self.draw_pipeline(frame, chunks[1]);
        self.draw_output(frame, chunks[2]);
    }

    fn draw_media(&self, frame: &mut Frame<'_>, area: Rect) {
        frame.render_widget(
            Paragraph::new(self.detail_lines())
                .block(Block::default().title(" Media ").borders(Borders::ALL))
                .wrap(Wrap { trim: true }),
            area,
        );
    }

    fn draw_preview(&self, frame: &mut Frame<'_>, area: Rect) {
        let lines = if let Some(error) = &self.preview_error {
            vec![Line::from(error.clone())]
        } else {
            match &self.preview {
                MediaPreview::Pixels { rows, .. } => self.pixel_preview_lines(rows, area),
                MediaPreview::Waveform { rows } => rows.iter().cloned().map(Line::from).collect(),
                MediaPreview::Unavailable => vec![Line::from("Preview unavailable")],
            }
        };
        frame.render_widget(
            Paragraph::new(lines).block(Block::default().title(" Preview ").borders(Borders::ALL)),
            area,
        );
    }

    fn pixel_preview_lines(
        &self,
        rows: &[Vec<preview::PreviewPixel>],
        area: Rect,
    ) -> Vec<Line<'static>> {
        let available_width = usize::from(area.width.saturating_sub(2));
        let available_height = usize::from(area.height.saturating_sub(2));
        let source_width = rows.iter().map(Vec::len).min().unwrap_or(0);
        if available_width == 0 || available_height == 0 || source_width == 0 || rows.is_empty() {
            return Vec::new();
        }
        let double_width = available_width >= 2;
        let target_width = source_width.min(if double_width {
            available_width / 2
        } else {
            available_width
        });
        let target_height = rows.len().min(available_height);

        (0..target_height)
            .map(|target_row| {
                let source_row = scaled_index(target_row, target_height, rows.len());
                let spans = (0..target_width)
                    .map(|target_column| {
                        let source_column = scaled_index(target_column, target_width, source_width);
                        let pixel = rows[source_row][source_column];
                        let style = if self.theme == TuiTheme::Monochrome {
                            Style::default()
                        } else {
                            let color = Color::Rgb(pixel.rgb[0], pixel.rgb[1], pixel.rgb[2]);
                            Style::default().fg(color).bg(color)
                        };
                        let character = pixel.character.to_string();
                        Span::styled(
                            if double_width {
                                character.repeat(2)
                            } else {
                                character
                            },
                            style,
                        )
                    })
                    .collect::<Vec<_>>();
                Line::from(spans)
            })
            .collect()
    }

    fn draw_output(&self, frame: &mut Frame<'_>, area: Rect) {
        let output_style = if self.editing_output {
            self.accent().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(self.output_directory.as_str())
                .style(output_style)
                .block(
                    Block::default()
                        .title(" Output directory ")
                        .borders(Borders::ALL),
                ),
            area,
        );
        if self.editing_output && area.width > 2 {
            let width = usize::from(area.width.saturating_sub(2));
            let cursor = self.output_directory.chars().count().min(width) as u16;
            frame.set_cursor_position((area.x + 1 + cursor, area.y + 1));
        }
    }

    fn draw_pipeline(&self, frame: &mut Frame<'_>, area: Rect) {
        let available_rows = usize::from(area.height.saturating_sub(2));
        let adding_filter = self.editing_filter && self.editing_filter_index.is_none();
        let filter_rows = available_rows.saturating_sub(usize::from(adding_filter));
        let filter_start = self
            .selected_filter
            .saturating_sub(filter_rows.saturating_sub(1));
        let mut lines = self
            .filters
            .iter()
            .enumerate()
            .skip(filter_start)
            .take(filter_rows)
            .map(|(index, filter)| {
                let selected = index == self.selected_filter;
                let specification = if self.editing_filter_index == Some(index) {
                    self.filter_input.clone()
                } else {
                    self.filter_specifications
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| filter.specification())
                };
                let style = if selected {
                    self.accent().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                Line::from(format!(
                    "{} {}. {specification}",
                    if selected { ">" } else { " " },
                    index + 1,
                ))
                .style(style)
            })
            .collect::<Vec<_>>();
        if adding_filter {
            lines.push(Line::from(vec![
                Span::styled("+ ", self.accent()),
                Span::raw(self.filter_input.clone()),
            ]));
        }
        for (index, plugin) in self.plugins.plugins().iter().enumerate() {
            let marker = if index == self.selected_plugin {
                ">"
            } else {
                " "
            };
            let state = if plugin.enabled {
                "enabled"
            } else {
                "disabled"
            };
            let compatibility = match &plugin.compatibility {
                PluginCompatibility::Compatible => "compatible".to_owned(),
                PluginCompatibility::Incompatible(reason) => format!("incompatible: {reason}"),
            };
            lines.push(Line::from(format!(
                "{marker} plugin {} {} [{state}; {compatibility}]",
                plugin.manifest.id, plugin.manifest.version
            )));
            lines.push(Line::from(format!("  {}", plugin.manifest_path.display())));
        }
        if let Some(error) = &self.pipeline_error {
            lines.push(Line::from(Span::styled(
                error.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )));
        }
        if lines.is_empty() {
            lines.push(Line::from("No filters"));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .block(Block::default().title(" Pipeline ").borders(Borders::ALL))
                .wrap(Wrap { trim: true }),
            area,
        );
        if self.editing_filter && area.width > 4 {
            let (prefix_width, row) = if let Some(index) = self.editing_filter_index {
                (
                    format!("> {}. ", index + 1).chars().count(),
                    1 + index.saturating_sub(filter_start),
                )
            } else {
                (
                    2,
                    1 + self
                        .filters
                        .len()
                        .saturating_sub(filter_start)
                        .min(filter_rows),
                )
            };
            let interior_width = usize::from(area.width.saturating_sub(2));
            let prefix_width = prefix_width.min(interior_width.saturating_sub(1));
            let width = interior_width.saturating_sub(prefix_width);
            let cursor = self
                .filter_input
                .chars()
                .count()
                .min(width.saturating_sub(1)) as u16;
            frame.set_cursor_position((
                area.x + 1 + prefix_width as u16 + cursor,
                area.y + row as u16,
            ));
        }
    }

    fn draw_queue(&self, frame: &mut Frame<'_>, area: Rect) {
        let snapshot = self.queue.snapshot();
        let mut lines = vec![Line::from(format!(
            "{:?}  completed={} failed={} pending={}",
            snapshot.state, snapshot.completed, snapshot.failed, snapshot.pending
        ))];
        if let Some(current) = snapshot.current {
            lines.push(Line::from(format!("active {}", current.display())));
        }
        if let Some(error) = &self.queue_error {
            lines.push(Line::from(Span::styled(
                error.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )));
        }
        let remaining = usize::from(area.height.saturating_sub(3 + lines.len() as u16));
        lines.extend(
            snapshot
                .logs
                .into_iter()
                .rev()
                .take(remaining)
                .rev()
                .map(Line::from),
        );
        frame.render_widget(
            Paragraph::new(lines)
                .block(Block::default().title(" Queue ").borders(Borders::ALL))
                .wrap(Wrap { trim: true }),
            area,
        );
    }

    fn detail_lines(&self) -> Vec<Line<'static>> {
        let Some(entry) = self.entries.get(self.selected) else {
            return vec![Line::from("No media files")];
        };
        let mut lines = vec![Line::from(vec![
            Span::styled("Path  ", self.accent()),
            Span::raw(entry.path.display().to_string()),
        ])];
        let probe = match &entry.probe {
            Ok(probe) => probe,
            Err(error) => {
                lines.push(Line::from(vec![
                    Span::styled("Probe ", self.accent()),
                    Span::raw(error.clone()),
                ]));
                return lines;
            }
        };
        lines.push(detail("Format", probe.format.to_string(), self.accent()));
        lines.push(detail(
            "Status",
            if probe.available {
                "available"
            } else {
                "unavailable"
            },
            self.accent(),
        ));
        if let Some((width, height)) = probe.dimensions {
            lines.push(detail("Size", format!("{width}x{height}"), self.accent()));
        }
        if let Some(video) = &probe.video_stream {
            lines.push(detail(
                "Video",
                format!(
                    "{} {}x{}",
                    video.codec_name, video.properties.width, video.properties.height
                ),
                self.accent(),
            ));
        }
        for (index, audio) in probe.audio_streams.iter().enumerate() {
            lines.push(detail(
                &format!("Audio {}", index + 1),
                format!(
                    "{} {} Hz, {} ch",
                    audio.codec_name, audio.properties.sample_rate, audio.properties.channels
                ),
                self.accent(),
            ));
        }
        if let Some(metadata) = &probe.metadata {
            for (key, value) in &metadata.format {
                lines.push(detail(key, value.clone(), self.accent()));
            }
        }
        let domains = probe
            .capabilities
            .domains
            .iter()
            .map(|domain| format!("{domain:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(detail("Filters", domains, self.accent()));
        let queue = self.queue.snapshot();
        lines.push(detail(
            "Queue",
            format!(
                "{:?} {}/{}",
                queue.state,
                queue.completed,
                queue.completed + queue.pending + usize::from(queue.current.is_some())
            ),
            self.accent(),
        ));
        lines
    }

    fn accent(&self) -> Style {
        match self.theme {
            TuiTheme::Standard => Style::default().fg(Color::Cyan),
            TuiTheme::HighContrast => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            TuiTheme::Monochrome => Style::default().add_modifier(Modifier::BOLD),
        }
    }
}

pub fn run(inputs: &[PathBuf], output_directory: PathBuf, no_color: bool) -> Result<()> {
    let theme = if no_color {
        TuiTheme::Monochrome
    } else {
        TuiTheme::Standard
    };
    run_with_pipeline(inputs, output_directory, theme, Vec::new(), 0)
}

pub fn run_with_pipeline(
    inputs: &[PathBuf],
    output_directory: PathBuf,
    theme: TuiTheme,
    filters: Vec<FilterSpec>,
    seed: u64,
) -> Result<()> {
    run_with_plugins(
        inputs,
        output_directory,
        theme,
        filters,
        seed,
        PluginRegistry::default(),
    )
}

pub fn run_with_plugins(
    inputs: &[PathBuf],
    output_directory: PathBuf,
    theme: TuiTheme,
    filters: Vec<FilterSpec>,
    seed: u64,
    plugins: PluginRegistry,
) -> Result<()> {
    let filter_specifications = filters.iter().map(FilterSpec::specification).collect();
    run_with_plugin_specifications(
        inputs,
        output_directory,
        theme,
        filters,
        filter_specifications,
        seed,
        plugins,
    )
}

pub fn run_with_plugin_specifications(
    inputs: &[PathBuf],
    output_directory: PathBuf,
    theme: TuiTheme,
    filters: Vec<FilterSpec>,
    filter_specifications: Vec<String>,
    seed: u64,
    plugins: PluginRegistry,
) -> Result<()> {
    if !TerminalCapabilities::detect().interactive {
        return Err(invalid_tui(
            "interactive terminal input and output are required",
        ));
    }
    let mut app = TuiApp::load_with_plugin_specifications(
        inputs,
        output_directory,
        theme,
        filters,
        filter_specifications,
        seed,
        plugins,
    )?;
    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).map_err(terminal_error)?;
    run_loop(&mut terminal, &mut app)
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().map_err(terminal_error)?;
        if let Err(source) = execute!(io::stdout(), EnterAlternateScreen, Hide) {
            let _ = disable_raw_mode();
            return Err(terminal_error(source));
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
    }
}

fn run_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut TuiApp) -> Result<()> {
    loop {
        terminal
            .draw(|frame| app.draw(frame))
            .map_err(terminal_error)?;
        if event::poll(Duration::from_millis(100)).map_err(terminal_error)? {
            if let Event::Key(key) = event::read().map_err(terminal_error)? {
                if app.handle_key(key) {
                    return Ok(());
                }
            }
        }
    }
}

fn collect_files(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let metadata = fs::metadata(path).map_err(|source| DatabenderError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.is_file() {
        if MediaFormat::from_path_extension(path).is_some() {
            files.push(path.to_path_buf());
        }
    } else if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .map_err(|source| DatabenderError::Io {
                path: path.to_path_buf(),
                source,
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| DatabenderError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            collect_files(&entry.path(), files)?;
        }
    }
    Ok(())
}

fn centered_rect(area: Rect, maximum_width: u16, maximum_height: u16) -> Rect {
    let width = if area.width > 4 {
        area.width.saturating_sub(4).min(maximum_width)
    } else {
        area.width
    };
    let height = if area.height > 2 {
        area.height.saturating_sub(2).min(maximum_height)
    } else {
        area.height
    };
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn scaled_index(index: usize, target_length: usize, source_length: usize) -> usize {
    if target_length <= 1 {
        0
    } else {
        index * (source_length - 1) / (target_length - 1)
    }
}

fn detail(label: &str, value: impl Into<String>, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<7}"), style),
        Span::raw(value.into()),
    ])
}

fn terminal_error(source: io::Error) -> DatabenderError {
    DatabenderError::Io {
        path: PathBuf::from("<terminal>"),
        source,
    }
}

fn invalid_tui(reason: impl Into<String>) -> DatabenderError {
    DatabenderError::InvalidParameter {
        parameter: "tui".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use image::{ImageBuffer, Rgba};
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::{
        plugin::{PLUGIN_ABI_VERSION, PLUGIN_MANIFEST_VERSION},
        PluginRegistryConfig,
    };

    fn fixture() -> (tempfile::TempDir, TuiApp) {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("a.png");
        let second = directory.path().join("b.png");
        ImageBuffer::from_pixel(2, 3, Rgba([10_u8, 20, 30, 255]))
            .save(&first)
            .unwrap();
        ImageBuffer::from_pixel(4, 5, Rgba([30_u8, 20, 10, 255]))
            .save(&second)
            .unwrap();
        let app = TuiApp::load(
            &[directory.path().to_path_buf()],
            PathBuf::from("out"),
            true,
        )
        .unwrap();
        (directory, app)
    }

    #[test]
    fn detects_interactivity_color_and_terminal_size_capabilities() {
        assert_eq!(
            TerminalCapabilities::from_signals(true, Some("xterm-256color"), false, Some((80, 24))),
            TerminalCapabilities {
                interactive: true,
                color: true,
                size: Some((80, 24)),
            }
        );
        assert_eq!(
            TerminalCapabilities::from_signals(false, Some("dumb"), true, Some((80, 24))),
            TerminalCapabilities {
                interactive: false,
                color: false,
                size: None,
            }
        );
    }

    #[test]
    fn input_collection_excludes_files_without_supported_extensions() {
        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(directory.path().join("image.PNG"), b"fixture").unwrap();
        fs::write(nested.join("audio.opus"), b"fixture").unwrap();
        fs::write(directory.path().join("notes.txt"), b"fixture").unwrap();
        fs::write(directory.path().join("extensionless"), b"fixture").unwrap();
        let mut files = Vec::new();

        collect_files(directory.path(), &mut files).unwrap();

        assert_eq!(
            files,
            [
                directory.path().join("image.PNG"),
                nested.join("audio.opus")
            ]
        );
    }

    fn plugin_registry(directory: &Path) -> PluginRegistry {
        fs::write(
            directory.join("example.plugin.json"),
            format!(
                r#"{{"manifest_version":{PLUGIN_MANIFEST_VERSION},"abi_version":{PLUGIN_ABI_VERSION},"id":"example.frames","name":"Example","version":"1.0.0","filters":[{{"id":"invert","name":"Invert","description":"Invert","domains":["image-frame"],"deterministic":true,"parameters":[]}}]}}"#
            ),
        )
        .unwrap();
        fs::write(
            directory.join("example.wasm"),
            br#"(module
                (memory (export "memory") 1)
                (func (export "databender_alloc") (param i32) (result i32) i32.const 0)
                (func (export "databender_run") (param i32 i32) (result i64) i64.const 0))"#,
        )
        .unwrap();
        PluginRegistry::discover(&PluginRegistryConfig {
            directories: vec![directory.to_path_buf()],
            disabled: Default::default(),
        })
        .unwrap()
    }

    #[test]
    fn toggles_selected_plugin_availability() {
        let directory = tempfile::tempdir().unwrap();
        let plugins = plugin_registry(directory.path());
        let input = directory.path().join("example.plugin.json");
        let mut app = TuiApp::load_with_plugins(
            &[input],
            directory.path().join("output"),
            TuiTheme::Standard,
            Vec::new(),
            42,
            plugins,
        )
        .unwrap();

        assert!(app.plugins.plugins()[0].available());
        app.handle_key(KeyEvent::from(KeyCode::Char('t')));
        assert!(!app.plugins.plugins()[0].available());
    }

    #[test]
    fn keyboard_navigation_and_output_editing_are_mouse_free() {
        let (_directory, mut app) = fixture();
        app.handle_key(KeyEvent::from(KeyCode::Down));
        app.handle_key(KeyEvent::from(KeyCode::Char('o')));
        app.handle_key(KeyEvent::from(KeyCode::Char('2')));
        app.handle_key(KeyEvent::from(KeyCode::Enter));

        assert_eq!(app.selected, 1);
        assert_eq!(app.selected_output(), Some(PathBuf::from("out2/b.png")));
        assert!(!app.editing_output);
    }

    #[test]
    fn narrow_snapshot_contains_media_probe_and_output() {
        let (_directory, app) = fixture();
        let backend = TestBackend::new(48, 18);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| app.draw(frame)).unwrap();
        let rows = terminal
            .backend()
            .buffer()
            .content
            .chunks(48)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        let rendered = rows.concat();

        assert!(rendered.contains("Inputs (2)"));
        assert!(rendered.contains("Format png"));
        assert!(rendered.contains("Pipeline"));
        assert!(rendered.contains("No filters"));
        assert!(rendered.contains("Output directory"));
        assert!(rendered.contains("? Help"));
        assert!(rows[16].contains('┘'));
        assert!(!rows[16].contains("? Help"));
        assert!(rows[17].contains("? Help"));
    }

    #[test]
    fn inline_preview_samples_the_full_image_height() {
        let (_directory, mut app) = fixture();
        app.preview = MediaPreview::Pixels {
            source_width: 4,
            source_height: 6,
            rows: (0..6)
                .map(|row| {
                    vec![
                        preview::PreviewPixel {
                            character: char::from_digit(row, 16).unwrap(),
                            rgb: [row as u8, 0, 0],
                        };
                        4
                    ]
                })
                .collect(),
        };
        let backend = TestBackend::new(10, 5);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| app.draw_preview(frame, frame.area()))
            .unwrap();
        let rows = terminal
            .backend()
            .buffer()
            .content
            .chunks(10)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();

        assert!(rows[1].contains("00000000"));
        assert!(rows[2].contains("22222222"));
        assert!(rows[3].contains("55555555"));
    }

    #[test]
    fn expanded_preview_uses_the_body_and_blocks_background_actions() {
        let (_directory, mut app) = fixture();
        app.filters.push(FilterSpec::Invert);

        app.handle_key(KeyEvent::from(KeyCode::Char('v')));
        app.handle_key(KeyEvent::from(KeyCode::Char('s')));
        assert!(app.show_preview);
        assert_eq!(app.queue.snapshot().state, QueueState::Idle);

        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Preview"));
        assert!(rendered.contains("Expanded preview"));
        assert!(!rendered.contains("Inputs"));

        assert!(!app.handle_key(KeyEvent::from(KeyCode::Char('q'))));
        assert!(!app.show_preview);
    }

    #[test]
    fn controls_guide_lists_keys_and_blocks_background_actions() {
        let (_directory, mut app) = fixture();
        app.filters.push(FilterSpec::Invert);
        assert!(!app.handle_key(KeyEvent::from(KeyCode::Char('?'))));
        assert!(app.show_help);

        app.handle_key(KeyEvent::from(KeyCode::Char('s')));
        assert_eq!(app.queue.snapshot().state, QueueState::Idle);
        assert!(app.show_help);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for expected in [
            "Controls",
            "Navigation",
            "Pipeline",
            "Preview",
            "Output",
            "Queue",
            "Plugins",
            "Application",
        ] {
            assert!(rendered.contains(expected), "missing {expected}");
        }

        assert!(!app.handle_key(KeyEvent::from(KeyCode::Esc)));
        assert!(!app.show_help);
        assert!(app.handle_key(KeyEvent::from(KeyCode::Char('q'))));
    }

    #[test]
    fn catalog_lists_formats_and_filters_and_blocks_background_actions() {
        let (_directory, mut app) = fixture();
        app.filters.push(FilterSpec::Invert);

        app.handle_key(KeyEvent::from(KeyCode::Char('f')));
        app.handle_key(KeyEvent::from(KeyCode::Char('s')));
        assert!(app.show_catalog);
        assert_eq!(app.queue.snapshot().state, QueueState::Idle);

        let backend = TestBackend::new(90, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for expected in [
            "Formats and filters",
            "jpeg",
            "mkv",
            "huffman-glitch",
            "expert-video-graph",
        ] {
            assert!(rendered.contains(expected), "missing {expected}");
        }

        app.handle_key(KeyEvent::from(KeyCode::Char('f')));
        assert!(!app.show_catalog);
    }

    #[test]
    fn pipeline_editor_preserves_order_and_reports_preflight_errors() {
        let (_directory, mut app) = fixture();
        app.handle_key(KeyEvent::from(KeyCode::Char('p')));
        for character in "invert".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(character)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        app.handle_key(KeyEvent::from(KeyCode::Char('p')));
        for character in "audio-noise".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(character)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));

        assert_eq!(app.filters[0], FilterSpec::Invert);
        assert_eq!(app.filters[1].name(), "audio-noise");
        assert!(app
            .pipeline_error
            .as_deref()
            .is_some_and(|error| error.contains("incompatible")));
    }

    #[test]
    fn edits_and_removes_the_selected_pipeline_entry() {
        let (_directory, mut app) = fixture();
        app.filters = vec![
            FilterSpec::ChannelShift { pixels: 12 },
            FilterSpec::Brightness { delta: 24 },
        ];

        app.handle_key(KeyEvent::from(KeyCode::Char('e')));
        assert_eq!(app.filter_input, "channel-shift:pixels=12");
        app.filter_input = "channel-shift:pixels=-8".to_owned();
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.filters[0], FilterSpec::ChannelShift { pixels: -8 });

        app.handle_key(KeyEvent::from(KeyCode::Right));
        app.handle_key(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(app.filters, [FilterSpec::ChannelShift { pixels: -8 }]);
        assert_eq!(app.selected_filter, 0);
    }

    #[test]
    fn selected_edit_stays_visible_and_retains_only_explicit_defaults() {
        let (_directory, mut app) = fixture();
        app.filters = vec![FilterSpec::ChannelShift { pixels: 4 }; 7];
        app.selected_filter = 6;

        app.handle_key(KeyEvent::from(KeyCode::Char('e')));
        assert_eq!(app.filter_input, "channel-shift");
        let backend = TestBackend::new(40, 7);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| app.draw_pipeline(frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("> 7. channel-shift"));
        assert!(!rendered.contains("+ channel-shift"));
        assert!(buffer
            .content
            .iter()
            .any(|cell| cell.modifier.contains(Modifier::REVERSED)));

        app.handle_key(KeyEvent::from(KeyCode::Esc));
        app.filters.clear();
        app.filter_specifications.clear();
        app.selected_filter = 0;
        app.handle_key(KeyEvent::from(KeyCode::Char('p')));
        for character in "channel-shift:pixels=4".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(character)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        app.handle_key(KeyEvent::from(KeyCode::Char('e')));
        assert_eq!(app.filter_input, "channel-shift:pixels=4");
    }

    #[test]
    fn wide_high_contrast_snapshot_uses_two_panes_and_bold_yellow_accents() {
        let (_directory, mut app) = fixture();
        app.theme = TuiTheme::HighContrast;
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("Inputs (2)"));
        assert!(rendered.contains("Size   2x3"));
        assert!(rendered.contains("Preview"));
        assert!(rendered.contains("Queue"));
        assert!(buffer
            .content
            .iter()
            .any(|cell| { cell.fg == Color::Yellow && cell.modifier.contains(Modifier::BOLD) }));
        assert!(buffer.content.iter().any(|cell| {
            cell.fg == Color::Rgb(10, 20, 30)
                && cell.bg == Color::Rgb(10, 20, 30)
                && cell
                    .symbol()
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
        }));
    }

    #[test]
    fn black_preview_pixels_fill_cells_with_true_black() {
        let (_directory, mut app) = fixture();
        app.theme = TuiTheme::Standard;
        app.preview = MediaPreview::Pixels {
            source_width: 1,
            source_height: 1,
            rows: vec![vec![preview::PreviewPixel {
                character: '0',
                rgb: [0, 0, 0],
            }]],
        };
        let backend = TestBackend::new(6, 3);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| app.draw_preview(frame, frame.area()))
            .unwrap();
        let cell = terminal.backend().buffer().cell((1, 1)).unwrap();

        assert_eq!(cell.symbol(), "0");
        assert_eq!(cell.fg, Color::Rgb(0, 0, 0));
        assert_eq!(cell.bg, Color::Rgb(0, 0, 0));
    }

    #[test]
    fn queue_start_key_transforms_inputs_with_per_file_progress() {
        let (directory, mut app) = fixture();
        app.filters.push(FilterSpec::Invert);
        app.output_directory = directory.path().join("out").display().to_string();

        app.handle_key(KeyEvent::from(KeyCode::Char('s')));
        let snapshot = app.queue.wait_for_terminal(Duration::from_secs(2));

        assert_eq!(snapshot.state, QueueState::Finished);
        assert_eq!(snapshot.completed, 2);
        assert_eq!(snapshot.failed, 0);
        assert!(directory.path().join("out/a.png").exists());
        assert!(directory.path().join("out/b.png").exists());
        assert!(snapshot.logs.iter().any(|line| line.contains("finished")));
    }

    #[test]
    fn tui_preflight_matches_shared_cli_command_plan() {
        let (directory, _) = fixture();
        let filters = vec![FilterSpec::Invert, FilterSpec::ChannelShift { pixels: 2 }];
        let app = TuiApp::load_with_pipeline(
            &[directory.path().join("a.png")],
            directory.path().join("out"),
            TuiTheme::Standard,
            filters.clone(),
            42,
        )
        .unwrap();
        let result = ApplicationService
            .execute(
                crate::ApplicationCommand::Plan {
                    format: crate::MediaFormat::Png,
                    filters,
                    seed: 42,
                },
                crate::CancellationToken::default(),
                |_| {},
            )
            .unwrap();

        let crate::ApplicationResult::Plan(plan) = result else {
            panic!("expected plan result");
        };
        assert!(app.pipeline_error.is_none());
        assert_eq!(plan.seed, app.seed);
        assert_eq!(plan.stages[0].filters, app.filters);
    }

    #[test]
    fn standard_theme_snapshot_renders_queue_failure_state() {
        let (_directory, mut app) = fixture();
        app.theme = TuiTheme::Standard;
        app.queue_error = Some("output collision".to_owned());
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("output collision"));
        assert!(buffer.content.iter().any(|cell| cell.fg == Color::Cyan));
    }
}
