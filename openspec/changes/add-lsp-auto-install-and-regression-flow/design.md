## Context

This fork already has the first phase of internal code search in place: `internal_code_search` is an experimental feature flag, `[code_search]` already carries `enabled`, `auto_detect`, and per-language `lsp.<language>.command`, and the runtime already prefers LSP-backed symbol/definition/reference lookup before falling back to built-in search.

What is still missing is the experience and validation model that the fork actually needs. Missing language servers currently stop at warning-plus-fallback instead of closing the loop with automatic installation. Successful LSP lookups still feel like an explicit experimental subsystem instead of the normal code-navigation path. Separately, there is no hard release-validation workflow that proves the packaged binary, the `~/.codex-fork/config.toml` config, and a real agent can still complete an LSP lookup end to end.

OpenCode provides useful prior art for the target UX. Its official LSP docs describe extension-driven automatic enablement, optional automatic downloads for built-in servers, and per-server configuration for `disabled`, `command`, `extensions`, `env`, and `initialization` (`https://opencode.ai/docs/lsp/`). Its public docs and README also show that LSP is configured as a normal runtime concern rather than a one-off manual step (`https://github.com/opencode-ai/opencode`, `https://opencode.ai/docs/lsp/`). This change should borrow the natural UX principles, but adapt them to this fork's existing feature gate and config layering instead of cloning OpenCode's config shape.

A second constraint is repository workflow discipline. The user-defined acceptance bar is now part of the change contract: LSP work is not done until the local packaged binary is rebuilt, installed into `/Users/wufei/.local/bin/codex-fork`, started with `CODEX_HOME="$HOME/.codex-fork"`, and verified by a real agent lookup that reports `backend=lsp`. The build workflow must also clean `codex-rs/target` first and preserve at least 15 GiB of free disk space.

## Goals / Non-Goals

**Goals:**
- Add an on-demand LSP auto-install path for the currently supported internal code-search languages.
- Keep `internal_code_search` as the product gate while extending `[code_search]` with runtime controls needed for the more natural UX.
- Make successful LSP-backed code navigation quiet and default-feeling for the agent, while still surfacing actionable fallback warnings.
- Add structured tracing so manual and automated regressions can prove whether a lookup really used LSP.
- Codify the packaged-binary LSP regression workflow in OpenSpec, `AGENTS.md`, and a reusable project skill.
- Anchor the design to concrete OpenCode reference behavior without expanding the first phase into full config parity.

**Non-Goals:**
- Replacing the existing experimental gate with an always-on stable feature.
- Adding the full OpenCode-style LSP config surface (`disabled`, `extensions`, `env`, `initialization`) in the first follow-up.
- Supporting new languages beyond the current internal code-search set in this change.
- Building a new installer daemon, background indexer, or IDE-style LSP feature set.
- Treating fallback grep/file-search behavior as a failure path to remove; it remains the resilience path.

## Decisions

### 1. Preserve the experimental feature gate and add `code_search.auto_install`
The feature gate remains `internal_code_search`; it continues to control tool exposure, app-server surface, and UX availability. Runtime policy stays in `[code_search]`, and this change adds `auto_install` there rather than creating a second feature flag or a new top-level section.

Why this choice:
- The current code already separates product exposure from runtime behavior.
- It keeps `/experimental` and config layering intact.
- It avoids a proliferation of partially overlapping toggles.

Alternatives considered:
- Make auto-install a separate experimental feature: rejected because install behavior is part of runtime policy, not a separate product surface.
- Remove the experimental gate and rely only on `[code_search]`: rejected because rollout control and UI entry already exist and are useful.

### 2. Resolve language servers in a fixed priority order
The runtime resolves a language server in this order:
1. Explicit `[code_search.lsp.<language>].command`
2. Codex-managed installed server path under `CODEX_HOME`
3. PATH-visible default command when `code_search.auto_detect = true`
4. On-demand install when `code_search.auto_install = true`
5. Existing fallback search

A broken explicit command is terminal for that attempt: the runtime warns and falls back instead of silently installing a different server.

Why this choice:
- Explicit user config must remain deterministic.
- Reusing Codex-managed installations avoids repeated package-manager work once a server was installed once.
- Auto-install stays opt-in and only runs when all cheaper paths failed.

Alternatives considered:
- Try auto-install before PATH detection: rejected because it wastes work on machines that already have a usable server.
- Replace invalid explicit commands automatically: rejected because it hides configuration bugs and weakens operator intent.

### 3. Use language-native installers and keep managed artifacts under `CODEX_HOME` when practical
The first follow-up supports the same language set as current code search: Rust, JavaScript, TypeScript, Python, and Go. Installation uses the native ecosystem entrypoint for each language, while placing reusable artifacts under `CODEX_HOME` when possible:
- Rust via `rustup component add rust-analyzer`
- Go via `GOBIN=$CODEX_HOME/lsp/bin go install ...`
- JavaScript/TypeScript/Python via `npm install --prefix $CODEX_HOME/lsp/npm ...`

