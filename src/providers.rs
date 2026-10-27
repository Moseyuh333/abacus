//! Providers: the API connections Abacus talks to, and the models they serve.
//!
//! A provider is one endpoint with its credentials and everything a request to
//! it needs: the base URL, the wire protocol, where the key comes from and how
//! it is presented, extra headers, body fields to force or strip. They live in
//! `[providers.<id>]` in `~/.abacus/config.toml`, and every model is named by
//! the provider that serves it, `{provider}/{model}` — `openrouter/openai/gpt-6`,
//! `llama/qwen-3-4b`. A profile (see [`crate::config::Profile`]) picks one of
//! those models and the settings to run it with.
//!
//! Providers are only ever read from the user's own config file, never from a
//! workspace: a provider can run a token command and send a bearer token to an
//! arbitrary URL, and a repository must not be able to introduce one.
//!
//! Each provider's model list is discovered from the endpoint and cached under
//! `~/.abacus/models/<provider>.json`, so pickers open instantly and a launch
//! does not have to ask the endpoint what the active model's limits are.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use reqwest::{Client, header};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::{AbacusPaths, Credentials, ProviderProtocol};
use crate::model_info::ModelCard;

/// A provider as configured in `[providers.<id>]`.
///
/// Scalar fields come before the two tables (`headers`, `body`): TOML has to
/// write a table's plain values before its sub-tables.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    /// Display name. The map key is the id that model references use.
    pub name: String,
    /// The API root, `/v1` included where the API has one. Requests go to
    /// `{base_url}/chat/completions`, `/responses` or `/v1/messages` by
    /// protocol, models are listed from `{base_url}/models`.
    pub base_url: String,
    pub protocol: ProviderProtocol,
    /// Environment variable holding the API key. A key pasted into Abacus is
    /// stored in `credentials.toml` under this provider's id instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Header the key is sent in. Unset: `Authorization`, or `x-api-key` for
    /// the Anthropic protocol.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<String>,
    /// How the key is written into that header; `{key}` is replaced. Unset:
    /// `Bearer {key}`, or the bare key for `x-api-key`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_format: Option<String>,
    /// A command whose output is the token, run before every request — for a
    /// token that refreshes or lives in a password manager.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub token_command: Vec<String>,
    /// A file to read the token from on every request, such as an OAuth
    /// `auth.json`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_file: Option<PathBuf>,
    /// Dotted path (`tokens.access_token`) or JSON pointer into `token_file`.
    /// Unset reads the whole file, trimmed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_field: Option<String>,
    /// Body keys removed from every request, for an endpoint that rejects a
    /// field Abacus sends (`parallel_tool_calls`, say).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
    /// Text sent as the first system block (Anthropic) or prepended to the
    /// system message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prefix: Option<String>,
    /// The full request URL, when the endpoint does not live at the usual
    /// path under `base_url`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_url: Option<String>,
    /// Where to list models, when it is not `{base_url}/models`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models_url: Option<String>,
    /// Model ids to offer beside the discovered ones — or instead of them, for
    /// an endpoint that lists nothing.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// Extra headers on every request. `{session}` in a value becomes a
    /// per-session id.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Fields deep-merged over every request body Abacus builds; these win.
    #[serde(skip_serializing_if = "toml::Table::is_empty")]
    pub body: toml::Table,
}

impl ProviderConfig {
    /// The name to show, falling back to the id.
    pub fn display_name<'a>(&'a self, id: &'a str) -> &'a str {
        if self.name.trim().is_empty() { id } else { &self.name }
    }

    /// Whether the endpoint runs on this machine, and so needs no key.
    pub fn is_local(&self) -> bool {
        is_local_url(&self.base_url)
    }

    /// Where this provider's key would come from, as a person reads it —
    /// never the key itself.
    pub fn key_status(&self, id: &str, credentials: &Credentials) -> KeyStatus {
        if !self.token_command.is_empty() {
            return KeyStatus::Command;
        }
        if self.token_file.is_some() {
            return KeyStatus::File;
        }
        if let Some(name) = self.api_key_env.as_deref()
            && std::env::var(name).is_ok_and(|value| !value.trim().is_empty())
        {
            return KeyStatus::Environment(name.to_owned());
        }
        if credentials.keys.get(id).is_some_and(|key| !key.trim().is_empty()) {
            return KeyStatus::Stored;
        }
        if self.is_local() {
            return KeyStatus::NotNeeded;
        }
        KeyStatus::Missing(self.api_key_env.clone())
    }

    /// A provider made from a preset, with nothing else set.
    pub fn from_preset(preset: &Preset) -> Self {
        Self {
            name: preset.name.to_owned(),
            base_url: preset.base_url.to_owned(),
            protocol: preset.protocol,
            api_key_env: preset.env_key.map(str::to_owned),
            ..Self::default()
        }
    }
}

/// Where a provider's credential comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyStatus {
    Environment(String),
    Stored,
    Command,
    File,
    NotNeeded,
    /// No key anywhere; the variable it would be read from, if one is named.
    Missing(Option<String>),
}

