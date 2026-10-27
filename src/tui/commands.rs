//! Slash commands: what each one says and does.

use super::*;

/// `n viewers`, for the remote badge and status.
pub(super) fn viewers(count: usize) -> String {
    match count {
        0 => "no viewers".to_owned(),
        1 => "1 viewer".to_owned(),
        n => format!("{n} viewers"),
    }
}

/// A pairing link's lifetime, in words.
pub(super) fn expiry(seconds: u64) -> String {
    let minutes = seconds.div_ceil(60).max(1);
    format!("{minutes} minute{}", if minutes == 1 { "" } else { "s" })
}

/// The row a 1-based `number` names in a listing.
pub(super) fn numbered<'a, T>(rows: &'a [T], number: &str) -> Option<&'a T> {
    rows.get(number.trim().parse::<usize>().ok()?.saturating_sub(1))
}

impl App {
    /// Run `input` if it is a slash command. Returns whether it was one.
    pub(super) fn slash_command(&mut self, input: &str) -> bool {
        let (command, argument) = input.split_once(' ').unwrap_or((input, ""));
        let argument = argument.trim();
        // The names older versions used are kept as quiet aliases: they do
        // what they always did, and only the current names are offered.
        match command {
            "/help" => self.show_help = true,
            "/new" | "/clear" => self.new_session(),
            "/fork" => self.fork_session(),
            "/quit" | "/q" | "/exit" => self.quit = true,
            "/btw" => self.btw_command(argument),
            "/effort" => self.effort_command(argument),
            "/profile" | "/model" | "/models" => self.profile_command(argument),
            "/providers" => self.open_config_tab(PROVIDERS_TAB),
            "/mode" => self.mode_command(argument),
            "/resume" | "/sessions" => self.resume_session(argument),
            "/rename" => self.rename_session(argument),
            "/usage" => self.open_usage(),
            "/tools" | "/skills" | "/plugins" | "/mcps" => self.list_extensions(),
            "/goal" => self.goal_command(argument),
            "/loop" => self.loop_command(argument),
            "/cancel-loop" => self.cancel_ralph_loop(),
            "/config" => self.open_config(argument),
            "/theme" => self.theme_command(argument),
            "/feedback" => self.open_feedback(),
            "/remote" => self.remote_command(argument),
            "/compact" => self.compact_command(),
            "/repair" => self.repair_command(),
            "/memory" | "/memories" => self.memory_command(argument),
            // A `/name` that is a skill or a plugin command is not a slash
            // command; the caller resolves it into a prompt.
            _ if command.starts_with('/') && !self.has_extension(&command[1..]) => {
                self.fail(format!("Unknown command: {command}"))
            }
            _ => return false,
        }
        self.follow = true;
        true
    }

    /// Whether an installed skill or plugin answers to `/name`.
    fn has_extension(&self, name: &str) -> bool {
        self.services.skills.read().expect("skill registry lock").get(name).is_some()
            || self.services.plugins.command(name).is_some()
    }

    /// Put the outcome of a settings change in the status bar: `done` when it
    /// saved, the error under `failure` when it did not.
    pub(super) fn report(&mut self, result: Result<()>, done: impl Into<String>, failure: &str) -> bool {
        let saved = result.is_ok();
        self.status = match result {
            Ok(()) => done.into(),
            Err(error) => format!("{failure}: {error:#}"),
        };
        saved
    }

    /// Switch this session to `mode`. A running turn follows from its next
    /// tool call.
    pub(super) fn set_mode(&mut self, mode: Permission) {
        self.yolo.store(mode == Permission::Yolo, Ordering::Relaxed);
        self.status = match mode {
            Permission::Auto => "AUTO — risky actions are reviewed first".to_owned(),
            Permission::Yolo => "YOLO — every action runs unreviewed".to_owned(),
        };
        if let Some(remote) = &mut self.remote {
            remote.mode_changed(mode, "set in the terminal");
        }
    }

