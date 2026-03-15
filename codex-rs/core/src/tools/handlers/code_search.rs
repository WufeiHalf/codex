use std::path::PathBuf;

use async_trait::async_trait;
use codex_app_server_protocol::TextPosition;
use codex_protocol::models::FunctionCallOutputBody;
use serde::Deserialize;
use serde_json::json;

use crate::code_search as runtime_search;
use crate::features::Feature;
use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

pub(crate) const FIND_CODE_SYMBOLS_TOOL_NAME: &str = "find_code_symbols";
pub(crate) const FIND_DEFINITIONS_TOOL_NAME: &str = "find_definitions";
pub(crate) const FIND_DOCUMENT_SYMBOLS_TOOL_NAME: &str = "find_document_symbols";
pub(crate) const FIND_REFERENCES_TOOL_NAME: &str = "find_references";

const DEFAULT_SYMBOL_LIMIT: usize = 20;

pub struct InternalCodeSearchHandler;

#[derive(Deserialize)]
struct FindCodeSymbolsArgs {
    query: String,
    #[serde(default)]
    roots: Option<Vec<String>>,
    #[serde(default)]
    language_hint: Option<String>,
    #[serde(default = "default_symbol_limit")]
    limit: usize,
}

#[derive(Deserialize)]
struct FindDefinitionsArgs {
    path: String,
    line: usize,
    column: usize,
    #[serde(default)]
    roots: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct FindReferencesArgs {
    path: String,
    line: usize,
    column: usize,
    #[serde(default)]
    roots: Option<Vec<String>>,
    #[serde(default)]
    include_declaration: bool,
}

#[derive(Deserialize)]
struct FindDocumentSymbolsArgs {
    path: String,
}

fn default_symbol_limit() -> usize {
    DEFAULT_SYMBOL_LIMIT
}

#[async_trait]
impl ToolHandler for InternalCodeSearchHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            tool_name,
            payload,
            ..
        } = invocation;

        if !session.features().enabled(Feature::InternalCodeSearch) {
            return Err(FunctionCallError::RespondToModel(
                "internal code search is disabled; enable the `internal_code_search` experimental feature before using these tools".to_string(),
            ));
        }

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "internal code search handler received unsupported payload".to_string(),
                ));
            }
        };

        let (body, success) = match tool_name.as_str() {
            FIND_CODE_SYMBOLS_TOOL_NAME => {
                let args: FindCodeSymbolsArgs = parse_arguments(&arguments)?;
                let outcome = runtime_search::find_symbols(
                    &turn.config,
                    runtime_search::SymbolSearchParams {
                        query: args.query,
                        cwd: turn.cwd.clone(),
                        roots: resolve_roots(&turn.cwd, args.roots)?,
                        language_hint: args.language_hint,
                        limit: args.limit,
                    },
                )
                .await
                .map_err(code_search_error)?;
                runtime_search::emit_session_notices(&session, &turn, &outcome.notices).await;
                let success = !outcome.data.is_empty();
                (serialize_symbol_outcome(outcome)?, success)
            }
            FIND_DEFINITIONS_TOOL_NAME => {
                let args: FindDefinitionsArgs = parse_arguments(&arguments)?;
                let outcome = runtime_search::find_definitions(
                    &turn.config,
                    runtime_search::DefinitionSearchParams {
                        path: resolve_required_path(&turn.cwd, args.path)?,
                        position: validate_position(args.line, args.column)?,
                        cwd: turn.cwd.clone(),
                        roots: resolve_roots(&turn.cwd, args.roots)?,
                    },
                )
                .await
                .map_err(code_search_error)?;
                runtime_search::emit_session_notices(&session, &turn, &outcome.notices).await;
                let success = !outcome.data.is_empty();
                (serialize_location_outcome(outcome)?, success)
            }
            FIND_DOCUMENT_SYMBOLS_TOOL_NAME => {
                let args: FindDocumentSymbolsArgs = parse_arguments(&arguments)?;
                let outcome = runtime_search::document_symbols(
                    &turn.config,
                    runtime_search::DocumentSymbolsParams {
                        path: resolve_required_path(&turn.cwd, args.path)?,
                        cwd: turn.cwd.clone(),
                    },
                )
                .await
                .map_err(code_search_error)?;
                runtime_search::emit_session_notices(&session, &turn, &outcome.notices).await;
                let success = !outcome.data.is_empty();
                (serialize_document_symbol_outcome(outcome)?, success)
            }
            FIND_REFERENCES_TOOL_NAME => {
                let args: FindReferencesArgs = parse_arguments(&arguments)?;
                let outcome = runtime_search::find_references(
                    &turn.config,
                    runtime_search::ReferencesSearchParams {
                        path: resolve_required_path(&turn.cwd, args.path)?,
                        position: validate_position(args.line, args.column)?,
                        cwd: turn.cwd.clone(),
                        roots: resolve_roots(&turn.cwd, args.roots)?,
                        include_declaration: args.include_declaration,
                    },
                )
                .await
                .map_err(code_search_error)?;
                runtime_search::emit_session_notices(&session, &turn, &outcome.notices).await;
                let success = !outcome.data.is_empty();
                (serialize_location_outcome(outcome)?, success)
            }
            other => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "unsupported internal code search tool {other}"
                )));
            }
        };

        Ok(ToolOutput::Function {
            body: FunctionCallOutputBody::Text(body),
            success: Some(success),
        })
    }
}