impl KeyStatus {
    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Missing(_))
    }

    pub fn label(&self) -> String {
        match self {
            Self::Environment(name) => format!("{name} from the environment"),
            Self::Stored => "stored in credentials.toml".to_owned(),
            Self::Command => "token command".to_owned(),
            Self::File => "token file".to_owned(),
            Self::NotNeeded => "none needed".to_owned(),
            Self::Missing(Some(name)) => format!("missing — paste one, or export {name}"),
            Self::Missing(None) => "missing".to_owned(),
        }
    }
}

/// A ready-made provider for setup and the "add a provider" pickers.
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
    pub env_key: Option<&'static str>,
    pub protocol: ProviderProtocol,
    /// One-line orientation shown beside the name.
    pub hint: &'static str,
}

/// One list, shared by `abacus setup`, the terminal and the desktop app, so
/// the three never offer different providers.
pub const PRESETS: &[Preset] = &[
    Preset {
        id: "kojo",
        name: "Kojo",
        base_url: "https://api.kojo.sh/v1",
        env_key: Some("KOJO_API_KEY"),
        protocol: ProviderProtocol::ChatCompletions,
        hint: "kojo.sh, one endpoint for many models",
    },
    Preset {
        id: "openrouter",
        name: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        env_key: Some("OPENROUTER_API_KEY"),
        protocol: ProviderProtocol::ChatCompletions,
        hint: "one key, most models",
    },
    Preset {
        id: "ollama",
        name: "Ollama",
        base_url: "http://localhost:11434/v1",
        env_key: None,
        protocol: ProviderProtocol::ChatCompletions,
        hint: "local models, no key",
    },
    Preset {
        id: "llama",
        name: "llama.cpp",
        base_url: "http://localhost:8080/v1",
        env_key: None,
        protocol: ProviderProtocol::ChatCompletions,
        hint: "llama-server on this machine",
    },
    Preset {
        id: "lmstudio",
        name: "LM Studio",
        base_url: "http://localhost:1234/v1",
        env_key: None,
        protocol: ProviderProtocol::ChatCompletions,
        hint: "LM Studio's local server",
    },
];

pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

/// Whether a preset's key is already exported, so a list can say so before
/// anyone picks it.
pub fn key_in_env(preset: &Preset) -> bool {
    preset.env_key.and_then(|name| std::env::var(name).ok()).is_some_and(|value| !value.trim().is_empty())
}

/// Whether `url` points at this machine.
pub fn is_local_url(url: &str) -> bool {
    let host = url_host(url);
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "0.0.0.0" | "::1") || host.ends_with(".localhost")
}

/// Whether `url` points at this machine or its local network: a loopback,
/// private or link-local address (Tailscale's 100.64.0.0/10 too), a name with
/// no dot, or one under a suffix only a local network resolves. A model server
/// there is the user's own: it bills nothing for a long prompt and keeps its
/// own cache.
pub fn is_local_network_url(url: &str) -> bool {
    if is_local_url(url) {
        return true;
    }
    let host = url_host(url);
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        let tailscale = matches!(ip, std::net::IpAddr::V4(v4) if v4.octets()[0] == 100 && v4.octets()[1] & 0xc0 == 64);
        return tailscale || crate::web::is_private_ip(ip);
    }
    !host.contains('.')
        || [".local", ".lan", ".home.arpa", ".internal", ".ts.net"].iter().any(|suffix| host.ends_with(suffix))
}

/// The host a URL names, lower-cased, without its port or brackets.
fn url_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    // An IPv6 host is bracketed, and its colons are not a port.
    match authority.strip_prefix('[') {
        Some(bracketed) => bracketed.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    }
    .to_ascii_lowercase()
}

/// Provider ids appear in every model reference (`{id}/{model}`), so they are
/// short and plain: letters, digits, `.`, `_` and `-`, no `/`.
pub fn validate_provider_id(id: &str) -> Result<&str> {
    let id = id.trim();
    if id.is_empty()
        || id.len() > 40
        || !id.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        bail!("a provider id is 1–40 characters: letters, digits, `.`, `_` or `-`");
    }
    Ok(id)
}

/// A lowercase id from a display name: `My Local/API` becomes `my-local-api`.
pub fn slug(value: &str) -> String {
    let value = value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch.to_ascii_lowercase() } else { '-' })
        .collect::<String>();
    let mut out = String::new();
    for ch in value.chars() {
        if ch == '-' && out.ends_with('-') {
            continue;
        }
        out.push(ch);
    }
    out.trim_matches('-').chars().take(40).collect()
}

/// `base`, or `base-2`, `base-3`… — the first that `taken` does not hold.
pub fn unused_id(base: &str, taken: impl Fn(&str) -> bool) -> String {
    std::iter::once(base.to_owned())
        .chain((2..).map(|suffix| format!("{base}-{suffix}")))
        .find(|candidate| !taken(candidate))
        .expect("an unused suffix exists")
}

/// Normalise what someone types into a base URL: add a scheme when it is
/// missing, drop a trailing slash, and reject anything that clearly is not a
/// URL before it becomes a confusing connection error later.
pub fn normalise_base_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        bail!("a base URL is required");
    }
    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_owned()
    } else if is_local_url(trimmed) {
        format!("http://{trimmed}")
    } else {
        format!("https://{trimmed}")
    };
    if with_scheme.contains(char::is_whitespace) {
        bail!("a base URL cannot contain spaces");
    }
    reqwest::Url::parse(&with_scheme).context("that is not a valid URL")?;
    Ok(with_scheme)
}

