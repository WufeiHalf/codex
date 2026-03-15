---
name: lsp-regression
description: Run the Codex fork LSP regression workflow end to end: clean prior artifacts, enforce disk-space guardrails, build/install the packaged binary into the fork path, launch with .codex-fork config, and verify that a real agent lookup succeeds with backend=lsp. Use when validating internal code search, LSP auto-install, or packaged-binary regressions.
---

# LSP Regression

Use this skill for LSP-related validation in this repository when the task is not just unit tests, but the real packaged-binary workflow.

## Required Workflow

1. Clean prior artifacts before every run:
   - Delete `/Users/wufei/.local/bin/codex-fork` if present.
   - Remove the repository-root `out=` artifact directory if present.
   - Remove `CODEX_HOME/lsp` for the validation home (for example `~/.codex-fork/lsp`) so stale managed language-server installs do not mask the current build.
   - Clean `codex-rs/target`.
2. Check free disk space after cleanup.
   - Do not continue unless at least 15 GiB is available.
3. Build/install the packaged binary from the repo root:
   - `OUT=/Users/wufei/.local/bin/codex-fork make release-codex`
   - Or `just release-codex out="/Users/wufei/.local/bin/codex-fork"`
4. Use `CODEX_HOME="$HOME/.codex-fork"` for the validation run.
5. Validate against `~/.codex-fork/config.toml`.
   - Unless the task says otherwise, ensure internal code search is enabled there:
     ```toml
     [features]
     internal_code_search = true

     [code_search]
     enabled = true
     auto_detect = true
     auto_install = true
     ```
6. Run a real agent lookup in the current repository.
   - Ask for code retrieval that should naturally use symbol/definition/reference lookup.
   - Treat the run as successful only if the lookup succeeds and logs or structured output confirm `backend=lsp`.

## Failure Handling

- If free space drops below 15 GiB, stop and report the disk-space blocker.
- If the packaged binary builds but the lookup uses fallback or `unavailable`, treat that as a failed regression rather than a partial pass.
- When the lookup fails, capture which stage broke:
  - install cleanup
  - disk preflight
  - release build/install
  - config under `.codex-fork`
  - LSP install/startup
  - agent/tool selection

## Output Expectations

- Report the cleanup actions performed.
- Report the free-space check result.
- Report the exact packaged binary path used.
- Report whether the final lookup hit `backend=lsp`.
- Do not declare success without the real agent validation step.