Why this choice:
- It is the smallest practical implementation on top of the current supported language set.
- It avoids inventing a cross-language package format or binary hosting story.
- It mirrors OpenCode's principle of automatic downloads without needing full parity with its server catalog.

Alternatives considered:
- Build a unified downloader for every language server: rejected as too large and too fragile for the immediate follow-up.
- Support only manually installed servers: rejected because it does not solve the user's workflow problem.

### 4. Successful LSP usage becomes quiet, while fallback remains explicit and deduplicated
When a lookup succeeds via LSP, the system records structured trace fields such as `language`, `resolution_source`, `install_attempted`, and `backend`, but it does not emit a visible success notice to the session. The session only gets warnings when a configured server is broken, a required server is missing and auto-install is disabled, an install attempt fails, or an LSP runtime error forces fallback. Warnings remain deduplicated per relevant session key.

Why this choice:
- The target experience is that LSP is the normal path, not an exceptional event.
- Users still need actionable feedback when the normal path could not be used.
- Trace data is sufficient for regression verification without cluttering agent interactions.

Alternatives considered:
- Keep success notices for every LSP hit: rejected because it keeps LSP feeling experimental and noisy.
- Silence all fallback behavior: rejected because missing installers and broken commands need operator visibility.

### 5. Agent guidance should prefer structured code-search tools whenever the feature is enabled
The model-facing surface remains small (`find_code_symbols`, `find_definitions`, `find_references`), but the tool descriptions and system guidance should treat them as the default code-navigation path whenever `internal_code_search` is on. Generic grep/read flows remain the backup path when structured lookups fail or are unsupported.

Why this choice:
- The current problem is partly product behavior, not only runtime infrastructure.
- A small high-signal tool set is easier for the model to use well.
- It matches the user's requirement that later code retrieval should naturally prefer LSP without repeated emphasis.

Alternatives considered:
- Expose more raw LSP operations to the model: rejected because it increases surface area without improving default behavior.
- Leave prompt/tool guidance unchanged: rejected because the runtime alone will not make the experience feel natural.

### 6. Treat the packaged-binary regression flow as a hard acceptance gate, not a best-effort manual check
Every LSP-related change must satisfy the same release-style validation path:
1. Remove `/Users/wufei/.local/bin/codex-fork` and the repo-root `out=` artifact directory.
2. Clean `codex-rs/target` before building.
3. Verify at least 15 GiB of free disk space before continuing.
4. Build/install to `/Users/wufei/.local/bin/codex-fork` via `make release-codex` or `just release-codex` with the output path overridden.
5. Run with `CODEX_HOME="$HOME/.codex-fork"` and config from `~/.codex-fork/config.toml`.
6. Use a real agent lookup against the repository and only pass if the lookup succeeds and reports `backend=lsp`.

Why this choice:
- It is the only validation that matches the user's real runtime path.
- It prevents the change from being declared done when only unit/integration tests pass.
- It turns storage and cleanup constraints into first-class workflow rules instead of tribal knowledge.

Alternatives considered:
- Keep validation at unit/integration level only: rejected because it misses the actual packaged-binary path.
- Skip target cleanup when builds are incremental: rejected because local disk pressure is an explicit hard rule.

## Risks / Trade-offs

- [Native installers behave differently across machines] → Mitigation: keep the supported language list narrow, preserve explicit command overrides, and warn/fallback cleanly when install steps fail.
- [Automatic installation can introduce network and package-manager flakiness] → Mitigation: make `auto_install` opt-in, log install attempts explicitly, and keep fallback search available.
- [Quiet-success UX may hide whether LSP is really active] → Mitigation: add trace fields and make `backend=lsp` part of the regression contract.
- [Strict cleanup and disk guards make validation slower] → Mitigation: document the rule once in `AGENTS.md` and encapsulate it in a reusable skill so the workflow becomes repeatable instead of ad hoc.
- [OpenCode parity could sprawl if taken too literally] → Mitigation: copy the interaction principles only, and defer broader config parity to later follow-ups.

## Migration Plan

1. Extend the `code_search` config model with `auto_install` and document the intended resolution order.
2. Implement managed-install lookup plus per-language installer execution and failure handling.
3. Update agent/tool guidance and success/warning behavior so LSP becomes the quiet default path.
4. Add structured trace fields required for manual regression validation.
5. Update `AGENTS.md` with the hard LSP regression rules and add a project skill that encodes the same workflow.
6. Validate the change through the packaged-binary `.codex-fork` flow and treat failure to hit `backend=lsp` as a blocking regression.

## Open Questions

- Whether a later follow-up should adopt more of OpenCode's per-server config surface (`disabled`, `extensions`, `env`, `initialization`) without destabilizing the current config schema.
- Whether the longer-term installer catalog should expand beyond the current five-language internal code-search set.
