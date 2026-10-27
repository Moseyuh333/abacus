//! The `/profile` surface: a fullscreen two-column browser over the profiles
//! and every model the configured providers serve.
//!
//! This module owns the hub's state and the pure parts of turning it into
//! rows. The frame, the hit regions, and the catalog fetches live in
//! [`crate::tui`], which is where everything else that touches an `App` lives
//! — so what is here stays testable without standing one up.
//!
//! The sidebar lists the profiles first, then each provider:
//!
//! ```text
//! ╭─ PROFILES ───────────┬──────────────────────────────────────────────╮
//! │ ❯ profiles         3 │ ● kojo/kojo                  1m ctx · high   │
//! │ ─────────────────────│   llama/qwen-3-4b             32k ctx        │
//! │   Kojo            12 │   review (openrouter/…/gpt-6)                │
//! │   OpenRouter     501 │                                              │
//! │   + add provider     │                                              │
//! ╰──────────────────────┴──────────────────────────────────────────────╯
//! ```
//!
//! Picking a model in a provider's list switches to the profile that runs it,
//! and makes one — named after the model — when none does. So "use this
//! model" is one keypress, and a profile is only ever made by asking for it.

use crate::config::Settings;
use crate::model_info::ModelCard;

/// Which column has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Sidebar,
    Body,
}

/// A sidebar entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Every profile.
    Profiles,
    /// A rule between the profiles entry and the providers. Never selectable.
    Separator,
    /// One provider's model list.
    Provider { id: String, name: String },
    /// Opens the provider form, so a model that is nowhere yet is one step
    /// away rather than a trip through the config file.
    AddProvider,
}

/// What a provider's model list is currently doing.
#[derive(Debug, Clone, PartialEq)]
pub enum Catalog {
    /// Never asked for. The hub fetches on first visit rather than at startup:
    /// a provider you never open should not cost a request.
    Idle,
    Loading,
    Ready(Vec<ModelCard>),
    Failed(String),
}

impl Catalog {
    /// The right-aligned sidebar annotation — a count once known, and a mark
    /// for every state that is not a count, so the column never goes blank and
    /// starts reading as "zero models".
    pub fn annotation(&self) -> String {
        match self {
            Catalog::Idle => "—".to_owned(),
            Catalog::Loading => "…".to_owned(),
            Catalog::Ready(cards) => cards.len().to_string(),
            Catalog::Failed(_) => "!".to_owned(),
        }
    }

    pub fn cards(&self) -> &[ModelCard] {
        match self {
            Catalog::Ready(cards) => cards,
            _ => &[],
        }
    }
}

/// What Enter on a model does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// Switch to the profile that runs the model, making one when none does.
    Use,
    /// Give the model to a role of the active profile — `default` being the
    /// profile's own model. Opened from `/config`.
    Assign(String),
}

/// A question the hub is asking about the profile under the cursor.
#[derive(Debug, Clone)]
pub enum HubPrompt {
    Rename { name: String, input: crate::input::InputBuffer },
    ConfirmDelete { name: String },
}

/// The hub's state.
#[derive(Debug, Clone)]
pub struct ModelHub {
    pub scopes: Vec<Scope>,
    pub scope: usize,
    pub pane: Pane,
    /// Filter over a provider's model list. Typing goes straight here — the
    /// search is always live, so there is no mode to enter and none to forget
    /// to leave.
    pub search: String,
    /// Cursor within the *filtered* body list.
    pub selected: usize,
    /// First visible body row, so a long list scrolls rather than jumping.
    pub window: usize,
    pub pick: Pick,
    pub prompt: Option<HubPrompt>,
}

impl ModelHub {
    /// Open the hub for `settings`, on the profiles list.
    pub fn new(settings: &Settings) -> Self {
        let mut scopes = vec![Scope::Profiles, Scope::Separator];
        for (id, provider) in &settings.providers {
            scopes.push(Scope::Provider { id: id.clone(), name: provider.display_name(id).to_owned() });
        }
        scopes.push(Scope::AddProvider);
        ModelHub {
            scopes,
            scope: 0,
            pane: Pane::Body,
            search: String::new(),
            selected: 0,
            window: 0,
            pick: Pick::Use,
            prompt: None,
        }
    }

    /// Open the hub to choose a model for `role` of the active profile,
    /// landing on the provider the role's model is on now.
    pub fn assigning(settings: &Settings, role: &str) -> Self {
        let mut hub = Self::new(settings);
        hub.pick = Pick::Assign(role.to_owned());
        let current = settings
            .active_profile()
            .and_then(|profile| profile.resolve_role(role))
            .map(|resolved| resolved.model)
            .unwrap_or_default();
        let provider = crate::providers::split_ref(&current).map_or("", |(provider, _)| provider);
        hub.scope = hub
            .scopes
            .iter()
            .position(|scope| matches!(scope, Scope::Provider { id, .. } if id == provider))
            .or_else(|| hub.scopes.iter().position(|scope| matches!(scope, Scope::Provider { .. })))
            .unwrap_or(0);
        hub
    }

