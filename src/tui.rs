use crate::man_db::ManDb;
use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Span, Spans},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

const PAGE_SIZE: usize = 30;
const LIST_SIZE: usize = 50;
const DEBOUNCE_DELAY_MS: u64 = 150;

/// Tracks command list state
struct CommandListState {
    input: String,
    filtered_commands: Arc<Vec<String>>,
    selected_idx: usize,
    list_scroll: usize,
    visible_range: (usize, usize),
}

/// Tracks man page state
struct ManPageState {
    content: Arc<Vec<String>>,
    scroll: usize,
}

/// Tracks search state
struct SearchState {
    query: String,
    matches: Arc<Vec<usize>>,
    current_match: usize,
}

/// Application state container
pub struct AppState {
    command_list: CommandListState,
    man_page: ManPageState,
    search: SearchState,
    focus: Focus,
    man_db: Arc<ManDb>,
    loading: bool,
    last_input_time: Instant,
    pending_man_load: bool,
    page_source: PageSource,
}

/// UI focus areas
enum Focus {
    CommandList,
    ManPage,
    Search,
}

/// Content source options
enum PageSource {
    Man,
    Tldr,
}

fn scroll_to_top(app: &mut AppState) {
    app.man_page.scroll = 0;
}

fn scroll_to_bottom(app: &mut AppState) {
    app.man_page.scroll = app.man_page.content.len().saturating_sub(PAGE_SIZE);
}

/// Runs the TUI application
pub async fn run_tui(man_db: ManDb) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let man_db = Arc::new(man_db);
    let commands = man_db.get_commands();
    let filtered_commands = Arc::new(commands.clone());

    let mut app = AppState {
        command_list: CommandListState {
            input: String::new(),
            filtered_commands,
            selected_idx: 0,
            list_scroll: 0,
            visible_range: (0, 0),
        },
        man_page: ManPageState {
            content: Arc::new(Vec::new()),
            scroll: 0,
        },
        search: SearchState {
            query: String::new(),
            matches: Arc::new(Vec::new()),
            current_match: 0,
        },
        focus: Focus::CommandList,
        man_db: man_db.clone(),
        loading: false,
        last_input_time: Instant::now(),
        pending_man_load: true,
        page_source: PageSource::Man,
    };

    loop {
        let now = Instant::now();

        // Handle delayed man page loading
        if app.pending_man_load
            && app.last_input_time.elapsed() > Duration::from_millis(DEBOUNCE_DELAY_MS)
        {
            load_current_page(&mut app).await;
            app.pending_man_load = false;
        }

        terminal.draw(|f| render_ui(f, &mut app))?;

        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                // Handle Ctrl combinations first
                if let KeyEvent {
                    code: KeyCode::Char('c'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                } = key
                {
                    break;
                }

                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Tab => toggle_focus(&mut app),
                    KeyCode::Esc => app.focus = Focus::CommandList,
                    KeyCode::Char('/') if matches!(app.focus, Focus::ManPage) => {
                        app.focus = Focus::Search;
                        app.search.query.clear();
                    }
                    KeyCode::Char('t') if matches!(app.focus, Focus::ManPage) => {
                        toggle_page_source(&mut app);
                        app.pending_man_load = true;
                        app.last_input_time = Instant::now();
                    }
                    _ => handle_key(&mut app, key).await,
                }

                match key {
                    KeyEvent {
                        code: KeyCode::Home,
                        modifiers: KeyModifiers::CONTROL,
                        ..
                    } => scroll_to_top(&mut app),
                    KeyEvent {
                        code: KeyCode::End,
                        modifiers: KeyModifiers::CONTROL,
                        ..
                    } => scroll_to_bottom(&mut app),
                    _ => {}
                }
            }
        }

        // Throttle CPU usage
        let elapsed = now.elapsed();
        if elapsed < Duration::from_millis(16) {
            tokio::time::sleep(Duration::from_millis(16) - elapsed).await;
        }
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn toggle_page_source(app: &mut AppState) {
    app.page_source = match app.page_source {
        PageSource::Man => PageSource::Tldr,
        PageSource::Tldr => PageSource::Man,
    };
}

fn toggle_focus(app: &mut AppState) {
    app.focus = match app.focus {
        Focus::CommandList => Focus::ManPage,
        Focus::ManPage => Focus::CommandList,
        Focus::Search => Focus::ManPage,
    };
}

async fn handle_key(app: &mut AppState, key: KeyEvent) {
    match app.focus {
        Focus::CommandList => handle_command_list_keys(app, key).await,
        Focus::ManPage => handle_man_page_keys(app, key),
        Focus::Search => handle_search_keys(app, key),
    }
}

