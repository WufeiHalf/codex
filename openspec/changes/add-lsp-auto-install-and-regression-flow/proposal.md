## Why

The current internal code-search integration already exposes LSP-backed lookups, but it still misses the workflow the fork actually needs: missing language servers are not installed automatically, the agent experience still feels like an explicit experimental path instead of the default code-navigation path, and regressions are not blocked by a fixed packaged-binary validation loop.

This needs to land now because the fork is already depending on internal code search for real code navigation, and repeated breakage will continue unless LSP enablement and regression validation are treated as first-class product behavior instead of ad hoc local setup.

## What Changes

- Add on-demand per-language LSP auto-install behind `[code_search].auto_install` while preserving `internal_code_search` as the product-level experimental gate.
- Make internal code search prefer LSP-backed results silently when successful, and only surface warnings when startup, installation, or runtime execution falls back to non-LSP search.
- Expand language-server resolution order to include Codex-managed installed servers before PATH auto-detection and before fallback search.
- Add structured tracing fields for code-search resolution and backend selection so real regressions can assert that lookups actually ran through `backend=lsp`.
- Define and document a mandatory LSP regression workflow: clean previous install artifacts, clean `codex-rs/target`, enforce a minimum of 15 GiB free disk space, build/install the packaged binary to `/Users/wufei/.local/bin/codex-fork`, run with `CODEX_HOME="$HOME/.codex-fork"`, and require a real agent LSP lookup to pass before calling the work complete.
- Update repository guidance in `AGENTS.md` and add a reusable project skill for the LSP regression workflow.
- Record relevant prior art from OpenCode's official LSP docs and repo docs so the implementation is anchored to concrete reference behavior instead of memory.

## Capabilities

### New Capabilities
- `natural-lsp-code-search`: Make internal code search behave like the default structured code-navigation path by adding on-demand LSP installation, quiet successful LSP usage, and stronger backend tracing.
- `lsp-regression-workflow`: Define the mandatory packaged-binary cleanup, disk-space guardrail, install path, and real-agent validation flow required for LSP-related changes.

### Modified Capabilities
- None.

## Impact

- Affected code: `codex-rs/core`, `codex-rs/cli`, `codex-rs/tui`, config/schema docs, root `AGENTS.md`, and `.codex/skills`.
- Affected runtime behavior: internal code-search resolution, warning/noise policy, auto-install flows, and developer regression workflow.
- Affected dependencies and systems: local language-server binaries, native package managers/installers (`rustup`, `go`, `npm`), packaged release build flow, and `CODEX_HOME`-scoped runtime state.
- Affected validation: LSP work is not complete until a packaged local install running from `~/.codex-fork/config.toml` proves that a real agent lookup succeeds through `backend=lsp`.
