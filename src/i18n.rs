//! The console surfaces' string catalog.
//!
//! `abacus setup` and `abacus doctor` print to an ordinary terminal rather than
//! through the TUI, and their text is the first thing anyone sees of this
//! program. Keeping it in one place means a locale is a table, not a sweep
//! through `println!` calls, and the tables can be checked against each other
//! instead of against a reviewer's memory.
//!
//! Scope, deliberately: the console commands. The TUI has its own vocabulary
//! (status bar, dialogs, slash-command help) and converting it is a separate
//! piece of work; a catalog that pretended to cover it would be worse than one
//! that says where it stops.
//!
//! Formatting stays at the call site. A key names a string, and a string with a
//! hole in it is filled by the caller's `format!`, so a translation cannot
//! change the number or order of arguments without the placeholder test
//! noticing.

use std::sync::atomic::{AtomicU8, Ordering};

/// A language the console surfaces can speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    En,
    ZhCn,
}

impl Lang {
    /// The tag this language is written as in `ABACUS_LANG` and `ui.language`.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::ZhCn => "zh-CN",
        }
    }

    /// Parse a tag. `zh`, `zh-cn`, `zh-Hans`, `zh_CN.UTF-8` and friends all
    /// land on `ZhCn`; anything unrecognised is not a language this build has.
    pub fn parse(value: &str) -> Option<Self> {
        // An OS locale looks like `zh_CN.UTF-8`, a setting or `ABACUS_LANG`
        // like `zh-CN`. Normalise both to a primary-language tag first.
        let tag = value.trim().split('.').next().unwrap_or_default().replace('_', "-").to_ascii_lowercase();
        if tag.is_empty() {
            return None;
        }
        if tag == "en" || tag.starts_with("en-") {
            return Some(Lang::En);
        }
        if tag == "zh" || tag.starts_with("zh-") {
            return Some(Lang::ZhCn);
        }
        None
    }

    fn table(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Lang::En => en::TABLE,
            Lang::ZhCn => zh_cn::TABLE,
        }
    }
}

/// One user-facing string of the console surfaces.
///
/// The variants are grouped the way the screens are: the setup wizard first,
/// then `doctor`. `ALL` is the list the table test walks, so adding a key
/// without a translation fails the build's tests rather than printing an empty
/// line in someone's terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    // -- setup: framing ------------------------------------------------------
    SetupNeedsTerminal,
    SetupBannerSubtitle,
    SetupAlreadyConfigured,
    SetupNothingChanged,
    SetupStepProvider,
    SetupStepModel,
    SetupKeysStayLocal,
    SetupCustomEndpoint,
    SetupPromptProvider,
    SetupPickProviderNumber,
    SetupEnvNotSet,
    SetupKeyStoredIn,
    SetupPasteKey,
    SetupUsingEnvKey,
    SetupAskingProvider,
    SetupModelsFound,
    SetupModelsNone,
    SetupModelsNoneHint,
    SetupUnreachable,
    SetupUnreachableHint,
    SetupPromptName,
    SetupPromptBaseUrl,
    SetupPromptProtocol,
    SetupPromptEnvVar,
    SetupFilterNoMatch,
    SetupFilterMore,
    SetupModelHint,
    SetupPromptModel,
    SetupModelNotListed,
    SetupPromptModelId,
    SetupModelIdRequired,
    SetupReady,
    SetupFieldProfile,
    SetupFieldEndpoint,
    SetupFieldAuth,
    SetupFieldSettings,
    SetupFieldStart,
    SetupStartCommand,
    SetupFieldCheck,
    SetupCheckCommand,
    SetupPresetRunning,
    SetupPresetNotRunning,

    // -- doctor: framing -----------------------------------------------------
    DoctorBanner,
    DoctorSectionEnvironment,
    DoctorSectionProvider,
    DoctorSectionLocalState,
    DoctorFieldHome,
    DoctorFieldWorkspace,
    DoctorFieldTerminal,
    DoctorTerminalDetail,
    DoctorFieldProfile,
    DoctorFieldModel,
    DoctorFieldProvider,
    DoctorFieldEndpoint,
    DoctorFieldProtocol,
    DoctorFieldMode,
    DoctorCheckCredential,
    DoctorCredentialAvailable,
    DoctorCredentialLocal,
    DoctorCredentialMissing,
    DoctorCheckReachable,
    DoctorReachableFound,
    DoctorReachableMissing,
    DoctorFieldLimits,
    DoctorLimitsDetail,
    DoctorFieldToolCalls,
    DoctorToolCallsNative,
    DoctorToolCallsFallback,
    DoctorCheckSettings,
    DoctorSettingsMissing,
    DoctorCheckProviders,
    DoctorProvidersDetail,
    DoctorCheckSessions,
    DoctorSessionsDetail,
    DoctorCheckTraces,
    DoctorTracesOn,
    DoctorTracesOff,
    DoctorCheckCron,
    DoctorCronNone,
    DoctorCronScheduled,
    DoctorCheckExtensions,
    DoctorExtensionsClean,
    DoctorCheckKeyPerms,
    DoctorKeyPermsInsecure,
    DoctorSummaryPassed,
    DoctorSummaryWarnings,
    DoctorSummaryFailures,

    // -- shared words --------------------------------------------------------
    WordUnset,
    WordNo,
    WordProvider,
    WordProviders,
    WordProfile,
    WordProfiles,
    WordWarning,
    WordWarnings,
    WordSession,
    WordSessions,
    WordProblem,
    WordProblems,
}

