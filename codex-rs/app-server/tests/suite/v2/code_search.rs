use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use app_test_support::DEFAULT_CLIENT_NAME;
use app_test_support::McpProcess;
use app_test_support::create_mock_responses_server_sequence_unchecked;
use app_test_support::to_response;
use app_test_support::write_mock_responses_config_toml;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::CodeSearchBackend;
use codex_app_server_protocol::CodeSearchDefinitionParams;
use codex_app_server_protocol::CodeSearchDocumentSymbolParams;
use codex_app_server_protocol::CodeSearchDocumentSymbolResponse;
use codex_app_server_protocol::CodeSearchReferencesParams;
use codex_app_server_protocol::CodeSearchReferencesResponse;
use codex_app_server_protocol::CodeSearchSymbolParams;
use codex_app_server_protocol::CodeSearchSymbolResponse;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_core::features::Feature;
use pretty_assertions::assert_eq;
use serde::de::DeserializeOwned;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::MockServer;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const INVALID_REQUEST_ERROR_CODE: i64 = -32600;

#[tokio::test]
async fn code_search_symbol_requires_experimental_api_capability() -> Result<()> {
    let codex_home = TempDir::new()?;
    let _server = write_code_search_config(codex_home.path(), true, "").await?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    let init = mcp
        .initialize_with_capabilities(
            ClientInfo {
                name: DEFAULT_CLIENT_NAME.to_string(),
                title: None,
                version: "0.1.0".to_string(),
            },
            Some(InitializeCapabilities {
                experimental_api: false,
                opt_out_notification_methods: None,
            }),
        )
        .await?;
    let JSONRPCMessage::Response(_) = init else {
        anyhow::bail!("expected initialize response, got {init:?}");
    };

    let request_id = mcp
        .send_code_search_symbol_request(CodeSearchSymbolParams {
            query: "Widget".to_string(),
            roots: vec![codex_home.path().display().to_string()],
        })
        .await?;

    let error = read_error(&mut mcp, request_id).await?;
    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(
        error.error.message,
        "codeSearch/symbol requires experimentalApi capability"
    );

    Ok(())
}

#[tokio::test]
async fn code_search_definition_rejects_when_feature_is_disabled() -> Result<()> {
    let codex_home = TempDir::new()?;
    let source = write_workspace_file(
        codex_home.path(),
        "workspace/lib.rs",
        "pub struct Widget;\nfn use_it() { let _ = Widget; }\n",
    )?;
    let _server = write_code_search_config(codex_home.path(), false, "").await?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_code_search_definition_request(CodeSearchDefinitionParams {
            path: Some(source.display().to_string()),
            uri: None,
            start_line: 2,
            start_column: 23,
            end_line: 2,
            end_column: 28,
        })
        .await?;

    let error = read_error(&mut mcp, request_id).await?;
    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(error.error.message, "internal code search is disabled");

    Ok(())
}

#[tokio::test]
async fn code_search_references_fall_back_when_lsp_is_not_configured() -> Result<()> {
    let codex_home = TempDir::new()?;
    let source = write_workspace_file(
        codex_home.path(),
        "workspace/lib.rs",
        "pub struct Widget;\nfn use_it() { let _ = Widget; }\n",
    )?;
    let expected_source = source.canonicalize()?;
    let _server = write_code_search_config(
        codex_home.path(),
        true,
        "[code_search]\nauto_detect = false\n",
    )
    .await?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_code_search_references_request(CodeSearchReferencesParams {
            path: Some(source.display().to_string()),
            uri: None,
            start_line: 2,
            start_column: 23,
            end_line: 2,
            end_column: 28,
        })
        .await?;

    let response: CodeSearchReferencesResponse = read_response(&mut mcp, request_id).await?;
    let mut lines = response
        .locations
        .iter()
        .map(|location| {
            location
                .location
                .range
                .as_ref()
                .map(|range| range.start.line)
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    lines.sort_unstable();

    assert_eq!(response.provenance.backend, CodeSearchBackend::GrepFallback);
    assert_eq!(response.provenance.provider.as_deref(), Some("rg"));
    assert_eq!(response.provenance.language.as_deref(), Some("rust"));
    assert_eq!(response.provenance.resolution_source, None);
    assert!(!response.provenance.install_attempted);
    assert_eq!(response.locations.len(), 2);
    assert_eq!(lines, vec![1, 2]);
    assert!(response.locations.iter().all(|location| {
        location.location.document.path.as_deref()
            == Some(expected_source.to_string_lossy().as_ref())
    }));
    assert_eq!(response.notice, None);
    assert!(
        response.warning.as_deref().is_some_and(
            |message| message.contains("reference lookup fell back to existing search")
        )
    );

    Ok(())
}

#[tokio::test]
async fn code_search_document_symbol_returns_unavailable_when_missing_lsp_finds_nothing()
-> Result<()> {
    let codex_home = TempDir::new()?;
    let source = write_workspace_file(
        codex_home.path(),
        "workspace/empty.rs",
        "// intentionally left blank\n",
    )?;
    let _server = write_code_search_config(
        codex_home.path(),
        true,
        "[code_search]\nauto_detect = false\n",
    )
    .await?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_code_search_document_symbol_request(CodeSearchDocumentSymbolParams {
            path: Some(source.display().to_string()),
            uri: None,
        })
        .await?;

    let response: CodeSearchDocumentSymbolResponse = read_response(&mut mcp, request_id).await?;

    assert!(response.symbols.is_empty());
    assert_eq!(response.provenance.backend, CodeSearchBackend::Unavailable);
    assert_eq!(response.provenance.provider, None);
    assert_eq!(response.provenance.language.as_deref(), Some("rust"));
    assert_eq!(response.provenance.resolution_source, None);
    assert!(!response.provenance.install_attempted);
    assert_eq!(response.notice, None);
    assert!(response.warning.as_deref().is_some_and(|message| {
        message.contains("document symbol lookup fell back to existing search")
    }));

    Ok(())
}

#[tokio::test]
async fn code_search_symbol_uses_configured_fake_lsp_when_available() -> Result<()> {
    let python = ["python3", "python"].into_iter().find(|command| {
        Command::new(command)
            .arg("--version")
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    });
    let Some(python) = python else {
        eprintln!("skipping fake-LSP code search test: python is unavailable");
        return Ok(());
    };

    let codex_home = TempDir::new()?;
    let source = write_workspace_file(
        codex_home.path(),
        "workspace/lib.rs",
        "pub struct Widget;\n",
    )?;
    let expected_source = source.canonicalize()?;
    let script_path = codex_home.path().join("fake_lsp.py");
    fs::write(
        &script_path,
        r#"import json
import sys
from pathlib import Path

source_path = Path(sys.argv[1]).resolve()

def read_message():
    content_length = None
    while True:
        header = sys.stdin.buffer.readline()
        if not header:
            return None
        if header in (b"\r\n", b"\n"):
            break
        if header.lower().startswith(b"content-length:"):
            content_length = int(header.split(b":", 1)[1].strip())
    if content_length is None:
        return None
    payload = sys.stdin.buffer.read(content_length)
    return json.loads(payload)

def send(message):
    payload = json.dumps(message).encode("utf-8")
    sys.stdout.buffer.write(f"Content-Length: {len(payload)}\r\n\r\n".encode("ascii"))
    sys.stdout.buffer.write(payload)
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"capabilities": {}}})
        continue
    if method == "workspace/symbol":
        send({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": [{
                "name": "Widget",
                "kind": 23,
                "containerName": "demo",
                "location": {
                    "uri": source_path.as_uri(),
                    "range": {
                        "start": {"line": 0, "character": 11},
                        "end": {"line": 0, "character": 17}
                    }
                }
            }]
        })
        continue
    if "id" in message:
        send({"jsonrpc": "2.0", "id": message["id"], "result": None})