/// Split a model reference into its provider and the model id the provider
/// knows it by. Only the first `/` separates them: `openrouter/openai/gpt-6`
/// is model `openai/gpt-6` on provider `openrouter`.
pub fn split_ref(reference: &str) -> Option<(&str, &str)> {
    let (provider, model) = reference.trim().split_once('/')?;
    (!provider.is_empty() && !model.trim().is_empty()).then_some((provider, model.trim()))
}

pub fn join_ref(provider: &str, model: &str) -> String {
    format!("{provider}/{model}")
}

/// A provider resolved for use: where requests go and how they authenticate.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Connection {
    /// The provider id, as model references name it.
    pub provider: String,
    pub name: String,
    pub base_url: String,
    pub protocol: ProviderProtocol,
    /// A static key: `--api-key`, the named environment variable, or the
    /// stored one, in that order.
    pub api_key: Option<String>,
    pub auth_header: String,
    pub auth_format: String,
    pub token_command: Vec<String>,
    pub token_file: Option<PathBuf>,
    pub token_field: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: Map<String, Value>,
    pub remove: Vec<String>,
    pub system_prefix: Option<String>,
    pub request_url: Option<String>,
    pub models_url: Option<String>,
    pub extra_models: Vec<String>,
}

impl Connection {
    /// Resolve `[providers.<id>]` against the environment and the stored keys.
    pub fn resolve(id: &str, provider: &ProviderConfig, credentials: &Credentials) -> Self {
        let api_key = provider
            .api_key_env
            .as_deref()
            .and_then(|name| std::env::var(name).ok())
            .filter(|key| !key.trim().is_empty())
            .or_else(|| credentials.keys.get(id).cloned().filter(|key| !key.trim().is_empty()));
        let anthropic = provider.protocol == ProviderProtocol::Anthropic;
        let auth_header = provider
            .auth_header
            .clone()
            .filter(|header| !header.trim().is_empty())
            .unwrap_or_else(|| if anthropic { "x-api-key" } else { "Authorization" }.to_owned());
        let auth_format =
            provider.auth_format.clone().filter(|format| !format.trim().is_empty()).unwrap_or_else(|| {
                if auth_header.eq_ignore_ascii_case("x-api-key") { "{key}" } else { "Bearer {key}" }.to_owned()
            });
        Self {
            provider: id.to_owned(),
            name: provider.display_name(id).to_owned(),
            base_url: provider.base_url.trim().trim_end_matches('/').to_owned(),
            protocol: provider.protocol,
            api_key,
            auth_header,
            auth_format,
            token_command: provider.token_command.clone(),
            token_file: provider.token_file.clone(),
            token_field: provider.token_field.clone(),
            headers: provider.headers.iter().map(|(name, value)| (name.clone(), value.clone())).collect(),
            body: table_to_json(&provider.body),
            remove: provider.remove.clone(),
            system_prefix: provider.system_prefix.clone().filter(|prefix| !prefix.is_empty()),
            request_url: provider.request_url.clone().filter(|url| !url.trim().is_empty()),
            models_url: provider.models_url.clone().filter(|url| !url.trim().is_empty()),
            extra_models: provider.models.clone(),
        }
    }

    /// A connection given entirely on the command line (`--base-url`).
    pub fn inline(base_url: &str, protocol: ProviderProtocol, api_key: Option<String>) -> Self {
        let provider = ProviderConfig {
            name: "command line".to_owned(),
            base_url: base_url.to_owned(),
            protocol,
            ..ProviderConfig::default()
        };
        Self { api_key, ..Self::resolve("cli", &provider, &Credentials::default()) }
    }

    /// The URL completions are posted to.
    pub fn request_url(&self) -> String {
        if let Some(url) = &self.request_url {
            return url.clone();
        }
        match self.protocol {
            ProviderProtocol::ChatCompletions => format!("{}/chat/completions", self.base_url),
            ProviderProtocol::Responses => format!("{}/responses", self.base_url),
            ProviderProtocol::Anthropic => format!("{}/v1/messages", self.base_url),
        }
    }

    /// The URL models are listed from.
    pub fn models_url(&self) -> String {
        if let Some(url) = &self.models_url {
            return url.clone();
        }
        match self.protocol {
            // The Messages API pages its model list, twenty at a time by
            // default; one page of a thousand is every model there is.
            ProviderProtocol::Anthropic => format!("{}/v1/models?limit=1000", self.base_url),
            _ => format!("{}/models", self.base_url),
        }
    }

    /// Whether `other` sends to the same endpoint, so what one learns about
    /// what the endpoint accepts holds for the other.
    pub fn same_endpoint(&self, other: &Connection) -> bool {
        self.provider == other.provider && self.request_url() == other.request_url() && self.protocol == other.protocol
    }

    pub fn is_local(&self) -> bool {
        is_local_url(&self.base_url)
    }

    /// Whether a credential of any kind is configured.
    pub fn has_credential(&self) -> bool {
        self.api_key.is_some() || !self.token_command.is_empty() || self.token_file.is_some()
    }