impl Key {
    /// Every key, in catalog order. Kept beside the enum so a new variant
    /// cannot be added without a line here and a translation in both tables.
    pub const ALL: &'static [Key] = &[
        Key::SetupNeedsTerminal,
        Key::SetupBannerSubtitle,
        Key::SetupAlreadyConfigured,
        Key::SetupNothingChanged,
        Key::SetupStepProvider,
        Key::SetupStepModel,
        Key::SetupKeysStayLocal,
        Key::SetupCustomEndpoint,
        Key::SetupPromptProvider,
        Key::SetupPickProviderNumber,
        Key::SetupEnvNotSet,
        Key::SetupKeyStoredIn,
        Key::SetupPasteKey,
        Key::SetupUsingEnvKey,
        Key::SetupAskingProvider,
        Key::SetupModelsFound,
        Key::SetupModelsNone,
        Key::SetupModelsNoneHint,
        Key::SetupUnreachable,
        Key::SetupUnreachableHint,
        Key::SetupPromptName,
        Key::SetupPromptBaseUrl,
        Key::SetupPromptProtocol,
        Key::SetupPromptEnvVar,
        Key::SetupFilterNoMatch,
        Key::SetupFilterMore,
        Key::SetupModelHint,
        Key::SetupPromptModel,
        Key::SetupModelNotListed,
        Key::SetupPromptModelId,
        Key::SetupModelIdRequired,
        Key::SetupReady,
        Key::SetupFieldProfile,
        Key::SetupFieldEndpoint,
        Key::SetupFieldAuth,
        Key::SetupFieldSettings,
        Key::SetupFieldStart,
        Key::SetupStartCommand,
        Key::SetupFieldCheck,
        Key::SetupCheckCommand,
        Key::SetupPresetRunning,
        Key::SetupPresetNotRunning,
        Key::DoctorBanner,
        Key::DoctorSectionEnvironment,
        Key::DoctorSectionProvider,
        Key::DoctorSectionLocalState,
        Key::DoctorFieldHome,
        Key::DoctorFieldWorkspace,
        Key::DoctorFieldTerminal,
        Key::DoctorTerminalDetail,
        Key::DoctorFieldProfile,
        Key::DoctorFieldModel,
        Key::DoctorFieldProvider,
        Key::DoctorFieldEndpoint,
        Key::DoctorFieldProtocol,
        Key::DoctorFieldMode,
        Key::DoctorCheckCredential,
        Key::DoctorCredentialAvailable,
        Key::DoctorCredentialLocal,
        Key::DoctorCredentialMissing,
        Key::DoctorCheckReachable,
        Key::DoctorReachableFound,
        Key::DoctorReachableMissing,
        Key::DoctorFieldLimits,
        Key::DoctorLimitsDetail,
        Key::DoctorFieldToolCalls,
        Key::DoctorToolCallsNative,
        Key::DoctorToolCallsFallback,
        Key::DoctorCheckSettings,
        Key::DoctorSettingsMissing,
        Key::DoctorCheckProviders,
        Key::DoctorProvidersDetail,
        Key::DoctorCheckSessions,
        Key::DoctorSessionsDetail,
        Key::DoctorCheckTraces,
        Key::DoctorTracesOn,
        Key::DoctorTracesOff,
        Key::DoctorCheckCron,
        Key::DoctorCronNone,
        Key::DoctorCronScheduled,
        Key::DoctorCheckExtensions,
        Key::DoctorExtensionsClean,
        Key::DoctorCheckKeyPerms,
        Key::DoctorKeyPermsInsecure,
        Key::DoctorSummaryPassed,
        Key::DoctorSummaryWarnings,
        Key::DoctorSummaryFailures,
        Key::WordUnset,
        Key::WordNo,
        Key::WordProvider,
        Key::WordProviders,
        Key::WordProfile,
        Key::WordProfiles,
        Key::WordWarning,
        Key::WordWarnings,
        Key::WordSession,
        Key::WordSessions,
        Key::WordProblem,
        Key::WordProblems,
    ];

    /// The table id for this key. Stable, so a translation file could one day
    /// be checked by name rather than by position.
    pub fn id(self) -> &'static str {
        match self {
            Key::SetupNeedsTerminal => "setup.needs_terminal",
            Key::SetupBannerSubtitle => "setup.banner_subtitle",
            Key::SetupAlreadyConfigured => "setup.already_configured",
            Key::SetupNothingChanged => "setup.nothing_changed",
            Key::SetupStepProvider => "setup.step_provider",
            Key::SetupStepModel => "setup.step_model",
            Key::SetupKeysStayLocal => "setup.keys_stay_local",
            Key::SetupCustomEndpoint => "setup.custom_endpoint",
            Key::SetupPromptProvider => "setup.prompt_provider",
            Key::SetupPickProviderNumber => "setup.pick_provider_number",
            Key::SetupEnvNotSet => "setup.env_not_set",
            Key::SetupKeyStoredIn => "setup.key_stored_in",
            Key::SetupPasteKey => "setup.paste_key",
            Key::SetupUsingEnvKey => "setup.using_env_key",
            Key::SetupAskingProvider => "setup.asking_provider",
            Key::SetupModelsFound => "setup.models_found",
            Key::SetupModelsNone => "setup.models_none",
            Key::SetupModelsNoneHint => "setup.models_none_hint",
            Key::SetupUnreachable => "setup.unreachable",
            Key::SetupUnreachableHint => "setup.unreachable_hint",
            Key::SetupPromptName => "setup.prompt_name",
            Key::SetupPromptBaseUrl => "setup.prompt_base_url",
            Key::SetupPromptProtocol => "setup.prompt_protocol",
            Key::SetupPromptEnvVar => "setup.prompt_env_var",
            Key::SetupFilterNoMatch => "setup.filter_no_match",
            Key::SetupFilterMore => "setup.filter_more",
            Key::SetupModelHint => "setup.model_hint",
            Key::SetupPromptModel => "setup.prompt_model",
            Key::SetupModelNotListed => "setup.model_not_listed",
            Key::SetupPromptModelId => "setup.prompt_model_id",
            Key::SetupModelIdRequired => "setup.model_id_required",
            Key::SetupReady => "setup.ready",
            Key::SetupFieldProfile => "setup.field_profile",
            Key::SetupFieldEndpoint => "setup.field_endpoint",
            Key::SetupFieldAuth => "setup.field_auth",
            Key::SetupFieldSettings => "setup.field_settings",
            Key::SetupFieldStart => "setup.field_start",
            Key::SetupStartCommand => "setup.start_command",
            Key::SetupFieldCheck => "setup.field_check",
            Key::SetupCheckCommand => "setup.check_command",
            Key::SetupPresetRunning => "setup.preset_running",
            Key::SetupPresetNotRunning => "setup.preset_not_running",
            Key::DoctorBanner => "doctor.banner",
            Key::DoctorSectionEnvironment => "doctor.section_environment",
            Key::DoctorSectionProvider => "doctor.section_provider",
            Key::DoctorSectionLocalState => "doctor.section_local_state",
            Key::DoctorFieldHome => "doctor.field_home",
            Key::DoctorFieldWorkspace => "doctor.field_workspace",
            Key::DoctorFieldTerminal => "doctor.field_terminal",
            Key::DoctorTerminalDetail => "doctor.terminal_detail",
            Key::DoctorFieldProfile => "doctor.field_profile",
            Key::DoctorFieldModel => "doctor.field_model",
            Key::DoctorFieldProvider => "doctor.field_provider",
            Key::DoctorFieldEndpoint => "doctor.field_endpoint",
            Key::DoctorFieldProtocol => "doctor.field_protocol",
            Key::DoctorFieldMode => "doctor.field_mode",
            Key::DoctorCheckCredential => "doctor.check_credential",
            Key::DoctorCredentialAvailable => "doctor.credential_available",
            Key::DoctorCredentialLocal => "doctor.credential_local",
            Key::DoctorCredentialMissing => "doctor.credential_missing",
            Key::DoctorCheckReachable => "doctor.check_reachable",
            Key::DoctorReachableFound => "doctor.reachable_found",
            Key::DoctorReachableMissing => "doctor.reachable_missing",
            Key::DoctorFieldLimits => "doctor.field_limits",
            Key::DoctorLimitsDetail => "doctor.limits_detail",
            Key::DoctorFieldToolCalls => "doctor.field_tool_calls",
            Key::DoctorToolCallsNative => "doctor.tool_calls_native",
            Key::DoctorToolCallsFallback => "doctor.tool_calls_fallback",
            Key::DoctorCheckSettings => "doctor.check_settings",
            Key::DoctorSettingsMissing => "doctor.settings_missing",
            Key::DoctorCheckProviders => "doctor.check_providers",
            Key::DoctorProvidersDetail => "doctor.providers_detail",
            Key::DoctorCheckSessions => "doctor.check_sessions",
            Key::DoctorSessionsDetail => "doctor.sessions_detail",
            Key::DoctorCheckTraces => "doctor.check_traces",
            Key::DoctorTracesOn => "doctor.traces_on",
            Key::DoctorTracesOff => "doctor.traces_off",
            Key::DoctorCheckCron => "doctor.check_cron",
            Key::DoctorCronNone => "doctor.cron_none",
            Key::DoctorCronScheduled => "doctor.cron_scheduled",
            Key::DoctorCheckExtensions => "doctor.check_extensions",
            Key::DoctorExtensionsClean => "doctor.extensions_clean",
            Key::DoctorCheckKeyPerms => "doctor.check_key_perms",
            Key::DoctorKeyPermsInsecure => "doctor.key_perms_insecure",
            Key::DoctorSummaryPassed => "doctor.summary_passed",
            Key::DoctorSummaryWarnings => "doctor.summary_warnings",
            Key::DoctorSummaryFailures => "doctor.summary_failures",
            Key::WordUnset => "word.unset",
            Key::WordNo => "word.no",
            Key::WordProvider => "word.provider",
            Key::WordProviders => "word.providers",
            Key::WordProfile => "word.profile",
            Key::WordProfiles => "word.profiles",
            Key::WordWarning => "word.warning",
            Key::WordWarnings => "word.warnings",
            Key::WordSession => "word.session",
            Key::WordSessions => "word.sessions",
            Key::WordProblem => "word.problem",
            Key::WordProblems => "word.problems",
        }
    }
}

