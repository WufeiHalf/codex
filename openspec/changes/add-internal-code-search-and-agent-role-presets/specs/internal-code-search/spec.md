## ADDED Requirements

### Requirement: Experimental internal code search gate
The system SHALL expose internal code search only when the corresponding experimental feature flag is enabled through the existing feature-flag system, and the enabled state SHALL persist in `config.toml`.

#### Scenario: Enable from experimental controls
- **WHEN** a user enables the internal code search experimental feature from a supported CLI or UI control
- **THEN** the system stores the enabled state in the user's `config.toml` feature settings

#### Scenario: Feature disabled
- **WHEN** the internal code search feature flag is disabled
- **THEN** the system does not expose `codeSearch/*` APIs or agent-facing internal code-search tools

### Requirement: Code search runtime configuration
The system SHALL read code-search runtime settings from layered Codex configuration, with user-level config as the default write target and trusted project config able to override read behavior.

#### Scenario: User-level default configuration
- **WHEN** a user enables internal code search and no code-search runtime config exists yet
- **THEN** the system creates or updates user-level config so the feature has a valid default runtime configuration

#### Scenario: Trusted project override
- **WHEN** a trusted project defines code-search runtime settings in project `.codex/config.toml`
- **THEN** the system uses those settings in preference to user defaults for that project

### Requirement: LSP server resolution order
The system SHALL resolve language server startup commands by preferring explicit per-language configuration and only falling back to common-language auto-detection when auto-detect is enabled.

#### Scenario: Explicit language server configured
- **WHEN** a supported language has an explicit configured command
- **THEN** the system starts that command instead of using a detected default

#### Scenario: Auto-detect fallback
- **WHEN** no explicit command is configured for a supported language and auto-detect is enabled
- **THEN** the system attempts the language's built-in default command

### Requirement: Missing language servers degrade gracefully
The system SHALL not fail an entire lookup or turn solely because a required language server command is unavailable, and SHALL fall back to existing built-in search behavior.

#### Scenario: Missing command during lookup
- **WHEN** a code-search request requires a language server and the configured or detected command cannot be executed
- **THEN** the system emits a warning and falls back to existing fuzzy or text-based search behavior

#### Scenario: Repeated missing command
- **WHEN** the same workspace, language, and missing command are encountered multiple times in one session
- **THEN** the system suppresses duplicate warnings after the first emitted warning

### Requirement: User-visible code search dependency notifications
The system SHALL provide user-visible notifications that distinguish between informational setup state and actionable missing-dependency fallback.

#### Scenario: Informational availability notice
- **WHEN** internal code search is enabled and supported-language discovery completes without an active failed lookup
- **THEN** the system may emit an informational notice describing available or expected language-server support

#### Scenario: Actionable fallback warning
- **WHEN** a code-search request falls back because the required language server is unavailable
- **THEN** the warning identifies the language, missing command, fallback behavior, and an installation hint

### Requirement: Structured app-server code search endpoints
The app-server SHALL provide experimental structured endpoints for symbol search, definition lookup, reference lookup, and document symbol enumeration.

#### Scenario: Symbol lookup request
- **WHEN** a client calls the symbol search endpoint while the feature is enabled
- **THEN** the server returns structured symbol matches with path or URI, range, and symbol metadata

#### Scenario: Definition lookup request
- **WHEN** a client requests definitions for a symbol location while the feature is enabled
- **THEN** the server returns structured definition locations with path or URI and range information

### Requirement: Agent-facing structured code search tools
The system SHALL expose high-signal internal tools for agent use that encapsulate structured symbol, definition, and reference lookup rather than exposing raw LSP method names.

#### Scenario: Agent uses code-search tool
- **WHEN** an agent requests code navigation in a workspace with internal code search enabled
- **THEN** the system offers structured internal code-search tools that return normalized results suitable for follow-up reads

#### Scenario: Fallback inside tool flow
- **WHEN** a structured code-search tool cannot complete through LSP
- **THEN** the system returns a result or warning that preserves the agent's ability to continue with fallback search paths