async fn handle_command_list_keys(app: &mut AppState, key: KeyEvent) {
    let commands_len = app.command_list.filtered_commands.len();

    match key.code {
        KeyCode::Char(c) => {
            app.command_list.input.push(c);
            filter_commands(app);
            app.pending_man_load = true;
            app.last_input_time = Instant::now();
        }
        KeyCode::Backspace => {
            app.command_list.input.pop();
            filter_commands(app);
            app.pending_man_load = true;
            app.last_input_time = Instant::now();
        }
        KeyCode::Up if commands_len > 0 => {
            if app.command_list.selected_idx > 0 {
                app.command_list.selected_idx -= 1;
                update_list_scroll(app);
                app.pending_man_load = true;
                app.last_input_time = Instant::now();
            }
        }
        KeyCode::Down if commands_len > 0 => {
            if app.command_list.selected_idx < commands_len - 1 {
                app.command_list.selected_idx += 1;
                update_list_scroll(app);
                app.pending_man_load = true;
                app.last_input_time = Instant::now();
            }
        }
        KeyCode::Home if commands_len > 0 => {
            app.command_list.selected_idx = 0;
            update_list_scroll(app);
            app.pending_man_load = true;
            app.last_input_time = Instant::now();
        }
        KeyCode::End if commands_len > 0 => {
            app.command_list.selected_idx = commands_len - 1;
            update_list_scroll(app);
            app.pending_man_load = true;
            app.last_input_time = Instant::now();
        }
        KeyCode::PageUp if commands_len > 0 => {
            app.command_list.selected_idx = app
                .command_list
                .selected_idx
                .saturating_sub(LIST_SIZE)
                .max(0);
            update_list_scroll(app);
            app.pending_man_load = true;
            app.last_input_time = Instant::now();
        }
        KeyCode::PageDown if commands_len > 0 => {
            app.command_list.selected_idx =
                (app.command_list.selected_idx + LIST_SIZE).min(commands_len - 1);
            update_list_scroll(app);
            app.pending_man_load = true;
            app.last_input_time = Instant::now();
        }
        KeyCode::Enter if commands_len > 0 => {
            app.pending_man_load = true;
            load_current_page(app).await;
            app.pending_man_load = false;
        }
        _ => {}
    }
}

fn update_list_scroll(app: &mut AppState) {
    let visible_height = app.command_list.visible_range.1 - app.command_list.visible_range.0;
    let selected_idx = app.command_list.selected_idx;

    if selected_idx == 0 {
        app.command_list.list_scroll = 0;
    } else if selected_idx == app.command_list.filtered_commands.len() - 1 {
        app.command_list.list_scroll = selected_idx.saturating_sub(visible_height - 1);
    } else if selected_idx < app.command_list.list_scroll {
        app.command_list.list_scroll = selected_idx;
    } else if selected_idx >= app.command_list.list_scroll + visible_height {
        app.command_list.list_scroll = selected_idx - visible_height + 1;
    }
}

fn filter_commands(app: &mut AppState) {
    let commands = app.man_db.get_commands();

    app.command_list.filtered_commands = if app.command_list.input.is_empty() {
        Arc::new(commands.clone())
    } else {
        let filtered: Vec<String> = commands
            .iter()
            .filter(|cmd| {
                cmd.to_lowercase()
                    .contains(&app.command_list.input.to_lowercase())
            })
            .cloned()
            .collect();
        Arc::new(filtered)
    };

    app.command_list.selected_idx = 0;
    app.command_list.list_scroll = 0;
}

async fn load_current_page(app: &mut AppState) {
    if app.command_list.filtered_commands.is_empty() {
        app.man_page.content = Arc::new(vec!["No commands found".to_string()]);
        return;
    }

    let cmd = app.command_list.filtered_commands[app.command_list.selected_idx].clone();
    app.loading = true;

    let content = match app.page_source {
        PageSource::Man => app.man_db.get_man_page(&cmd).await,
        PageSource::Tldr => app.man_db.get_tldr_page(&cmd).await,
    };

    app.man_page.content = content;
    app.loading = false;
    app.man_page.scroll = 0;
    update_search_matches(app);
}