"#,
    )?;

    let code_search_config = format!(
        r#"[code_search]
auto_detect = false

[code_search.lsp.rust]
command = ["{python}", "{script_path}", "{source_path}"]
"#,
        python = python.replace('\\', "\\\\").replace('"', "\\\""),
        script_path = script_path
            .display()
            .to_string()
            .replace('\\', "\\\\")
            .replace('"', "\\\""),
        source_path = source
            .display()
            .to_string()
            .replace('\\', "\\\\")
            .replace('"', "\\\""),
    );
    let _server = write_code_search_config(codex_home.path(), true, &code_search_config).await?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_code_search_symbol_request(CodeSearchSymbolParams {
            query: "Widget".to_string(),
            roots: vec![codex_home.path().join("workspace").display().to_string()],
        })
        .await?;

    let response: CodeSearchSymbolResponse = read_response(&mut mcp, request_id).await?;
    let symbol = response
        .matches
        .first()
        .context("expected one symbol match")?;

    assert_eq!(response.provenance.backend, CodeSearchBackend::Lsp);
    assert_eq!(response.provenance.provider.as_deref(), Some(python));
    assert_eq!(response.provenance.language.as_deref(), Some("rust"));
    assert_eq!(
        response.provenance.resolution_source.as_deref(),
        Some("explicit")
    );
    assert!(!response.provenance.install_attempted);
    assert_eq!(response.matches.len(), 1);
    assert_eq!(symbol.name, "Widget");
    assert_eq!(symbol.kind.as_deref(), Some("struct"));
    assert_eq!(symbol.container_name.as_deref(), Some("demo"));
    assert_eq!(
        symbol.location.document.path.as_deref(),
        Some(expected_source.to_string_lossy().as_ref())
    );
    assert_eq!(
        symbol.location.range.as_ref().map(|range| range.start.line),
        Some(1)
    );
    assert_eq!(
        symbol
            .location
            .range
            .as_ref()
            .map(|range| range.start.column),
        Some(12)
    );
    assert_eq!(response.notice, None);
    assert_eq!(response.warning, None);

    Ok(())
}

async fn write_code_search_config(
    codex_home: &Path,
    feature_enabled: bool,
    code_search_toml: &str,
) -> Result<MockServer> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let mut feature_flags = BTreeMap::new();
    feature_flags.insert(Feature::InternalCodeSearch, feature_enabled);
    write_mock_responses_config_toml(
        codex_home,
        &server.uri(),
        &feature_flags,
        1_000,
        None,
        "mock_provider",
        "Summarize the conversation.",
    )
    .context("write code search config")?;

    if !code_search_toml.is_empty() {
        let config_path = codex_home.join("config.toml");
        let mut config_toml = fs::read_to_string(&config_path)?;
        config_toml.push('\n');
        config_toml.push_str(code_search_toml);
        fs::write(config_path, config_toml)?;
    }

    Ok(server)
}

fn write_workspace_file(codex_home: &Path, relative_path: &str, contents: &str) -> Result<PathBuf> {
    let path = codex_home.join(relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, contents)?;
    Ok(path)
}

async fn read_response<T: DeserializeOwned>(mcp: &mut McpProcess, request_id: i64) -> Result<T> {
    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    to_response(response)
}

async fn read_error(mcp: &mut McpProcess, request_id: i64) -> Result<JSONRPCError> {
    timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await?
}
