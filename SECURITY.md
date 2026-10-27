# Security

Abacus is a local coding agent. It can read and modify source code and execute arbitrary shell commands with the permissions of the account that launched it. In AUTO mode a reviewer model checks every action that is not a workspace edit or a read-only command before it runs; in YOLO mode nothing is checked.

## Boundaries

- File tools are confined to the canonical workspace path and reject `..`, absolute paths, and symlink escapes.
- `.env` and `.env.*` files are blocked, except `.env.example`.
- In AUTO mode (the default), edits inside the workspace and read-only commands run directly. Other commands, deletions, subagents and MCP calls are judged by a reviewer model against the user's own messages in the session; a blocked action does not run, and the person is asked when the reviewer cannot decide or cannot be reached. The reviewer is a model: it lowers the risk, it is not a security boundary. YOLO mode runs everything without review.
- API keys are read from environment variables or `~/.abacus/credentials.toml`. On Unix, Abacus creates that file with mode `0600`.
- Provider responses, command output, and repository contents are untrusted input. The system prompt instructs the model not to treat them as higher-priority instructions.
- Project-local plugins, hooks, and MCP configuration are disabled until the canonical workspace is added with `abacus trust`. Agent Skills are instructions and load without that executable trust grant; inspect unfamiliar skills before invoking them.
- Installed plugin hooks are executable code. Relative hook commands are confined to the plugin directory, receive bounded JSON context on stdin, and have timeouts, but still run with the user's OS permissions.
- MCP calls go to the reviewer in AUTO mode. `auto_approve = true` runs that server's calls without review; use environment-backed headers instead of literal secrets.
- Subagents run in disposable git worktrees, in the session's mode, and cannot ask the person: in AUTO, what the reviewer cannot clear does not run. Worktrees isolate repository writes from each other, not processes, credentials, or the network.
- Scheduled jobs run in AUTO mode, where what the reviewer cannot clear does not run, since no one is there to ask; a job created with `--yolo` runs everything. Job prompts, status, and logs are private local state; service daemons run as the current user.
- `/feedback` never automatically includes transcripts or source files. Optional diagnostics, session ID, workspace name, platform metadata, and the user-authored message are sent to the configured endpoint; review the endpoint in `/config` before submitting.

These controls are not process isolation. In particular, a shell command that runs (approved, cleared by the reviewer, or in YOLO mode), plugin hook, MCP server, or unattended job can read credentials, access the network, or modify files outside the workspace. Run Abacus inside a container, VM, or OS sandbox when working with untrusted code or unattended prompts.

Do not commit `credentials.toml` or pass keys through command-line arguments on shared machines. Prefer provider-specific environment variables.

## Reporting

Do not open a public issue for a suspected vulnerability until a private reporting channel has been established for the repository. Include the Abacus version, operating system, reproduction steps, and impact.
