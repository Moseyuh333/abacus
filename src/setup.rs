//! `abacus setup`: connect a provider and pick a model, in two steps.
//!
//! The wizard offers the same presets as the terminal and the desktop app,
//! marks the ones that will just work — a key already exported, a local
//! server already answering — and defaults to the first of those. What it
//! leaves behind is a provider and a profile for the chosen model, which is
//! all a session needs; everything else has a default and lives in `/config`.
//! Run again, it adds another provider rather than replacing the first.

use std::io::{self, IsTerminal, Write};

use anyhow::{Result, bail};

use crate::config::{AbacusPaths, Credentials, ProviderProtocol, Settings};
use crate::console;
use crate::providers::{self, Connection, KeyStatus, PRESETS, Preset, ProviderConfig};

/// What the machine already offers for one preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ready {
    /// Its key is exported.
    KeyFound,
    /// It is a local server, and it is answering.
    Running,
    /// It is a local server that is not running now.
    NotRunning,
    /// It needs a key, and none is exported.
    NeedsKey,
}

/// Look at every preset at once: environment variables, and a quick knock on
/// each local server's port.
async fn survey() -> Vec<Ready> {
    let checks = PRESETS.iter().map(|preset| async move {
        if providers::is_local_url(preset.base_url) {
            if providers::local_server_answers(preset.base_url).await { Ready::Running } else { Ready::NotRunning }
        } else if providers::key_in_env(preset) {
            Ready::KeyFound
        } else {
            Ready::NeedsKey
        }
    });
    futures_util::future::join_all(checks).await
}

fn print_presets(ready: &[Ready]) {
    let marks = console::marks();
    let name_width = PRESETS.iter().map(|preset| preset.name.len()).max().unwrap_or(10);
    let hint_width = PRESETS.iter().map(|preset| preset.hint.len()).max().unwrap_or(20);
    for (index, (preset, ready)) in PRESETS.iter().zip(ready).enumerate() {
        let status = match (ready, preset.env_key) {
            (Ready::KeyFound, Some(name)) => console::ok(&format!("{} {name}", marks.pass)),
            (Ready::Running, _) => console::ok(&format!("{} running", marks.pass)),
            (Ready::NotRunning, _) => console::dim("  not running"),
            (_, Some(name)) => console::dim(&format!("  {name}")),
            (_, None) => String::new(),
        };
        println!(
            "    {}  {}  {}  {}",
            console::accent(&format!("{:>2}", index + 1)),
            console::pad(preset.name, name_width),
            console::dim(&console::pad(preset.hint, hint_width)),
            status,
        );
    }
    println!(
        "    {}  Custom endpoint (OpenAI-compatible, Responses or Anthropic)",
        console::accent(&format!("{:>2}", PRESETS.len() + 1)),
    );
}