    /// `/remote [on|off|qr|url|status]`: share this session live with the
    /// account's browsers, stop sharing it, pair a phone, or say how it is
    /// going. Bare `/remote` toggles.
    pub(super) fn remote_command(&mut self, argument: &str) {
        match argument.to_ascii_lowercase().as_str() {
            "" if self.sharing() => self.remote_off(),
            "" | "on" => self.remote_on(),
            "off" => self.remote_off(),
            "qr" | "pair" => self.open_pairing(PairingView::Qr),
            "url" | "link" => self.open_pairing(PairingView::Url),
            "status" => {
                let summary = self.remote_summary();
                self.say(summary);
            }
            other => self.fail(format!("Unknown /remote option `{other}`. Usage: /remote [on|off|qr|url|status]")),
        }
    }

    fn remote_on(&mut self) {
        if self.sharing() {
            self.status = "already sharing — /remote qr opens it on your phone".to_owned();
            return;
        }
        // A bridge that gave up is replaced, not resumed.
        self.remote = None;
        self.remote_muted = false;
        self.persist_session();
        match self.start_remote() {
            Ok(()) => {
                self.remote_announce = true;
                self.status = "sharing — connecting…".to_owned();
            }
            Err(reason) => self.fail(format!("Cannot share this session: {reason}.")),
        }
    }

    fn remote_off(&mut self) {
        // Off sticks for this session: auto-share does not bring it back.
        self.remote_muted = true;
        self.remote_announce = false;
        if self.sharing() {
            self.stop_remote("Sharing was turned off in the terminal.");
            self.status = "remote off for this session — /remote turns it back on".to_owned();
        } else {
            self.remote = None;
            self.status = "this session is not shared".to_owned();
        }
    }

    /// Ask the server for a single-use link that signs a phone in, opening
    /// this session when it is shared.
    fn open_pairing(&mut self, view: PairingView) {
        if !crate::sync::is_configured(&self.credentials) {
            return self.fail("Pairing a phone needs Abacus Sync. Run `abacus sync login` first.");
        }
        let client = match crate::sync::configured_client(&self.config.paths) {
            Ok(client) => client,
            Err(error) => return self.fail(format!("Cannot pair a phone: {error:#}")),
        };
        let session =
            self.remote.as_ref().filter(|bridge| bridge.is_active()).map(|bridge| bridge.session_id().to_owned());
        match view {
            PairingView::Qr => self.qr_overlay = Some(QrOverlay::Loading),
            PairingView::Url => self.status = "requesting a pairing link…".to_owned(),
        }
        let events = self.background_tx.clone();
        tokio::spawn(async move {
            let mut result = client.pairing_url(session.as_deref()).await;
            // Sharing may still be starting up; a link to the session list
            // pairs the phone all the same.
            if session.is_some() && matches!(result, Err(crate::sync::SyncError::Gone)) {
                result = client.pairing_url(None).await;
            }
            let result = result.map_err(|error| match error {
                crate::sync::SyncError::Gone => "this sync server does not offer phone pairing yet".to_owned(),
                other => other.to_string(),
            });
            let _ = events.send(Background::Pairing { view, result });
        });
    }

    pub(super) fn pairing_ready(&mut self, view: PairingView, result: Result<crate::sync::Pairing, String>) {
        match (view, result) {
            (PairingView::Qr, result) => {
                // Closed while the request was out: leave it closed.
                if self.qr_overlay.is_none() {
                    return;
                }
                self.qr_overlay = Some(match result {
                    Ok(pairing) => QrOverlay::Ready {
                        rows: crate::remote::qr::rows(&pairing.pairing_url),
                        opens_session: pairing.session_id.is_some(),
                        expires_in: pairing.expires_in,
                        url: pairing.pairing_url,
                    },
                    Err(error) => QrOverlay::Failed(error),
                });
            }
            (PairingView::Url, Ok(pairing)) => {
                let opens = if pairing.session_id.is_some() { "this session" } else { "your sessions" };
                self.say(format!(
                    "Open this link on your phone to see {opens}. It works once and expires in {} — and it signs \
                     in as you, so keep it to yourself:\n{}",
                    expiry(pairing.expires_in),
                    pairing.pairing_url
                ));
                self.status = "pairing link ready".to_owned();
            }
            (PairingView::Url, Err(error)) => self.fail(format!("Could not create a pairing link: {error}")),
        }
    }

