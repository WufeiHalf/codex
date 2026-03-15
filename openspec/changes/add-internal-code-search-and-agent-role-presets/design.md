## Context

This fork already contains three relevant building blocks:
- `codex-file-search`, which provides fast path-based search and incremental search sessions.
- app-server experimental search APIs, currently limited to `fuzzyFileSearch`.
- a native agent-role system that loads role declarations from `[agents.<role>]` config plus `config_file`-backed TOML role files.

The change is cross-cutting because it spans feature gating, config loading, app-server API shape, core tool exposure, and user-level role preset guidance. The design must also preserve the existing extension model: plugins and MCP remain valid extension points, but they are not the first implementation path for internal code navigation.

A second constraint is user experience. The new code-search feature must feel discoverable through the existing `/experimental` pathway, but it must store its state in the same `config.toml` layering model as every other feature. The design cannot introduce a second settings store.

A third constraint is failure handling. Local LSP support depends on external binaries that may not be installed. Missing language servers must never fail an entire turn when existing fuzzy search and grep-based fallbacks can still produce useful results.

## Goals / Non-Goals

**Goals:**
- Add an internal, LSP-backed code-search path on top of the existing built-in search stack.
- Gate the feature through the existing experimental feature system and persist the toggle in `config.toml`.
- Define a configuration model for LSP startup that supports both explicit configuration and common-language auto-detection.
- Surface agent-usable structured tools for symbol, definition, reference, and document-symbol lookups.
- Provide clear info/warn notifications when a language server is needed but unavailable.
- Normalize user-level role presets onto the native agent-role system and provide curated presets for common single-agent and team workflows.

**Non-Goals:**
- Building a full IDE-grade editor or navigation UI in the first phase.
- Supporting code actions, rename, diagnostics-driven edits, or write-back flows through LSP in the first phase.
- Reworking the plugin architecture or making plugins the primary delivery path.
- Adding a new team-preset DSL in the first phase.
- Adding runtime compatibility for legacy `~/.codex/agents/*.md` files.

## Decisions

### 1. Gate the feature through `[features]` and expose it via `/experimental`
The feature will be added to the existing `Feature` registry as a new experimental feature, tentatively `internal_code_search`. The user-visible enable/disable entry appears in the existing experimental-feature surfaces, while the persisted state is written into `config.toml` under `[features]` using the current feature-flag pipeline.

Why this choice:
- It reuses existing feature metadata, menu rendering, config writing, and layer merging.
- It avoids inventing a parallel `[experimental]` settings table.
- It allows the feature to be rolled out, hidden, or promoted using the same lifecycle model as other experimental capabilities.

Alternatives considered:
- Separate `[experimental]` config section: rejected because it duplicates the existing feature toggle system.
- Always-on behavior with only `code_search.enabled`: rejected because the user asked for an explicit experimental on-ramp and the repository already has one.

### 2. Split product gating from runtime configuration
The feature gate controls whether code-search APIs and model tools are exposed. Runtime behavior is configured separately in a new `code_search` config section.

Expected shape:
- `[features] internal_code_search = true|false`
- `[code_search] enabled = true|false, auto_detect = true|false`
- `[code_search.lsp.<language>] command = [..]`

Why this choice:
- Feature exposure and execution settings are different concerns.
- It allows `/experimental` to turn the feature on without forcing a single hardcoded runtime configuration forever.
- It makes future UI writing safe: toggles write `[features]`, advanced settings write `[code_search]`.

Alternatives considered:
- Single boolean feature flag only: rejected because it cannot express per-language startup commands.
- `code_search.enabled` only, no feature gate: rejected because it weakens rollout control.

### 3. First-phase config writes only user-level settings
When enabled from CLI/TUI experimental surfaces, the feature writes to `~/.codex/config.toml`. Project-level `.codex/config.toml` remains readable and can override behavior when trusted, but first-phase UI flows do not edit project config.

Why this choice:
- It keeps the write path simple and low-risk.
- It matches how a user expects `/experimental` toggles to behave.
- It avoids project trust and multi-root policy complexity in the first release.

Alternatives considered:
- Editing project config from `/experimental`: rejected for first phase because it introduces trust and ownership questions.
- No automatic config writes: rejected because the user explicitly wants the feature to be enabled through CLI and persisted to config.

### 4. LSP discovery uses “explicit config first, common-language auto-detect second”
The runtime will resolve an LSP server using this order:
1. Explicit `[code_search.lsp.<language>]` command configuration.
2. If `code_search.auto_detect = true`, try common defaults for a small supported language set.
3. If neither path yields a runnable server, warn and fall back to existing fuzzy/grep/read flows.

First-phase common-language targets are Rust, TypeScript/JavaScript, Python, and Go.

Why this choice:
- Explicit config gives determinism.
- Auto-detect gives acceptable out-of-box experience.
- The fallback preserves usefulness when local dependencies are missing.