pub async fn run(paths: &AbacusPaths) -> Result<()> {
    if !io::stdin().is_terminal() {
        bail!("setup needs an interactive terminal; configure {} directly", paths.config_file.display());
    }
    let mut settings = Settings::load(paths)?;
    let mut credentials = Credentials::load(paths)?;
    console::banner("Fast, focused coding from the terminal.");
    if settings.is_configured() {
        console::blank();
        console::field("profile", &settings.default_profile);
        if let Some(profile) = settings.active_profile() {
            console::field("model", &profile.model);
        }
        console::blank();
        if !console::confirm("Abacus is set up already. Connect another provider?", false)? {
            println!("  {}", console::dim("Nothing changed. /config and /profile manage what is there."));
            return Ok(());
        }
    }

    // ---- 1/2 provider ------------------------------------------------------
    console::step(1, 2, "Connect a provider");
    console::note("Keys stay on this machine. Anything else can be changed later in /config → Providers.");
    console::blank();
    let ready = survey().await;
    print_presets(&ready);
    console::blank();
    let suggested = ready
        .iter()
        .position(|ready| *ready == Ready::KeyFound)
        .or_else(|| ready.iter().position(|ready| *ready == Ready::Running))
        .unwrap_or(0);
    let choice = loop {
        let answer = console::prompt("Provider", Some(&(suggested + 1).to_string()))?;
        match answer.trim().parse::<usize>() {
            Ok(number) if (1..=PRESETS.len() + 1).contains(&number) => break number - 1,
            _ => println!("      {}", console::err(&format!("Pick a number from 1 to {}.", PRESETS.len() + 1))),
        }
    };
    let (id, provider) = match PRESETS.get(choice) {
        Some(preset) => from_preset(&settings, preset),
        None => custom_provider(&settings)?,
    };

    let mut stored_key = false;
    if let KeyStatus::Missing(env) = provider.key_status(&id, &credentials) {
        console::blank();
        if let Some(env) = &env {
            console::note(&format!("{env} is not set. A pasted key is kept in {}.", paths.credentials_file.display()));
        } else {
            console::note(&format!("A pasted key is kept in {}.", paths.credentials_file.display()));
        }
        let pasted = rpassword::prompt_password(format!(
            "  {} Paste an API key (blank to skip): ",
            console::accent(console::marks().arrow)
        ))?;
        if !pasted.trim().is_empty() {
            credentials.keys.insert(id.clone(), pasted.trim().to_owned());
            stored_key = true;
        }
    } else if let KeyStatus::Environment(name) = provider.key_status(&id, &credentials) {
        console::blank();
        println!(
            "  {} {}",
            console::ok(console::marks().pass),
            console::dim(&format!("Using {name} from your environment."))
        );
    }

    // ---- 2/2 model -----------------------------------------------------------
    console::step(2, 2, "Choose a model");
    print!("       {}", console::dim("Asking the provider what it serves… "));
    io::stdout().flush()?;
    let connection = Connection::resolve(&id, &provider, &credentials);
    let model = match providers::refresh_catalog(paths, &connection).await {
        Ok(catalog) if !catalog.models.is_empty() => {
            println!("{}", console::ok(&format!("{} found", catalog.models.len())));
            let ids: Vec<String> = catalog.models.into_iter().map(|card| card.id).collect();
            choose_model(&ids)?
        }
        Ok(_) => {
            println!("{}", console::warn("none listed"));
            console::note("The provider answered but listed no models; type one.");
            prompt_model()?
        }
        Err(error) => {
            println!("{}", console::err("unreachable"));
            console::note(&crate::text::clip(&crate::text::flat(&format!("{error:#}")), 200, ""));
            console::note(
                "Type the model to use; the connection can be fixed later in /config or with `abacus doctor`.",
            );
            prompt_model()?
        }
    };

    settings.providers.insert(id.clone(), provider);
    let reference = providers::join_ref(&id, &model);
    let profile = match settings.profile_for(&reference) {
        Some(existing) => existing.to_owned(),
        None => settings.add_profile(&reference),
    };
    settings.default_profile = profile.clone();
    settings.save(paths)?;
    if stored_key {
        credentials.save(paths)?;
    }

    // ---- summary -------------------------------------------------------------
    let provider = &settings.providers[&id];
    console::blank();
    println!("  {}", console::dim(&console::marks().rule.repeat(console::WIDTH)));
    println!("  {} {}", console::ok(console::marks().pass), console::bold("Ready"));
    console::blank();
    console::field("profile", &profile);
    console::field("endpoint", &provider.base_url);
    console::field("auth", &provider.key_status(&id, &credentials).label());
    console::field("settings", &paths.config_file.display().to_string());
    console::blank();
    println!("  {}  {}", console::dim("start"), console::accent("cd your-project && abacus"));
    println!("  {}  {}", console::dim("check"), console::accent("abacus doctor"));
    console::blank();
    Ok(())
}

/// A preset as a provider: the one already configured for it when there is
/// one, so running setup twice does not make a second copy.
fn from_preset(settings: &Settings, preset: &Preset) -> (String, ProviderConfig) {
    if let Some((id, existing)) = settings
        .providers
        .iter()
        .find(|(_, existing)| existing.base_url == preset.base_url && existing.protocol == preset.protocol)
    {
        return (id.clone(), existing.clone());
    }
    let id = providers::unused_id(preset.id, |candidate| settings.providers.contains_key(candidate));
    (id, ProviderConfig::from_preset(preset))
}