    /// One line on the state of sharing, for `/remote status`.
    pub(super) fn remote_summary(&self) -> String {
        let auto = if self.settings.remote.auto_share { "on" } else { "off" };
        let Some(bridge) = &self.remote else {
            return if !crate::sync::is_configured(&self.credentials) {
                "Remote: off — sign in with `abacus sync login` to share sessions with your phone.".to_owned()
            } else if self.remote_muted {
                format!("Remote: off for this session (/remote turns it on). Auto-share is {auto}.")
            } else if self.settings.remote.auto_share {
                "Remote: off — sharing starts with this session's first prompt (auto-share is on).".to_owned()
            } else {
                "Remote: off. Auto-share is off; /remote shares this session.".to_owned()
            };
        };
        match bridge.state() {
            LinkState::Connecting => "Remote: connecting…".to_owned(),
            LinkState::Live => {
                format!("Remote: live · {}. /remote qr opens it on your phone.", viewers(bridge.browsers()))
            }
            LinkState::Reconnecting(reason) => format!("Remote: reconnecting — {reason}."),
            LinkState::Stopped(reason) => format!("Remote: stopped — {reason}. /remote tries again."),
        }
    }

    /// Show or hide the model's reasoning, everywhere, and persist the choice.
    pub(super) fn set_show_thinking(&mut self, show: bool) -> bool {
        let previous = std::mem::replace(&mut self.settings.ui.show_thinking, show);
        let result = self.save_and_apply_settings();
        // Say where it went, so hiding it does not look like the reasoning
        // stopped being captured.
        let done = if show { "thinking shown" } else { "thinking hidden (still recorded in traces)" };
        let saved = self.report(result, done, "configuration error");
        if !saved {
            self.settings.ui.show_thinking = previous;
        }
        saved
    }

    pub(super) fn btw_command(&mut self, note: &str) {
        if note.is_empty() {
            return self.fail("Usage: /btw <side question or remark>");
        }
        if self.running.is_none() {
            // With nothing running there is nothing to avoid derailing, and a
            // note the model only sees "later" would just be lost.
            return self.say("/btw is for while a turn is running — ask it directly instead.");
        }
        self.say(format!("Noted, by the way: {note}"));
        self.state.injections.push(crate::agent::Injection::SideNote(note.to_owned()));
        self.status = "noted · delivered after the current step".to_owned();
    }

    pub(super) fn effort_command(&mut self, argument: &str) {
        let label = |effort: Option<crate::config::ReasoningEffort>, unset: &str| {
            effort.map_or(unset.to_owned(), |effort| effort.label().to_owned())
        };
        if argument.is_empty() {
            let current = label(self.config.reasoning_effort, "auto (provider default)");
            return self.say(format!(
                "Reasoning effort: {current}. Set it with /effort \
                 minimal|low|medium|high|xhigh|max, or /effort auto to leave it to the provider."
            ));
        }
        let cleared = matches!(argument.to_ascii_lowercase().as_str(), "auto" | "default" | "unset");
        let parsed = crate::config::ReasoningEffort::parse(argument);
        if !cleared && parsed.is_none() {
            return self.fail("Usage: /effort minimal|low|medium|high|xhigh|max|auto");
        }
        if let Ok(profile) = self.active_profile_mut() {
            profile.reasoning_effort = parsed;
        }
        let described = label(parsed, "auto");
        let result = self.save_and_apply_settings();
        if self.report(result, format!("effort {described}"), "configuration error") {
            self.say(match parsed {
                Some(_) => format!(
                    "Reasoning effort set to {described}. Sent with every request on this \
                     profile; models without reasoning ignore it."
                ),
                None => "Reasoning effort cleared — the provider's own default applies.".to_owned(),
            });
        }
    }