    /// The auth header for the next request, reading a token command or file
    /// fresh — tokens that refresh do so on disk — and the static key
    /// otherwise. `None` when nothing is configured.
    pub fn auth(&self) -> Result<Option<(String, String)>> {
        let token = if !self.token_command.is_empty() {
            Some(token_from_command(&self.token_command).with_context(|| format!("{} token command", self.name))?)
        } else if let Some(file) = &self.token_file {
            Some(token_from_file(&expand_home(file), self.token_field.as_deref())?)
        } else {
            self.api_key.clone()
        };
        let Some(token) = token.map(|token| token.trim().to_owned()).filter(|token| !token.is_empty()) else {
            return Ok(None);
        };
        let value = self.auth_format.replace("{key}", &token).replace("{token}", &token);
        Ok(Some((self.auth_header.clone(), value)))
    }

    /// The extra headers with `{session}` (and the older `{uuid}`) replaced.
    pub fn resolved_headers(&self, session: &str) -> Vec<(String, String)> {
        self.headers
            .iter()
            .map(|(name, value)| (name.clone(), value.replace("{session}", session).replace("{uuid}", session)))
            .collect()
    }

    /// Whether this provider sets `name` itself, so a default Abacus would add
    /// is left out rather than sent twice.
    pub fn sets_header(&self, name: &str) -> bool {
        self.headers.iter().any(|(header, _)| header.eq_ignore_ascii_case(name))
    }

    /// Deep-merge the configured body over `body`, then drop removed keys.
    pub fn apply_to_body(&self, body: &mut Value) {
        let Some(object) = body.as_object_mut() else {
            return;
        };
        for (key, value) in &self.body {
            merge_value(object.entry(key.clone()).or_insert(Value::Null), value);
        }
        for key in &self.remove {
            object.remove(key);
        }
    }

    /// GET `url` from this provider, authenticated the way a request is.
    pub async fn get_json(&self, url: &str, timeout: Duration) -> Result<Value> {
        let client = Client::builder()
            .timeout(timeout)
            .user_agent(concat!("abacus-agent/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let mut request = client.get(url).header(header::ACCEPT, "application/json");
        if self.protocol == ProviderProtocol::Anthropic && !self.sets_header("anthropic-version") {
            request = request.header("anthropic-version", ANTHROPIC_VERSION);
        }
        for (name, value) in self.resolved_headers("models") {
            request = request.header(name.as_str(), value);
        }
        if let Some((name, value)) = self.auth()? {
            request = request.header(name.as_str(), value);
        }
        let response = request.send().await.with_context(|| format!("could not reach {}", self.name))?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            bail!("{} returned {status}: {}", self.name, crate::text::clip(&crate::text::flat(&detail), 240, ""));
        }
        response.json().await.with_context(|| format!("{} returned invalid JSON", self.name))
    }

    /// Ask the endpoint what it serves, with the configured extra models
    /// added. An endpoint that lists nothing still offers the extras, and so
    /// does one that cannot be listed at all when extras exist.
    pub async fn discover(&self) -> Result<Vec<ModelCard>> {
        let listed = self.get_json(&self.models_url(), DISCOVERY_TIMEOUT).await;
        let mut cards = match listed {
            Ok(value) => crate::model_info::parse_model_cards(&value),
            Err(_) if !self.extra_models.is_empty() => Vec::new(),
            Err(error) => return Err(error),
        };
        for model in &self.extra_models {
            if !cards.iter().any(|card| card.id == *model) {
                cards.push(ModelCard::named(model));
            }
        }
        cards.sort_by_key(|card| card.id.to_ascii_lowercase());
        Ok(cards)
    }
}

/// The Messages API version header, required on every request.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// How long an endpoint gets to list its models.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(12);

/// A provider's model list as last discovered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    pub fetched_at: DateTime<Utc>,
    pub models: Vec<ModelCard>,
}

impl Catalog {
    /// Older than a day: still shown, but worth refreshing in the background.
    pub fn is_stale(&self) -> bool {
        Utc::now().signed_duration_since(self.fetched_at) > chrono::Duration::hours(24)
    }

    pub fn card(&self, model: &str) -> Option<&ModelCard> {
        self.models.iter().find(|card| card.id == model)
    }
}

fn catalog_path(paths: &AbacusPaths, provider: &str) -> PathBuf {
    paths.models_dir.join(format!("{provider}.json"))
}

/// The cached model list for `provider`, if one was ever discovered.
pub fn cached_catalog(paths: &AbacusPaths, provider: &str) -> Option<Catalog> {
    let content = std::fs::read_to_string(catalog_path(paths, provider)).ok()?;
    serde_json::from_str(&content).ok()
}

/// Discover `connection`'s models and cache them.
pub async fn refresh_catalog(paths: &AbacusPaths, connection: &Connection) -> Result<Catalog> {
    let catalog = Catalog { fetched_at: Utc::now(), models: connection.discover().await? };
    crate::config::write_json(&catalog_path(paths, &connection.provider), &catalog);
    Ok(catalog)
}

/// The cached list when it is fresh, a newly discovered one otherwise — and the
/// stale cache when discovery fails, since an old list beats none.
pub async fn catalog(paths: &AbacusPaths, connection: &Connection, refresh: bool) -> Result<Catalog> {
    let cached = cached_catalog(paths, &connection.provider);
    if !refresh && let Some(cached) = cached.as_ref().filter(|catalog| !catalog.is_stale()) {
        return Ok(cached.clone());
    }
    match refresh_catalog(paths, connection).await {
        Ok(catalog) => Ok(catalog),
        Err(error) => cached.ok_or(error),
    }
}