    pub fn current_scope(&self) -> &Scope {
        self.scopes.get(self.scope).unwrap_or(&Scope::Profiles)
    }

    /// The provider whose models the body is showing, if the scope names one.
    pub fn scoped_provider(&self) -> Option<&str> {
        match self.current_scope() {
            Scope::Provider { id, .. } => Some(id),
            _ => None,
        }
    }

    /// Move the sidebar cursor by `delta`, stepping over separators so a rule
    /// is never something you have to press down twice to get past.
    pub fn move_scope(&mut self, delta: isize) {
        let count = self.scopes.len() as isize;
        if count == 0 {
            return;
        }
        let mut next = self.scope as isize;
        for _ in 0..count {
            next = (next + delta).clamp(0, count - 1);
            if self.scopes[next as usize] != Scope::Separator {
                break;
            }
            if next == 0 || next == count - 1 {
                return;
            }
        }
        if self.scope != next as usize {
            self.scope = next as usize;
            self.search.clear();
            self.reset_body();
        }
    }

    /// Send the body cursor back to the top. Called whenever the list under it
    /// changes out from under the cursor.
    pub fn reset_body(&mut self) {
        self.selected = 0;
        self.window = 0;
    }

    /// Move the body cursor by `delta` within a list of `len` rows, keeping it
    /// inside a window of `visible` rows.
    pub fn move_selection(&mut self, delta: isize, len: usize, visible: usize) {
        if len == 0 {
            self.selected = 0;
            self.window = 0;
            return;
        }
        self.selected = (self.selected as isize + delta).clamp(0, len as isize - 1) as usize;
        self.scroll_into_view(len, visible);
    }

    /// Re-clamp the window so the cursor is on screen — after a move, and
    /// after a filter change that shortened the list under it.
    pub fn scroll_into_view(&mut self, len: usize, visible: usize) {
        if visible == 0 || len == 0 {
            self.window = 0;
            return;
        }
        self.selected = self.selected.min(len - 1);
        self.window = self.window.min(len.saturating_sub(visible));
        if self.selected < self.window {
            self.window = self.selected;
        } else if self.selected >= self.window + visible {
            self.window = self.selected + 1 - visible;
        }
    }
}

/// One row of the profiles list.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileRow {
    pub name: String,
    /// `provider/model`.
    pub model: String,
    pub active: bool,
    /// What sets it apart from the defaults: `1m ctx · high`.
    pub settings: String,
}

/// The profiles, in name order, with what each one changes from the defaults.
pub fn profile_rows(settings: &Settings) -> Vec<ProfileRow> {
    settings
        .profiles
        .iter()
        .map(|(name, profile)| {
            let mut facts = Vec::new();
            if let Some(context) = profile.context_window {
                facts.push(format!("{} ctx", crate::model_info::compact_count(context)));
            }
            if let Some(output) = profile.max_output_tokens {
                facts.push(format!("{} out", crate::model_info::compact_count(output)));
            }
            if let Some(effort) = profile.reasoning_effort {
                facts.push(effort.label().to_owned());
            }
            if !profile.roles.is_empty() {
                facts.push(format!("{} role(s)", profile.roles.len()));
            }
            ProfileRow {
                name: name.clone(),
                model: profile.model.clone(),
                active: *name == settings.default_profile,
                settings: facts.join(" · "),
            }
        })
        .collect()
}

/// Filter `cards` by `query`, best match first.
///
/// Matching is a subsequence rather than a substring, so `clsonnet` finds
/// `claude-sonnet` — model ids are long, hyphenated, and mostly typed from
/// memory, which is exactly the case substring search handles worst.
pub fn filter<'a>(cards: &'a [ModelCard], query: &str) -> Vec<&'a ModelCard> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return cards.iter().collect();
    }
    let mut scored: Vec<(usize, &ModelCard)> = cards
        .iter()
        .filter_map(|card| score(&card.id.to_ascii_lowercase(), &query).map(|score| (score, card)))
        .collect();
    // Lower score is a tighter match; ties fall back to id order so the list
    // does not reshuffle as you type through an ambiguous prefix.
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));
    scored.into_iter().map(|(_, card)| card).collect()
}

