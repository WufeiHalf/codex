## ADDED Requirements

### Requirement: Internal code search can auto-install supported language servers
The system SHALL keep `internal_code_search` as the product gate and SHALL allow on-demand installation through `[code_search].auto_install` for the supported internal code-search languages.

#### Scenario: On-demand install for a missing supported language server
- **WHEN** `internal_code_search` is enabled, `[code_search].enabled = true`, `[code_search].auto_detect = true`, `[code_search].auto_install = true`, and a code-search lookup needs a supported language whose explicit command, Codex-managed installed server path, and auto-detected default command are all unavailable
- **THEN** the system SHALL attempt the language-specific install flow, re-resolve the language server, and retry the lookup before falling back

#### Scenario: Explicit command failure does not trigger silent replacement
- **WHEN** a user configured `[code_search.lsp.<language>].command` and that command is unavailable or fails to launch
- **THEN** the system SHALL warn, SHALL fall back to existing search when possible, and SHALL NOT auto-install or substitute a different command for that lookup attempt

### Requirement: Internal code search SHALL prefer LSP quietly when it succeeds
When a supported language server is available, structured code-navigation lookups SHALL prefer LSP-backed results without emitting success-only session notices.

#### Scenario: Successful definition lookup records LSP backend without success noise
- **WHEN** a definition, reference, or symbol lookup succeeds through a working language server
- **THEN** the returned structured result or trace data SHALL identify `backend=lsp`, SHALL include `language`, `resolution_source`, and `install_attempted`, and the session SHALL NOT receive an additional success notice solely because LSP was used

#### Scenario: Install or runtime failure falls back with a deduplicated warning
- **WHEN** automatic installation fails or a launched language server fails at runtime
- **THEN** the system SHALL emit a deduplicated warning that explains the failed command or install attempt and SHALL return existing fallback search results when they are available

### Requirement: Agent code navigation SHALL prefer internal code-search tools when enabled
When `internal_code_search` is enabled, the agent-facing guidance and tool descriptions SHALL prefer structured code-search tools before generic grep or file-reading flows for code navigation tasks.

#### Scenario: Symbol lookup request favors structured code-search tools
- **WHEN** the agent needs to find a symbol, definition, or references in a repository where `internal_code_search` is enabled
- **THEN** the model guidance SHALL direct the agent toward `find_code_symbols`, `find_definitions`, or `find_references` before falling back to grep or broader file search
