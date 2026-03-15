## Why

This fork already has stronger multi-agent primitives than upstream Codex, but code navigation is still dominated by file-name fuzzy search, grep, and manual file reading. That leaves agents slower and less reliable on large repositories, and it prevents this fork from fully benefiting from its agent-team and hook workflow extensions.

At the same time, the current user-level agent presets are not flowing through this fork's native role loader. The local `~/.codex/agents/*.md` files were migrated from another toolchain, while this fork expects role declarations in `config.toml` plus `*.toml` role config files. That mismatch means useful presets exist on disk but are effectively outside the supported execution path.

## What Changes

- Add an internal code-search capability that extends the existing built-in search stack from file-path search into symbol-aware navigation.
- Introduce a first-class LSP-backed query layer for symbol search, definition lookup, reference lookup, and document symbol enumeration.
- Make agent-facing tooling prefer structured code-search results before falling back to grep/read flows.
- Keep plugin and MCP integrations out of the first delivery path; they remain extension points rather than the primary implementation route.
- Normalize user-level agent presets onto the fork's native role system based on `[agents.<role>]` declarations and `~/.codex/agents/*.toml` config files.
- Add a curated set of native role presets intended for single-agent work and agent-team compositions.
- Document explicit data boundaries and code-diff boundaries so the first implementation phase stays narrow and reversible.

## Capabilities

### New Capabilities
- `internal-code-search`: Built-in structured code navigation that combines existing file search with LSP-backed symbol and location queries for agent and client use.
- `native-agent-role-presets`: Native user-level role preset support and migration path for reusable agent and team-oriented role configurations.

### Modified Capabilities
- None.

## Impact

- Affected code: `codex-rs/file-search`, `codex-rs/app-server`, `codex-rs/app-server-protocol`, `codex-rs/core`, and docs for config/app-server usage.
- Affected APIs: new experimental app-server `codeSearch/*` endpoints and new agent-visible tool definitions for structured code lookup.
- Data boundary:
  - Read-only access to repository source trees, LSP indexes, and user role config files under `~/.codex`.
  - No first-phase writes to project source files from the LSP/code-search layer.
  - No first-phase persistence of semantic indexes, embeddings, or external search databases.
  - No first-phase ingestion of remote code, SaaS search backends, or plugin-managed state.
- Diff boundary:
  - First implementation change stays inside OpenSpec-defined work plus the Rust crates that own file search, protocol, app-server routing, agent tool exposure, and role loading/docs.
  - No first-phase rewrite of plugin architecture, no new team-preset DSL, and no Markdown-based runtime compatibility layer for legacy `~/.codex/agents/*.md` files.
- Dependencies and systems: local LSP servers become optional runtime dependencies for supported languages; when unavailable, behavior must degrade to existing built-in search paths instead of failing the turn.