fn resolve_required_path(cwd: &PathBuf, path: String) -> Result<PathBuf, FunctionCallError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "path must not be empty".to_string(),
        ));
    }
    let path = PathBuf::from(trimmed);
    let resolved = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    ensure_workspace_path(cwd, resolved, "path")
}

fn resolve_roots(
    cwd: &PathBuf,
    roots: Option<Vec<String>>,
) -> Result<Vec<PathBuf>, FunctionCallError> {
    roots
        .unwrap_or_default()
        .into_iter()
        .map(|root| {
            let root = PathBuf::from(root);
            let resolved = if root.is_absolute() {
                root
            } else {
                cwd.join(root)
            };
            ensure_workspace_path(cwd, resolved, "root")
        })
        .collect()
}

fn ensure_workspace_path(
    cwd: &PathBuf,
    path: PathBuf,
    label: &str,
) -> Result<PathBuf, FunctionCallError> {
    let canonical_cwd = std::fs::canonicalize(cwd).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to resolve workspace root: {err}"))
    })?;
    let canonical_path = std::fs::canonicalize(&path).map_err(|err| {
        FunctionCallError::RespondToModel(format!(
            "{label} `{}` must exist within the current workspace: {err}",
            path.display()
        ))
    })?;

    if canonical_path.starts_with(&canonical_cwd) {
        Ok(canonical_path)
    } else {
        Err(FunctionCallError::RespondToModel(format!(
            "{label} `{}` must be within the current workspace",
            path.display()
        )))
    }
}

fn validate_position(line: usize, column: usize) -> Result<TextPosition, FunctionCallError> {
    if line == 0 || column == 0 {
        return Err(FunctionCallError::RespondToModel(
            "line and column must be 1-indexed values greater than zero".to_string(),
        ));
    }
    Ok(TextPosition { line, column })
}

fn code_search_error(error: runtime_search::CodeSearchError) -> FunctionCallError {
    FunctionCallError::RespondToModel(error.to_string())
}

fn serialize_symbol_outcome(
    outcome: runtime_search::CodeSearchOutcome<runtime_search::CodeSearchSymbol>,
) -> Result<String, FunctionCallError> {
    let payload = json!({
        "provenance": {
            "backend": outcome.backend.as_str(),
            "provider": outcome.provider,
            "language": outcome.trace.language,
            "resolutionSource": outcome.trace.resolution_source,
            "installAttempted": outcome.trace.install_attempted,
        },
        "notice": outcome.notices.info_message,
        "warning": outcome.notices.warning_message,
        "matches": outcome.data.into_iter().map(|symbol| json!({
            "name": symbol.name,
            "kind": symbol.kind,
            "containerName": symbol.container_name,
            "path": symbol.path.display().to_string(),
            "range": symbol.range,
        })).collect::<Vec<_>>(),
    });
    serde_json::to_string(&payload).map_err(serialization_error)
}

fn serialize_location_outcome(
    outcome: runtime_search::CodeSearchOutcome<runtime_search::CodeSearchLocation>,
) -> Result<String, FunctionCallError> {
    let payload = json!({
        "provenance": {
            "backend": outcome.backend.as_str(),
            "provider": outcome.provider,
            "language": outcome.trace.language,
            "resolutionSource": outcome.trace.resolution_source,
            "installAttempted": outcome.trace.install_attempted,
        },
        "notice": outcome.notices.info_message,
        "warning": outcome.notices.warning_message,
        "locations": outcome.data.into_iter().map(|location| json!({
            "path": location.path.display().to_string(),
            "range": location.range,
        })).collect::<Vec<_>>(),
    });
    serde_json::to_string(&payload).map_err(serialization_error)
}

fn serialize_document_symbol_outcome(
    outcome: runtime_search::CodeSearchOutcome<runtime_search::CodeSearchDocumentSymbol>,
) -> Result<String, FunctionCallError> {
    let payload = json!({
        "provenance": {
            "backend": outcome.backend.as_str(),
            "provider": outcome.provider,
            "language": outcome.trace.language,
            "resolutionSource": outcome.trace.resolution_source,
            "installAttempted": outcome.trace.install_attempted,
        },
        "notice": outcome.notices.info_message,
        "warning": outcome.notices.warning_message,
        "symbols": outcome.data.into_iter().map(serialize_document_symbol).collect::<Vec<_>>(),
    });
    serde_json::to_string(&payload).map_err(serialization_error)
}