/// Subsequence match cost: the span of `haystack` the match had to stretch
/// across, so a tight run scores better than characters scattered end to end.
/// `None` when `needle` is not a subsequence at all.
fn score(haystack: &str, needle: &str) -> Option<usize> {
    let mut start = None;
    let mut end = 0usize;
    let mut chars = haystack.char_indices();
    for wanted in needle.chars() {
        let (index, _) = chars.find(|(_, ch)| *ch == wanted)?;
        start.get_or_insert(index);
        end = index;
    }
    Some(end - start.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Profile;
    use crate::providers::ProviderConfig;

    fn settings() -> Settings {
        let mut settings = Settings::default();
        settings
            .providers
            .insert("kojo".to_owned(), ProviderConfig::from_preset(crate::providers::preset("kojo").unwrap()));
        settings.providers.insert(
            "llama".to_owned(),
            ProviderConfig { name: String::new(), base_url: "http://localhost:8080/v1".into(), ..Default::default() },
        );
        settings.profiles.insert(
            "kojo/kojo".to_owned(),
            Profile { context_window: Some(1_000_000), ..Profile::for_model("kojo/kojo") },
        );
        settings.profiles.insert("llama/qwen-3-4b".to_owned(), Profile::for_model("llama/qwen-3-4b"));
        settings.default_profile = "kojo/kojo".to_owned();
        settings
    }

    fn card(id: &str) -> ModelCard {
        ModelCard { id: id.to_owned(), ..ModelCard::default() }
    }

    #[test]
    fn the_sidebar_lists_profiles_then_providers_then_a_way_to_add_one() {
        let hub = ModelHub::new(&settings());
        assert_eq!(hub.scopes[0], Scope::Profiles);
        assert_eq!(hub.scopes[1], Scope::Separator);
        assert!(matches!(&hub.scopes[2], Scope::Provider { id, name } if id == "kojo" && name == "Kojo"));
        // A provider with no display name falls back to its id rather than
        // rendering a blank row.
        assert!(matches!(&hub.scopes[3], Scope::Provider { name, .. } if name == "llama"));
        assert_eq!(hub.scopes.last(), Some(&Scope::AddProvider));
    }

    #[test]
    fn assigning_a_role_opens_on_the_provider_it_uses_now() {
        let mut settings = settings();
        settings.profiles.get_mut("kojo/kojo").unwrap().roles.insert("aux".into(), "llama/qwen-3-4b".into());
        let hub = ModelHub::assigning(&settings, "aux");
        assert_eq!(hub.pick, Pick::Assign("aux".to_owned()));
        assert_eq!(hub.scoped_provider(), Some("llama"));
        // An unassigned role inherits, and opens where its model is.
        assert_eq!(ModelHub::assigning(&settings, "compaction").scoped_provider(), Some("kojo"));
    }

    #[test]
    fn moving_the_sidebar_cursor_steps_over_the_rule() {
        let mut hub = ModelHub::new(&settings());
        hub.move_scope(1);
        assert_eq!(hub.scope, 2, "the separator is skipped, not landed on");
        hub.move_scope(-1);
        assert_eq!(hub.scope, 0);
        hub.move_scope(-1);
        assert_eq!(hub.scope, 0, "and the ends hold");
    }

    #[test]
    fn profile_rows_say_what_differs_from_the_defaults() {
        let rows = profile_rows(&settings());
        assert_eq!(rows.len(), 2);
        assert!(rows[0].active && rows[0].name == "kojo/kojo");
        assert_eq!(rows[0].settings, "1m ctx");
        assert_eq!(rows[1].settings, "", "a profile at its defaults says nothing");
    }

    #[test]
    fn filtering_matches_a_subsequence_and_ranks_tight_runs_first() {
        let cards = vec![
            card("anthropic/claude-sonnet-4.5"),
            card("openai/gpt-5"),
            card("meta/llama-3-70b-chat-optimised-nightly"),
        ];
        let matched = filter(&cards, "sonnet");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].id, "anthropic/claude-sonnet-4.5");
        // A subsequence typed from memory still lands.
        assert_eq!(filter(&cards, "clsonnet").len(), 1);
        assert!(filter(&cards, "zzz").is_empty());
        assert_eq!(filter(&cards, "  ").len(), 3, "an empty query filters nothing");
        // `gpt` is a tight run in one id and scattered across the whole of the
        // other; the tight one ranks first.
        let scattered = vec![card("google/pathways-turbo"), card("openai/gpt-5")];
        let matched = filter(&scattered, "gpt");
        assert_eq!(matched.len(), 2);
        assert_eq!(matched[0].id, "openai/gpt-5");
    }

    #[test]
    fn the_window_follows_the_cursor_in_both_directions() {
        let mut hub = ModelHub::new(&settings());
        hub.move_selection(20, 100, 10);
        assert_eq!(hub.selected, 20);
        assert_eq!(hub.window, 11, "the cursor lands on the last visible row");
        hub.move_selection(-20, 100, 10);
        assert_eq!(hub.selected, 0);
        assert_eq!(hub.window, 0);
        // A filter that shortens the list pulls the cursor and window back in
        // rather than leaving them pointing past the end.
        hub.move_selection(90, 100, 10);
        hub.scroll_into_view(3, 10);
        assert_eq!(hub.selected, 2);
        assert_eq!(hub.window, 0);
    }
}