/// The active language, as an index into [`Lang`] plus one, so `0` can mean
/// "not chosen yet".
static ACTIVE: AtomicU8 = AtomicU8::new(0);

/// Pick the language for this run.
///
/// Precedence, highest first: `ABACUS_LANG` (one run, no config edit), the
/// `ui.language` setting, the operating system's own answer, English. `auto`
/// is not a language: it means "ask the next source", which is why a machine
/// whose locale is not `zh-*` keeps the English interface it had.
pub fn init(setting: Option<&str>) {
    let lang = resolve(std::env::var("ABACUS_LANG").ok().as_deref(), setting, os_locale().as_deref());
    set_lang(lang);
}

/// The precedence chain itself, as a pure function so it can be tested without
/// touching the environment or the operating system.
pub fn resolve(env: Option<&str>, setting: Option<&str>, os: Option<&str>) -> Lang {
    [env, setting, os]
        .into_iter()
        .flatten()
        .find_map(|value| if value.trim().eq_ignore_ascii_case("auto") { None } else { Lang::parse(value) })
        .unwrap_or(Lang::En)
}

/// Point the catalog at `lang`.
pub fn set_lang(lang: Lang) {
    ACTIVE.store(
        match lang {
            Lang::En => 1,
            Lang::ZhCn => 2,
        },
        Ordering::Relaxed,
    );
}