/// Forget a removed provider's cached list.
pub fn drop_catalog(paths: &AbacusPaths, provider: &str) {
    let _ = std::fs::remove_file(catalog_path(paths, provider));
}

/// One upstream able to serve a model, as OpenRouter reports it.
pub struct Upstream {
    /// The name `provider.order` takes.
    pub name: String,
    /// The more specific endpoint tag, which `order` also accepts.
    pub tag: String,
    pub context_length: u64,
    pub quantization: String,
}

/// Ask OpenRouter which upstreams serve `model`. They differ in what matters —
/// one offers 1M context at fp8, another 96k at fp4 — so this makes the choice
/// visible rather than something accepted by default.
pub async fn discover_upstreams(connection: &Connection, model: &str) -> Result<Vec<Upstream>> {
    if !connection.base_url.contains("openrouter.ai") {
        bail!("upstream routing is an OpenRouter feature; {} is not OpenRouter", connection.name);
    }
    let url = format!("{}/models/{}/endpoints", connection.base_url, model.trim_matches('/'));
    let value = connection.get_json(&url, DISCOVERY_TIMEOUT).await?;
    Ok(value["data"]["endpoints"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|endpoint| {
            Some(Upstream {
                name: endpoint["provider_name"].as_str()?.to_owned(),
                tag: endpoint["tag"].as_str().unwrap_or_default().to_owned(),
                context_length: endpoint["context_length"].as_u64().unwrap_or(0),
                quantization: endpoint["quantization"].as_str().unwrap_or("unknown").to_owned(),
            })
        })
        .collect())
}

/// Whether a local server answers at `base_url` — setup offers what it finds
/// running. Short, because a closed port answers at once.
pub async fn local_server_answers(base_url: &str) -> bool {
    let Ok(client) = Client::builder().timeout(Duration::from_millis(600)).build() else {
        return false;
    };
    client.get(format!("{base_url}/models")).send().await.is_ok_and(|response| response.status().is_success())
}

/// `headers` as one editable line: `Name: value; Other: value`.
pub fn headers_line(headers: &BTreeMap<String, String>) -> String {
    headers.iter().map(|(name, value)| format!("{name}: {value}")).collect::<Vec<_>>().join("; ")
}

/// Parse [`headers_line`]'s format back.
pub fn parse_headers_line(line: &str) -> Result<BTreeMap<String, String>> {
    let mut headers = BTreeMap::new();
    for pair in line.split(';').map(str::trim).filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once(':').with_context(|| format!("`{pair}` is not `Name: value`"))?;
        let name = name.trim();
        if name.is_empty() || !name.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_')) {
            bail!("`{name}` is not a valid header name");
        }
        headers.insert(name.to_owned(), value.trim().to_owned());
    }
    Ok(headers)
}

/// `body` as one editable line of JSON.
pub fn body_line(body: &toml::Table) -> String {
    if body.is_empty() { String::new() } else { Value::Object(table_to_json(body)).to_string() }
}

/// Parse a JSON object typed into the body field.
pub fn parse_body_line(line: &str) -> Result<toml::Table> {
    if line.trim().is_empty() {
        return Ok(toml::Table::new());
    }
    let value: Value = serde_json::from_str(line).context("body overrides must be a JSON object")?;
    if !value.is_object() {
        bail!("body overrides must be a JSON object, like {{\"store\": false}}");
    }
    json_to_table(&value)
}

/// A token command typed as one line, split on whitespace with quotes kept
/// together: `pass show "api keys/x"`.
pub fn parse_command_line(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for ch in line.chars() {
        match (quote, ch) {
            (Some(open), ch) if ch == open => quote = None,
            (Some(_), ch) => current.push(ch),
            (None, '"' | '\'') => {
                quote = Some(ch);
                started = true;
            }
            (None, ch) if ch.is_whitespace() => {
                if started || !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, ch) => current.push(ch),
        }
    }
    if started || !current.is_empty() {
        words.push(current);
    }
    words
}

