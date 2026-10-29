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
use crate::i18n::{self, Key};
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
            (Ready::Running, _) => console::ok(&format!("{} {}", marks.pass, i18n::t(Key::SetupPresetRunning))),
            (Ready::NotRunning, _) => console::dim(&format!("  {}", i18n::t(Key::SetupPresetNotRunning))),
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
    println!("    {}  {}", console::accent(&format!("{:>2}", PRESETS.len() + 1)), i18n::t(Key::SetupCustomEndpoint),);
}

pub async fn run(paths: &AbacusPaths) -> Result<()> {
    if !io::stdin().is_terminal() {
        bail!(i18n::t(Key::SetupNeedsTerminal).replace("{}", &paths.config_file.display().to_string()));
    }
    let mut settings = Settings::load(paths)?;
    // Before the first line is printed: the wizard is the whole interface for a
    // new user, so it should be in their language from the banner on.
    i18n::init(settings.ui.language.as_deref());
    let mut credentials = Credentials::load(paths)?;
    console::banner(i18n::t(Key::SetupBannerSubtitle));
    if settings.is_configured() {
        console::blank();
        console::field(i18n::t(Key::SetupFieldProfile), &settings.default_profile);
        if let Some(profile) = settings.active_profile() {
            console::field(i18n::t(Key::DoctorFieldModel), &profile.model);
        }
        console::blank();
        if !console::confirm(i18n::t(Key::SetupAlreadyConfigured), false)? {
            println!("  {}", console::dim(i18n::t(Key::SetupNothingChanged)));
            return Ok(());
        }
    }

    // ---- 1/2 provider ------------------------------------------------------
    console::step(1, 2, i18n::t(Key::SetupStepProvider));
    console::note(i18n::t(Key::SetupKeysStayLocal));
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
        let answer = console::prompt(i18n::t(Key::SetupPromptProvider), Some(&(suggested + 1).to_string()))?;
        match answer.trim().parse::<usize>() {
            Ok(number) if (1..=PRESETS.len() + 1).contains(&number) => break number - 1,
            _ => println!(
                "      {}",
                console::err(&i18n::t(Key::SetupPickProviderNumber).replace("{}", &(PRESETS.len() + 1).to_string()))
            ),
        }
    };
    let (id, provider) = match PRESETS.get(choice) {
        Some(preset) => from_preset(&settings, preset),
        None => custom_provider(&settings)?,
    };

    let mut stored_key = false;
    if let KeyStatus::Missing(env) = provider.key_status(&id, &credentials) {
        console::blank();
        let credentials_file = paths.credentials_file.display().to_string();
        if let Some(env) = &env {
            console::note(&i18n::t(Key::SetupEnvNotSet).replacen("{}", env, 1).replacen("{}", &credentials_file, 1));
        } else {
            console::note(&i18n::t(Key::SetupKeyStoredIn).replace("{}", &credentials_file));
        }
        let pasted = rpassword::prompt_password(format!(
            "  {} {} ",
            console::accent(console::marks().arrow),
            i18n::t(Key::SetupPasteKey)
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
            console::dim(&i18n::t(Key::SetupUsingEnvKey).replace("{}", &name))
        );
    }

    // ---- 2/2 model -----------------------------------------------------------
    console::step(2, 2, i18n::t(Key::SetupStepModel));
    print!("       {} ", console::dim(i18n::t(Key::SetupAskingProvider)));
    io::stdout().flush()?;
    let connection = Connection::resolve(&id, &provider, &credentials);
    let model = match providers::refresh_catalog(paths, &connection).await {
        Ok(catalog) if !catalog.models.is_empty() => {
            println!(
                "{}",
                console::ok(&i18n::t(Key::SetupModelsFound).replace("{}", &catalog.models.len().to_string()))
            );
            let ids: Vec<String> = catalog.models.into_iter().map(|card| card.id).collect();
            choose_model(&ids)?
        }
        Ok(_) => {
            println!("{}", console::warn(i18n::t(Key::SetupModelsNone)));
            console::note(i18n::t(Key::SetupModelsNoneHint));
            prompt_model()?
        }
        Err(error) => {
            println!("{}", console::err(i18n::t(Key::SetupUnreachable)));
            console::note(&crate::text::clip(&crate::text::flat(&format!("{error:#}")), 200, ""));
            console::note(i18n::t(Key::SetupUnreachableHint));
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
    println!("  {} {}", console::ok(console::marks().pass), console::bold(i18n::t(Key::SetupReady)));
    console::blank();
    console::field(i18n::t(Key::SetupFieldProfile), &profile);
    console::field(i18n::t(Key::SetupFieldEndpoint), &provider.base_url);
    console::field(i18n::t(Key::SetupFieldAuth), &provider.key_status(&id, &credentials).label());
    console::field(i18n::t(Key::SetupFieldSettings), &paths.config_file.display().to_string());
    console::blank();
    println!("  {}  {}", console::dim(i18n::t(Key::SetupFieldStart)), console::accent(i18n::t(Key::SetupStartCommand)));
    println!("  {}  {}", console::dim(i18n::t(Key::SetupFieldCheck)), console::accent(i18n::t(Key::SetupCheckCommand)));
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
    let name = console::prompt(i18n::t(Key::SetupPromptName), Some("Custom"))?;
    let base_url = loop {
        let raw = console::prompt(i18n::t(Key::SetupPromptBaseUrl), Some("http://localhost:8000/v1"))?;
        match providers::normalise_base_url(&raw) {
            Ok(url) => break url,
            Err(error) => println!("      {}", console::err(&format!("{error}"))),
        }
    };
    let protocol = match console::prompt(i18n::t(Key::SetupPromptProtocol), Some("1"))?.trim() {
        "2" | "responses" => ProviderProtocol::Responses,
        "3" | "anthropic" => ProviderProtocol::Anthropic,
        _ => ProviderProtocol::ChatCompletions,
    };
    let env = console::prompt(i18n::t(Key::SetupPromptEnvVar), None)?;
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
            println!("      {}", console::warn(i18n::t(Key::SetupFilterNoMatch)));
        }
        for (index, model) in matched.iter().take(PAGE).enumerate() {
            println!("    {}  {model}", console::accent(&format!("{:>2}", index + 1)));
        }
        let hidden = matched.len().saturating_sub(PAGE);
        if hidden > 0 {
            println!("        {}", console::dim(&i18n::t(Key::SetupFilterMore).replace("{}", &hidden.to_string())));
        }
        console::blank();
        println!("        {}", console::dim(i18n::t(Key::SetupModelHint)));
        let answer = console::prompt(i18n::t(Key::SetupPromptModel), None)?;
        if answer.is_empty() {
            return prompt_model();
        }
        if let Ok(index) = answer.parse::<usize>() {
            if index >= 1 && index <= matched.len().min(PAGE) {
                return Ok(matched[index - 1].clone());
            }
            println!("      {}", console::err(i18n::t(Key::SetupModelNotListed)));
            continue;
        }
        filter = answer;
    }
}

/// Ask for a model id, asking again on an empty answer. Bailing here would
/// throw away everything already chosen, a harsh penalty for pressing enter.
fn prompt_model() -> Result<String> {
    loop {
        let model = console::prompt(i18n::t(Key::SetupPromptModelId), None)?;
        if !model.trim().is_empty() {
            return Ok(model.trim().to_owned());
        }
        println!("      {}", console::err(i18n::t(Key::SetupModelIdRequired)));
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