fn handle_man_page_keys(app: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Char('f') => {
            app.focus = Focus::Search;
            app.search.query.clear();
        }
        KeyCode::Up => app.man_page.scroll = app.man_page.scroll.saturating_sub(1),
        KeyCode::Down => app.man_page.scroll = app.man_page.scroll.saturating_add(1),
        KeyCode::Home => app.man_page.scroll = 0,
        KeyCode::End => app.man_page.scroll = app.man_page.content.len().saturating_sub(PAGE_SIZE),
        KeyCode::PageUp => app.man_page.scroll = app.man_page.scroll.saturating_sub(PAGE_SIZE),
        KeyCode::PageDown => {
            app.man_page.scroll = (app.man_page.scroll + PAGE_SIZE)
                .min(app.man_page.content.len().saturating_sub(PAGE_SIZE))
        }
        KeyCode::Char('n') => next_search_match(app),
        KeyCode::Char('N') => prev_search_match(app),
        _ => {}
    }
}

fn handle_search_keys(app: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            update_search_matches(app);
            app.focus = Focus::ManPage;
        }
        KeyCode::Char(c) => {
            app.search.query.push(c);
            update_search_matches(app);
        }
        KeyCode::Backspace => {
            app.search.query.pop();
            update_search_matches(app);
        }
        KeyCode::Esc => {
            app.search.query.clear();
            app.search.matches = Arc::new(Vec::new());
            app.focus = Focus::ManPage;
        }
        _ => {}
    }
}

fn update_search_matches(app: &mut AppState) {
    let mut matches = Vec::new();

    if !app.search.query.is_empty() {
        for (i, line) in app.man_page.content.iter().enumerate() {
            if line
                .to_lowercase()
                .contains(&app.search.query.to_lowercase())
            {
                matches.push(i);
            }
        }
    }

    app.search.matches = Arc::new(matches);
    app.search.current_match = 0;

    if !app.search.matches.is_empty() {
        app.man_page.scroll = app.search.matches[0].saturating_sub(PAGE_SIZE / 2);
    }
}

fn next_search_match(app: &mut AppState) {
    if app.search.matches.is_empty() {
        return;
    }

    app.search.current_match = (app.search.current_match + 1) % app.search.matches.len();
    let target_line = app.search.matches[app.search.current_match];
    app.man_page.scroll = target_line.saturating_sub(PAGE_SIZE / 2);
}

fn prev_search_match(app: &mut AppState) {
    if app.search.matches.is_empty() {
        return;
    }

    app.search.current_match = app
        .search
        .current_match
        .checked_sub(1)
        .unwrap_or(app.search.matches.len() - 1);

    let target_line = app.search.matches[app.search.current_match];
    app.man_page.scroll = target_line.saturating_sub(PAGE_SIZE / 2);
}

fn render_ui<B: tui::backend::Backend>(f: &mut tui::Frame<B>, app: &mut AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Min(10),
            ]
            .as_ref(),
        )
        .split(f.size());

    render_status_bar(f, app, chunks[0]);
    render_input(f, app, chunks[1]);
    render_main_content(f, app, chunks[2]);
}

fn render_status_bar<B: tui::backend::Backend>(f: &mut tui::Frame<B>, app: &AppState, area: Rect) {
    let source_label = match app.page_source {
        PageSource::Man => "MAN",
        PageSource::Tldr => "TLDR",
    };

    let status = if app.loading {
        format!("Loading {source_label}...")
    } else {
        let x = &*format!("RTFM // {source_label} PAGE [Tab:Switch /:Search t:Toggle Home/End]");
        match app.focus {
            Focus::CommandList => "RTFM // COMMAND LIST [Tab:Switch Home/End]",
            Focus::ManPage => x,
            Focus::Search => "RTFM // SEARCH MODE [Enter:Apply Esc:Cancel]",
        }
        .parse()
        .unwrap()
    };

    let status_bar = Paragraph::new(status)
        .block(Block::default())
        .style(Style::default().bg(Color::DarkGray));

    f.render_widget(status_bar, area);
}

fn render_input<B: tui::backend::Backend>(f: &mut tui::Frame<B>, app: &AppState, area: Rect) {
    let input_text = match app.focus {
        Focus::CommandList | Focus::ManPage => format!("> {}", app.command_list.input),
        Focus::Search => format!("/{}", app.search.query),
    };

    let input = Paragraph::new(input_text.as_str())
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(Color::Yellow));

    f.render_widget(input, area);
}

fn render_main_content<B: tui::backend::Backend>(
    f: &mut tui::Frame<B>,
    app: &mut AppState,
    area: Rect,
) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)].as_ref())
        .split(area);

    render_command_list(f, app, chunks[0]);
    render_man_page(f, app, chunks[1]);
}

fn render_command_list<B: tui::backend::Backend>(
    f: &mut tui::Frame<B>,
    app: &mut AppState,
    area: Rect,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(3)].as_ref())
        .split(area);

    render_command_list_items(f, app, chunks[0]);
    render_command_description(f, app, chunks[1]);
}