fn serialize_document_symbol(
    symbol: runtime_search::CodeSearchDocumentSymbol,
) -> serde_json::Value {
    json!({
        "name": symbol.name,
        "kind": symbol.kind,
        "range": symbol.range,
        "selectionRange": symbol.selection_range,
        "detail": symbol.detail,
        "children": symbol.children.into_iter().map(serialize_document_symbol).collect::<Vec<_>>(),
    })
}

fn serialization_error(error: serde_json::Error) -> FunctionCallError {
    FunctionCallError::Fatal(format!(
        "failed to serialize internal code search output: {error}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::TextRange;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[test]
    fn serialize_symbol_outcome_preserves_optional_ranges() {
        let serialized = serialize_symbol_outcome(runtime_search::CodeSearchOutcome {
            data: vec![runtime_search::CodeSearchSymbol {
                name: "Widget".to_string(),
                kind: Some("struct".to_string()),
                path: PathBuf::from("/repo/src/lib.rs"),
                range: None,
                container_name: Some("crate".to_string()),
            }],
            backend: runtime_search::CodeSearchBackend::FileSearchFallback,
            provider: Some("file-search".to_string()),
            notices: runtime_search::CodeSearchNotices {
                info_message: None,
                info_key: None,
                warning_message: Some("fallback".to_string()),
                warning_key: Some("warn".to_string()),
            },
            trace: runtime_search::CodeSearchTrace {
                language: Some("rust".to_string()),
                resolution_source: None,
                install_attempted: false,
            },
        })
        .expect("serialize symbol outcome");

        let value: serde_json::Value =
            serde_json::from_str(&serialized).expect("symbol output should be valid json");
        assert_eq!(
            value,
            json!({
                "provenance": {
                    "backend": "file_search_fallback",
                    "provider": "file-search",
                    "language": "rust",
                    "resolutionSource": null,
                    "installAttempted": false
                },
                "notice": null,
                "warning": "fallback",
                "matches": [
                    {
                        "name": "Widget",
                        "kind": "struct",
                        "containerName": "crate",
                        "path": "/repo/src/lib.rs",
                        "range": null
                    }
                ]
            })
        );
    }

    #[test]
    fn serialize_location_outcome_includes_ranges() {
        let serialized = serialize_location_outcome(runtime_search::CodeSearchOutcome {
            data: vec![runtime_search::CodeSearchLocation {
                path: PathBuf::from("/repo/src/lib.rs"),
                range: TextRange {
                    start: TextPosition { line: 4, column: 5 },
                    end: TextPosition {
                        line: 4,
                        column: 11,
                    },
                },
            }],
            backend: runtime_search::CodeSearchBackend::GrepFallback,
            provider: Some("rg".to_string()),
            notices: runtime_search::CodeSearchNotices {
                info_message: Some("notice".to_string()),
                info_key: Some("info".to_string()),
                warning_message: None,
                warning_key: None,
            },
            trace: runtime_search::CodeSearchTrace {
                language: Some("rust".to_string()),
                resolution_source: Some("managed_install".to_string()),
                install_attempted: true,
            },
        })
        .expect("serialize location outcome");

        let value: serde_json::Value =
            serde_json::from_str(&serialized).expect("location output should be valid json");
        assert_eq!(
            value,
            json!({
                "provenance": {
                    "backend": "grep_fallback",
                    "provider": "rg",
                    "language": "rust",
                    "resolutionSource": "managed_install",
                    "installAttempted": true
                },
                "notice": "notice",
                "warning": null,
                "locations": [
                    {
                        "path": "/repo/src/lib.rs",
                        "range": {
                            "start": { "line": 4, "column": 5 },
                            "end": { "line": 4, "column": 11 }
                        }
                    }
                ]
            })
        );
    }

    #[test]
    fn resolve_required_path_rejects_paths_outside_workspace() {
        let workspace = TempDir::new().expect("workspace temp dir");
        let outside = TempDir::new().expect("outside temp dir");
        let outside_path = outside.path().join("lib.rs");
        std::fs::write(&outside_path, "pub struct Widget;\n").expect("write outside file");

        let err = resolve_required_path(
            &workspace.path().to_path_buf(),
            outside_path.display().to_string(),
        )
        .expect_err("outside path should fail");

        let FunctionCallError::RespondToModel(message) = err else {
            panic!("expected model-facing error");
        };
        assert!(message.contains("must be within the current workspace"));
    }

    #[test]
    fn resolve_roots_rejects_roots_outside_workspace() {
        let workspace = TempDir::new().expect("workspace temp dir");
        let outside = TempDir::new().expect("outside temp dir");

        let err = resolve_roots(
            &workspace.path().to_path_buf(),
            Some(vec![outside.path().display().to_string()]),
        )
        .expect_err("outside root should fail");

        let FunctionCallError::RespondToModel(message) = err else {
            panic!("expected model-facing error");
        };
        assert!(message.contains("must be within the current workspace"));
    }
}