    /// Ctrl+T: step the effort through auto → low → medium → high and back to
    /// auto. A level off that ladder, set with /effort, steps to the next rung.
    pub(super) fn cycle_effort(&mut self) {
        use crate::config::ReasoningEffort;
        let next = match self.config.reasoning_effort {
            None | Some(ReasoningEffort::Minimal) => Some(ReasoningEffort::Low),
            Some(ReasoningEffort::Low) => Some(ReasoningEffort::Medium),
            Some(ReasoningEffort::Medium) => Some(ReasoningEffort::High),
            Some(ReasoningEffort::High | ReasoningEffort::XHigh | ReasoningEffort::Max) => None,
        };
        if let Ok(profile) = self.active_profile_mut() {
            profile.reasoning_effort = next;
        }
        let result = self.save_and_apply_settings();
        let label = next.map_or("auto", ReasoningEffort::label);
        self.report(result, format!("effort {label}"), "configuration error");
    }

    /// `/tools`: everything the agent can reach — its tools, then what
    /// extensions add: skills, plugin commands, MCP tools.
    pub(super) fn list_extensions(&mut self) {
        let services = self.services.clone();
        let mut names: Vec<String> = services
            .tool_specs()
            .iter()
            .filter_map(|spec| spec["function"]["name"].as_str().map(str::to_owned))
            .collect();
        names.extend(["goal_status", "goal_update", "spawn_subagents"].map(str::to_owned));
        let mut sections = vec![format!("Tools: {}", names.join(", "))];
        let skills: Vec<String> = services
            .skills
            .read()
            .expect("skill registry lock")
            .list()
            .map(|skill| format!("  /{}  {}", skill.name, skill.description))
            .collect();
        if !skills.is_empty() {
            sections.push(format!("Skills (run one with /<name>):\n{}", skills.join("\n")));
        }
        let plugins: Vec<String> = services
            .plugins
            .list()
            .map(|plugin| format!("  {} {}  {}", plugin.name, plugin.version, plugin.description))
            .collect();
        if !plugins.is_empty() {
            sections.push(format!("Plugins:\n{}", plugins.join("\n")));
        }
        let mcp: Vec<String> =
            services.mcp.tools().map(|tool| format!("  {}  {}", tool.exposed_name, tool.description)).collect();
        if !mcp.is_empty() {
            sections.push(format!("MCP tools:\n{}", mcp.join("\n")));
        }
        self.say(sections.join("\n\n"));
    }

    pub(super) fn mode_command(&mut self, argument: &str) {
        match argument.trim().to_ascii_lowercase().as_str() {
            "" => self.say(format!(
                "Mode: {}\nAUTO runs reading and workspace edits at once and puts anything riskier to the safety \
                 reviewer, which blocks what you did not ask for. YOLO runs everything without review.\n\
                 Switch with /mode auto, /mode yolo, or Shift+Tab; the default for new sessions is in /config → Agent.",
                self.mode().label()
            )),
            "auto" => self.set_mode(Permission::Auto),
            "yolo" => self.set_mode(Permission::Yolo),
            _ => self.fail("Usage: /mode auto|yolo"),
        }
    }

    /// Manual quick-compaction: a synchronous drop-only shrink for when the
    /// user wants to cut context immediately. The rolling summary runs on its
    /// own each turn and is left untouched. Sized from the model's window —
    /// a fixed number over-cuts a large context and under-cuts a small one.
    pub(super) fn compact_command(&mut self) {
        let before = self.messages.len();
        let target = self.config.model_limits.compaction_budget().recent_budget_chars;
        self.messages = compact_messages(&self.messages, target);
        self.persist_session();
        self.say(format!(
            "Quick-compacted conversation from {before} to {} messages, targeting {target} chars \
             for this model. Dropped messages are not summarised; rolling-summary compaction \
             runs automatically as the context grows.",
            self.messages.len(),
        ));
    }

