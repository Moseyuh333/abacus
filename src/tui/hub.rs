//! The `/profile` hub: the profiles, and every provider's searchable model
//! list, in one surface.

use super::*;

/// The `/profile` hub: a sidebar of scopes beside a list.
///
/// Both columns live inside one bordered surface, joined by `┬`/`┴` junctions
/// punched into the border after the block is drawn — ratatui has no notion of
/// an interior rule, and drawing two blocks side by side would fence the
/// columns apart instead of tying them together.
pub(super) fn draw_model_hub(frame: &mut Frame<'_>, area: Rect, app: &App) {
    use crate::model_hub::{Pane, Pick, Scope};
    let Some(hub) = &app.model_hub else {
        return;
    };
    let assigning = match &hub.pick {
        Pick::Assign(role) => Some(crate::roles::role(role).map_or(role.as_str(), |role| role.label)),
        Pick::Use => None,
    };
    let hints: &[(&str, &str)] = match (hub.pane, hub.current_scope(), assigning, hub.prompt.is_some()) {
        (_, _, _, true) => &[("enter", "confirm"), ("esc", "cancel")],
        (Pane::Sidebar, _, _, _) => &[("↑↓", "scope"), ("→ enter", "open"), ("esc", "close")],
        (Pane::Body, Scope::Profiles, _, _) => &[
            ("↑↓", "select"),
            ("enter", "switch"),
            ("e", "edit"),
            ("r", "rename"),
            ("d", "delete"),
            ("n", "new"),
            ("esc", "close"),
        ],
        (Pane::Body, _, Some(_), _) => &[("↑↓", "select"), ("enter", "assign"), ("type", "filter"), ("esc", "back")],
        (Pane::Body, _, None, _) => &[
            ("↑↓", "select"),
            ("enter", "use"),
            ("type", "filter"),
            ("^R", "refresh"),
            ("←", "scopes"),
            ("esc", "close"),
        ],
    };
    // Full-bleed rather than a centred card: this is a surface you go *to*,
    // and a floating panel with the composer showing around its edges reads as
    // a dialog you are meant to dismiss.
    let popup = area;
    let title = match assigning {
        Some(role) => format!("PROFILE · choose the {}", role.to_ascii_lowercase()),
        None => "PROFILES".to_owned(),
    };
    // The hint strip is a row of the body, not a caption on the bottom border:
    // the column rule has to land somewhere, and it lands on the rule above
    // the hints rather than in the middle of their text.
    let inner = open_overlay(frame, popup, &title, primary(), &[]);
    let columns = Rect { height: inner.height.saturating_sub(2), ..inner };
    let footer = Rect { y: inner.bottom().saturating_sub(2), height: 2, ..inner };

    // Sidebar width is content-driven within bounds: wide enough for the
    // longest provider name and its count, never wide enough to starve the
    // list of the columns that carry the metadata.
    let widest = hub
        .scopes
        .iter()
        .map(|scope| match scope {
            Scope::Profiles => "profiles".len() + 4,
            Scope::Separator => 0,
            Scope::Provider { name, .. } => name.chars().count() + 4,
            Scope::AddProvider => "+ add provider".len(),
        })
        .max()
        .unwrap_or(0);
    let sidebar_width = (widest as u16 + 6).clamp(18, 30);
    let split = ui::split(columns, sidebar_width);
    let narrow = split.body.width < ui::MIN_SPLIT_BODY;

    frame.render_widget(Paragraph::new(Text::from(vec![ui::rule(inner.width), Line::from(ui::hints(hints))])), footer);
    if !narrow {
        draw_hub_divider(frame, popup, footer.y, split.divider_x);
        draw_hub_sidebar(frame, split.sidebar, app, hub);
    }
    let body = if narrow { columns } else { split.body };
    match hub.current_scope() {
        Scope::Profiles => draw_hub_profiles(frame, body, app, hub),
        Scope::Separator => {}
        Scope::Provider { id, .. } => {
            let catalog = app.catalogs.get(id).unwrap_or(&crate::model_hub::Catalog::Idle);
            draw_hub_models(frame, body, app, hub, id, catalog);
        }
        Scope::AddProvider => {
            let lines = vec![
                Line::from(fg("  Connect another provider: a preset, or any endpoint by URL.", text())),
                Line::default(),
                Line::from(fg("  Enter opens the list.", muted())),
            ];
            frame.render_widget(Paragraph::new(Text::from(lines)), body);
        }
    }
}

