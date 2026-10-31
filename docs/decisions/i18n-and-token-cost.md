# Decision record: i18n scope, and the token-cost ideas in #4

Status: decided 2026-10-28. Written to answer issues #3 and #4, which are
feature requests rather than defects.

## #3 — i18n

**Decision.** Ship a string catalog and a Simplified Chinese translation for the
console surfaces (`abacus setup`, `abacus doctor`) now; treat the TUI as the
next step, on the same catalog.

**Why this split.** The console commands are the first thing a new user reads
and the last thing they read when something is broken, and they hold roughly 90
strings. The TUI holds the other ~770: status bar, dialogs, slash-command help,
transcript labels. Translating both at once would have made the review about the
volume of text rather than the mechanism, and the mechanism (a key per string, a
table per language, a test that both tables agree) is what the rest of the work
depends on.

**What exists after this change.**

- `src/i18n.rs`: `enum Lang`, `enum Key` with `ALL`, `i18n::t(key)`, and tests
  that fail when a key is missing, empty, or has a different number of `{}`
  holes than the English.
- `src/i18n/en.rs`, `src/i18n/zh_cn.rs`: one table each.
- Language selection: `ABACUS_LANG`, then `ui.language`, then the OS locale,
  then English. `auto` follows the OS but only ever selects a language this
  build has.
- `CONTRIBUTING.md` describes how to add a third language in three edits.

**What is not decided here.** The TUI's vocabulary is not just strings: the
status bar and help lines are width-constrained, and CJK glyphs are two cells
wide. A TUI pass needs its own measurement work, and that is a separate plan.

**Translation review.** The zh-CN table is marked community-review pending. The
reporter of #3 offered to translate; they are the natural reviewer.

## #4 — token cost

**Decision.** Not now, and not as a defect. Recorded so the report does not
languish unanswered.

**Why.** The issue points at two external projects (`yc-duan/fastctx`,
`T-Auto/dsh-ops`) as sources of ideas for reducing token cost. Abacus already
spends real effort here: `src/compaction.rs` (1,295 lines) summarises a session
as it grows, compacts while idle, and marks where a compaction cut the history;
`src/model_info.rs` detects a model's context window so the budget is not a
guess; `doctor` reports the resulting compaction point. Whether another
implementation does better is an empirical question, and answering it means
building an evaluation harness (same sessions, same tasks, measure tokens and
task success), not adopting a library.

**What would make this actionable.** A concrete measurement: for a named set of
sessions, tokens spent and tasks completed under the current compaction versus a
candidate. Anyone with that data is welcome to open a proposal; the compaction
budget is already a single structure (`CompactionBudget`), so a change there is
contained.