    /// An interrupted or failed turn can leave history that strict providers
    /// reject wholesale, after which every turn errors — so the fix has to be
    /// reachable from inside the stuck session.
    pub(super) fn repair_command(&mut self) {
        if self.running.is_some() {
            self.status = "cannot repair while a turn is running".to_owned();
            return;
        }
        let fixes = crate::session::repair_messages(&mut self.messages);
        if fixes.is_empty() {
            return self.say("No corruption found: every tool call parses and has a result.");
        }
        self.ctx_chars = message_chars(&self.messages);
        self.persist_session();
        self.say(format!("Repaired the session history: {}.", fixes.join("; ")));
    }

    /// `/memory`: what Abacus remembers in this workspace, numbered, and
    /// `forget` to remove one.
    pub(super) fn memory_command(&mut self, argument: &str) {
        let argument = argument.trim();
        let (verb, rest) =
            argument.split_once(char::is_whitespace).map_or((argument, ""), |(verb, rest)| (verb, rest.trim()));
        // One ordering for display and forgetting alike, or the numbers shown
        // would target different entries.
        let memories = self.state.memories.snapshot();
        match verb {
            "" => {}
            "forget" | "delete" => {
                let reference = rest.to_ascii_lowercase();
                if let Some(memory) = numbered(&memories, reference.strip_prefix('m').unwrap_or(&reference))
                    && self.state.memories.remove(memory.id)
                {
                    return self.say(format!("Memory \"{}\" forgotten.", memory.title));
                }
                return self.fail("Usage: /memory forget m<number> — numbers from /memory");
            }
            _ => return self.fail("Usage: /memory [forget m1]"),
        }
        if memories.is_empty() {
            return self.say("No memories here yet. Abacus records facts and decisions worth keeping as it works.");
        }
        let mut lines = vec![format!("Memories ({}), newest first:", memories.len())];
        lines.extend(memories.iter().enumerate().map(|(index, memory)| {
            format!("  m{}. {} — {}", index + 1, memory.title, ui::truncate(&memory.body, 120))
        }));
        lines.push("/memory forget m1 removes one.".to_owned());
        self.say(lines.join("\n"));
    }

    /// Shift+Tab: AUTO and YOLO, back and forth, for this session.
    pub(super) fn toggle_mode(&mut self) {
        self.set_mode(self.mode().toggled());
    }

