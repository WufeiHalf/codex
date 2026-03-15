## ADDED Requirements

### Requirement: LSP regression builds SHALL enforce cleanup and disk-space guardrails
The LSP regression workflow SHALL remove stale install artifacts, clean previous build artifacts, and refuse to continue when local free space is below the required threshold.

#### Scenario: Previous install artifacts are removed before a regression build
- **WHEN** the LSP regression workflow starts
- **THEN** it SHALL delete `/Users/wufei/.local/bin/codex-fork` if present, SHALL remove the repository-root `out=` artifact directory if present, and SHALL clear the validation home's managed LSP install directory (for example `~/.codex-fork/lsp`) before building

#### Scenario: Build artifacts are cleaned before packaging
- **WHEN** the LSP regression workflow starts
- **THEN** it SHALL clean `codex-rs/target` before invoking the packaged release build

#### Scenario: Low disk space blocks the regression run
- **WHEN** available disk space after cleanup is below 15 GiB
- **THEN** the workflow SHALL stop and report the disk-space failure instead of proceeding to build and install

### Requirement: Final LSP validation SHALL use the packaged binary and a real agent lookup
The regression workflow SHALL validate the same packaged binary and `CODEX_HOME` path that the user uses locally, and it SHALL only pass when a real agent lookup succeeds through LSP.

#### Scenario: Packaged binary is installed to the required path
- **WHEN** the regression build succeeds
- **THEN** the workflow SHALL install the binary to `/Users/wufei/.local/bin/codex-fork` and SHALL run it with `CODEX_HOME="$HOME/.codex-fork"`

#### Scenario: Validation uses the `.codex-fork` configuration directory
- **WHEN** the packaged binary is launched for LSP validation
- **THEN** it SHALL read configuration from `~/.codex-fork/config.toml`

#### Scenario: Real agent lookup is mandatory for completion
- **WHEN** the final regression prompt asks the agent to retrieve code using internal code search
- **THEN** the workflow SHALL only pass if the lookup succeeds and the resulting trace or structured output confirms `backend=lsp`
