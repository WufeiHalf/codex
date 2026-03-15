## ADDED Requirements

### Requirement: Native agent role preset format
The system SHALL treat native agent role presets as configuration-layer entities defined through `[agents.<role>]` declarations and TOML role config files.

#### Scenario: Native role declaration
- **WHEN** a role is declared in Codex config with a `config_file`
- **THEN** the system loads that role through the existing native role loader

#### Scenario: Missing role config file
- **WHEN** a declared role references a missing TOML config file
- **THEN** the system rejects the role configuration with a clear configuration error

### Requirement: Legacy Markdown preset files are not runtime presets
The system SHALL NOT treat legacy `~/.codex/agents/*.md` files as active runtime role presets.

#### Scenario: Legacy Markdown file present
- **WHEN** a legacy Markdown preset file exists under `~/.codex/agents`
- **THEN** the runtime loader ignores it for native role activation

### Requirement: Curated native role presets
The system SHALL provide a curated set of native role presets that cover common single-agent and multi-agent collaboration patterns.

#### Scenario: Single-agent preset set available
- **WHEN** a user inspects the provided native presets
- **THEN** the available set includes common planning, exploration, implementation, review, debugging, or documentation roles

#### Scenario: Team-oriented preset set available
- **WHEN** a user composes an agent team from provided native presets
- **THEN** the available set includes role combinations suitable for planning, parallel implementation, integration, and risk review

### Requirement: Native role preset migration guidance
The system SHALL document how users move from legacy migrated preset content into the supported native role format.

#### Scenario: User has migrated Markdown presets
- **WHEN** documentation describes agent preset setup for this fork
- **THEN** it explains that supported presets require `[agents.<role>]` declarations and TOML role files rather than Markdown-only files