Alternatives considered:
- Explicit config only: rejected because setup friction would be too high for first-time users.
- Auto-detect only: rejected because power users need deterministic overrides and nonstandard command paths.

### 5. Missing LSP dependencies produce user-visible info/warn events, not hard failure
Two notification levels are required:
- `info`: feature enabled or language support detected, but no active lookup has yet failed.
- `warn`: a lookup attempted to use a language server, but the configured or default command was unavailable; the system fell back to existing search.

Warnings must include the language, missing command, fallback behavior, and a brief install hint. Repeated warnings for the same `(workspace, language, command)` should be deduplicated within a session.

Why this choice:
- Missing binaries are common and should not break turns.
- The user still needs to understand why structured results were unavailable.
- Deduplication avoids noisy repeated warnings in long sessions.

Alternatives considered:
- Failing the request: rejected because graceful fallback already exists.
- Silent fallback: rejected because it hides a fixable local setup problem.

### 6. First-phase app-server contract adds `codeSearch/*` endpoints next to existing fuzzy search
The app-server keeps `fuzzyFileSearch` intact and adds a parallel experimental API family for code-aware search. The initial surface includes:
- `codeSearch/symbol`
- `codeSearch/definition`
- `codeSearch/references`
- `codeSearch/documentSymbol`

Responses return structured paths/URIs, ranges, symbol metadata, and source provenance so both UI clients and agents can consume the same results.

Why this choice:
- It preserves backward compatibility for existing fuzzy file search clients.
- It makes code-aware queries explicit instead of overloading one endpoint with incompatible shapes.
- It creates a stable seam for future plugin/MCP-backed providers.

Alternatives considered:
- Extending `fuzzyFileSearch` with code-aware modes: rejected because path search and semantic location lookup have different request/response semantics.

### 7. Core model tools expose high-signal operations rather than raw LSP methods
The model-facing tool layer will expose a small set of task-oriented tools, not raw protocol names. The initial tool set is:
- `find_code_symbols`
- `find_definitions`
- `find_references`

`documentSymbol` can stay behind app-server/UI wiring or be folded into `find_code_symbols` depending on implementation detail, but the model surface stays small.

Why this choice:
- Smaller tool surfaces are easier for the model to use correctly.
- The abstraction allows backends to evolve without retraining the model on protocol details.
- It makes fallback behavior easier to encapsulate.

Alternatives considered:
- Exposing raw `workspace/symbol` and similar LSP calls directly: rejected because it leaks transport detail into the tool contract.

### 8. Native role presets remain config-native; legacy Markdown files are migration input only
The supported runtime path is `[agents.<role>]` in `config.toml` plus TOML role files such as `~/.codex/agents/<role>.toml`. Existing Markdown files under `~/.codex/agents/*.md` are treated as user content that can inform migration, but they will not be added to the runtime loader.

Why this choice:
- It aligns with the current implementation and tests.
- It keeps the loader contract narrow and typed.
- It avoids permanently coupling this fork to another tool’s preset format.

Alternatives considered:
- Runtime compatibility with `*.md`: rejected because it expands parsing surface and keeps an unsupported format alive indefinitely.

## Risks / Trade-offs

- [Local LSP binaries vary across machines] → Mitigation: explicit per-language config, auto-detect only for a small initial language set, warn and graceful fallback.
- [Cross-cutting implementation scope could expand] → Mitigation: keep first phase read-only, limit API surface to four search operations, and defer plugin/team-preset DSL work.
- [Feature and config duplication could confuse users] → Mitigation: document feature gate vs runtime config responsibilities clearly in config and app-server docs.
- [Warning spam during repeated searches] → Mitigation: deduplicate warnings per session and only escalate to warn when a real lookup needed the missing server.
- [Role preset migration may drift from user expectations] → Mitigation: ship curated native presets, document the supported format, and avoid pretending legacy Markdown files are already active.

## Migration Plan

1. Add the new experimental feature flag and expose it through existing feature-list and config-write surfaces.
2. Add `code_search` config parsing and default handling without exposing new APIs yet.
3. Implement the LSP manager and app-server `codeSearch/*` endpoints behind the feature gate.
4. Expose agent-facing tools and fallback behavior only when the feature is enabled.
5. Add docs for enabling the feature, configuring language servers, and understanding warn/info fallback messages.
6. Add native role preset examples and migration guidance for `~/.codex/agents/*.toml` plus `[agents.<role>]` declarations.
7. Roll back by disabling `[features].internal_code_search`; existing fuzzy file search and native role loading continue to work.

## Open Questions

- The exact config key names under `[code_search]` need to match repository naming conventions once implementation starts, but the structure and ownership are fixed.
- Whether `documentSymbol` is exposed as a dedicated model tool or remains app-server-only can be finalized during implementation without changing the external capability contract.
