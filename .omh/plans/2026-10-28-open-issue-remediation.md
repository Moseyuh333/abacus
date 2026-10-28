# Abacus 0.7.4 -- open-issue remediation plan

Status: **accepted by the user on 2026-10-28**, with one scope extension: i18n for #3
is in scope as a scaffold plus a zh-CN catalog for the console surfaces (`setup`,
`doctor`), not the TUI. Nothing in this document is implementation evidence.
Recorded: 2026-10-28. Repo: `D:\New folder\abacus` (HEAD `edb15ad`, in sync with `origin/main`).
Note: the `omh hermes plan-accept` CLI is not installed on this machine, so acceptance is
recorded here and in the plan todo rather than through that command.

## 1. What was read (observed facts)

| Area | Fact | Source |
| --- | --- | --- |
| Stack | Rust 2024, `rust-version = 1.88`, one binary `abacus`, 24 crates of deps | `Cargo.toml` |
| Size | 70 source files, 56,619 lines under `src/`; 8 integration test files under `tests/` | `wc -l`, `find` |
| Shape | `src/main.rs` (clap CLI) -> `tui/` (ratatui TUI), `setup.rs` (line-mode wizard), `agent.rs`, `provider.rs`, `tools.rs`, `sync/`, `remote/` (phone bridge), `extensions/` (skills, plugins), `mcp.rs`, `cron.rs`, `sft.rs`, `headless.rs` | source tree |
| CI | `fmt --check`, `clippy --all-targets -D warnings`, `test --all-targets` on ubuntu, ubuntu-arm, macos, windows + MSRV 1.88 `cargo check` | `.github/workflows/ci.yml` |
| Open issues | #11 garbled characters in provider setup (zh-CN Windows), #10 numeric keypad dead after restarting in the same terminal, #4 two external ideas for token cost, #3 request for i18n | `gh issue list -R empero-org/abacus` |
| TODOs in code | none real: 5 grep hits, all inside test fixtures or prompt-injection fixtures | `grep -rn TODO\|FIXME\|HACK` |
| Local toolchain | `stable-x86_64-pc-windows-gnu` 1.98.1, no MSVC, no WSL; `gcc` only at `D:/dev/mingw64/bin` (needed by `ring`'s cc build) | `rustup show`, `cargo check` |
| Local verification today | `cargo fmt --all -- --check` clean; `cargo clippy --lib -- -D warnings` clean; `cargo test --lib` 646 passed / 0 failed / 3 ignored (15.6 s) | commands run this session |
| Local verification broken | `cargo clippy --all-targets` and `cargo test --all-targets` fail: rustc **ICE** `no type-dependent def for method call` at `src/main.rs:755` and `:774`, then `crate regex_syntax required to be available in rlib format` for `tests/cli_headless.rs`. rustc 1.99.1 is available via `rustup check` | baseline log |

## 2. Issue analysis (what each issue actually is)

### #11 -- garbled characters while configuring a provider (bug)

Reporter is on Chinese Windows. The attached screenshot is the **line-mode setup wizard** (`abacus setup`, `src/setup.rs`), step 1/2, at the prompt `Paste an API key (blank to skip):`; the arrow glyph before that prompt renders as `釟?`.

Observed code path:

- `src/setup.rs:124` prints `console::accent(console::marks().arrow)`, i.e. `›` (U+203A), before `rpassword::prompt_password`.
- `src/console.rs:80` picks the **RICH** mark set (`✓ ✗ ─ • ›`) whenever `ui::glyphs().wordmark.is_some()`.
- `src/ui.rs:272-284` (`GlyphChoice::Auto`) decides Unicode vs ASCII **only** from the `TERM` env var (`dumb`/`linux` -> ASCII). On Windows `TERM` is normally unset, so RICH is always chosen.
- Nothing in `src/` ever reads or sets the Windows console output codepage (`grep -rn "chcp\|SetConsole\|65001" src` -> no hits). Rust writes UTF-8 bytes to the console handle, so on a console whose output codepage is 936 (the zh-CN default) every non-ASCII glyph is mojibake, and `read_line` on stdin decodes the console's input codepage as if it were UTF-8, which also breaks non-ASCII typed input (a Chinese provider name).
- The TUI has the same class of bug in one place: `src/tui/draw.rs:1098` hardcodes `"•"` as the API-key mask instead of taking it from the glyph table.

So the issue is not "the key input is broken"; it is "Abacus never makes the console UTF-8, and has no codepage-aware glyph fallback".

### #10 -- numeric keypad dead after exiting and restarting in the same terminal (bug, root cause unproven)

Reporter: Abacus 0.7.1, Windows Terminal, Windows 11 26H2. Exit the program on the provider configuration page without closing the terminal; restart in that same terminal; numpad digits no longer register (main-keyboard digits still do); closing and reopening the terminal fixes it.

Facts that constrain the cause:

- The TUI pushes the kitty keyboard protocol unconditionally: `src/tui/mod.rs:689-695` (`PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES | REPORT_EVENT_TYPES)`, comment explicitly says "unconditionally rather than gating on a capability query").
- Teardown pops it: `src/tui/mod.rs:850-859` `restore_terminal()`, covered by tests at `src/tui/tests.rs:57-69`; both `process::exit` sites (`src/tui/mod.rs:818`, `:900`) call it first.
- crossterm 0.29 decodes kitty keypad codepoints **only in its unix parser** (`event/sys/unix/parse.rs:398-426`: 57399-57427 -> `Char('0')`..`Char('=')`, `KeypadBegin`). The Windows event source (`event/sys/windows/parse.rs`) has no CSI-u path at all; it maps `VK_NUMPAD0..9` key records to `Char(ch)` and nothing else.
- Therefore, once a terminal is in kitty mode, keypad keys it reports as `ESC [ 574xx u` have no decoder on Windows. Terminal state left behind by an earlier run explains why the breakage survives a restart and only a new terminal clears it.

What is **not** proven: which exact event (or none) reaches the app, and whether the leftover state comes from an exit path that skips `restore_terminal()` (rpassword's prompt on the setup page, a Ctrl+C during the wizard, or a pop that the terminal ignores). The plan opens with a diagnosis lane instead of guessing.

### #3 -- i18n request (feature request, not a bug)

Reporter offers Simplified Chinese translations. Abacus has no string catalog: ~861 user-facing string literals in `src/`, English-only, interleaved with formatting. This is a design decision (which surfaces, which locale format, how translations are maintained), not a fix.

### #4 -- two external projects for token cost (idea, not a bug)

Points at `yc-duan/fastctx` and `T-Auto/dsh-ops` as references. Abacus already has compaction (`src/compaction.rs`, 1,295 lines: summariser, idle compaction, markers) and a usage reporter. Acting on this is a research/decision task, not a defect fix.

## 3. Target state

Consumers of this work, and how their workflow must keep working:

1. **A zh-CN Windows user running `abacus setup`** -- today they see mojibake around the key prompt and may get a read error on non-ASCII input; target: every glyph and every typed character is correct, or the wizard degrades to pure ASCII with no corruption.
2. **A Windows Terminal user who exits the TUI and starts it again in the same window** -- today the numpad can go dead until the window is closed; target: numpad keys keep working across any number of runs, and no exit path can leave keyboard state behind.
3. **A maintainer reviewing the change** -- today they have no documented way to reproduce either bug; target: a reproducible command sequence and a test that fails before the fix.
4. **The reporter of #3 / #4** -- today their reports sit unanswered; target: an explicit, recorded decision with a reply they can act on.
5. **A zh-CN user of the console surfaces** -- today `setup` and `doctor` speak English only; target: they run in Chinese when the machine or the user asks for it, with English as the fallback and no change for anyone else.

Differences between current and target state, each with why it matters:

| # | Difference | Why it matters |
| --- | --- | --- |
| D1 | Console encoding is made UTF-8 on Windows (or glyphs fall back to ASCII when it cannot be) | Without it, every non-ASCII glyph in `setup`, `doctor`, warnings and the TUI is corrupt on any non-UTF-8 Windows codepage, not just zh-CN |
| D2 | Glyph choice accounts for the console codepage, not only `TERM` | `TERM`-only detection is structurally wrong on Windows; the same bug returns for ja/ko/ru consoles |
| D3 | No user-facing string is hardcoded outside the glyph table (the `•` mask) | One hardcoded glyph defeats the fallback for the most sensitive field (the API key) |
| D4 | Keyboard enhancement flags are popped on every exit path, and pushing them is a decision made per platform | A leaked flag breaks the numpad for every later run in that terminal |
| D5 | The kitty protocol is only pushed where the event source can decode what it produces | Windows cannot decode keypad CSI-u; pushing it there trades a working numpad for Shift+Enter |
| D6 | Each bug has a failing-before/passing-after test or a recorded manual repro | Without an outside judge, "fixed" is an opinion |
| D7 | #3 and #4 have a recorded decision and a drafted reply | The reports are otherwise unbounded scope with no owner |
| D8 | `setup` and `doctor` strings come from a catalog with an English and a zh-CN table, selected by `ABACUS_LANG`, then `ui.language`, then the OS locale | Without a catalog, #3 cannot be answered with code at all, and every new string is another place a translation can drift |
| D9 | The catalog is checked mechanically (every key present in every locale, placeholders identical) | A missing or mismatched string is a runtime blank or a panic-shaped format error; a test makes it a build failure instead |

## 4. Options and rejected alternatives

**#11, how to make the console safe**

- **A (chosen): set the console codepage to UTF-8 at startup, and fall back to ASCII glyphs if that fails.** One call in `main()` before any command; no new dependency (two `kernel32` externs); fixes display and stdin at once; degrades safely when the console refuses the switch.
- **B: detect the codepage and only fall back to ASCII, never switch it.** Safest for the user's terminal but leaves them with an ASCII interface forever, and does not fix stdin decoding of non-ASCII input.
- **C: write all output through a UTF-16 console API (`WriteConsoleW`) instead of `println!`.** Correct in principle, but it means replacing `console.rs`, `setup.rs`, `doctor` output and ratatui's backend -- an XL change for a bug with a one-call fix. Rejected.
- **D: ship a `zh-CN` translation first.** Treats the symptom's language, not the encoding. Rejected (and it belongs to #3).

**#10, how to protect the keypad**

- **A (chosen): diagnose first, then stop pushing kitty flags on Windows and guarantee the pop on every exit.** Two small changes, each independently testable; restores numpad by returning the terminal to the state the OS default expects.
- **B: keep pushing the flags and add our own CSI-u keypad decoder in front of crossterm's Windows reader.** Would preserve Shift+Enter, but means parsing raw console input ourselves or vendoring crossterm's unix parser -- M/L work against a third-party crate, with the same outcome for the user. Rejected unless diagnosis shows the flags are *not* the cause.
- **C: only fix the exit path (pop on all paths), keep pushing on Windows.** Cheap, but if the diagnosis shows Windows cannot decode keypad CSI-u, numpad is still broken *during* a normal run. Kept as a fallback, not the plan.
- **D: drop the kitty protocol everywhere.** Removes Shift+Enter on Linux/macOS where it works today. Rejected.

**#3, how to get from "no i18n" to a translated console**

- **A (chosen, user-approved): a hand-rolled catalog module (`src/i18n.rs`) with `En` and `ZhCn` tables of `&'static str`, a `Lang` selector (`ABACUS_LANG` -> `ui.language` -> OS locale -> English), covering `setup` and `doctor` only.** No new dependency, no build step, ~200 strings instead of 861, and a mechanical test can prove the tables agree.
- **B: adopt `fluent`/`rust-i18n` and convert every surface including the TUI.** The right end state for a mature product, but it is a new dependency, a message-file format and a full string sweep (861 literals) in one change -- an XL rewrite disguised as a feature request. Rejected for this plan; D1's catalog is the piece it can grow from.
- **C: translate the TUI first because it is what people stare at.** The TUI is where the string count explodes (status bar, dialogs, help, slash commands) and where a bad translation costs the most review time. Rejected for ordering.
- **D: translate only by documentation (README.zh-CN) and keep the binary English.** Cheapest, but the reporter asked for the interface and offered to maintain translations. Rejected.

**#4, scope**

- **A (chosen): record a decision and draft a reply; implement nothing in this plan.** Both are feature requests with real design cost (i18n touches ~861 strings; token-cost work touches `compaction.rs` and the provider path). Planning them here would be a second plan wearing this one's clothes.
- **B: implement a minimal i18n scaffold now (English default + zh-CN catalog for `setup`/`doctor` only).** This is what the user chose; it is planned as Lane D, not as a side effect of #3's triage.
- **C: benchmark `fastctx` / `dsh-ops` and change compaction.** A research plan, not a fix. Deferred to its own plan.

## 5. Risk register

| Risk | Likelihood | Impact | Mitigation |
| --- | --- | --- | --- |
| `SetConsoleOutputCP(65001)` changes behaviour for other programs sharing the console | Low | Medium | Call it only for console-attached runs; document in CHANGELOG; keep `ABACUS_GLYPHS`/`ABACUS_ASCII` as the escape hatch |
| Forcing UTF-8 breaks a legacy console that cannot render it | Medium | Low | The fallback path (ASCII glyphs) is the same commit; verified with `chcp 936` |
| The #10 diagnosis finds a different cause (e.g. rpassword leaving console mode set) | Medium | Medium | Lane B2 is written so the fix is chosen from the recorded key log, not from this plan's hypothesis; the plan is revised before B3 |
| Removing kitty flags on Windows regresses Shift+Enter newline | High | Low | Ctrl+J already inserts a newline (`README` keys table); note it in the issue reply |
| Local toolchain ICE blocks "run the tests" verification | Certain | Medium | Local judge is `fmt` + `clippy --lib` + `test --lib` (all green today); full matrix is CI's job; `rustup update` to 1.99.1 is offered as an optional environment task |
| Adding `windows`-only code without CI coverage on Windows | Low | Medium | CI already runs `windows-latest`; the codepage helper is `#[cfg(windows)]` with a pure, cross-platform unit test for the decision function |
| A fix for #11 silently changes TUI output for macOS/Linux users | Low | Medium | No behaviour change when the codepage is already UTF-8 or the platform is not Windows |
| zh-CN translations read like machine output to a native speaker | Medium | Medium | Ask the #3 reporter (who offered to translate) to review the catalog before merge; until then the PR marks the catalog `community-review-pending` and records the gap |
| Locale selection surprises someone (auto-detect flips their interface) | Low | Medium | `ABACUS_LANG=en` always wins; `ui.language` defaults to `auto`, and `auto` falls back to English whenever the locale is not `zh-*` |
| A translated format string mismatches its arguments | Medium | High | D9's test compares placeholder sets between locales; format arguments stay at the call site, so a mismatch fails a unit test rather than at runtime |

## 6. Success criteria

| Criterion | Task | Verification scenario |
| --- | --- | --- |
| D1: a zh-CN console shows the wizard's arrow, rules and marks correctly | A1 | On Windows: `chcp 936`, then `abacus setup`; the key prompt line contains no mojibake and `GetConsoleOutputCP()` returns 65001 (checked with `chcp` before/after) |
| D1b: when the codepage cannot be switched, output is pure ASCII | A1 | Unit test: `glyph_choice_for(936, false, switch_failed = true)` returns `Ascii`; manual: `ABACUS_ASCII=1 abacus setup` output contains no byte > 0x7F |
| D1c: non-ASCII input is read correctly | A3 | Manual on a CP936 console: type a Chinese provider name at the `Name` prompt; the saved `config.toml` holds the same characters |
| D2: glyph choice is codepage-aware | A1 | Unit tests over `(codepage, TERM, override)` combinations in `ui.rs`/`terminal.rs` |
| D3: no hardcoded mask glyph remains | A2 | `grep -n '"•"' src/tui/draw.rs` returns nothing; a TUI test asserts the mask equals the active glyph set's mask |
| D4: every exit path pops the keyboard flags | B2 | Existing `restore_terminal` tests plus a new test asserting the claimed/enhanced atomics are false after each exit path; manual: run TUI, `/exit`, then check the numpad in the same window |
| D5: kitty flags are not pushed where keypad cannot be decoded | B3 | Manual repro of issue #10 on Windows Terminal: run, exit at the provider page, run again, press numpad 1-9 -> digits appear |
| D6: both bugs have a before/after judge | A1, A2, B2, B3 | `cargo test --lib` passes with the new tests; the same tests fail when the fix is reverted (checked once by hand, recorded in the PR body) |
| D7 | #3 and #4 are answered | C1 | Decision record committed under `docs/`; the drafted replies quoted in the PR body; posted only after the user approves |
| D8 | `setup` and `doctor` speak zh-CN when asked | D2, D3, D4 | Unit tests over locale selection; `ABACUS_LANG=zh-CN abacus setup` prints the Chinese banner and prompts; `ABACUS_LANG=en` prints English |
| D9 | the catalog cannot drift silently | D1 | Catalog test fails when a key is missing from a locale or a placeholder set differs; verified by deleting one entry by hand once and recording the failure in the PR body |

## 7. Lanes

Sizes: XS one small edit, S one file plus its test, M several files in one module, L several modules or one contract change.

### Lane A -- #11 console encoding (no dependency)

**A1** (S) -- `TASK`: make Windows console output/input UTF-8 at startup with an ASCII fallback. `DELIVERABLE`: `src/terminal.rs` (or a `#[cfg(windows)]` block in `console.rs`) exposing `ensure_utf8_console() -> bool` and a pure `glyph_choice_for(codepage, term_is_bare, switch_failed) -> GlyphChoice`; called once at the top of `main()` before `setup`, `doctor` or the TUI; `ui::GlyphChoice::resolve` consults it. `SCOPE`: `src/main.rs`, `src/ui.rs`, new module, unit tests. `VERIFY`: `cargo test --lib`, `cargo clippy --lib -- -D warnings`; manual `chcp 936 && abacus setup`. `STOP WHEN`: tests pass and the manual run shows no mojibake (or clean ASCII). `depends_on`: none.

**A2** (XS) -- `TASK`: take the API-key mask from the glyph table. `DELIVERABLE`: a `mask` field (or equivalent) in `Glyphs` with `•` for Unicode/Nerd and `*` for ASCII; `src/tui/draw.rs:1098` uses it. `SCOPE`: `src/ui.rs`, `src/tui/draw.rs`, `src/tui/tests.rs`. `VERIFY`: `cargo test --lib`; grep shows no hardcoded mask. `STOP WHEN`: test asserts the ASCII set masks with `*`. `depends_on`: A1 (shares the glyph decision).

**A3** (S) -- `TASK`: make stdin decoding match the console on Windows. `DELIVERABLE`: input codepage set alongside output, plus a note in `console.rs` on why; if the switch fails, an explicit error instead of silent mojibake. `SCOPE`: same module as A1, `src/console.rs`. `VERIFY`: manual CP936 run typing a Chinese provider name; `cargo test --lib`. `STOP WHEN`: the saved `config.toml` holds the typed characters. `depends_on`: A1.

**A4** (XS) -- `TASK`: record the change. `DELIVERABLE`: CHANGELOG entry under the next version + a line in `README`/`docs` about `ABACUS_GLYPHS=ascii` on legacy consoles. `SCOPE`: `CHANGELOG.md`, docs only. `VERIFY`: `cargo fmt --all -- --check` unaffected; reviewer reads the entry. `STOP WHEN`: entry describes the symptom and the escape hatch. `depends_on`: A1, A2, A3.

### Lane B -- #10 keypad (diagnosis-gated)

**B1** (M) -- `TASK`: capture what the terminal actually sends when the numpad is dead. `DELIVERABLE`: an opt-in key-event log (`ABACUS_DEBUG_KEYS=<path>`, appended from the TUI event loop, one line per `KeyEvent`: code, modifiers, kind, state) plus a recorded repro transcript from Windows Terminal: run, exit at the provider page, run again, press numpad 1-9. `SCOPE`: `src/tui/mod.rs` (log only), scratch log file, a short note under `.omh/plans/` or the issue thread. `VERIFY`: the log shows either keypad events with an unusable code, or no events at all; the note states which. `STOP WHEN`: the event shape (or its absence) is recorded and the root cause is written in one sentence. `depends_on`: none.

**B2** (S) -- `TASK`: make "the terminal is given back" a guarantee rather than a convention. `DELIVERABLE`: one shutdown path used by the normal return, both `process::exit` sites and the panic hook; a test asserting the atomics are cleared and the pop is emitted once, per path. `SCOPE`: `src/tui/mod.rs`, `src/tui/tests.rs`. `VERIFY`: `cargo test --lib`; the new test fails when the pop is commented out. `STOP WHEN`: every exit path is covered by a test. `depends_on`: B1 (so the fix targets the path B1 identified).

**B3** (S) -- `TASK`: stop pushing keyboard flags where they cannot be decoded. `DELIVERABLE`: the push gated by platform and by the B1 finding (default: not pushed on Windows, still pushed on unix), with an override (`ABACUS_KEYBOARD=kitty|none`) and a comment naming the crossterm limitation; if B1 contradicts the hypothesis, this lane is rewritten before it starts. `SCOPE`: `src/tui/mod.rs`, `src/ui.rs`/`src/terminal.rs` for the override, docs. `VERIFY`: manual repro of #10 on Windows Terminal (numpad works after restart); `cargo test --lib`. `STOP WHEN`: the repro sequence passes twice in a row. `depends_on`: B1, B2.

**B4** (XS) -- `TASK`: record the change. `DELIVERABLE`: CHANGELOG entry; a line in the issue reply about Shift+Enter -> Ctrl+J on Windows. `SCOPE`: `CHANGELOG.md`. `VERIFY`: reviewer reads it against the diff. `STOP WHEN`: entry matches behaviour. `depends_on`: B3.

### Lane D -- #3 i18n scaffold and zh-CN console catalog (user-approved extension)

**D1** (M) -- `TASK`: give the console surfaces a string catalog. `DELIVERABLE`: `src/i18n.rs` with `enum Lang { En, ZhCn }`, `enum Key { ... }` (one variant per user-facing string in `setup` and `doctor`), `fn t(key: Key) -> &'static str` backed by `mod en` and `mod zh_cn` tables, and a test that walks every `Key` and asserts (a) each locale has a non-empty entry and (b) the `{}`/`{name}` placeholder sets are identical across locales; format arguments stay at the call sites. `SCOPE`: new `src/i18n/` module, `src/lib.rs`, unit tests. `VERIFY`: `cargo test --lib`, `cargo clippy --lib -- -D warnings`. `STOP WHEN`: the catalog test exists and fails when one zh-CN entry is deleted (recorded once by hand). `depends_on`: A1 (the module reads the same console/locale decision).

**D2** (S) -- `TASK`: choose the language. `DELIVERABLE`: precedence `ABACUS_LANG` -> `settings.ui.language` (`auto|en|zh-CN`, default `auto`) -> OS locale (`GetUserDefaultUILanguage` on Windows, `LC_ALL`/`LANG` elsewhere) -> English; `auto` selects `ZhCn` only for `zh-*`, so nobody's interface flips unexpectedly. `SCOPE`: `src/config.rs` (new field with serde default so existing configs load), `src/i18n.rs`, tests. `VERIFY`: unit tests over the precedence chain with injected values; `cargo test --lib`. `STOP WHEN`: all four precedence levels have a test and existing `config.toml` files still parse. `depends_on`: D1.

**D3** (M) -- `TASK`: route the wizard through the catalog. `DELIVERABLE`: every user-facing string in `src/setup.rs` and the shared pieces of `src/console.rs` (banner subtitle, step titles, notes, prompt labels, error lines) comes from `i18n::t`; `console::marks()` keeps its glyph logic. `SCOPE`: `src/setup.rs`, `src/console.rs`, catalog entries, tests for at least the banner and the provider prompt. `VERIFY`: `ABACUS_LANG=zh-CN cargo run -- setup` shows Chinese prompts, `ABACUS_LANG=en` shows English; `cargo test --lib`. `STOP WHEN`: `grep` finds no English literal left in `setup.rs` outside catalog keys and tests. `depends_on`: D1, D2.

**D4** (M) -- `TASK`: route `doctor` through the catalog. `DELIVERABLE`: the health rows, section headings and detail lines printed from `src/main.rs` (the `doctor` path around lines 700-790) come from `i18n::t`; the detail strings that embed a value keep `format!` at the call site. `SCOPE`: `src/main.rs`, catalog entries, one test per health row shape. `VERIFY`: `ABACUS_LANG=zh-CN abacus doctor`; `cargo test --lib`; `cargo clippy --lib -- -D warnings`. `STOP WHEN`: doctor's output is fully catalogued and the English output is byte-identical to today's. `depends_on`: D1, D2.

**D5** (XS) -- `TASK`: make the next locale cheap. `DELIVERABLE`: a `CONTRIBUTING.md` section (how to add a locale: one module, one enum arm, run the catalog test) and a CHANGELOG entry noting zh-CN for `setup`/`doctor` and the `ABACUS_LANG` knob. `SCOPE`: `CONTRIBUTING.md`, `CHANGELOG.md`. `VERIFY`: a reviewer follows the instructions to add a stub locale in a scratch branch and the catalog test tells them what is missing. `STOP WHEN`: the instructions work without asking anyone. `depends_on`: D1, D3, D4.

### Lane C -- #3 / #4 triage (no code)

**C1** (S) -- `TASK`: turn two feature requests into recorded decisions. `DELIVERABLE`: `docs/decisions/i18n-and-token-cost.md` (or the repo's existing decision format if one exists) with: the options from section 4, the string-count evidence for i18n, and a recommendation; plus drafted replies to #3 and #4 quoting the decision -- the #3 reply says zh-CN ships for `setup`/`doctor` and asks the reporter to review the catalog, so it is written after D3 lands. `SCOPE`: `docs/`, issue text (posting requires the user's explicit go-ahead). `VERIFY`: a human reviewer reads the record; the drafted replies name what would be accepted as a contribution. `STOP WHEN`: the record exists and the drafts are quoted in the PR body. `depends_on`: none to write; the #3 draft needs D3.

### Lane E -- environment (optional, not part of the fix)

**E1** (XS) -- `TASK`: unblock the full local test suite. `DELIVERABLE`: `rustup update stable` to 1.99.1, then `cargo test --all-targets`; record whether the ICE clears. `SCOPE`: machine toolchain only, no repo change. `VERIFY`: exit code of `cargo test --all-targets`. `STOP WHEN`: it passes, or the ICE persists and is recorded as a CI-only verification. `depends_on`: none. This is offered, not assumed: it changes the user's toolchain.

## 8. Verification commands

Run from `D:\New folder\abacus`, in git-bash, with the C compiler on PATH (the `ring` build needs it):

```sh
export PATH="/d/dev/mingw64/bin:$PATH"
cargo fmt --all -- --check
cargo clippy --lib -- -D warnings      # clean today
cargo test --lib                       # 646 passed today
```

Outside judges:

- **CI** (`.github/workflows/ci.yml`) on the PR: `fmt`, `clippy --all-targets -D warnings`, `test --all-targets` on ubuntu, ubuntu-arm, macos, windows. This is the judge for everything the local toolchain cannot build.
- **Manual, Windows console**: `chcp 936` then `abacus setup` (issue #11).
- **Manual, locale**: `ABACUS_LANG=zh-CN abacus setup` and `ABACUS_LANG=zh-CN abacus doctor`; `ABACUS_LANG=en` for the English control.
- **Manual, Windows Terminal**: the #10 repro sequence (run, exit at the provider page, run again, numpad).
- **Unit tests** added by A1, A2, B2, B3, D1, D2, D3, D4 -- each fails when its fix is reverted.

Not usable locally today: `cargo test --all-targets` (rustc 1.98.1 ICE at `src/main.rs:755`, `:774`, then an rlib error for `tests/cli_headless.rs`). Treat that as a known local limitation, not a regression to chase inside this plan.

## 9. Evidence gaps

1. **#10 key-event shape** -- unresolved until B1 runs. Everything in B2/B3 is written to be revised by that finding.
2. **Which exit path leaks the flags** -- not reproducible without a Windows Terminal session; B1 must record the exact exit used by the reporter.
3. **Reporter's version** -- #10 is filed against 0.7.1; HEAD is 0.7.4 with the same push/restore code, but the reporter should retest on 0.7.4 before the fix is claimed to close it.
4. **rustc 1.99.1 clears the ICE** -- untested; E1 is the probe.
5. **i18n string count** -- "~861 candidate literals" is a grep estimate, not an extraction; C1 records it as an estimate.
6. **`fastctx` / `dsh-ops` content** -- not read in this session; #4's decision is deliberately "not now" rather than an evaluation of the ideas.
7. **Native zh-CN review** -- no native speaker is available in this session. The translations are drafted by the planner; the #3 reporter offered to translate and is the natural reviewer. Until that review happens the catalog is `community-review-pending`, and that is the one acceptance criterion (D8's quality half) with no outside judge yet.

## 10. Target-state coverage

| Difference | Owning task | Verification scenario | Status |
| --- | --- | --- | --- |
| D1 | A1 | manual CP936 wizard run | mapped |
| D1b | A1 | unit test + `ABACUS_ASCII=1` run | mapped |
| D1c | A3 | typed Chinese provider name round-trips | mapped |
| D2 | A1 | unit tests over codepage/TERM/override | mapped |
| D3 | A2 | grep + TUI test | mapped |
| D4 | B2 | per-path test + manual `/exit` | mapped |
| D5 | B3 | #10 repro sequence | mapped |
| D6 | A1, A2, B2, B3 | revert-check recorded in PR body | mapped |
| D7 | C1 | decision record + drafted replies | mapped |
| D8 | D2, D3, D4 | locale tests + `ABACUS_LANG` runs | mapped (translation quality: evidence gap 7) |
| D9 | D1 | catalog test, verified by deleting an entry | mapped |

No unmapped difference. The target state is what the user accepted: #11 and #10 fixed, #3 shipped as a console-surface scaffold with zh-CN, #4 answered rather than implemented. i18n for the TUI is deliberately out (see non-goals), and that exclusion was named before acceptance, not after.

## 11. Non-goals (explicit)

- Translating the TUI (#3's larger half): the catalog is built so that surface can follow, but converting ~861 literals is its own plan.
- Implementing the token-cost ideas (#4).
- Any change to the Abacus Desktop repo (`empero-org/abacus-desktop`).
- Reworking compaction, provider transport or the sync engine.
- Committing `.omh/` state; this plan file stays out of the diff unless the user asks otherwise.

## 12. Handoff (accepted; execution needs the user's go-ahead)

Accepted shape: three implementation lanes plus triage. **A** (#11, console encoding/glyphs), **B** (#10, keyboard; internally gated on B1), **D** (#3, i18n scaffold; D1 -> D2 -> D3/D4 -> D5), **C** (#3/#4 decision record, independent). A and B and C are disjoint; D depends on A1's console decision but only through one function signature, so it can start after A1 lands.

Recommended follow-on: `ulw-work` coordinated lanes for A + B + D in one delivery cycle, with C1 as a fourth lane, because the four lanes touch different files and only meet at `src/ui.rs`/`src/console.rs`. A direct executor handoff fits only if the user wants one issue at a time.

Nothing above is started. Starting requires the user's explicit go-ahead in this conversation.