fn render_command_list_items<B: tui::backend::Backend>(
    f: &mut tui::Frame<B>,
    app: &mut AppState,
    area: Rect,
) {
    let height = area.height as usize;
    app.command_list.visible_range = (
        app.command_list.list_scroll,
        app.command_list.list_scroll + height,
    );

    if app.command_list.filtered_commands.is_empty() {
        let empty_msg = ListItem::new("No commands found".to_string());
        let list = List::new(vec![empty_msg])
            .block(Block::default().borders(Borders::ALL).title("Commands"));
        f.render_widget(list, area);
        return;
    }

    let end = std::cmp::min(
        app.command_list.list_scroll + height,
        app.command_list.filtered_commands.len(),
    );
    let visible_commands = &app.command_list.filtered_commands[app.command_list.list_scroll..end];

    let items: Vec<ListItem> = visible_commands
        .iter()
        .map(|cmd| {
            let prefix = { "  " };
            ListItem::new(format!("{prefix}{cmd}"))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Commands"))
        .highlight_style(Style::default().bg(Color::DarkGray));

    let mut state = ListState::default();
    state.select(Some(
        app.command_list.selected_idx - app.command_list.list_scroll,
    ));
    f.render_stateful_widget(list, area, &mut state);
}

fn render_command_description<B: tui::backend::Backend>(
    f: &mut tui::Frame<B>,
    app: &AppState,
    area: Rect,
) {
    let description = if !app.command_list.filtered_commands.is_empty() {
        if let Some(cmd) = app
            .command_list
            .filtered_commands
            .get(app.command_list.selected_idx)
        {
            app.man_db.get_description(cmd).unwrap_or_default()
        } else {
            String::new()
        }
    } else {
        "No commands to show".to_string()
    };

    let desc_block = Paragraph::new(description)
        .block(Block::default().borders(Borders::ALL).title("Description"))
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(Color::Cyan));

    f.render_widget(desc_block, area);
}

fn render_man_page<B: tui::backend::Backend>(f: &mut tui::Frame<B>, app: &AppState, area: Rect) {
    let height = area.height as usize;
    let start_line = app.man_page.scroll;
    let end_line = std::cmp::min(start_line + height, app.man_page.content.len());

    let visible_content: Vec<Spans> = app
        .man_page
        .content
        .iter()
        .enumerate()
        .skip(start_line)
        .take(end_line - start_line)
        .map(|(idx, line)| {
            let global_idx = idx + start_line;
            if app.search.matches.contains(&global_idx) {
                let search_index = app
                    .search
                    .matches
                    .iter()
                    .position(|&i| i == global_idx)
                    .unwrap();
                let highlight = search_index == app.search.current_match;

                let mut spans = Vec::new();
                let mut remaining = line.as_str();

                while let Some(pos) = remaining.find(&app.search.query) {
                    let (before, after) = remaining.split_at(pos);
                    let (match_text, rest) = after.split_at(app.search.query.len());

                    spans.push(Span::raw(before));
                    spans.push(Span::styled(
                        match_text,
                        Style::default()
                            .bg(if highlight {
                                Color::Red
                            } else {
                                Color::DarkGray
                            })
                            .fg(if highlight {
                                Color::White
                            } else {
                                Color::Black
                            }),
                    ));

                    remaining = rest;
                }
                spans.push(Span::raw(remaining));

                Spans::from(spans)
            } else {
                // Apply syntax highlighting
                let highlighted = syntax_highlight(line);
                Spans::from(highlighted)
            }
        })
        .collect();

    let paragraph = Paragraph::new(visible_content)
        .block(Block::default().borders(Borders::ALL).title("Content"))
        .wrap(Wrap { trim: true });

    f.render_widget(paragraph, area);
}

/// Basic syntax highlighting for man pages
fn syntax_highlight(line: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut words = line.split_whitespace();

    if let Some(first) = words.next() {
        // Highlight headings
        if first.ends_with(':') {
            spans.push(Span::styled(
                first,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        // Highlight options
        else if first.starts_with('-') {
            spans.push(Span::styled(
                first,
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::raw(first));
        }

        for word in words {
            spans.push(Span::raw(" "));

            if word.starts_with('-') {
                spans.push(Span::styled(word, Style::default().fg(Color::Green)));
            } else if word.starts_with('[') && word.ends_with(']') {
                spans.push(Span::styled(word, Style::default().fg(Color::Magenta)));
            } else if word.starts_with('<') && word.ends_with('>') {
                spans.push(Span::styled(word, Style::default().fg(Color::Blue)));
            } else {
                spans.push(Span::raw(word));
            }
        }
    } else {
        spans.push(Span::raw(line));
    }

    spans
}