/// The language in use. Before [`init`] runs this is English, which is what a
/// string printed during startup should be.
pub fn lang() -> Lang {
    match ACTIVE.load(Ordering::Relaxed) {
        2 => Lang::ZhCn,
        _ => Lang::En,
    }
}

/// The operating system's language, if it names one.
fn os_locale() -> Option<String> {
    for variable in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(variable)
            && !value.trim().is_empty()
        {
            return Some(value);
        }
    }
    platform_locale()
}

#[cfg(windows)]
fn platform_locale() -> Option<String> {
    // The user's UI language, as a LANGID: the low ten bits are the primary
    // language, and 0x04 is Chinese in any script or region.
    const LANG_CHINESE: u16 = 0x04;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }
    // SAFETY: the call takes no arguments and returns a value.
    let langid = unsafe { GetUserDefaultUILanguage() };
    (langid & 0x03ff == LANG_CHINESE).then(|| "zh-CN".to_owned())
}

#[cfg(not(windows))]
fn platform_locale() -> Option<String> {
    None
}

/// The string for `key` in the active language.
///
/// A key with no entry in the active table falls back to English rather than to
/// an empty line: a missing translation should read as untranslated, not as a
/// bug in the interface.
pub fn t(key: Key) -> &'static str {
    lookup(lang(), key).or_else(|| lookup(Lang::En, key)).unwrap_or_else(|| key.id())
}