fn custom_provider(settings: &Settings) -> Result<(String, ProviderConfig)> {
    console::blank();
    let name = console::prompt("Name", Some("Custom"))?;
    let base_url = loop {
        let raw = console::prompt("API base URL (with /v1 where it has one)", Some("http://localhost:8000/v1"))?;
        match providers::normalise_base_url(&raw) {
            Ok(url) => break url,
            Err(error) => println!("      {}", console::err(&format!("{error}"))),
        }
    };
    let protocol = match console::prompt("Protocol — 1 chat completions, 2 responses, 3 anthropic", Some("1"))?.trim()
    {
        "2" | "responses" => ProviderProtocol::Responses,
        "3" | "anthropic" => ProviderProtocol::Anthropic,
        _ => ProviderProtocol::ChatCompletions,
    };
    let env = console::prompt("Environment variable with the API key (blank for none)", None)?;
    let base = Some(providers::slug(&name)).filter(|slug| !slug.is_empty()).unwrap_or_else(|| "custom".to_owned());
    let id = providers::unused_id(&base, |candidate| settings.providers.contains_key(candidate));
    let provider = ProviderConfig {
        name: name.trim().to_owned(),
        base_url,
        protocol,
        api_key_env: Some(env.trim().to_owned()).filter(|env| !env.is_empty()),
        ..ProviderConfig::default()
    };
    Ok((id, provider))
}

/// Present the discovered models. A provider can list hundreds, so the list is
/// filterable: typing a fragment narrows it rather than forcing a scroll
/// through everything, and an empty filter shows a bounded first page.
fn choose_model(models: &[String]) -> Result<String> {
    const PAGE: usize = 24;
    let mut filter = String::new();
    loop {
        let matched = models
            .iter()
            .filter(|model| filter.is_empty() || model.to_ascii_lowercase().contains(&filter.to_ascii_lowercase()))
            .collect::<Vec<_>>();
        console::blank();
        if matched.is_empty() {
            println!("      {}", console::warn("Nothing matches that filter."));
        }
        for (index, model) in matched.iter().take(PAGE).enumerate() {
            println!("    {}  {model}", console::accent(&format!("{:>2}", index + 1)));
        }
        let hidden = matched.len().saturating_sub(PAGE);
        if hidden > 0 {
            println!("        {}", console::dim(&format!("+{hidden} more — type a fragment to filter")));
        }
        console::blank();
        println!("        {}", console::dim("number to pick  ·  text to filter  ·  blank to type an id"));
        let answer = console::prompt("Model", None)?;
        if answer.is_empty() {
            return prompt_model();
        }
        if let Ok(index) = answer.parse::<usize>() {
            if index >= 1 && index <= matched.len().min(PAGE) {
                return Ok(matched[index - 1].clone());
            }
            println!("      {}", console::err("That number is not on the list."));
            continue;
        }
        filter = answer;
    }
}

/// Ask for a model id, asking again on an empty answer. Bailing here would
/// throw away everything already chosen, a harsh penalty for pressing enter.
fn prompt_model() -> Result<String> {
    loop {
        let model = console::prompt("Model id", None)?;
        if !model.trim().is_empty() {
            return Ok(model.trim().to_owned());
        }
        println!("      {}", console::err("A model id is required."));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preset_already_configured_is_reused_not_copied() {
        let mut settings = Settings::default();
        let kojo = providers::preset("kojo").unwrap();
        let (id, provider) = from_preset(&settings, kojo);
        assert_eq!(id, "kojo");
        settings.providers.insert(id, provider);
        assert_eq!(from_preset(&settings, kojo).0, "kojo", "the same endpoint is the same provider");

        // A different provider squatting on the id gets a numbered sibling.
        let mut taken = Settings::default();
        taken.providers.insert(
            "kojo".into(),
            ProviderConfig { base_url: "https://elsewhere.example/v1".into(), ..ProviderConfig::default() },
        );
        assert_eq!(from_preset(&taken, kojo).0, "kojo-2");
    }
}
