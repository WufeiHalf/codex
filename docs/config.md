# Configuration

For basic configuration instructions, see [this documentation](https://developers.openai.com/codex/config-basic).

For advanced configuration instructions, see [this documentation](https://developers.openai.com/codex/config-advanced).

For a full configuration reference, see [this documentation](https://developers.openai.com/codex/config-reference).

## Native agent role presets

Codex currently ships three built-in agent roles:

- `default`
- `explorer`
- `worker`

The supported custom preset format is native Codex config:

1. Declare the role under `[agents.<role>]` in `~/.codex/config.toml`.
2. Point `config_file` at a TOML role file, usually under `~/.codex/agents/`.
3. Optionally add `description` and `nickname_candidates`.

Example declaration in `~/.codex/config.toml`:

```toml
[agents.reviewer]
description = "Review code for regressions, edge cases, and missing tests."
config_file = "./agents/reviewer.toml"
nickname_candidates = ["Noether", "Sagan"]
```

Example role file at `~/.codex/agents/reviewer.toml`:

```toml
model_reasoning_effort = "high"

developer_instructions = """
You are a review specialist.
Focus on bugs, regressions, risky assumptions, and missing tests.
Present findings before summaries.
"""
```

Notes:

- Role files use the same config schema as `config.toml`; keep only the overrides you want for that role.
- Relative `config_file` paths are resolved relative to the `config.toml` that declared the role.
- If `config_file` points to a missing path or a directory, Codex rejects the role configuration.
- `nickname_candidates` are optional, but when present they must be non-empty, unique, and ASCII-safe.
- Built-in roles do not need declarations in `config.toml`.

Legacy `~/.codex/agents/*.md` files are migration input only in this fork. They are not active runtime presets and are not loaded by name. To activate a migrated role, create a TOML role file and add the matching `[agents.<role>]` declaration.

Starter examples live in [`docs/examples/agent-role-presets/`](./examples/agent-role-presets/README.md).

## Custom model providers

You can define custom providers in `~/.codex/config.toml` and select them via `model_provider`.

Example Anthropic provider:

```toml
[model_providers.anthropic]
name = "Anthropic"
base_url = "https://api.anthropic.com"
env_key = "ANTHROPIC_API_KEY"
wire_api = "anthropic"

model_provider = "anthropic"
model = "claude-sonnet-4-5"
```

You can also set provider overrides inside a role's TOML config file. First declare the role in `~/.codex/config.toml`:

```toml
[agents.researcher]
description = "Research-focused role."
config_file = "./agents/researcher.toml"
```

Then add the provider override in `~/.codex/agents/researcher.toml`:

```toml
model_provider = "anthropic"

[model_providers.anthropic]
name = "Anthropic"
base_url = "https://api.anthropic.com"
env_key = "ANTHROPIC_API_KEY"
wire_api = "anthropic"
```

Before running Codex, set your key in the environment:

```bash
export ANTHROPIC_API_KEY="..."
```

## Connecting to MCP servers

Codex can connect to MCP servers configured in `~/.codex/config.toml`. See the configuration reference for the latest MCP server options:

- https://developers.openai.com/codex/config-reference

## Internal code search

The experimental `internal_code_search` feature exposes structured code-navigation tools and app-server RPCs. Runtime behavior stays under `[code_search]` in `config.toml`:

```toml
[features]
internal_code_search = true

[code_search]
enabled = true
auto_detect = true
auto_install = false

[code_search.lsp.rust]
command = ["rust-analyzer"]
```

Resolution order is:

1. `[code_search.lsp.<language>].command`
2. Codex-managed installed server path under `CODEX_HOME/lsp`
3. PATH-visible default command when `auto_detect = true`
4. On-demand installation when `auto_install = true`
5. Built-in fallback search

Notes:

- `auto_install` is opt-in. When enabled, Codex can install supported language servers for Rust, Go, JavaScript, TypeScript, and Python into `CODEX_HOME` when no explicit, managed, or PATH-visible server is available.
- A broken explicit `command` remains authoritative for that lookup: Codex warns and falls back instead of silently replacing it.
- Successful LSP lookups are intentionally quiet; fallback, install, and runtime failures surface through warnings and provenance metadata instead.
- `auto_install` covers the language server package plus known runtime package dependencies that the server needs after install. It does not bootstrap missing host package managers themselves: Rust still requires `rustup`, Go still requires `go`, and JavaScript, TypeScript, and Python currently require `npm` on the host.

Auto-install matrix:

| Language | Managed command under `CODEX_HOME` | Install command | Extra dependency Codex installs | Source and proxy controls |
| --- | --- | --- | --- | --- |
| Rust | `lsp/bin/rust-analyzer` | `rustup component add rust-analyzer rust-src` | `rust-src` | `rustup` controls the download source. Mirrors and proxies follow normal `rustup` settings such as `RUSTUP_DIST_SERVER`, `RUSTUP_UPDATE_ROOT`, and standard proxy env vars like `HTTPS_PROXY`, `HTTP_PROXY`, and `ALL_PROXY`. |
| Go | `lsp/bin/gopls` | `GOBIN=$CODEX_HOME/lsp/bin go install golang.org/x/tools/gopls@latest` | none beyond `gopls` | `go install` follows Go module settings such as `GOPROXY`, `GONOPROXY`, `GOPRIVATE`, `GONOSUMDB`, and `GOSUMDB`, plus standard proxy env vars when the Go toolchain uses them. |
| JavaScript | `lsp/npm/node_modules/.bin/typescript-language-server --stdio` | `npm install --prefix $CODEX_HOME/lsp/npm typescript typescript-language-server` | `typescript` | `npm` controls the registry and proxy path. It follows npm config such as `registry`, `proxy`, and `https-proxy`, plus environment variables like `NPM_CONFIG_REGISTRY`, `HTTP_PROXY`, `HTTPS_PROXY`, and `ALL_PROXY`. |
| TypeScript | `lsp/npm/node_modules/.bin/typescript-language-server --stdio` | `npm install --prefix $CODEX_HOME/lsp/npm typescript typescript-language-server` | `typescript` | Same npm registry and proxy behavior as JavaScript. |
| Python | `lsp/npm/node_modules/.bin/pyright-langserver --stdio` | `npm install --prefix $CODEX_HOME/lsp/npm pyright` | none beyond `pyright` | Python code search currently uses the Node-distributed `pyright-langserver`, so it follows the same npm registry and proxy settings as JavaScript and TypeScript rather than `pip`. |

Codex forwards the current process environment to these installers. That means:

- registry and mirror selection stay with the native package manager instead of Codex adding a second source-selection layer
- proxy changes should usually be made in the package manager config or the standard environment variables that package manager already honors
- if a machine needs a custom internal mirror, set it where `rustup`, `go`, or `npm` already expects it and Codex auto-install will inherit that behavior

## Apps (Connectors)

Use `$` in the composer to insert a ChatGPT connector; the popover lists accessible
apps. The `/apps` command lists available and installed apps. Connected apps appear first
and are labeled as connected; others are marked as can be installed.

## Hooks

Codex can run hooks at lifecycle boundaries such as `session_start`, `session_end`, `user_prompt_submit`, `pre_tool_use`, `permission_request`, `notification`, `post_tool_use`, `post_tool_use_failure`, `stop`, `subagent_start`, `subagent_stop`, `teammate_idle`, `task_completed`, `config_change` (currently emitted for skills file changes; source is "skills"), `pre_compact`, `worktree_create`, and `worktree_remove`.

Example:

```toml
[hooks]

[[hooks.pre_tool_use]]
command = ["python3", "/Users/me/.codex/hooks/check_tool.py"]
timeout = 5
once = true

[hooks.pre_tool_use.matcher]
tool_name_regex = "^(shell|exec)$"
```

Hooks receive a JSON payload on `stdin`. If the hook exits with code `0`, Codex will attempt to parse a JSON object from `stdout` (either the full output or the first parseable JSON line). Exit code `2` blocks execution for hook events that support blocking; other non-zero exit codes are treated as non-blocking errors. All matching hooks run in parallel and identical handlers are deduplicated.

`command` can be either an argv list (`["python3", "..."]`) or a shell command string (`"python3 ..."`). Matchers can filter by `matcher`, and tool events can also filter by `tool_name` / `tool_name_regex`.

See `docs/hooks.md` for hook payload fields and `stdout` response options.

Project hooks can also be configured in `./.codex/config.toml`. If the project directory is untrusted, project layers may load as disabled; mark it trusted via your user config (for example, `[projects."/abs/path"].trust_level = "trusted"`).

See the configuration reference for the latest hook settings:

- https://developers.openai.com/codex/config-reference

When Codex knows which client started the turn, the legacy notify JSON payload also includes a top-level `client` field. The TUI reports `codex-tui`, and the app server reports the `clientInfo.name` value from `initialize`.

## Scheduled tasks

Scheduled-task tools and the TUI `/loop` shortcut are enabled by default. To keep `/loop` available, leave `disable_cron` unset or set it to `false` in `config.toml`:

```toml
disable_cron = false
```

To disable `/loop` and the scheduled-task tools globally:

```toml
disable_cron = true
```

You can also override the setting per profile. The active profile wins over the root setting:

```toml
disable_cron = true
profile = "scheduled"

[profiles.scheduled]
disable_cron = false
```

## GitHub webhook

`codex serve` can load non-sensitive webhook defaults from the top-level `[github_webhook]` table in `~/.codex/config.toml`.
Secrets stay in environment variables; the config only stores env var names and runtime defaults.

Example:

```toml
[github_webhook]
enabled = true
listen = "127.0.0.1:8787"
webhook_secret_env = "GITHUB_WEBHOOK_SECRET"
github_token_env = "GITHUB_TOKEN"
github_app_id_env = "GITHUB_APP_ID"
github_app_private_key_env = "GITHUB_APP_PRIVATE_KEY"
auth_mode = "auto"
min_permission = "read"
allow_repos = ["owner/repo"]
command_prefix = "/codex"
delivery_ttl_days = 7
repo_ttl_days = 0
sources = ["repo", "organization", "github-app"]

[github_webhook.events]
issue_comment = true
issues = true
pull_request = true
pull_request_review = true
pull_request_review_comment = true
push = true
```

Notes:

- CLI overrides still override config defaults (for example, `codex serve -c github_webhook.min_permission=write`).
- If `[github_webhook]` is absent or `enabled = false`, the webhook route is disabled.
- `issues`, `pull_request`, and `push` only trigger when the issue body, PR body, or head commit message explicitly starts with the configured command prefix.
- `auth_mode = "auto"` prefers GitHub App installation tokens when available and falls back to `GITHUB_TOKEN`.
- When running under `codex serve`, `github_webhook.listen` is ignored; the webhook is served at `POST /github/webhook` on the same host/port as `codex serve`.
- GitHub Kanban sync uses `CODEX_HOME/github-repos.json` when present; otherwise it uses `github_webhook.allow_repos`, and if both are empty it attempts to infer a single repo from the current working directory's `git remote origin`.

## JSON Schema

The generated JSON Schema for `config.toml` lives at `codex-rs/core/config.schema.json`.

## SQLite State DB

Codex stores the SQLite-backed state DB under `sqlite_home` (config key) or the
`CODEX_SQLITE_HOME` environment variable. When unset, it defaults to `CODEX_HOME`.

## Notices

Codex stores "do not show again" flags for some UI prompts under the `[notice]` table.

## Plan mode defaults

`plan_mode_reasoning_effort` lets you set a Plan-mode-specific default reasoning
effort override. When unset, Plan mode uses the built-in Plan preset default
(currently `medium`). When explicitly set (including `none`), it overrides the
Plan preset. The string value `none` means "no reasoning" (an explicit Plan
override), not "inherit the global default". There is currently no separate
config value for "follow the global default in Plan mode".

Ctrl+C/Ctrl+D quitting uses a ~1 second double-press hint (`ctrl + c again to quit`).