/// The interior column rule, plus the junctions where it meets the border.
pub(super) fn draw_hub_divider(frame: &mut Frame<'_>, popup: Rect, footer_y: u16, column: u16) {
    let palette = crate::theme::active();
    let set = ui::glyphs();
    let buffer = frame.buffer_mut();
    for row in popup.y..=footer_y {
        let glyph = if row == popup.y {
            set.tee_down
        } else if row == footer_y {
            set.tee_up
        } else {
            set.track
        };
        if let Some(cell) = buffer.cell_mut((column, row)) {
            cell.set_symbol(glyph).set_style(Style::default().fg(palette.border).bg(palette.overlay));
        }
    }
}

pub(super) fn draw_hub_sidebar(frame: &mut Frame<'_>, area: Rect, app: &App, hub: &crate::model_hub::ModelHub) {
    use crate::model_hub::{Catalog, Pane, Scope};
    let width = area.width as usize;
    let focused = hub.pane == Pane::Sidebar;
    let active_provider =
        app.settings.active_profile().map(|profile| profile.provider().to_owned()).unwrap_or_default();
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (index, scope) in hub.scopes.iter().enumerate() {
        if lines.len() >= area.height as usize {
            break;
        }
        let rect = Rect { y: area.y + lines.len() as u16, height: 1, ..area };
        if matches!(scope, Scope::Separator) {
            lines.push(ui::rule(area.width));
            continue;
        }
        let selected = index == hub.scope;
        app.hits.borrow_mut().hub_scope.push((rect, index));
        // The cursor only shows in the column that has the keyboard, so it is
        // always unambiguous which list the arrow keys are driving.
        let state = app.row_state(rect, selected && focused);
        let (label, annotation, mark) = match scope {
            Scope::Profiles => ("profiles".to_owned(), app.settings.profiles.len().to_string(), None),
            Scope::Provider { id, name } => {
                let catalog = app.catalogs.get(id).unwrap_or(&Catalog::Idle);
                (name.clone(), catalog.annotation(), Some(*id == active_provider))
            }
            Scope::AddProvider => ("+ add provider".to_owned(), String::new(), None),
            Scope::Separator => unreachable!("handled above"),
        };
        let mut content: Vec<Span<'static>> = Vec::new();
        if let Some(active) = mark {
            content.push(ui::state_mark(active));
            content.push(Span::raw(" "));
        }
        let label_width = width.saturating_sub(2 + ui::spans_width(&content) + annotation.chars().count() + 1);
        content.push(Span::styled(ui::truncate(&label, label_width), emphasis(selected)));
        let used = ui::spans_width(&content) + 2;
        let gap = width.saturating_sub(used + annotation.chars().count()).max(1);
        content.push(Span::raw(" ".repeat(gap)));
        content.push(fg(annotation, rail()));
        lines.push(ui::list_row(state, width, content));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

/// Rows reserved below a hub list for the rule and the detail block.
pub(super) const HUB_DETAIL_ROWS: u16 = 3;

pub(super) fn draw_hub_profiles(frame: &mut Frame<'_>, area: Rect, app: &App, hub: &crate::model_hub::ModelHub) {
    use crate::model_hub::{HubPrompt, Pane};
    let rows = crate::model_hub::profile_rows(&app.settings);
    let width = area.width as usize;
    let focused = hub.pane == Pane::Body;
    let mut lines: Vec<Line<'static>> = Vec::new();
    if rows.is_empty() {
        lines.push(Line::from(fg("  No profiles yet — pick a model in a provider's list to make one.", muted())));
    }
    let name_width = rows.iter().map(|row| row.name.chars().count()).max().unwrap_or(0).min(width / 2);
    let visible = area.height.saturating_sub(HUB_DETAIL_ROWS) as usize;
    app.hub_rows.set(visible.max(1));
    for (index, row) in rows.iter().enumerate().skip(hub.window).take(visible) {
        let rect = Rect { y: area.y + lines.len() as u16, height: 1, ..area };
        app.hits.borrow_mut().hub_body.push((rect, index));
        let selected = index == hub.selected;
        let state = app.row_state(rect, selected && focused);
        let mut content = vec![ui::state_mark(row.active), Span::raw(" ")];
        content.push(Span::styled(format!("{:<name_width$}", ui::truncate(&row.name, name_width)), emphasis(selected)));
        // A profile named after its model says it once.
        if row.model != row.name {
            content.push(fg(format!("  {}", row.model), rail()));
        }
        if !row.settings.is_empty() {
            content.push(fg(format!("  {}", row.settings), muted()));
        }
        lines.push(ui::list_row(state, width, ui::fit(content, width.saturating_sub(3))));
    }
    while lines.len() + HUB_DETAIL_ROWS as usize <= area.height as usize {
        lines.push(Line::default());
    }
    lines.truncate(area.height.saturating_sub(2) as usize);
    lines.push(ui::rule(area.width));
    lines.push(match &hub.prompt {
        Some(HubPrompt::Rename { name, input }) => {
            let field = Line::from(vec![fg(format!("  rename {name} → "), muted()), fg(input.text(), text())]);
            let (_, column) = input.cursor_position();
            let x = area.x + 2 + format!("rename {name} → ").chars().count() as u16 + column as u16;
            frame.set_cursor_position((x.min(area.right().saturating_sub(1)), area.bottom().saturating_sub(1)));
            field
        }
        Some(HubPrompt::ConfirmDelete { name }) => {
            Line::from(vec![bold(format!("  delete {name}?"), danger()), fg("  enter confirms · esc cancels", muted())])
        }
        None => {
            let model = rows.get(hub.selected).map(|row| row.model.as_str()).unwrap_or_default();
            Line::from(fg(format!("  {}", ui::truncate(model, width.saturating_sub(2))), muted()))
        }
    });
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

pub(super) fn draw_hub_models(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    hub: &crate::model_hub::ModelHub,
    provider: &str,
    catalog: &crate::model_hub::Catalog,
) {
    use crate::model_hub::{Catalog, Pane, Pick};
    let width = area.width as usize;
    let focused = hub.pane == Pane::Body;
    let mut lines: Vec<Line<'static>> = Vec::new();

    // The search row is always live — there is no mode to enter, so there is
    // none to forget to leave.
    let query = if hub.search.is_empty() { fg("type to filter", rail()) } else { fg(hub.search.clone(), text()) };
    lines.push(Line::from(vec![bold(format!("{} ", ui::glyphs().prompt), primary()), query]));
    lines.push(Line::default());

    let list_height = area.height.saturating_sub(lines.len() as u16 + HUB_DETAIL_ROWS) as usize;
    app.hub_rows.set(list_height.max(1));
    let matched = crate::model_hub::filter(catalog.cards(), &hub.search);
    // The model marked is the one Enter would replace: the role's when
    // assigning, the session's otherwise.
    let current = match &hub.pick {
        Pick::Assign(role) => {
            app.settings.active_profile().and_then(|profile| profile.resolve_role(role)).map(|r| r.model)
        }
        Pick::Use => Some(app.config.model_ref()),
    }
    .unwrap_or_default();
    let has_profile = |model: &str| app.settings.profile_for(&crate::providers::join_ref(provider, model)).is_some();

    // Column widths are measured over the visible window alone, so scrolling
    // through a stretch of unpriced models closes the price column instead of
    // holding a gutter open for rows that are not on screen.
    let visible: Vec<&crate::model_info::ModelCard> =
        matched.iter().skip(hub.window).take(list_height).copied().collect();
    let cells: Vec<Vec<String>> = visible.iter().map(|card| vec![card.context_label(), card.cost_label()]).collect();
    let widths = ui::metric_widths(cells.iter().map(Vec::as_slice), 2);
    let metrics = ui::metrics_width(&widths);

    if visible.is_empty() {
        lines.push(Line::from(fg(
            match catalog {
                Catalog::Loading => "  Asking the provider what it serves…".to_owned(),
                Catalog::Failed(error) => format!("  {error}"),
                Catalog::Idle => "  Nothing listed yet.".to_owned(),
                Catalog::Ready(_) if !hub.search.is_empty() => "  Nothing matches that filter.".to_owned(),
                Catalog::Ready(_) => {
                    "  The provider lists no models. Add some under /config → Providers → Extra models.".to_owned()
                }
            },
            if matches!(catalog, Catalog::Failed(_)) { danger() } else { muted() },
        )));
    }
    for (offset, card) in visible.iter().enumerate() {
        let index = hub.window + offset;
        let rect = Rect { y: area.y + lines.len() as u16, height: 1, ..area };
        app.hits.borrow_mut().hub_body.push((rect, index));
        let selected = index == hub.selected;
        let state = app.row_state(rect, selected && focused);
        let mut content: Vec<Span<'static>> = Vec::new();
        // The supplier is a dim prefix rather than an indent level: it groups
        // a long list visually while every id still starts at the same column.
        if let Some(owner) = &card.provider {
            content.push(fg(format!("{owner}/"), rail()));
        }
        content.push(Span::styled(
            card.short_id().to_owned(),
            Style::default().fg(if selected { primary() } else { text() }).add_modifier(if selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        ));
        if crate::providers::join_ref(provider, &card.id) == current {
            content.push(fg(format!(" {}", ui::glyphs().ok), success()));
        } else if hub.pick == Pick::Use && has_profile(&card.id) {
            content.push(fg(" ·profile", rail()));
        }
        let label_width = width.saturating_sub(metrics + 4);
        let mut row = ui::fit(content, label_width);
        row.push(Span::raw("  "));
        row.extend(ui::metric_cells(&cells[offset], &widths));
        lines.push(ui::list_row(state, width, row));
    }

    while lines.len() + HUB_DETAIL_ROWS as usize <= area.height as usize {
        lines.push(Line::default());
    }
    lines.truncate(area.height.saturating_sub(2) as usize);
    lines.push(ui::rule(area.width));
    lines.push(hub_detail(matched.get(hub.selected).copied(), width.saturating_sub(2)));
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

/// The one-line fact block under the model list.
///
/// A row of comparable, `·`-joined facts about the selected model — the shape
/// of it, then the prose. Truncation eats the description first, which is the
/// part you can afford to lose.
pub(super) fn hub_detail(card: Option<&crate::model_info::ModelCard>, width: usize) -> Line<'static> {
    let Some(card) = card else {
        return Line::default();
    };
    let mut facts = vec![card.name.clone().unwrap_or_else(|| card.id.clone())];
    let context = card.context_label();
    if !context.is_empty() {
        facts.push(context);
    }
    if let Some(output) = card.max_output_tokens {
        facts.push(format!("{} out", ui::format_count(output as u64)));
    }
    let cost = card.cost_label();
    if !cost.is_empty() {
        facts.push(if cost == "free" { cost } else { format!("{cost} per M") });
    }
    if card.reasoning {
        facts.push("reasoning".to_owned());
    }
    if card.vision {
        facts.push("vision".to_owned());
    }
    if let Some(description) = &card.description {
        facts.push(description.clone());
    }
    // Indented to the content column, so the facts sit under the model names
    // rather than under the cursor gutter.
    let mut spans = vec![Span::raw("  ")];
    spans.extend(ui::fit(ui::facts(&facts).spans, width));
    Line::from(spans)
}

/// Keys for the `/profile` hub.
///
/// A model list is searchable, so plain letters type rather than navigate —
/// there is no vim-style `j`/`k` there, because a model list is something you
/// filter your way through, not something you walk. The sidebar and the
/// profiles list have no search and keep them; the profiles list also takes
/// single-letter actions.
pub(super) fn handle_model_hub_key(app: &mut App, key: KeyEvent) {
    use crate::model_hub::{HubPrompt, Pane, Pick, Scope};
    let Some(hub) = &app.model_hub else {
        return;
    };
    if hub.prompt.is_some() {
        return hub_prompt_key(app, key);
    }
    let visible = app.hub_rows.get().max(1);
    let scope = hub.current_scope().clone();
    let in_body = hub.pane == Pane::Body;
    let in_profiles = matches!(scope, Scope::Profiles);
    let in_models = matches!(scope, Scope::Provider { .. });
    let len = match &scope {
        Scope::Profiles => app.settings.profiles.len(),
        Scope::Provider { .. } => crate::model_hub::filter(app.hub_catalog().cards(), &hub.search).len(),
        _ => 0,
    };
    let selected_profile =
        || crate::model_hub::profile_rows(&app.settings).get(hub.selected).map(|row| row.name.clone());

    match key.code {
        KeyCode::Esc => {
            let hub = app.model_hub.as_mut().expect("checked above");
            if in_body && !hub.search.is_empty() {
                hub.search.clear();
                hub.reset_body();
            } else if hub.pane == Pane::Body && hub.pick == Pick::Use && !in_profiles {
                hub.pane = Pane::Sidebar;
            } else {
                app.model_hub = None;
            }
        }
        KeyCode::Tab | KeyCode::Right if !in_body => {
            if let Some(hub) = app.model_hub.as_mut() {
                hub.pane = Pane::Body;
            }
            app.enter_hub_scope();
        }
        KeyCode::Left => {
            if let Some(hub) = app.model_hub.as_mut() {
                hub.pane = Pane::Sidebar;
            }
        }
        KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
            let page = visible as isize;
            let delta = match key.code {
                KeyCode::Up => -1,
                KeyCode::Down => 1,
                KeyCode::PageUp => -page,
                _ => page,
            };
            let hub = app.model_hub.as_mut().expect("checked above");
            if in_body {
                hub.move_selection(delta, len, visible);
            } else {
                hub.move_scope(delta.signum());
                app.enter_hub_scope();
            }
        }
        KeyCode::Char('k') | KeyCode::Char('j') if !in_body || in_profiles => {
            let delta = if key.code == KeyCode::Char('k') { -1 } else { 1 };
            let hub = app.model_hub.as_mut().expect("checked above");
            if in_body {
                hub.move_selection(delta, len, visible);
            } else {
                hub.move_scope(delta);
                app.enter_hub_scope();
            }
        }
        KeyCode::Enter => app.accept_model_hub(),
        KeyCode::Char('r') if ctrl(key, 'r') && in_models => {
            if let Some(id) = app.model_hub.as_ref().and_then(|hub| hub.scoped_provider()).map(str::to_owned) {
                app.refresh_catalog(&id, true);
                app.status = format!("asking {id} for its models again");
            }
        }
        KeyCode::Char('r') if in_body && in_profiles => {
            if let Some(name) = selected_profile() {
                let mut input = InputBuffer::new();
                input.insert_str(&name);
                app.model_hub.as_mut().expect("checked above").prompt = Some(HubPrompt::Rename { name, input });
            }
        }
        KeyCode::Char('d') if in_body && in_profiles => {
            if app.settings.profiles.len() <= 1 {
                app.status = "that is the only profile; add another before deleting it".to_owned();
            } else if let Some(name) = selected_profile() {
                app.model_hub.as_mut().expect("checked above").prompt = Some(HubPrompt::ConfirmDelete { name });
            }
        }
        KeyCode::Char('e') if in_body && in_profiles => {
            if let Some(name) = selected_profile() {
                if name != app.settings.default_profile || !app.follows_profile {
                    app.switch_profile(&name);
                }
                app.model_hub = None;
                app.open_config_tab(0);
            }
        }
        KeyCode::Char('n') if in_body && in_profiles => {
            let hub = app.model_hub.as_mut().expect("checked above");
            if let Some(index) = hub.scopes.iter().position(|scope| matches!(scope, Scope::Provider { .. })) {
                hub.scope = index;
                hub.pane = Pane::Body;
                hub.search.clear();
                hub.reset_body();
                app.enter_hub_scope();
                app.status = "pick a model — Enter makes a profile for it".to_owned();
            } else {
                app.model_hub = None;
                app.open_provider_picker();
            }
        }
        KeyCode::Backspace if in_body && in_models => {
            let hub = app.model_hub.as_mut().expect("checked above");
            hub.search.pop();
            hub.reset_body();
        }
        KeyCode::Char(ch) if in_body && in_models && !key.modifiers.contains(KeyModifiers::CONTROL) => {
            let hub = app.model_hub.as_mut().expect("checked above");
            hub.search.push(ch);
            hub.reset_body();
        }
        _ => {}
    }
}

/// Keys while the hub asks about a profile: a new name, or a yes to delete.
fn hub_prompt_key(app: &mut App, key: KeyEvent) {
    use crate::model_hub::HubPrompt;
    let Some(prompt) = app.model_hub.as_mut().and_then(|hub| hub.prompt.as_mut()) else {
        return;
    };
    match (prompt, key.code) {
        (_, KeyCode::Esc) => {
            if let Some(hub) = app.model_hub.as_mut() {
                hub.prompt = None;
            }
        }
        (HubPrompt::Rename { name, input }, KeyCode::Enter) => {
            let (from, to) = (name.clone(), input.text());
            if let Some(hub) = app.model_hub.as_mut() {
                hub.prompt = None;
            }
            let result = app.settings.rename_profile(&from, &to).and_then(|to| {
                app.save_and_apply_settings()?;
                Ok(to)
            });
            match result {
                Ok(to) => app.status = format!("renamed to {to}"),
                Err(error) => app.status = format!("could not rename: {error:#}"),
            }
        }
        (HubPrompt::Rename { input, .. }, _) => edit_buffer(input, key, false),
        (HubPrompt::ConfirmDelete { name }, KeyCode::Enter | KeyCode::Char('y')) => {
            let name = name.clone();
            if let Some(hub) = app.model_hub.as_mut() {
                hub.prompt = None;
                hub.reset_body();
            }
            let result = app.settings.remove_profile(&name).and_then(|()| app.save_and_apply_settings());
            match result {
                Ok(()) => app.status = format!("profile {name} deleted"),
                Err(error) => app.status = format!("could not delete: {error:#}"),
            }
        }
        (HubPrompt::ConfirmDelete { .. }, KeyCode::Char('n')) => {
            if let Some(hub) = app.model_hub.as_mut() {
                hub.prompt = None;
            }
        }
        _ => {}
    }
}

impl App {
    /// The session's mode, as the shared flag has it now.
    pub(super) fn mode(&self) -> Permission {
        if self.yolo.load(Ordering::Relaxed) { Permission::Yolo } else { Permission::Auto }
    }

    /// Open the hub on the profiles.
    pub(super) fn open_model_hub(&mut self) {
        let mut hub = crate::model_hub::ModelHub::new(&self.settings);
        hub.selected =
            crate::model_hub::profile_rows(&self.settings).iter().position(|row| row.active).unwrap_or_default();
        self.model_hub = Some(hub);
    }

    /// Open the hub to choose the model for `role` of the active profile.
    pub(super) fn open_role_picker(&mut self, role: &str) {
        self.model_hub = Some(crate::model_hub::ModelHub::assigning(&self.settings, role));
        self.enter_hub_scope();
    }

    /// Open the hub on one provider's models, to run one of them.
    pub(super) fn open_provider_models(&mut self, provider: &str) {
        let mut hub = crate::model_hub::ModelHub::new(&self.settings);
        if let Some(index) = hub
            .scopes
            .iter()
            .position(|scope| matches!(scope, crate::model_hub::Scope::Provider { id, .. } if id == provider))
        {
            hub.scope = index;
        }
        self.model_hub = Some(hub);
        self.enter_hub_scope();
    }

    /// Ask `provider` for its models in the background. Unless `force`d, a
    /// list already showing is only refreshed once the copy on disk is a day
    /// old; one being fetched is never fetched twice.
    pub(super) fn refresh_catalog(&mut self, provider: &str, force: bool) {
        use crate::model_hub::Catalog;
        let Some(config) = self.settings.providers.get(provider) else {
            return;
        };
        let showing = matches!(self.catalogs.get(provider), Some(Catalog::Ready(_)));
        if matches!(self.catalogs.get(provider), Some(Catalog::Loading)) {
            return;
        }
        let stale =
            crate::providers::cached_catalog(&self.config.paths, provider).is_none_or(|catalog| catalog.is_stale());
        if !force && showing && !stale {
            return;
        }
        // `try_current` rather than `spawn`: the app is also driven by
        // synchronous tests, where there is no reactor to fetch on.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        // The session's own provider is resolved already, with a key given on
        // the command line where there was one.
        let connection = if self.config.connection.provider == provider {
            self.config.connection.clone()
        } else {
            crate::providers::Connection::resolve(provider, config, &self.credentials)
        };
        if !showing {
            self.catalogs.insert(provider.to_owned(), Catalog::Loading);
        }
        let paths = self.config.paths.clone();
        let sender = self.background_tx.clone();
        let provider = provider.to_owned();
        runtime.spawn(async move {
            let result = crate::providers::catalog(&paths, &connection, force)
                .await
                .map(|catalog| catalog.models)
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send(Background::Catalog { provider, result });
        });
    }

    /// The catalog for whichever provider the hub is scoped to.
    pub(super) fn hub_catalog(&self) -> &crate::model_hub::Catalog {
        const IDLE: crate::model_hub::Catalog = crate::model_hub::Catalog::Idle;
        self.model_hub
            .as_ref()
            .and_then(|hub| hub.scoped_provider())
            .and_then(|id| self.catalogs.get(id))
            .unwrap_or(&IDLE)
    }

    /// Make sure the provider under the sidebar cursor has a list on the way.
    pub(super) fn enter_hub_scope(&mut self) {
        let provider = self.model_hub.as_ref().and_then(|hub| hub.scoped_provider()).map(str::to_owned);
        if let Some(provider) = provider {
            self.refresh_catalog(&provider, false);
        }
    }

    /// Switch the session to profile `name`, and make it the default.
    pub(super) fn switch_profile(&mut self, name: &str) {
        let previous = std::mem::replace(&mut self.settings.default_profile, name.to_owned());
        let followed = std::mem::replace(&mut self.follows_profile, true);
        match self.save_and_apply_settings() {
            Ok(()) => self.status = format!("profile {name} · {}", self.config.model_ref()),
            Err(error) => {
                self.settings.default_profile = previous;
                self.follows_profile = followed;
                self.status = format!("could not switch profile: {error:#}");
            }
        }
    }

    /// Run `reference`: switch to the profile that runs it, or make one named
    /// after it.
    pub(super) fn use_model(&mut self, reference: &str) {
        let (name, created) = match self.settings.profile_for(reference) {
            Some(name) => (name.to_owned(), false),
            None => (self.settings.add_profile(reference), true),
        };
        self.switch_profile(&name);
        if created && self.config.profile == name {
            self.status = format!("new profile {name} · /config tunes its limits");
        } else if created {
            self.settings.profiles.remove(&name);
        }
    }

    /// Give `reference` to `role` of the active profile — `default` being the
    /// profile's own model.
    pub(super) fn assign_role(&mut self, role: &str, reference: Option<String>) {
        let label = crate::roles::role(role).map_or(role, |role| role.label);
        let result = self.active_profile_mut().map(|profile| profile.set_role_model(role, reference.clone()));
        match result.and_then(|()| self.save_and_apply_settings()) {
            Ok(()) => {
                self.status = match reference {
                    Some(model) => format!("{label}: {model}"),
                    None => format!("{label}: inherits again"),
                };
            }
            Err(error) => self.status = format!("could not save: {error:#}"),
        }
    }

    /// Enter on the hub: step into a scope, switch to a profile, or use or
    /// assign the model under the cursor.
    pub(super) fn accept_model_hub(&mut self) {
        use crate::model_hub::{Pane, Pick, Scope};
        let Some(hub) = &self.model_hub else {
            return;
        };
        if hub.pane == Pane::Sidebar && !matches!(hub.current_scope(), Scope::AddProvider) {
            if let Some(hub) = self.model_hub.as_mut() {
                hub.pane = Pane::Body;
                hub.reset_body();
            }
            self.enter_hub_scope();
            return;
        }
        match hub.current_scope().clone() {
            Scope::Separator => {}
            Scope::AddProvider => {
                self.model_hub = None;
                self.open_provider_picker();
            }
            Scope::Profiles => {
                let Some(row) = crate::model_hub::profile_rows(&self.settings).get(hub.selected).cloned() else {
                    return;
                };
                self.model_hub = None;
                self.switch_profile(&row.name);
            }
            Scope::Provider { id, .. } => {
                let cards = self.hub_catalog().cards();
                let Some(model) =
                    crate::model_hub::filter(cards, &hub.search).get(hub.selected).map(|card| card.id.clone())
                else {
                    return;
                };
                let reference = crate::providers::join_ref(&id, &model);
                let pick = hub.pick.clone();
                self.model_hub = None;
                match pick {
                    Pick::Use => self.use_model(&reference),
                    Pick::Assign(role) => self.assign_role(&role, Some(reference)),
                }
            }
        }
    }
}