fn lookup(lang: Lang, key: Key) -> Option<&'static str> {
    lang.table().iter().find(|(id, _)| *id == key.id()).map(|(_, text)| *text)
}

/// `3 warnings` in English, `3 个警告` in Chinese.
///
/// The plural suffix is a language rule, so the count is rendered here instead
/// of by appending an `s` at the call site.
pub fn count(n: usize, one: Key, many: Key) -> String {
    format!("{n} {}", t(if n == 1 { one } else { many }))
}

mod en;
mod zh_cn;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Every key needs an entry in every table, or a screen prints an id at
    /// someone. This is the test that makes adding a string a two-table edit.
    #[test]
    fn every_key_exists_in_every_language() {
        for lang in [Lang::En, Lang::ZhCn] {
            let ids: HashSet<&str> = lang.table().iter().map(|(id, _)| *id).collect();
            for &key in Key::ALL {
                assert!(ids.contains(key.id()), "{:?} is missing from {}", key, lang.code());
            }
        }
    }

    /// A translation that drops or adds a `{}` would panic at `format!` time --
    /// in the terminal of the person least able to read the error.
    #[test]
    fn placeholders_match_across_languages() {
        for &key in Key::ALL {
            let english = lookup(Lang::En, key).expect("english is the base table");
            let expected = english.matches("{}").count();
            for lang in [Lang::En, Lang::ZhCn] {
                let text = lookup(lang, key).expect("checked above");
                assert_eq!(
                    text.matches("{}").count(),
                    expected,
                    "{:?} in {} has a different number of holes than the English: {text:?}",
                    key,
                    lang.code()
                );
            }
        }
    }

    /// An empty entry is worse than a missing one: it renders as a blank label
    /// rather than falling back to English.
    #[test]
    fn no_translation_is_empty() {
        for lang in [Lang::En, Lang::ZhCn] {
            for (id, text) in lang.table() {
                assert!(!text.trim().is_empty(), "{id} is empty in {}", lang.code());
            }
        }
    }

    /// The tables must not carry entries no key names: dead weight that a
    /// translator would keep updating for nothing.
    #[test]
    fn no_entry_is_orphaned() {
        let known: HashSet<&str> = Key::ALL.iter().map(|key| key.id()).collect();
        for lang in [Lang::En, Lang::ZhCn] {
            for (id, _) in lang.table() {
                assert!(known.contains(id), "{id} is in {} but names no key", lang.code());
            }
        }
    }

    #[test]
    fn the_precedence_chain_prefers_env_then_setting_then_os() {
        assert_eq!(resolve(Some("zh-CN"), Some("en"), Some("en_US.UTF-8")), Lang::ZhCn);
        assert_eq!(resolve(None, Some("zh"), Some("en_US.UTF-8")), Lang::ZhCn);
        assert_eq!(resolve(None, None, Some("zh_CN.UTF-8")), Lang::ZhCn);
        assert_eq!(resolve(None, None, Some("de_DE.UTF-8")), Lang::En);
        // `auto` is "ask the next source", not a language.
        assert_eq!(resolve(Some("auto"), Some("zh-CN"), None), Lang::ZhCn);
        assert_eq!(resolve(None, Some("auto"), None), Lang::En);
        // A tag this build does not have falls through to the next source.
        assert_eq!(resolve(Some("fr"), Some("zh-CN"), None), Lang::ZhCn);
    }

    #[test]
    fn tags_parse_by_primary_language() {
        assert_eq!(Lang::parse("zh"), Some(Lang::ZhCn));
        assert_eq!(Lang::parse("zh_CN.UTF-8"), Some(Lang::ZhCn), "an OS locale is a tag too");
        assert_eq!(Lang::parse("ZH-hans"), Some(Lang::ZhCn));
        assert_eq!(Lang::parse("en-GB"), Some(Lang::En));
        assert_eq!(Lang::parse("klingon"), None);
        assert_eq!(Lang::parse("   "), None);
    }

    /// A count reads as a phrase in both languages, and English keeps its
    /// plural.
    #[test]
    fn counts_pluralise_per_language() {
        set_lang(Lang::En);
        assert_eq!(count(1, Key::WordWarning, Key::WordWarnings), "1 warning");
        assert_eq!(count(3, Key::WordWarning, Key::WordWarnings), "3 warnings");
        set_lang(Lang::ZhCn);
        assert_eq!(count(3, Key::WordWarning, Key::WordWarnings), "3 个警告");
        set_lang(Lang::En);
    }
}
