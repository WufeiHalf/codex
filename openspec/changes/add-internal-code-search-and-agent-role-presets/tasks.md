## 1. Feature Gate And Config

- [x] 1.1 Add a new experimental feature flag for internal code search to the core feature registry and feature-list surfaces
- [x] 1.2 Add layered `code_search` config types and parsing for enablement, auto-detect, and per-language LSP commands
- [x] 1.3 Wire experimental enable/disable flows so CLI or UI feature toggles persist the feature flag in user `config.toml`
- [x] 1.4 Add tests covering feature gating, config parsing, and user-level persisted toggle behavior

## 2. Internal Code Search Runtime

- [x] 2.1 Add an internal LSP manager that resolves language servers using explicit config first and common-language auto-detect second
- [x] 2.2 Implement missing-dependency handling with info and warn notifications plus per-session deduplication
- [x] 2.3 Reuse existing file-search and grep paths as fallback behavior when LSP is unavailable or unsupported
- [x] 2.4 Add runtime tests for supported-language resolution, missing-command fallback, and warning deduplication

## 3. App-Server And Tooling Surface

- [x] 3.1 Add experimental `codeSearch/symbol`, `codeSearch/definition`, `codeSearch/references`, and `codeSearch/documentSymbol` app-server endpoints
- [x] 3.2 Add protocol types and tests for normalized structured code-search request and response payloads
- [x] 3.3 Expose high-signal agent-facing tools for symbol, definition, and reference lookup behind the feature gate
- [x] 3.4 Add integration tests covering enabled, disabled, success, fallback, and unavailable-LSP request flows

## 4. Native Agent Role Presets

- [x] 4.1 Document the supported native role preset format based on `[agents.<role>]` declarations and TOML role files
- [x] 4.2 Add a curated starter set of native role presets for single-agent workflows and common team compositions
- [x] 4.3 Document that legacy `~/.codex/agents/*.md` files are migration input only and are not active runtime presets
- [x] 4.4 Add tests or validation coverage for new preset examples and native role-loading behavior where applicable