    pub(super) fn goal_command(&mut self, argument: &str) {
        let argument = argument.trim();
        let (result, start_prompt) = if argument.is_empty() {
            (
                Ok(self
                    .state
                    .goal
                    .snapshot()
                    .map(|goal| {
                        format!(
                            "Goal · {:?}\n{}{}",
                            goal.status,
                            goal.objective,
                            goal.note.map(|note| format!("\n\nLatest update: {note}")).unwrap_or_default()
                        )
                    })
                    .unwrap_or_else(|| "No goal is set. Use /goal <objective>.".to_owned())),
                None,
            )
        } else if argument == "pause" {
            let result = self.state.goal.pause().map(|_| "Goal paused. Use /goal resume when ready.".to_owned());
            if result.is_ok()
                && let Some(handle) = self.running.take()
            {
                handle.abort();
                self.approval = None;
                self.receiving_delta = false;
                if let Some(state) = &mut self.ralph_loop
                    && state.is_active()
                {
                    let _ = state.pause();
                }
            }
            (result, None)
        } else if argument == "resume" {
            match self.state.goal.resume() {
                Ok(goal) => (Ok("Goal resumed.".to_owned()), Some(("Resume goal".to_owned(), goal.objective))),
                Err(error) => (Err(error), None),
            }
        } else if argument == "clear" {
            (self.state.goal.set(None).map(|()| "Goal cleared.".to_owned()), None)
        } else if matches!(argument, "done" | "complete") {
            (
                Ok(self
                    .state
                    .goal
                    .execute("goal_update", r#"{"status":"complete"}"#)
                    .unwrap_or_else(|| "Error: no goal is set".to_owned())),
                None,
            )
        } else if let Some(objective) = argument.strip_prefix("edit ") {
            (self.state.goal.edit(objective).map(|goal| format!("Goal updated: {}", goal.objective)), None)
        } else {
            match self.state.goal.create(argument) {
                Ok(goal) => {
                    (Ok(format!("Goal set: {}", goal.objective)), Some((goal.objective.clone(), goal.objective)))
                }
                Err(error) => (Err(error), None),
            }
        };
        match result {
            Ok(text) => self.say(text),
            Err(error) => self.fail(format!("Goal error: {error:#}")),
        }
        self.persist_session();
        if let Some((display, prompt)) = start_prompt {
            self.start_turn(display, prompt, true);
        }
    }

    pub(super) fn loop_command(&mut self, argument: &str) {
        let argument = argument.trim();
        if argument.is_empty() || argument == "status" {
            let text = self.ralph_loop.as_ref().map_or_else(
                || "No Ralph loop is configured.\n\nUsage: /loop \"<prompt>\" --max-iterations 20 --completion-promise \"DONE\"".to_owned(),
                |state| format!(
                    "Ralph loop · {:?}\nIteration: {}{}\nCompletion promise: {}\n\n{}",
                    state.status,
                    state.iteration,
                    state.max_iterations.map(|limit| format!(" / {limit}")).unwrap_or_else(|| " / unlimited".to_owned()),
                    state.completion_promise,
                    state.prompt
                ),
            );
            self.say(text);
            return;
        }
        if argument == "pause" {
            let result = self.ralph_loop.as_mut().context("no Ralph loop is configured").and_then(RalphLoop::pause);
            self.status = result
                .map(|()| "loop pauses after the current turn".to_owned())
                .unwrap_or_else(|error| format!("loop pause failed: {error}"));
            self.persist_session();
            return;
        }
        if argument == "resume" {
            let result = self.ralph_loop.as_mut().context("no Ralph loop is configured").and_then(RalphLoop::resume);
            match result {
                Ok(()) => self.continue_ralph_loop(),
                Err(error) => self.status = format!("loop resume failed: {error}"),
            }
            return;
        }
        if argument == "cancel" {
            return self.cancel_ralph_loop();
        }
        match RalphLoop::from_command(argument) {
            Ok(state) => {
                self.ralph_loop = Some(state);
                self.persist_session();
                self.continue_ralph_loop();
            }
            Err(error) => {
                self.fail(format!("Could not start loop: {error:#}\n\nUsage: /loop \"<prompt>\" --max-iterations 20 --completion-promise \"DONE\""));
            }
        }
    }

    pub(super) fn continue_ralph_loop(&mut self) {
        if self.running.is_some() {
            return;
        }
        let Some(state) = &mut self.ralph_loop else {
            return;
        };
        if !state.is_active() {
            return;
        }
        let iteration = match state.begin_iteration() {
            Ok(iteration) => iteration,
            Err(error) => {
                self.status = format!("loop stopped: {error}");
                self.persist_session();
                return;
            }
        };
        let prompt = state.prompt.clone();
        self.say(format!("Ralph loop · iteration {iteration}"));
        self.persist_session();
        self.start_turn(prompt.clone(), prompt, false);
    }

    pub(super) fn cancel_ralph_loop(&mut self) {
        let Some(state) = &mut self.ralph_loop else {
            self.status = "no Ralph loop is active".to_owned();
            return;
        };
        state.cancel();
        if let Some(handle) = self.running.take() {
            handle.abort();
            self.approval = None;
            self.receiving_delta = false;
        }
        self.persist_session();
        self.status = "Ralph loop cancelled".to_owned();
        self.say("Ralph loop cancelled by user.");
    }

    /// `/theme [auto|dark|light]` — switch the palette live and persist it.
    pub(super) fn theme_command(&mut self, argument: &str) {
        let argument = argument.trim();
        if argument.is_empty() {
            let resolved = self.settings.ui.theme.resolve();
            let custom = crate::theme::available(&self.config.paths.themes_dir);
            let mut body = format!(
                "Theme: {} (showing {}).\nSwitch with /theme dark, /theme light, /theme auto, or /theme <name>.",
                self.settings.ui.theme.label(),
                if resolved == ThemeMode::Dark { "dark" } else { "light" },
            );
            body.push_str(&format!("\n\nTheme files live in {}.", self.config.paths.themes_dir.display()));
            if custom.is_empty() {
                body.push_str(
                    "\nNothing there yet — `/theme export <name>` writes the current palette out as a starting point.",
                );
            } else {
                body.push_str(&format!("\nAvailable: {}", custom.join(", ")));
            }
            self.say(body);
            return;
        }
        if let Some(name) = argument.strip_prefix("export") {
            self.export_theme(name.trim());
            return;
        }
        let choice = crate::theme::ThemeChoice::parse(argument);
        // Load before saving: a name that does not resolve should leave the
        // setting alone rather than persist a theme that will fail on every
        // later launch too.
        let (theme, error) = crate::theme::resolve(&choice, &self.config.paths.themes_dir);
        if let Some(error) = error {
            self.fail(error);
            return;
        }
        crate::theme::set_active(theme);
        self.settings.ui.theme = choice;
        let label = self.settings.ui.theme.label().to_owned();
        match self.settings.save(&self.config.paths) {
            Ok(()) => self.status = format!("theme: {label} · saved"),
            Err(error) => self.status = format!("theme save failed: {error:#}"),
        }
        self.follow = true;
    }

    /// Write the active palette out as a theme file to edit.
    pub(super) fn export_theme(&mut self, name: &str) {
        let name = if name.is_empty() { "custom" } else { name };
        if !name.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')) {
            self.fail("Theme names may use letters, digits, `.`, `_`, and `-`.");
            return;
        }
        let mode = self.settings.ui.theme.resolve();
        // The full palette, not the one adapted to this terminal: a 16- or
        // 256-colour rendering writes names like `white` or `indexed(123)`
        // that a theme file cannot load back, and NO_COLOR writes nothing.
        let full = crate::theme::ColorDepth::TrueColor;
        let palette = match &self.settings.ui.theme {
            crate::theme::ThemeChoice::Named(current) => {
                crate::theme::load(&self.config.paths.themes_dir, current, full).ok()
            }
            _ => None,
        }
        .unwrap_or_else(|| crate::theme::Theme::for_mode_at(mode, full));
        let file = crate::theme::ThemeFile::from_theme(name, mode, &palette);
        let directory = self.config.paths.themes_dir.clone();
        let path = directory.join(format!("{name}.json"));
        let written = std::fs::create_dir_all(&directory).and_then(|()| {
            let json = serde_json::to_string_pretty(&file)?;
            std::fs::write(&path, format!("{json}\n"))
        });
        match written {
            Ok(()) => {
                self.say(format!("Wrote {}.\nEdit the colours, then `/theme {name}` to use it.", path.display()));
                self.status = format!("theme exported: {name}");
            }
            Err(error) => self.fail(format!("could not write {}: {error}", path.display())),
        }
        self.follow = true;
    }

    /// `/profile` opens the profile hub; `/profile <name>` switches straight
    /// to one, by its name or a unique part of it.
    pub(super) fn profile_command(&mut self, argument: &str) {
        let argument = argument.trim();
        if argument.is_empty() {
            return self.open_model_hub();
        }
        if self.settings.profiles.contains_key(argument) {
            return self.switch_profile(argument);
        }
        let needle = argument.to_ascii_lowercase();
        let matches: Vec<String> =
            self.settings.profiles.keys().filter(|name| name.to_ascii_lowercase().contains(&needle)).cloned().collect();
        match matches.as_slice() {
            [only] => {
                let only = only.clone();
                self.switch_profile(&only);
            }
            [] => self.fail(format!("No profile matches `{argument}`. /profile lists them.")),
            several => self.fail(format!("`{argument}` matches {} — say which.", several.join(", "))),
        }
    }
}
