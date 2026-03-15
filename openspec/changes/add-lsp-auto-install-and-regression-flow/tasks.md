## 1. Config And Runtime Resolution

- [x] 1.1 Add `[code_search].auto_install` to config parsing, defaults, schema, and user-facing docs
- [x] 1.2 Add Codex-managed installed-server discovery plus per-language installer flows for Rust, Go, JavaScript, TypeScript, and Python
- [x] 1.3 Update language-server resolution order, install retry behavior, and fallback warnings so explicit commands remain authoritative
- [x] 1.4 Add tests for auto-install success, explicit-command failure, install failure, and backend tracing

## 2. Agent Experience And Visibility

- [x] 2.1 Update internal code-search tool descriptions and model guidance so enabled sessions prefer structured code-search tools before grep/file-search flows
- [x] 2.2 Suppress success-only LSP notices while preserving deduplicated fallback and install warnings
- [x] 2.3 Add structured trace fields for language, resolution source, install attempt, and backend so regressions can assert `backend=lsp`
- [x] 2.4 Verify app-server and tool outputs still degrade cleanly when LSP install or runtime startup fails

## 3. Regression Workflow And Repo Guidance

- [x] 3.1 Update `AGENTS.md` with the hard LSP build-and-validation rules: stale install cleanup, `codex-rs/target` cleanup, 15 GiB free-space minimum, `.codex-fork` config path, and real-agent `backend=lsp` validation
- [x] 3.2 Add a reusable project skill for the LSP regression workflow so future sessions can run the same packaged-binary validation steps consistently
- [x] 3.3 Document the OpenCode prior-art references used for the natural LSP UX decisions in the change artifacts
- [x] 3.4 Run the packaged-binary regression flow end to end and only close the implementation after a real agent lookup succeeds through `backend=lsp`