/// [`parse_command_line`]'s inverse, quoting words that need it.
pub fn command_line(words: &[String]) -> String {
    words
        .iter()
        .map(|word| {
            if word.is_empty() || word.chars().any(|ch| ch.is_whitespace() || ch == '"') {
                format!("'{word}'")
            } else {
                word.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn table_to_json(table: &toml::Table) -> Map<String, Value> {
    match serde_json::to_value(table) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

fn json_to_table(value: &Value) -> Result<toml::Table> {
    match toml::Value::try_from(value).context("those body values cannot be stored (TOML has no null)")? {
        toml::Value::Table(table) => Ok(table),
        _ => bail!("body overrides must be a JSON object"),
    }
}

/// Deep-merge `overlay` into `base`, overlay winning at the leaves.
fn merge_value(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Object(base), Value::Object(overlay)) => {
            for (key, value) in overlay {
                merge_value(base.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
        (base, overlay) => *base = overlay.clone(),
    }
}

fn token_from_file(path: &Path, field: Option<&str>) -> Result<String> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("could not read token file {}", path.display()))?;
    let Some(field) = field.filter(|field| !field.trim().is_empty()) else {
        return Ok(content.trim().to_owned());
    };
    let value: Value =
        serde_json::from_str(&content).with_context(|| format!("token file {} is not JSON", path.display()))?;
    let found = if let Some(pointer) = field.strip_prefix('/') {
        value.pointer(&format!("/{pointer}"))
    } else {
        let mut current = &value;
        for key in field.split('.') {
            current = current.get(key).unwrap_or(&Value::Null);
        }
        Some(current)
    };
    found
        .and_then(Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("token file {} has no string at `{field}`", path.display()))
}

fn token_from_command(command: &[String]) -> Result<String> {
    let (program, args) = command.split_first().context("the token command is empty")?;
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("could not run `{program}`"))?;
    if !output.status.success() {
        bail!("`{program}` failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Expand a leading `~` to the home directory; other paths pass through.
pub fn expand_home(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    {
        return PathBuf::from(home).join(rest);
    }
    path.to_path_buf()
}

/// What an old `~/.abacus/endpoints/<name>.yaml` file said, for the one-time
/// move into `[providers]`.
#[derive(Debug, Clone, Deserialize)]
struct LegacyEndpoint {
    name: Option<String>,
    url: String,
    #[serde(default)]
    protocol: ProviderProtocol,
    model: Option<String>,
    models_url: Option<String>,
    auth: Option<LegacyAuth>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    system_prefix: Option<String>,
    #[serde(default)]
    body: Map<String, Value>,
    #[serde(default)]
    remove: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct LegacyAuth {
    header: Option<String>,
    format: Option<String>,
    token: Option<String>,
    env: Option<String>,
    file: Option<PathBuf>,
    file_field: Option<String>,
    command: Option<Vec<String>>,
}

/// A provider converted from an old scripted endpoint file: the provider, the
/// model the file named, and a literal token it carried, which belongs in
/// `credentials.toml` rather than in the settings.
pub struct ImportedEndpoint {
    pub provider: ProviderConfig,
    pub model: Option<String>,
    pub token: Option<String>,
}

/// Convert `~/.abacus/endpoints/<reference>.yaml` (or a path) into a provider.
pub fn import_endpoint(reference: &str, endpoints_dir: &Path) -> Result<ImportedEndpoint> {
    let candidate = if reference.contains('/') || reference.ends_with(".yaml") || reference.ends_with(".yml") {
        expand_home(Path::new(reference))
    } else {
        endpoints_dir.join(format!("{reference}.yaml"))
    };
    let content = std::fs::read_to_string(&candidate)
        .with_context(|| format!("could not read endpoint `{reference}` at {}", candidate.display()))?;
    let legacy: LegacyEndpoint =
        serde_yaml::from_str(&content).with_context(|| format!("invalid endpoint file {}", candidate.display()))?;
    let url = legacy.url.trim().trim_end_matches('/').to_owned();
    if url.is_empty() {
        bail!("endpoint file {} has no `url`", candidate.display());
    }
    let suffix = match legacy.protocol {
        ProviderProtocol::ChatCompletions => "/chat/completions",
        ProviderProtocol::Responses => "/responses",
        ProviderProtocol::Anthropic => "/v1/messages",
    };
    let (base_url, request_url) = match url.strip_suffix(suffix) {
        Some(base) => (base.to_owned(), None),
        None => (url.clone(), Some(url.clone())),
    };
    let auth = legacy.auth;
    let field = |pick: fn(&LegacyAuth) -> Option<String>| auth.as_ref().and_then(pick);
    let provider = ProviderConfig {
        name: legacy
            .name
            .unwrap_or_else(|| candidate.file_stem().and_then(|stem| stem.to_str()).unwrap_or(reference).to_owned()),
        base_url,
        protocol: legacy.protocol,
        api_key_env: field(|auth| auth.env.clone()),
        // The old files always spelled the header out; Anthropic OAuth sends a
        // bearer, so the protocol default must not take over from it.
        auth_header: Some(field(|auth| auth.header.clone()).unwrap_or_else(|| "Authorization".to_owned())),
        auth_format: Some(field(|auth| auth.format.clone()).unwrap_or_else(|| "Bearer {key}".to_owned())),
        token_command: auth.as_ref().and_then(|auth| auth.command.clone()).unwrap_or_default(),
        token_file: auth.as_ref().and_then(|auth| auth.file.clone()),
        token_field: field(|auth| auth.file_field.clone()),
        remove: legacy.remove,
        system_prefix: legacy.system_prefix,
        request_url,
        models_url: legacy.models_url,
        models: legacy.model.iter().cloned().collect(),
        headers: legacy.headers,
        body: json_to_table(&Value::Object(legacy.body)).unwrap_or_default(),
    };
    Ok(ImportedEndpoint { provider, model: legacy.model, token: field(|auth| auth.token.clone()) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn references_split_at_the_first_slash() {
        assert_eq!(split_ref("openrouter/openai/gpt-6-astra"), Some(("openrouter", "openai/gpt-6-astra")));
        assert_eq!(split_ref("llama/qwen-3-4b"), Some(("llama", "qwen-3-4b")));
        assert_eq!(split_ref("bare-model"), None);
        assert_eq!(split_ref("/model"), None);
        assert_eq!(split_ref("provider/"), None);
    }

    #[test]
    fn base_urls_gain_a_scheme_and_lose_a_trailing_slash() {
        assert_eq!(normalise_base_url("api.example.com/v1/").unwrap(), "https://api.example.com/v1");
        // A local address defaults to http: https on localhost is the rarer
        // case and fails with a confusing TLS error.
        assert_eq!(normalise_base_url("localhost:11434/v1").unwrap(), "http://localhost:11434/v1");
        assert_eq!(normalise_base_url("http://127.0.0.1:8000/v1").unwrap(), "http://127.0.0.1:8000/v1");
        assert!(normalise_base_url("  ").is_err());
        assert!(normalise_base_url("not a url").is_err());
    }

    #[test]
    fn every_preset_is_well_formed() {
        let mut ids = std::collections::HashSet::new();
        for preset in PRESETS {
            assert!(ids.insert(preset.id), "duplicate preset id {}", preset.id);
            assert!(validate_provider_id(preset.id).is_ok(), "{} is not a valid id", preset.id);
            assert_eq!(
                normalise_base_url(preset.base_url).unwrap(),
                preset.base_url,
                "{} is not normalised",
                preset.id
            );
            // A hosted endpoint needs a key; a local one must not claim to.
            assert_eq!(preset.env_key.is_none(), is_local_url(preset.base_url), "{} disagrees about a key", preset.id);
        }
    }

    #[test]
    fn local_urls_are_recognised() {
        for url in
            ["http://localhost:11434/v1", "http://127.0.0.1:8080", "http://[::1]:8000/v1", "http://app.localhost"]
        {
            assert!(is_local_url(url), "{url}");
        }
        for url in ["https://openrouter.ai/api/v1", "https://localhost.example.com/v1"] {
            assert!(!is_local_url(url), "{url}");
        }
    }

    #[test]
    fn a_server_on_the_local_network_counts_as_local() {
        for url in [
            "http://localhost:11434/v1",
            "http://192.168.1.20:8080/v1",
            "http://10.0.0.5/v1",
            "http://172.20.1.1:1234/v1",
            "http://100.101.102.103:11434/v1",
            "http://[fd12::1]:8080/v1",
            "http://nas:11434/v1",
            "http://gpu-box.local:8080/v1",
            "http://ollama.lan/v1",
            "http://llm.home.arpa/v1",
            "http://host.docker.internal:11434/v1",
            "http://gpu.tail1234.ts.net:8080/v1",
        ] {
            assert!(is_local_network_url(url), "{url}");
        }
        for url in
            ["https://api.kojo.sh/v1", "https://openrouter.ai/api/v1", "http://8.8.8.8/v1", "http://100.200.1.1/v1"]
        {
            assert!(!is_local_network_url(url), "{url}");
        }
    }

    #[test]
    fn slugs_and_unused_ids() {
        assert_eq!(slug("My Local/API"), "my-local-api");
        assert_eq!(slug("  OpenRouter!! "), "openrouter");
        assert_eq!(unused_id("openrouter", |id| id == "openrouter"), "openrouter-2");
        assert_eq!(unused_id("local", |_| false), "local");
    }

    #[test]
    fn the_key_header_follows_the_protocol_unless_set() {
        let mut credentials = Credentials::default();
        credentials.keys.insert("anthropic".into(), "sk-ant".into());
        let anthropic = ProviderConfig {
            base_url: "https://api.anthropic.com".into(),
            protocol: ProviderProtocol::Anthropic,
            ..Default::default()
        };
        let connection = Connection::resolve("anthropic", &anthropic, &credentials);
        assert_eq!(connection.auth().unwrap(), Some(("x-api-key".to_owned(), "sk-ant".to_owned())));
        assert_eq!(connection.request_url(), "https://api.anthropic.com/v1/messages");
        assert_eq!(connection.models_url(), "https://api.anthropic.com/v1/models?limit=1000");

        credentials.keys.insert("router".into(), "sk-or".into());
        let router = ProviderConfig { base_url: "https://openrouter.ai/api/v1/".into(), ..Default::default() };
        let connection = Connection::resolve("router", &router, &credentials);
        assert_eq!(connection.auth().unwrap(), Some(("Authorization".to_owned(), "Bearer sk-or".to_owned())));
        assert_eq!(connection.request_url(), "https://openrouter.ai/api/v1/chat/completions");

        // A custom header and format, the way a gateway wants them.
        let gateway = ProviderConfig {
            base_url: "https://gateway.example/v1".into(),
            auth_header: Some("X-Api-Key".into()),
            auth_format: Some("{key}".into()),
            ..Default::default()
        };
        let mut credentials = Credentials::default();
        credentials.keys.insert("gateway".into(), "abc".into());
        let connection = Connection::resolve("gateway", &gateway, &credentials);
        assert_eq!(connection.auth().unwrap(), Some(("X-Api-Key".to_owned(), "abc".to_owned())));
    }

    #[test]
    fn a_token_file_is_read_fresh_by_field() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("auth.json");
        std::fs::write(&file, json!({"tokens": {"access_token": "first"}}).to_string()).unwrap();
        let provider = ProviderConfig {
            base_url: "https://example.test/v1".into(),
            token_file: Some(file.clone()),
            token_field: Some("tokens.access_token".into()),
            ..Default::default()
        };
        let connection = Connection::resolve("oauth", &provider, &Credentials::default());
        assert_eq!(connection.auth().unwrap().unwrap().1, "Bearer first");
        std::fs::write(&file, json!({"tokens": {"access_token": "refreshed"}}).to_string()).unwrap();
        assert_eq!(connection.auth().unwrap().unwrap().1, "Bearer refreshed", "read on every request");
        assert_eq!(token_from_file(&file, Some("/tokens/access_token")).unwrap(), "refreshed");
    }

    #[test]
    fn body_overrides_merge_deep_and_removals_fire_last() {
        let provider = ProviderConfig {
            base_url: "https://chatgpt.com/backend-api/codex".into(),
            protocol: ProviderProtocol::Responses,
            body: parse_body_line(r#"{"store": false, "reasoning": {"effort": "low"}}"#).unwrap(),
            remove: vec!["parallel_tool_calls".into()],
            ..Default::default()
        };
        let connection = Connection::resolve("codex", &provider, &Credentials::default());
        let mut body = json!({
            "model": "m", "stream": true, "store": true, "parallel_tool_calls": true,
            "reasoning": {"effort": "high", "summary": "auto"}
        });
        connection.apply_to_body(&mut body);
        assert_eq!(body["store"], json!(false), "the override wins");
        assert_eq!(body["reasoning"]["effort"], json!("low"), "nested objects merge");
        assert_eq!(body["reasoning"]["summary"], json!("auto"), "unmentioned keys survive");
        assert!(body.get("parallel_tool_calls").is_none());
        assert_eq!(body["stream"], json!(true));
    }

    #[test]
    fn editable_lines_round_trip() {
        let headers = parse_headers_line("X-Title: abacus; HTTP-Referer: https://empero.org").unwrap();
        assert_eq!(headers["HTTP-Referer"], "https://empero.org", "only the first colon splits");
        assert_eq!(parse_headers_line(&headers_line(&headers)).unwrap(), headers);
        assert!(parse_headers_line("no colon here").is_err());
        assert!(parse_headers_line("").unwrap().is_empty());

        let body = parse_body_line(r#"{"store": false, "nested": {"a": 1}}"#).unwrap();
        assert_eq!(parse_body_line(&body_line(&body)).unwrap(), body);
        assert!(parse_body_line("[1, 2]").is_err());
        assert!(parse_body_line(r#"{"a": null}"#).is_err(), "TOML cannot store null");

        let command = parse_command_line(r#"pass show "api keys/x" --clip"#);
        assert_eq!(command, ["pass", "show", "api keys/x", "--clip"]);
        assert_eq!(parse_command_line(&command_line(&command)), command);
    }

    /// The shipped examples are copy-paste starting points, so a typo in one
    /// is a user-facing bug: parse every one of them, from the real directory.
    #[test]
    fn every_shipped_provider_example_parses() {
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/providers");
        let mut seen = 0;
        for entry in std::fs::read_dir(&examples).expect("docs/providers") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("toml") {
                continue;
            }
            #[derive(Deserialize)]
            struct Example {
                providers: BTreeMap<String, ProviderConfig>,
            }
            let example: Example = toml::from_str(&std::fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|error| panic!("{} does not parse: {error}", path.display()));
            for (id, provider) in &example.providers {
                validate_provider_id(id).unwrap();
                assert!(provider.base_url.starts_with("https://"), "{} must use https", path.display());
            }
            seen += 1;
        }
        assert!(seen >= 4, "expected the shipped examples, found {seen}");
    }

    #[test]
    fn an_old_endpoint_file_becomes_a_provider() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("codex.yaml"),
            "name: ChatGPT Codex\nurl: https://chatgpt.com/backend-api/codex/responses\nprotocol: responses\n\
             model: gpt-5.6-luna\nauth:\n  file: ~/.codex/auth.json\n  file_field: tokens.access_token\n\
             body:\n  store: false\nheaders:\n  X-Session: \"{uuid}\"\n",
        )
        .unwrap();
        let imported = import_endpoint("codex", dir.path()).unwrap();
        assert_eq!(imported.provider.base_url, "https://chatgpt.com/backend-api/codex");
        assert_eq!(imported.provider.request_url, None, "the usual path needs no override");
        assert_eq!(imported.provider.token_field.as_deref(), Some("tokens.access_token"));
        assert_eq!(imported.model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(imported.provider.models, ["gpt-5.6-luna"]);
        let connection = Connection::resolve("codex", &imported.provider, &Credentials::default());
        assert_eq!(connection.request_url(), "https://chatgpt.com/backend-api/codex/responses");
        assert_eq!(connection.resolved_headers("abc"), [("X-Session".to_owned(), "abc".to_owned())]);

        std::fs::write(
            dir.path().join("odd.yaml"),
            "url: https://gateway.example/run\nauth:\n  token: literal-secret\n  header: X-Key\n  format: \"{token}\"\n",
        )
        .unwrap();
        let imported = import_endpoint("odd", dir.path()).unwrap();
        assert_eq!(imported.provider.request_url.as_deref(), Some("https://gateway.example/run"));
        assert_eq!(imported.token.as_deref(), Some("literal-secret"), "a literal token goes to credentials");
        let mut credentials = Credentials::default();
        credentials.keys.insert("odd".into(), imported.token.unwrap());
        let connection = Connection::resolve("odd", &imported.provider, &credentials);
        assert_eq!(connection.auth().unwrap(), Some(("X-Key".to_owned(), "literal-secret".to_owned())));
    }
}
