use crate::codex::Session;
use crate::codex::TurnContext;
use crate::config::Config;
use crate::protocol::EventMsg;
use crate::protocol::WarningEvent;
use anyhow::Context;
use codex_app_server_protocol::TextPosition;
use codex_app_server_protocol::TextRange;
use codex_file_search::FileSearchOptions;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::collections::HashSet;
use std::num::NonZero;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use thiserror::Error;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::Child;
use tokio::process::ChildStdin;
use tokio::process::ChildStdout;
use tokio::process::Command;
use tokio::time::sleep;
use tokio::time::timeout;

const MAX_LIMIT: usize = 200;
const LSP_TIMEOUT: Duration = Duration::from_secs(30);
const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
const LSP_EMPTY_RESULT_RETRY_DELAY: Duration = Duration::from_secs(2);
const LSP_EMPTY_RESULT_MAX_ATTEMPTS: usize = 8;
const MAX_FALLBACK_RESULTS: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeSearchBackend {
    Lsp,
    GrepFallback,
    FileSearchFallback,
    Unavailable,
}

impl CodeSearchBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lsp => "lsp",
            Self::GrepFallback => "grep_fallback",
            Self::FileSearchFallback => "file_search_fallback",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CodeSearchNotices {
    pub info_message: Option<String>,
    pub info_key: Option<String>,
    pub warning_message: Option<String>,
    pub warning_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CodeSearchTrace {
    pub language: Option<String>,
    pub resolution_source: Option<String>,
    pub install_attempted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeSearchOutcome<T> {
    pub data: Vec<T>,
    pub backend: CodeSearchBackend,
    pub provider: Option<String>,
    pub notices: CodeSearchNotices,
    pub trace: CodeSearchTrace,
}

impl<T> CodeSearchOutcome<T> {
    fn new(data: Vec<T>, backend: CodeSearchBackend) -> Self {
        Self {
            data,
            backend,
            provider: None,
            notices: CodeSearchNotices::default(),
            trace: CodeSearchTrace::default(),
        }
    }

    fn with_notices(mut self, notices: CodeSearchNotices) -> Self {
        self.notices = notices;
        self
    }

    fn with_provider(mut self, provider: Option<String>) -> Self {
        self.provider = provider;
        self
    }

    fn with_trace(
        mut self,
        language: Option<CodeSearchLanguage>,
        resolution_source: Option<LanguageServerSource>,
        install_attempted: bool,
    ) -> Self {
        self.trace = CodeSearchTrace {
            language: language.map(|value| value.key().to_string()),
            resolution_source: resolution_source
                .map(LanguageServerSource::as_str)
                .map(str::to_string),
            install_attempted,
        };
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeSearchLocation {
    pub path: PathBuf,
    pub range: TextRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeSearchSymbol {
    pub name: String,
    pub kind: Option<String>,
    pub path: PathBuf,
    pub range: Option<TextRange>,
    pub container_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeSearchDocumentSymbol {
    pub name: String,
    pub kind: Option<String>,
    pub range: TextRange,
    pub selection_range: TextRange,
    pub detail: Option<String>,
    pub children: Vec<CodeSearchDocumentSymbol>,
}

#[derive(Debug, Clone)]
pub struct SymbolSearchParams {
    pub query: String,
    pub cwd: PathBuf,
    pub roots: Vec<PathBuf>,
    pub language_hint: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct DefinitionSearchParams {
    pub path: PathBuf,
    pub position: TextPosition,
    pub cwd: PathBuf,
    pub roots: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ReferencesSearchParams {
    pub path: PathBuf,
    pub position: TextPosition,
    pub cwd: PathBuf,
    pub roots: Vec<PathBuf>,
    pub include_declaration: bool,
}

#[derive(Debug, Clone)]
pub struct DocumentSymbolsParams {
    pub path: PathBuf,
    pub cwd: PathBuf,
}

#[derive(Debug, Error)]
pub enum CodeSearchError {
    #[error("{0}")]
    InvalidRequest(String),
    #[error("failed to read `{path}`: {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid text position {line}:{column} for `{path}`")]
    InvalidPosition {
        path: PathBuf,
        line: usize,
        column: usize,
    },
    #[error("internal code search failed: {0}")]
    OperationFailed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeSearchLanguage {
    Rust,
    JavaScript,
    TypeScript,
    Python,
    Go,
}

impl CodeSearchLanguage {
    fn detect(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "rs" => Some(Self::Rust),
            "js" | "jsx" | "cjs" | "mjs" => Some(Self::JavaScript),
            "ts" | "tsx" | "cts" | "mts" => Some(Self::TypeScript),
            "py" => Some(Self::Python),
            "go" => Some(Self::Go),
            _ => None,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "rust" | "rs" => Some(Self::Rust),
            "javascript" | "js" => Some(Self::JavaScript),
            "typescript" | "ts" => Some(Self::TypeScript),
            "python" | "py" => Some(Self::Python),
            "go" | "golang" => Some(Self::Go),
            _ => None,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Python => "python",
            Self::Go => "go",
        }
    }

    fn display(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::JavaScript => "JavaScript",
            Self::TypeScript => "TypeScript",
            Self::Python => "Python",
            Self::Go => "Go",
        }
    }

    fn language_id(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Python => "python",
            Self::Go => "go",
        }
    }

    fn default_command(self) -> Vec<String> {
        match self {
            Self::Rust => vec!["rust-analyzer".to_string()],
            Self::JavaScript | Self::TypeScript => {
                vec![
                    "typescript-language-server".to_string(),
                    "--stdio".to_string(),
                ]
            }
            Self::Python => vec!["pyright-langserver".to_string(), "--stdio".to_string()],
            Self::Go => vec!["gopls".to_string()],
        }
    }

    fn heuristic_keywords(self) -> &'static [&'static str] {
        match self {
            Self::Rust => &[
                "fn ", "struct ", "enum ", "trait ", "mod ", "const ", "static ",
            ],
            Self::JavaScript | Self::TypeScript => &[
                "function ",
                "class ",
                "interface ",
                "type ",
                "const ",
                "let ",
                "var ",
            ],
            Self::Python => &["def ", "class "],
            Self::Go => &["func ", "type ", "var ", "const "],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodeSearchOperation {
    Symbol,
    Definition,
    References,
    DocumentSymbol,
}

impl CodeSearchOperation {
    fn description(self) -> &'static str {
        match self {
            Self::Symbol => "symbol lookup",
            Self::Definition => "definition lookup",
            Self::References => "reference lookup",
            Self::DocumentSymbol => "document symbol lookup",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLanguageServer {
    pub language: CodeSearchLanguage,
    pub command: Vec<String>,
    pub source: LanguageServerSource,
    pub install_attempted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageServerSource {
    Explicit,
    ManagedInstall,
    AutoDetected,
}

impl LanguageServerSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::ManagedInstall => "managed_install",
            Self::AutoDetected => "auto_detect",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LanguageServerResolution {
    Resolved(ResolvedLanguageServer),
    MissingCommand {
        language: CodeSearchLanguage,
        suggested_command: Vec<String>,
        reason: MissingCommandReason,
        explicit_command: bool,
        install_attempted: bool,
    },
    InstallFailed {
        language: CodeSearchLanguage,
        install_command: InstallCommand,
        error: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MissingCommandReason {
    NotConfigured,
    NotInstalled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstallCommand {
    argv: Vec<String>,
    env: Vec<(String, String)>,
}

impl InstallCommand {
    fn display(&self) -> String {
        let env = self
            .env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>();
        let argv = self.argv.join(" ");
        if env.is_empty() {
            argv
        } else {
            format!("{} {argv}", env.join(" "))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TextMatch {
    path: PathBuf,
    range: TextRange,
}

#[derive(Debug, Deserialize)]
struct JsonRpcErrorObject {
    code: i64,
    message: String,
}

#[derive(Debug)]
struct LspClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl LspClient {
    async fn start(
        command: &[String],
        process_cwd: &Path,
        workspace_root: &Path,
    ) -> Result<Self, CodeSearchError> {
        let Some(program) = command.first() else {
            return Err(CodeSearchError::OperationFailed(
                "language server command must not be empty".to_string(),
            ));
        };

        let mut child = Command::new(program);
        child
            .args(command.iter().skip(1))
            .current_dir(process_cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        let mut child = child.spawn().map_err(|err| {
            CodeSearchError::OperationFailed(format!(
                "failed to launch language server `{}`: {err}",
                command.join(" ")
            ))
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            CodeSearchError::OperationFailed("language server stdin unavailable".to_string())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            CodeSearchError::OperationFailed("language server stdout unavailable".to_string())
        })?;
        let mut client = Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        };

        let workspace_uri = path_to_uri_string(workspace_root)?;
        let workspace_name = workspace_root
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("workspace");
        let initialize_params = json!({
            "processId": std::process::id(),
            "clientInfo": {
                "name": "codex",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "rootUri": workspace_uri,
            "capabilities": {},
            "trace": "off",
            "workspaceFolders": [
                {
                    "uri": workspace_uri,
                    "name": workspace_name,
                }
            ],
        });
        let _: Value = client
            .request_value("initialize", initialize_params)
            .await?;
        client.notify("initialized", json!({})).await?;
        Ok(client)
    }

    async fn request_value(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<Value, CodeSearchError> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await?;

        loop {
            let message = self.read_message().await?;
            if let Some(request_id) = message.get("id")
                && message.get("method").is_some()
            {
                self.write_message(&json!({
                    "jsonrpc": "2.0",
                    "id": request_id.clone(),
                    "result": Value::Null,
                }))
                .await?;
                continue;
            }

            if message.get("id") != Some(&json!(id)) {
                continue;
            }

            if let Some(error) = message.get("error") {
                let error =
                    serde_json::from_value::<JsonRpcErrorObject>(error.clone()).map_err(|err| {
                        CodeSearchError::OperationFailed(format!(
                            "failed to parse language server error response: {err}"
                        ))
                    })?;
                return Err(CodeSearchError::OperationFailed(format!(
                    "language server request `{method}` failed with {}: {}",
                    error.code, error.message
                )));
            }

            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), CodeSearchError> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    async fn write_message(&mut self, message: &Value) -> Result<(), CodeSearchError> {
        let payload = serde_json::to_vec(message).map_err(|err| {
            CodeSearchError::OperationFailed(format!(
                "failed to serialize language server message: {err}"
            ))
        })?;
        let header = format!("Content-Length: {}\r\n\r\n", payload.len());
        timeout(LSP_TIMEOUT, self.stdin.write_all(header.as_bytes()))
            .await
            .map_err(|_| {
                CodeSearchError::OperationFailed(
                    "timed out writing language server headers".to_string(),
                )
            })?
            .map_err(|err| {
                CodeSearchError::OperationFailed(format!(
                    "failed to write language server headers: {err}"
                ))
            })?;
        timeout(LSP_TIMEOUT, self.stdin.write_all(&payload))
            .await
            .map_err(|_| {
                CodeSearchError::OperationFailed(
                    "timed out writing language server payload".to_string(),
                )
            })?
            .map_err(|err| {
                CodeSearchError::OperationFailed(format!(
                    "failed to write language server payload: {err}"
                ))
            })?;
        timeout(LSP_TIMEOUT, self.stdin.flush())
            .await
            .map_err(|_| {
                CodeSearchError::OperationFailed(
                    "timed out flushing language server stdin".to_string(),
                )
            })?
            .map_err(|err| {
                CodeSearchError::OperationFailed(format!(
                    "failed to flush language server stdin: {err}"
                ))
            })?;
        Ok(())
    }

    async fn read_message(&mut self) -> Result<Value, CodeSearchError> {
        let mut content_length = None;
        loop {
            let mut line = String::new();
            let bytes = timeout(LSP_TIMEOUT, self.stdout.read_line(&mut line))
                .await
                .map_err(|_| {
                    CodeSearchError::OperationFailed(
                        "timed out reading language server headers".to_string(),
                    )
                })?
                .map_err(|err| {
                    CodeSearchError::OperationFailed(format!(
                        "failed to read language server headers: {err}"
                    ))
                })?;
            if bytes == 0 {
                return Err(CodeSearchError::OperationFailed(
                    "language server closed stdout unexpectedly".to_string(),
                ));
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length:") {
                let length = value.trim().parse::<usize>().map_err(|err| {
                    CodeSearchError::OperationFailed(format!(
                        "invalid Content-Length from language server: {err}"
                    ))
                })?;
                content_length = Some(length);
            }
        }

        let length = content_length.ok_or_else(|| {
            CodeSearchError::OperationFailed(
                "language server message missing Content-Length".to_string(),
            )
        })?;
        let mut payload = vec![0_u8; length];
        timeout(LSP_TIMEOUT, self.stdout.read_exact(&mut payload))
            .await
            .map_err(|_| {
                CodeSearchError::OperationFailed(
                    "timed out reading language server payload".to_string(),
                )
            })?
            .map_err(|err| {
                CodeSearchError::OperationFailed(format!(
                    "failed to read language server payload: {err}"
                ))
            })?;
        serde_json::from_slice::<Value>(&payload).map_err(|err| {
            CodeSearchError::OperationFailed(format!(
                "failed to parse language server payload: {err}"
            ))
        })
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

pub(crate) async fn emit_session_notices(
    session: &Arc<Session>,
    turn: &TurnContext,
    notices: &CodeSearchNotices,
) {
    let notified = session.code_search_notified().await;
    let mut new_keys = Vec::new();

    if let (Some(info_key), Some(info_message)) = (&notices.info_key, &notices.info_message)
        && !notified.contains(info_key)
    {
        session
            .notify_background_event(turn, info_message.clone())
            .await;
        new_keys.push(info_key.clone());
    }

    if let (Some(warning_key), Some(warning_message)) =
        (&notices.warning_key, &notices.warning_message)
        && !notified.contains(warning_key)
    {
        session
            .send_event(
                turn,
                EventMsg::Warning(WarningEvent {
                    message: warning_message.clone(),
                }),
            )
            .await;
        new_keys.push(warning_key.clone());
    }

    if !new_keys.is_empty() {
        session.record_code_search_notified(new_keys).await;
    }
}

async fn resolve_language_server(
    config: &Config,
    language: CodeSearchLanguage,
) -> LanguageServerResolution {
    resolve_language_server_with_installers(config, language, &InstallerCommands::default()).await
}

#[derive(Debug, Clone)]
struct InstallerCommands {
    rustup: String,
    go: String,
    npm: String,
}

impl Default for InstallerCommands {
    fn default() -> Self {
        Self {
            rustup: "rustup".to_string(),
            go: "go".to_string(),
            npm: "npm".to_string(),
        }
    }
}

async fn resolve_language_server_with_installers(
    config: &Config,
    language: CodeSearchLanguage,
    installers: &InstallerCommands,
) -> LanguageServerResolution {
    let explicit_command = config
        .code_search
        .lsp
        .get(language.key())
        .and_then(|server| server.command.clone())
        .filter(|command| !command.is_empty());

    if let Some(command) = explicit_command {
        if command_exists(&command) {
            return LanguageServerResolution::Resolved(ResolvedLanguageServer {
                language,
                command,
                source: LanguageServerSource::Explicit,
                install_attempted: false,
            });
        }

        return LanguageServerResolution::MissingCommand {
            language,
            suggested_command: command,
            reason: MissingCommandReason::NotInstalled,
            explicit_command: true,
            install_attempted: false,
        };
    }

    if language == CodeSearchLanguage::Rust
        && let Err(err) =
            refresh_managed_rust_analyzer_wrapper_if_present(&config.codex_home, &installers.rustup)
                .await
    {
        tracing::warn!(error = %err, "failed to refresh managed rust-analyzer wrapper");
    }

    let managed_command = managed_language_server_command(config, language);
    if command_exists(&managed_command) {
        return LanguageServerResolution::Resolved(ResolvedLanguageServer {
            language,
            command: managed_command,
            source: LanguageServerSource::ManagedInstall,
            install_attempted: false,
        });
    }

    if config.code_search.auto_detect {
        let command = language.default_command();
        if command_exists(&command) {
            return LanguageServerResolution::Resolved(ResolvedLanguageServer {
                language,
                command,
                source: LanguageServerSource::AutoDetected,
                install_attempted: false,
            });
        }
    }

    if config.code_search.auto_install {
        let install_command = install_command_for_language(config, language, installers);
        match install_language_server(config, language, &install_command).await {
            Ok(()) => {
                let managed_command = managed_language_server_command(config, language);
                if command_exists(&managed_command) {
                    return LanguageServerResolution::Resolved(ResolvedLanguageServer {
                        language,
                        command: managed_command,
                        source: LanguageServerSource::ManagedInstall,
                        install_attempted: true,
                    });
                }

                return LanguageServerResolution::MissingCommand {
                    language,
                    suggested_command: managed_command,
                    reason: MissingCommandReason::NotInstalled,
                    explicit_command: false,
                    install_attempted: true,
                };
            }
            Err(error) => {
                return LanguageServerResolution::InstallFailed {
                    language,
                    install_command,
                    error,
                };
            }
        }
    }

    let reason = if config.code_search.auto_detect {
        MissingCommandReason::NotInstalled
    } else {
        MissingCommandReason::NotConfigured
    };
    LanguageServerResolution::MissingCommand {
        language,
        suggested_command: language.default_command(),
        reason,
        explicit_command: false,
        install_attempted: false,
    }
}

fn code_search_lsp_root(codex_home: &Path) -> PathBuf {
    codex_home.join("lsp")
}

fn managed_bin_dir(codex_home: &Path) -> PathBuf {
    code_search_lsp_root(codex_home).join("bin")
}

fn managed_npm_prefix(codex_home: &Path) -> PathBuf {
    code_search_lsp_root(codex_home).join("npm")
}

fn managed_npm_bin_dir(codex_home: &Path) -> PathBuf {
    managed_npm_prefix(codex_home)
        .join("node_modules")
        .join(".bin")
}

fn managed_script_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.cmd")
    } else {
        name.to_string()
    }
}

fn managed_binary_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn managed_language_server_command(config: &Config, language: CodeSearchLanguage) -> Vec<String> {
    match language {
        CodeSearchLanguage::Rust => vec![
            managed_bin_dir(&config.codex_home)
                .join(managed_script_name("rust-analyzer"))
                .display()
                .to_string(),
        ],
        CodeSearchLanguage::Go => vec![
            managed_bin_dir(&config.codex_home)
                .join(managed_binary_name("gopls"))
                .display()
                .to_string(),
        ],
        CodeSearchLanguage::JavaScript | CodeSearchLanguage::TypeScript => vec![
            managed_npm_bin_dir(&config.codex_home)
                .join(managed_script_name("typescript-language-server"))
                .display()
                .to_string(),
            "--stdio".to_string(),
        ],
        CodeSearchLanguage::Python => vec![
            managed_npm_bin_dir(&config.codex_home)
                .join(managed_script_name("pyright-langserver"))
                .display()
                .to_string(),
            "--stdio".to_string(),
        ],
    }
}

fn install_command_for_language(
    config: &Config,
    language: CodeSearchLanguage,
    installers: &InstallerCommands,
) -> InstallCommand {
    match language {
        CodeSearchLanguage::Rust => InstallCommand {
            argv: vec![
                installers.rustup.clone(),
                "component".to_string(),
                "add".to_string(),
                "rust-analyzer".to_string(),
                "rust-src".to_string(),
            ],
            env: Vec::new(),
        },
        CodeSearchLanguage::Go => InstallCommand {
            argv: vec![
                installers.go.clone(),
                "install".to_string(),
                "golang.org/x/tools/gopls@latest".to_string(),
            ],
            env: vec![(
                "GOBIN".to_string(),
                managed_bin_dir(&config.codex_home).display().to_string(),
            )],
        },
        CodeSearchLanguage::JavaScript | CodeSearchLanguage::TypeScript => InstallCommand {
            argv: vec![
                installers.npm.clone(),
                "install".to_string(),
                "--prefix".to_string(),
                managed_npm_prefix(&config.codex_home).display().to_string(),
                "typescript".to_string(),
                "typescript-language-server".to_string(),
            ],
            env: Vec::new(),
        },
        CodeSearchLanguage::Python => InstallCommand {
            argv: vec![
                installers.npm.clone(),
                "install".to_string(),
                "--prefix".to_string(),
                managed_npm_prefix(&config.codex_home).display().to_string(),
                "pyright".to_string(),
            ],
            env: Vec::new(),
        },
    }
}

async fn install_language_server(
    config: &Config,
    language: CodeSearchLanguage,
    install_command: &InstallCommand,
) -> Result<(), String> {
    match language {
        CodeSearchLanguage::Rust => {
            tokio::fs::create_dir_all(managed_bin_dir(&config.codex_home))
                .await
                .map_err(|err| format!("failed to create managed LSP bin directory: {err}"))?;
            run_install_command(&config.codex_home, install_command).await?;
            let Some(rustup_program) = install_command.argv.first() else {
                return Err("language-server install command must not be empty".to_string());
            };
            write_managed_rust_analyzer_wrapper(&config.codex_home, rustup_program).await
        }
        CodeSearchLanguage::Go => {
            tokio::fs::create_dir_all(managed_bin_dir(&config.codex_home))
                .await
                .map_err(|err| format!("failed to create managed LSP bin directory: {err}"))?;
            run_install_command(&config.codex_home, install_command).await
        }
        CodeSearchLanguage::JavaScript
        | CodeSearchLanguage::TypeScript
        | CodeSearchLanguage::Python => {
            tokio::fs::create_dir_all(managed_npm_prefix(&config.codex_home))
                .await
                .map_err(|err| format!("failed to create managed npm prefix: {err}"))?;
            run_install_command(&config.codex_home, install_command).await
        }
    }
}

async fn run_install_command(cwd: &Path, install_command: &InstallCommand) -> Result<(), String> {
    let Some(program) = install_command.argv.first() else {
        return Err("language-server install command must not be empty".to_string());
    };
    let mut command = Command::new(program);
    command
        .args(install_command.argv.iter().skip(1))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &install_command.env {
        command.env(key, value);
    }

    let output = timeout(INSTALL_TIMEOUT, command.output())
        .await
        .map_err(|_| format!("timed out while running `{}`", install_command.display()))?
        .map_err(|err| format!("failed to run `{}`: {err}", install_command.display()))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        output.status.to_string()
    };
    Err(format!("`{}` failed: {detail}", install_command.display()))
}

fn resolve_wrapper_program_path(program: &str) -> Option<PathBuf> {
    if Path::new(program).is_absolute() || program.contains(std::path::MAIN_SEPARATOR) {
        Some(PathBuf::from(program))
    } else {
        which::which(program).ok()
    }
}

async fn write_managed_rust_analyzer_wrapper(
    codex_home: &Path,
    rustup_program: &str,
) -> Result<(), String> {
    let wrapper_path = managed_bin_dir(codex_home).join(managed_script_name("rust-analyzer"));
    let target_path = resolve_rust_analyzer_binary_path(codex_home, rustup_program).await?;
    let target_dir = Path::new(&target_path)
        .parent()
        .ok_or_else(|| {
            format!("failed to determine rust-analyzer parent directory for `{target_path}`")
        })?
        .display()
        .to_string();
    let contents = if cfg!(windows) {
        let target_path = target_path.replace('"', "\"\"");
        let target_dir = target_dir.replace('"', "\"\"");
        format!(
            "@echo off\r\nset \"TARGET={target_path}\"\r\nset \"PATH={target_dir};%PATH%\"\r\n\"%TARGET%\" %*\r\n"
        )
    } else {
        let target_path = serde_json::to_string(&target_path)
            .map_err(|err| format!("failed to encode managed rust-analyzer target path: {err}"))?;
        let target_dir = serde_json::to_string(&target_dir).map_err(|err| {
            format!("failed to encode managed rust-analyzer target directory: {err}")
        })?;
        format!(
            "#!/bin/sh\nset -eu\nTARGET={target_path}\nPATH={target_dir}:$PATH\nexport PATH\nexec \"$TARGET\" \"$@\"\n"
        )
    };
    tokio::fs::write(&wrapper_path, contents)
        .await
        .map_err(|err| format!("failed to write managed rust-analyzer wrapper: {err}"))?;
    #[cfg(unix)]
    {
        let mut permissions = tokio::fs::metadata(&wrapper_path)
            .await
            .map_err(|err| format!("failed to read managed rust-analyzer wrapper metadata: {err}"))?
            .permissions();
        permissions.set_mode(0o755);
        tokio::fs::set_permissions(&wrapper_path, permissions)
            .await
            .map_err(|err| {
                format!("failed to set managed rust-analyzer wrapper permissions: {err}")
            })?;
    }
    Ok(())
}

async fn refresh_managed_rust_analyzer_wrapper_if_present(
    codex_home: &Path,
    rustup_program: &str,
) -> Result<(), String> {
    let wrapper_path = managed_bin_dir(codex_home).join(managed_script_name("rust-analyzer"));
    if !tokio::fs::try_exists(&wrapper_path)
        .await
        .map_err(|err| format!("failed to check managed rust-analyzer wrapper: {err}"))?
    {
        return Ok(());
    }
    if resolve_wrapper_program_path(rustup_program).is_none() {
        return Ok(());
    }
    write_managed_rust_analyzer_wrapper(codex_home, rustup_program).await
}

async fn resolve_rust_analyzer_binary_path(
    codex_home: &Path,
    rustup_program: &str,
) -> Result<String, String> {
    let rustup_path = resolve_wrapper_program_path(rustup_program).ok_or_else(|| {
        format!("failed to resolve `{rustup_program}` to an absolute rustup path")
    })?;
    let output = Command::new(&rustup_path)
        .arg("which")
        .arg("rust-analyzer")
        .current_dir(codex_home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|err| {
            format!(
                "failed to run `{}` while resolving rust-analyzer: {err}",
                rustup_path.display()
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            output.status.to_string()
        };
        return Err(format!(
            "`{}` failed while resolving rust-analyzer: {detail}",
            rustup_path.display()
        ));
    }
    let target_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if target_path.is_empty() {
        return Err("rustup which rust-analyzer returned an empty path".to_string());
    }
    Ok(target_path)
}

fn resolution_source_for_fallback(
    explicit_command: bool,
    auto_detect_enabled: bool,
    install_attempted: bool,
) -> Option<LanguageServerSource> {
    if explicit_command {
        Some(LanguageServerSource::Explicit)
    } else if install_attempted {
        Some(LanguageServerSource::ManagedInstall)
    } else if auto_detect_enabled {
        Some(LanguageServerSource::AutoDetected)
    } else {
        None
    }
}

fn log_code_search_outcome<T>(
    operation: CodeSearchOperation,
    outcome: CodeSearchOutcome<T>,
) -> CodeSearchOutcome<T> {
    let result_count = outcome.data.len();
    tracing::info!(
        operation = operation.description(),
        backend = outcome.backend.as_str(),
        provider = outcome.provider.as_deref().unwrap_or(""),
        language = outcome.trace.language.as_deref().unwrap_or(""),
        resolution_source = outcome.trace.resolution_source.as_deref().unwrap_or(""),
        install_attempted = outcome.trace.install_attempted,
        result_count,
        "internal code search completed"
    );
    outcome
}

pub async fn find_symbols(
    config: &Config,
    params: SymbolSearchParams,
) -> Result<CodeSearchOutcome<CodeSearchSymbol>, CodeSearchError> {
    let query = params.query.trim();
    if query.is_empty() {
        return Err(CodeSearchError::InvalidRequest(
            "query must not be empty".to_string(),
        ));
    }

    let limit = sanitize_limit(params.limit);
    let roots = normalize_roots(&params.cwd, params.roots);
    let workspace_root = roots.first().cloned().unwrap_or_else(|| params.cwd.clone());
    let languages = requested_languages(params.language_hint.as_deref())?;

    if config.code_search.enabled {
        let mut all_results = Vec::new();
        let mut provider = None;
        let mut trace_language = None;
        let mut trace_source = None;
        let mut install_attempted = false;
        let mut missing = Vec::new();
        let mut runtime_failure = None;
        let mut install_failure = None;

        for language in languages {
            match resolve_language_server(config, language).await {
                LanguageServerResolution::Resolved(server) => {
                    install_attempted |= server.install_attempted;
                    let symbols = async {
                        let mut client =
                            LspClient::start(&server.command, &params.cwd, &workspace_root).await?;
                        request_workspace_symbols_with_retry(&mut client, query).await
                    }
                    .await;

                    match symbols {
                        Ok(symbols) => {
                            if !symbols.is_empty() && trace_source.is_none() {
                                provider = provider_for_command(&server.command);
                                trace_language = Some(server.language);
                                trace_source = Some(server.source);
                            }
                            all_results.extend(symbols);
                        }
                        Err(err) => {
                            if runtime_failure.is_none() {
                                runtime_failure = Some((server, err));
                            }
                        }
                    }
                }
                LanguageServerResolution::MissingCommand {
                    language,
                    suggested_command,
                    reason,
                    explicit_command,
                    install_attempted: attempted,
                } => {
                    install_attempted |= attempted;
                    missing.push((
                        language,
                        suggested_command,
                        reason,
                        explicit_command,
                        attempted,
                    ));
                }
                LanguageServerResolution::InstallFailed {
                    language,
                    install_command,
                    error,
                } => {
                    install_attempted = true;
                    if install_failure.is_none() {
                        install_failure = Some((language, install_command, error));
                    }
                }
            }
        }

        if !all_results.is_empty() {
            dedupe_symbols(&mut all_results);
            all_results.truncate(limit);
            return Ok(log_code_search_outcome(
                CodeSearchOperation::Symbol,
                CodeSearchOutcome::new(all_results, CodeSearchBackend::Lsp)
                    .with_provider(provider)
                    .with_trace(trace_language, trace_source, install_attempted),
            ));
        }

        if let Some((server, err)) = runtime_failure {
            let notices = runtime_failure_notices(
                CodeSearchOperation::Symbol,
                &workspace_root,
                server.language,
                &server.command,
                &err,
            );
            let fallback = fallback_symbol_search(query, &params.cwd, &roots, limit).await?;
            let backend = if fallback.is_empty() {
                CodeSearchBackend::Unavailable
            } else if fallback.iter().all(|symbol| symbol.range.is_none()) {
                CodeSearchBackend::FileSearchFallback
            } else {
                CodeSearchBackend::GrepFallback
            };
            return Ok(log_code_search_outcome(
                CodeSearchOperation::Symbol,
                CodeSearchOutcome::new(fallback, backend)
                    .with_provider(provider_for_backend(backend))
                    .with_notices(notices)
                    .with_trace(
                        Some(server.language),
                        Some(server.source),
                        server.install_attempted,
                    ),
            ));
        }

        if let Some((language, install_command, error)) = install_failure {
            let notices = install_failure_notices(
                CodeSearchOperation::Symbol,
                &workspace_root,
                language,
                &install_command,
                &error,
            );
            let fallback = fallback_symbol_search(query, &params.cwd, &roots, limit).await?;
            let backend = if fallback.is_empty() {
                CodeSearchBackend::Unavailable
            } else if fallback.iter().all(|symbol| symbol.range.is_none()) {
                CodeSearchBackend::FileSearchFallback
            } else {
                CodeSearchBackend::GrepFallback
            };
            return Ok(log_code_search_outcome(
                CodeSearchOperation::Symbol,
                CodeSearchOutcome::new(fallback, backend)
                    .with_provider(provider_for_backend(backend))
                    .with_notices(notices)
                    .with_trace(
                        Some(language),
                        Some(LanguageServerSource::ManagedInstall),
                        true,
                    ),
            ));
        }

        if let Some((language, command, reason, explicit_command, attempted)) = missing.first() {
            let notices = missing_command_notices(
                CodeSearchOperation::Symbol,
                &workspace_root,
                *language,
                command,
                *reason,
                *explicit_command,
            );
            let fallback = fallback_symbol_search(query, &params.cwd, &roots, limit).await?;
            let backend = if fallback.is_empty() {
                CodeSearchBackend::Unavailable
            } else if fallback.iter().all(|symbol| symbol.range.is_none()) {
                CodeSearchBackend::FileSearchFallback
            } else {
                CodeSearchBackend::GrepFallback
            };
            return Ok(log_code_search_outcome(
                CodeSearchOperation::Symbol,
                CodeSearchOutcome::new(fallback, backend)
                    .with_provider(provider_for_backend(backend))
                    .with_notices(notices)
                    .with_trace(
                        Some(*language),
                        resolution_source_for_fallback(
                            *explicit_command,
                            config.code_search.auto_detect,
                            *attempted,
                        ),
                        *attempted,
                    ),
            ));
        }
    }

    let fallback = fallback_symbol_search(query, &params.cwd, &roots, limit).await?;
    let backend = if fallback.is_empty() {
        CodeSearchBackend::Unavailable
    } else if fallback.iter().all(|symbol| symbol.range.is_none()) {
        CodeSearchBackend::FileSearchFallback
    } else {
        CodeSearchBackend::GrepFallback
    };
    Ok(log_code_search_outcome(
        CodeSearchOperation::Symbol,
        CodeSearchOutcome::new(fallback, backend).with_provider(provider_for_backend(backend)),
    ))
}

pub async fn find_definitions(
    config: &Config,
    params: DefinitionSearchParams,
) -> Result<CodeSearchOutcome<CodeSearchLocation>, CodeSearchError> {
    let path = resolve_path(&params.cwd, &params.path);
    let roots = normalize_roots(&params.cwd, params.roots);
    let workspace_root = workspace_root_for_path(&path, &roots, &params.cwd);
    let position = params.position.clone();

    if config.code_search.enabled
        && let Some(language) = CodeSearchLanguage::detect(&path)
    {
        match resolve_language_server(config, language).await {
            LanguageServerResolution::Resolved(server) => {
                let locations = async {
                    let mut client =
                        LspClient::start(&server.command, &params.cwd, &workspace_root).await?;
                    open_document(&mut client, language, &path).await?;
                    request_definition_locations_with_retry(&mut client, &path, position.clone())
                        .await
                }
                .await;

                match locations {
                    Ok(locations) if !locations.is_empty() => {
                        return Ok(log_code_search_outcome(
                            CodeSearchOperation::Definition,
                            CodeSearchOutcome::new(locations, CodeSearchBackend::Lsp)
                                .with_provider(provider_for_command(&server.command))
                                .with_trace(
                                    Some(server.language),
                                    Some(server.source),
                                    server.install_attempted,
                                ),
                        ));
                    }
                    Ok(_) => {}
                    Err(err) => {
                        let notices = runtime_failure_notices(
                            CodeSearchOperation::Definition,
                            &workspace_root,
                            language,
                            &server.command,
                            &err,
                        );
                        let fallback = fallback_location_search(
                            CodeSearchOperation::Definition,
                            &path,
                            params.position,
                            &params.cwd,
                            &roots,
                        )
                        .await?;
                        let backend = if fallback.is_empty() {
                            CodeSearchBackend::Unavailable
                        } else {
                            CodeSearchBackend::GrepFallback
                        };
                        return Ok(log_code_search_outcome(
                            CodeSearchOperation::Definition,
                            CodeSearchOutcome::new(fallback, backend)
                                .with_provider(provider_for_backend(backend))
                                .with_notices(notices)
                                .with_trace(
                                    Some(server.language),
                                    Some(server.source),
                                    server.install_attempted,
                                ),
                        ));
                    }
                }
            }
            LanguageServerResolution::MissingCommand {
                language,
                suggested_command,
                reason,
                explicit_command,
                install_attempted,
            } => {
                let notices = missing_command_notices(
                    CodeSearchOperation::Definition,
                    &workspace_root,
                    language,
                    &suggested_command,
                    reason,
                    explicit_command,
                );
                let fallback = fallback_location_search(
                    CodeSearchOperation::Definition,
                    &path,
                    params.position,
                    &params.cwd,
                    &roots,
                )
                .await?;
                let backend = if fallback.is_empty() {
                    CodeSearchBackend::Unavailable
                } else {
                    CodeSearchBackend::GrepFallback
                };
                return Ok(log_code_search_outcome(
                    CodeSearchOperation::Definition,
                    CodeSearchOutcome::new(fallback, backend)
                        .with_provider(provider_for_backend(backend))
                        .with_notices(notices)
                        .with_trace(
                            Some(language),
                            resolution_source_for_fallback(
                                explicit_command,
                                config.code_search.auto_detect,
                                install_attempted,
                            ),
                            install_attempted,
                        ),
                ));
            }
            LanguageServerResolution::InstallFailed {
                language,
                install_command,
                error,
            } => {
                let notices = install_failure_notices(
                    CodeSearchOperation::Definition,
                    &workspace_root,
                    language,
                    &install_command,
                    &error,
                );
                let fallback = fallback_location_search(
                    CodeSearchOperation::Definition,
                    &path,
                    params.position,
                    &params.cwd,
                    &roots,
                )
                .await?;
                let backend = if fallback.is_empty() {
                    CodeSearchBackend::Unavailable
                } else {
                    CodeSearchBackend::GrepFallback
                };
                return Ok(log_code_search_outcome(
                    CodeSearchOperation::Definition,
                    CodeSearchOutcome::new(fallback, backend)
                        .with_provider(provider_for_backend(backend))
                        .with_notices(notices)
                        .with_trace(
                            Some(language),
                            Some(LanguageServerSource::ManagedInstall),
                            true,
                        ),
                ));
            }
        }
    }

    let fallback = fallback_location_search(
        CodeSearchOperation::Definition,
        &path,
        position,
        &params.cwd,
        &roots,
    )
    .await?;
    let backend = if fallback.is_empty() {
        CodeSearchBackend::Unavailable
    } else {
        CodeSearchBackend::GrepFallback
    };
    Ok(log_code_search_outcome(
        CodeSearchOperation::Definition,
        CodeSearchOutcome::new(fallback, backend).with_provider(provider_for_backend(backend)),
    ))
}

pub async fn find_references(
    config: &Config,
    params: ReferencesSearchParams,
) -> Result<CodeSearchOutcome<CodeSearchLocation>, CodeSearchError> {
    let path = resolve_path(&params.cwd, &params.path);
    let roots = normalize_roots(&params.cwd, params.roots);
    let workspace_root = workspace_root_for_path(&path, &roots, &params.cwd);
    let position = params.position.clone();

    if config.code_search.enabled
        && let Some(language) = CodeSearchLanguage::detect(&path)
    {
        match resolve_language_server(config, language).await {
            LanguageServerResolution::Resolved(server) => {
                let locations = async {
                    let mut client =
                        LspClient::start(&server.command, &params.cwd, &workspace_root).await?;
                    open_document(&mut client, language, &path).await?;
                    request_reference_locations_with_retry(
                        &mut client,
                        &path,
                        position.clone(),
                        params.include_declaration,
                    )
                    .await
                }
                .await;

                match locations {
                    Ok(locations) if !locations.is_empty() => {
                        return Ok(log_code_search_outcome(
                            CodeSearchOperation::References,
                            CodeSearchOutcome::new(locations, CodeSearchBackend::Lsp)
                                .with_provider(provider_for_command(&server.command))
                                .with_trace(
                                    Some(server.language),
                                    Some(server.source),
                                    server.install_attempted,
                                ),
                        ));
                    }
                    Ok(_) => {}
                    Err(err) => {
                        let notices = runtime_failure_notices(
                            CodeSearchOperation::References,
                            &workspace_root,
                            language,
                            &server.command,
                            &err,
                        );
                        let fallback = fallback_location_search(
                            CodeSearchOperation::References,
                            &path,
                            params.position,
                            &params.cwd,
                            &roots,
                        )
                        .await?;
                        let backend = if fallback.is_empty() {
                            CodeSearchBackend::Unavailable
                        } else {
                            CodeSearchBackend::GrepFallback
                        };
                        return Ok(log_code_search_outcome(
                            CodeSearchOperation::References,
                            CodeSearchOutcome::new(fallback, backend)
                                .with_provider(provider_for_backend(backend))
                                .with_notices(notices)
                                .with_trace(
                                    Some(server.language),
                                    Some(server.source),
                                    server.install_attempted,
                                ),
                        ));
                    }
                }
            }
            LanguageServerResolution::MissingCommand {
                language,
                suggested_command,
                reason,
                explicit_command,
                install_attempted,
            } => {
                let notices = missing_command_notices(
                    CodeSearchOperation::References,
                    &workspace_root,
                    language,
                    &suggested_command,
                    reason,
                    explicit_command,
                );
                let fallback = fallback_location_search(
                    CodeSearchOperation::References,
                    &path,
                    params.position,
                    &params.cwd,
                    &roots,
                )
                .await?;
                let backend = if fallback.is_empty() {
                    CodeSearchBackend::Unavailable
                } else {
                    CodeSearchBackend::GrepFallback
                };
                return Ok(log_code_search_outcome(
                    CodeSearchOperation::References,
                    CodeSearchOutcome::new(fallback, backend)
                        .with_provider(provider_for_backend(backend))
                        .with_notices(notices)
                        .with_trace(
                            Some(language),
                            resolution_source_for_fallback(
                                explicit_command,
                                config.code_search.auto_detect,
                                install_attempted,
                            ),
                            install_attempted,
                        ),
                ));
            }
            LanguageServerResolution::InstallFailed {
                language,
                install_command,
                error,
            } => {
                let notices = install_failure_notices(
                    CodeSearchOperation::References,
                    &workspace_root,
                    language,
                    &install_command,
                    &error,
                );
                let fallback = fallback_location_search(
                    CodeSearchOperation::References,
                    &path,
                    params.position,
                    &params.cwd,
                    &roots,
                )
                .await?;
                let backend = if fallback.is_empty() {
                    CodeSearchBackend::Unavailable
                } else {
                    CodeSearchBackend::GrepFallback
                };
                return Ok(log_code_search_outcome(
                    CodeSearchOperation::References,
                    CodeSearchOutcome::new(fallback, backend)
                        .with_provider(provider_for_backend(backend))
                        .with_notices(notices)
                        .with_trace(
                            Some(language),
                            Some(LanguageServerSource::ManagedInstall),
                            true,
                        ),
                ));
            }
        }
    }

    let fallback = fallback_location_search(
        CodeSearchOperation::References,
        &path,
        position,
        &params.cwd,
        &roots,
    )
    .await?;
    let backend = if fallback.is_empty() {
        CodeSearchBackend::Unavailable
    } else {
        CodeSearchBackend::GrepFallback
    };
    Ok(log_code_search_outcome(
        CodeSearchOperation::References,
        CodeSearchOutcome::new(fallback, backend).with_provider(provider_for_backend(backend)),
    ))
}

pub async fn document_symbols(
    config: &Config,
    params: DocumentSymbolsParams,
) -> Result<CodeSearchOutcome<CodeSearchDocumentSymbol>, CodeSearchError> {
    let path = resolve_path(&params.cwd, &params.path);
    let Some(language) = CodeSearchLanguage::detect(&path) else {
        return Ok(log_code_search_outcome(
            CodeSearchOperation::DocumentSymbol,
            CodeSearchOutcome::new(Vec::new(), CodeSearchBackend::Unavailable)
                .with_provider(provider_for_backend(CodeSearchBackend::Unavailable)),
        ));
    };
    let workspace_root = path.parent().unwrap_or(params.cwd.as_path()).to_path_buf();

    if config.code_search.enabled {
        match resolve_language_server(config, language).await {
            LanguageServerResolution::Resolved(server) => {
                let symbols = async {
                    let mut client =
                        LspClient::start(&server.command, &params.cwd, &workspace_root).await?;
                    open_document(&mut client, language, &path).await?;
                    request_document_symbols_with_retry(&mut client, &path).await
                }
                .await;

                match symbols {
                    Ok(symbols) if !symbols.is_empty() => {
                        return Ok(log_code_search_outcome(
                            CodeSearchOperation::DocumentSymbol,
                            CodeSearchOutcome::new(symbols, CodeSearchBackend::Lsp)
                                .with_provider(provider_for_command(&server.command))
                                .with_trace(
                                    Some(server.language),
                                    Some(server.source),
                                    server.install_attempted,
                                ),
                        ));
                    }
                    Ok(_) => {}
                    Err(err) => {
                        let notices = runtime_failure_notices(
                            CodeSearchOperation::DocumentSymbol,
                            &workspace_root,
                            language,
                            &server.command,
                            &err,
                        );
                        let fallback = fallback_document_symbols(&path, language).await?;
                        let backend = if fallback.is_empty() {
                            CodeSearchBackend::Unavailable
                        } else {
                            CodeSearchBackend::GrepFallback
                        };
                        return Ok(log_code_search_outcome(
                            CodeSearchOperation::DocumentSymbol,
                            CodeSearchOutcome::new(fallback, backend)
                                .with_provider(provider_for_backend(backend))
                                .with_notices(notices)
                                .with_trace(
                                    Some(server.language),
                                    Some(server.source),
                                    server.install_attempted,
                                ),
                        ));
                    }
                }
            }
            LanguageServerResolution::MissingCommand {
                language,
                suggested_command,
                reason,
                explicit_command,
                install_attempted,
            } => {
                let notices = missing_command_notices(
                    CodeSearchOperation::DocumentSymbol,
                    &workspace_root,
                    language,
                    &suggested_command,
                    reason,
                    explicit_command,
                );
                let fallback = fallback_document_symbols(&path, language).await?;
                let backend = if fallback.is_empty() {
                    CodeSearchBackend::Unavailable
                } else {
                    CodeSearchBackend::GrepFallback
                };
                return Ok(log_code_search_outcome(
                    CodeSearchOperation::DocumentSymbol,
                    CodeSearchOutcome::new(fallback, backend)
                        .with_provider(provider_for_backend(backend))
                        .with_notices(notices)
                        .with_trace(
                            Some(language),
                            resolution_source_for_fallback(
                                explicit_command,
                                config.code_search.auto_detect,
                                install_attempted,
                            ),
                            install_attempted,
                        ),
                ));
            }
            LanguageServerResolution::InstallFailed {
                language,
                install_command,
                error,
            } => {
                let notices = install_failure_notices(
                    CodeSearchOperation::DocumentSymbol,
                    &workspace_root,
                    language,
                    &install_command,
                    &error,
                );
                let fallback = fallback_document_symbols(&path, language).await?;
                let backend = if fallback.is_empty() {
                    CodeSearchBackend::Unavailable
                } else {
                    CodeSearchBackend::GrepFallback
                };
                return Ok(log_code_search_outcome(
                    CodeSearchOperation::DocumentSymbol,
                    CodeSearchOutcome::new(fallback, backend)
                        .with_provider(provider_for_backend(backend))
                        .with_notices(notices)
                        .with_trace(
                            Some(language),
                            Some(LanguageServerSource::ManagedInstall),
                            true,
                        ),
                ));
            }
        }
    }

    let fallback = fallback_document_symbols(&path, language).await?;
    let backend = if fallback.is_empty() {
        CodeSearchBackend::Unavailable
    } else {
        CodeSearchBackend::GrepFallback
    };
    Ok(log_code_search_outcome(
        CodeSearchOperation::DocumentSymbol,
        CodeSearchOutcome::new(fallback, backend).with_provider(provider_for_backend(backend)),
    ))
}

fn sanitize_limit(limit: usize) -> usize {
    limit.clamp(1, MAX_LIMIT)
}

fn normalize_roots(cwd: &Path, roots: Vec<PathBuf>) -> Vec<PathBuf> {
    if roots.is_empty() {
        return vec![cwd.to_path_buf()];
    }

    roots
        .into_iter()
        .map(|root| {
            if root.is_absolute() {
                root
            } else {
                cwd.join(root)
            }
        })
        .collect()
}

fn requested_languages(
    language_hint: Option<&str>,
) -> Result<Vec<CodeSearchLanguage>, CodeSearchError> {
    if let Some(language_hint) = language_hint {
        let language = CodeSearchLanguage::parse(language_hint).ok_or_else(|| {
            CodeSearchError::InvalidRequest(format!("unsupported language hint `{language_hint}`"))
        })?;
        return Ok(vec![language]);
    }

    Ok(vec![
        CodeSearchLanguage::Rust,
        CodeSearchLanguage::JavaScript,
        CodeSearchLanguage::TypeScript,
        CodeSearchLanguage::Python,
        CodeSearchLanguage::Go,
    ])
}

fn workspace_root_for_path(path: &Path, roots: &[PathBuf], cwd: &Path) -> PathBuf {
    roots
        .iter()
        .find(|root| path.starts_with(root))
        .cloned()
        .unwrap_or_else(|| path.parent().unwrap_or(cwd).to_path_buf())
}

fn resolve_path(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn command_exists(command: &[String]) -> bool {
    let Some(program) = command.first() else {
        return false;
    };
    if Path::new(program).is_absolute() || program.contains(std::path::MAIN_SEPARATOR) {
        return Path::new(program).exists();
    }
    which::which(program).is_ok()
}

fn notice_key(
    prefix: &str,
    workspace_root: &Path,
    language: CodeSearchLanguage,
    command: &[String],
) -> String {
    format!(
        "{prefix}:{}:{}:{}",
        workspace_root.display(),
        language.key(),
        command.join(" ")
    )
}

fn missing_command_notices(
    operation: CodeSearchOperation,
    workspace_root: &Path,
    language: CodeSearchLanguage,
    command: &[String],
    reason: MissingCommandReason,
    explicit_command: bool,
) -> CodeSearchNotices {
    let command_text = command.join(" ");
    let warning_message = match reason {
        MissingCommandReason::NotInstalled if explicit_command => format!(
            "{} fell back to existing search because `[code_search.lsp.{}].command` points to `{command_text}`, which is unavailable for {}. Fix the configured command; Codex will not auto-install a replacement for an explicit override.",
            operation.description(),
            language.key(),
            language.display(),
        ),
        MissingCommandReason::NotConfigured => format!(
            "{} fell back to existing search because no {} language server command is configured. Set `[code_search.lsp.{}].command = [\"{}\"]`, enable `code_search.auto_detect`, or enable `code_search.auto_install`.",
            operation.description(),
            language.display(),
            language.key(),
            command.first().map(String::as_str).unwrap_or("server")
        ),
        MissingCommandReason::NotInstalled => format!(
            "{} fell back to existing search because `{command_text}` is unavailable for {}. Install the command, enable `code_search.auto_install`, or set `[code_search.lsp.{}].command` to a working language server.",
            operation.description(),
            language.display(),
            language.key(),
        ),
    };
    CodeSearchNotices {
        info_message: None,
        info_key: None,
        warning_message: Some(warning_message),
        warning_key: Some(notice_key("warn", workspace_root, language, command)),
    }
}

fn install_failure_notices(
    operation: CodeSearchOperation,
    workspace_root: &Path,
    language: CodeSearchLanguage,
    install_command: &InstallCommand,
    error: &str,
) -> CodeSearchNotices {
    let install_command_text = install_command.display();
    CodeSearchNotices {
        info_message: None,
        info_key: None,
        warning_message: Some(format!(
            "{} fell back to existing search because Codex could not install a {} language server via `{install_command_text}`: {error}",
            operation.description(),
            language.display(),
        )),
        warning_key: Some(notice_key(
            "warn-install",
            workspace_root,
            language,
            &install_command.argv,
        )),
    }
}

fn runtime_failure_notices(
    operation: CodeSearchOperation,
    workspace_root: &Path,
    language: CodeSearchLanguage,
    command: &[String],
    error: &CodeSearchError,
) -> CodeSearchNotices {
    let command_text = command.join(" ");
    CodeSearchNotices {
        info_message: None,
        info_key: None,
        warning_message: Some(format!(
            "{} fell back to existing search after `{command_text}` failed for {}: {error}",
            operation.description(),
            language.display(),
        )),
        warning_key: Some(notice_key(
            "warn-runtime",
            workspace_root,
            language,
            command,
        )),
    }
}

fn provider_for_backend(backend: CodeSearchBackend) -> Option<String> {
    match backend {
        CodeSearchBackend::Lsp => None,
        CodeSearchBackend::GrepFallback => Some("rg".to_string()),
        CodeSearchBackend::FileSearchFallback => Some("file-search".to_string()),
        CodeSearchBackend::Unavailable => None,
    }
}

fn provider_for_command(command: &[String]) -> Option<String> {
    command.first().cloned()
}

async fn open_document(
    client: &mut LspClient,
    language: CodeSearchLanguage,
    path: &Path,
) -> Result<(), CodeSearchError> {
    let text =
        tokio::fs::read_to_string(path)
            .await
            .map_err(|source| CodeSearchError::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
    let uri = path_to_uri_string(path)?;
    client
        .notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language.language_id(),
                    "version": 1,
                    "text": text,
                }
            }),
        )
        .await
}

async fn request_workspace_symbols(
    client: &mut LspClient,
    query: &str,
) -> Result<Vec<CodeSearchSymbol>, CodeSearchError> {
    let value = client
        .request_value("workspace/symbol", json!({ "query": query }))
        .await?;
    let Some(items) = value.as_array() else {
        return Ok(Vec::new());
    };
    Ok(items.iter().filter_map(parse_workspace_symbol).collect())
}

async fn request_workspace_symbols_with_retry(
    client: &mut LspClient,
    query: &str,
) -> Result<Vec<CodeSearchSymbol>, CodeSearchError> {
    let mut attempt = 0;
    loop {
        let symbols = request_workspace_symbols(client, query).await?;
        attempt += 1;
        if !symbols.is_empty() || attempt == LSP_EMPTY_RESULT_MAX_ATTEMPTS {
            return Ok(symbols);
        }
        sleep(LSP_EMPTY_RESULT_RETRY_DELAY).await;
    }
}

async fn request_definition_locations(
    client: &mut LspClient,
    path: &Path,
    position: TextPosition,
) -> Result<Vec<CodeSearchLocation>, CodeSearchError> {
    let uri = path_to_uri_string(path)?;
    let value = client
        .request_value(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri },
                "position": to_lsp_position(position),
            }),
        )
        .await?;
    Ok(parse_locations(&value))
}

async fn request_definition_locations_with_retry(
    client: &mut LspClient,
    path: &Path,
    position: TextPosition,
) -> Result<Vec<CodeSearchLocation>, CodeSearchError> {
    let mut attempt = 0;
    loop {
        let locations = request_definition_locations(client, path, position.clone()).await?;
        attempt += 1;
        if !locations.is_empty() || attempt == LSP_EMPTY_RESULT_MAX_ATTEMPTS {
            return Ok(locations);
        }
        sleep(LSP_EMPTY_RESULT_RETRY_DELAY).await;
    }
}

async fn request_reference_locations(
    client: &mut LspClient,
    path: &Path,
    position: TextPosition,
    include_declaration: bool,
) -> Result<Vec<CodeSearchLocation>, CodeSearchError> {
    let uri = path_to_uri_string(path)?;
    let value = client
        .request_value(
            "textDocument/references",
            json!({
                "textDocument": { "uri": uri },
                "position": to_lsp_position(position),
                "context": { "includeDeclaration": include_declaration },
            }),
        )
        .await?;
    Ok(parse_locations(&value))
}

async fn request_reference_locations_with_retry(
    client: &mut LspClient,
    path: &Path,
    position: TextPosition,
    include_declaration: bool,
) -> Result<Vec<CodeSearchLocation>, CodeSearchError> {
    let mut attempt = 0;
    loop {
        let locations =
            request_reference_locations(client, path, position.clone(), include_declaration)
                .await?;
        attempt += 1;
        if !locations.is_empty() || attempt == LSP_EMPTY_RESULT_MAX_ATTEMPTS {
            return Ok(locations);
        }
        sleep(LSP_EMPTY_RESULT_RETRY_DELAY).await;
    }
}

async fn request_document_symbols(
    client: &mut LspClient,
    path: &Path,
) -> Result<Vec<CodeSearchDocumentSymbol>, CodeSearchError> {
    let uri = path_to_uri_string(path)?;
    let value = client
        .request_value(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
        )
        .await?;
    let Some(items) = value.as_array() else {
        return Ok(Vec::new());
    };
    Ok(items.iter().filter_map(parse_document_symbol).collect())
}

async fn request_document_symbols_with_retry(
    client: &mut LspClient,
    path: &Path,
) -> Result<Vec<CodeSearchDocumentSymbol>, CodeSearchError> {
    let mut attempt = 0;
    loop {
        let symbols = request_document_symbols(client, path).await?;
        attempt += 1;
        if !symbols.is_empty() || attempt == LSP_EMPTY_RESULT_MAX_ATTEMPTS {
            return Ok(symbols);
        }
        sleep(LSP_EMPTY_RESULT_RETRY_DELAY).await;
    }
}

fn parse_workspace_symbol(value: &Value) -> Option<CodeSearchSymbol> {
    let name = value.get("name")?.as_str()?.to_string();
    let kind = value
        .get("kind")
        .and_then(Value::as_u64)
        .and_then(symbol_kind_name)
        .map(ToOwned::to_owned);
    let container_name = value
        .get("containerName")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    if let Some(location) = value.get("location") {
        let location = parse_location(location)?;
        return Some(CodeSearchSymbol {
            name,
            kind,
            path: location.path,
            range: Some(location.range),
            container_name,
        });
    }

    let uri = value.get("uri").and_then(Value::as_str)?;
    let path = uri_to_path(uri)?;
    let range = value.get("range").and_then(parse_range);
    Some(CodeSearchSymbol {
        name,
        kind,
        path,
        range,
        container_name,
    })
}

fn parse_document_symbol(value: &Value) -> Option<CodeSearchDocumentSymbol> {
    if value.get("location").is_some() {
        let symbol = parse_workspace_symbol(value)?;
        let range = symbol.range?;
        return Some(CodeSearchDocumentSymbol {
            name: symbol.name,
            kind: symbol.kind,
            range: range.clone(),
            selection_range: range,
            detail: None,
            children: Vec::new(),
        });
    }

    let name = value.get("name")?.as_str()?.to_string();
    let kind = value
        .get("kind")
        .and_then(Value::as_u64)
        .and_then(symbol_kind_name)
        .map(ToOwned::to_owned);
    let range = parse_range(value.get("range")?)?;
    let selection_range = parse_range(value.get("selectionRange")?)?;
    let detail = value
        .get("detail")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let children = value
        .get("children")
        .and_then(Value::as_array)
        .map(|children| children.iter().filter_map(parse_document_symbol).collect())
        .unwrap_or_default();
    Some(CodeSearchDocumentSymbol {
        name,
        kind,
        range,
        selection_range,
        detail,
        children,
    })
}

fn parse_locations(value: &Value) -> Vec<CodeSearchLocation> {
    if value.is_null() {
        return Vec::new();
    }
    if let Some(array) = value.as_array() {
        return array.iter().filter_map(parse_location).collect();
    }
    parse_location(value).into_iter().collect()
}

fn parse_location(value: &Value) -> Option<CodeSearchLocation> {
    if let (Some(target_uri), Some(target_range)) =
        (value.get("targetUri"), value.get("targetSelectionRange"))
    {
        let path = uri_to_path(target_uri.as_str()?)?;
        let range = parse_range(target_range)?;
        return Some(CodeSearchLocation { path, range });
    }

    let uri = value.get("uri")?.as_str()?;
    let path = uri_to_path(uri)?;
    let range = parse_range(value.get("range")?)?;
    Some(CodeSearchLocation { path, range })
}

fn parse_range(value: &Value) -> Option<TextRange> {
    let start = value.get("start")?;
    let end = value.get("end")?;
    let start = TextPosition {
        line: start.get("line")?.as_u64()? as usize + 1,
        column: start.get("character")?.as_u64()? as usize + 1,
    };
    let end = TextPosition {
        line: end.get("line")?.as_u64()? as usize + 1,
        column: end.get("character")?.as_u64()? as usize + 1,
    };
    Some(TextRange { start, end })
}

fn to_lsp_position(position: TextPosition) -> Value {
    json!({
        "line": position.line.saturating_sub(1),
        "character": position.column.saturating_sub(1),
    })
}

fn path_to_uri_string(path: &Path) -> Result<String, CodeSearchError> {
    url::Url::from_file_path(path)
        .map(|uri| uri.to_string())
        .map_err(|_| {
            CodeSearchError::OperationFailed(format!(
                "failed to convert `{}` to a file URI",
                path.display()
            ))
        })
}

fn uri_to_path(uri: &str) -> Option<PathBuf> {
    url::Url::parse(uri).ok()?.to_file_path().ok()
}

fn symbol_kind_name(kind: u64) -> Option<&'static str> {
    match kind {
        1 => Some("file"),
        2 => Some("module"),
        3 => Some("namespace"),
        4 => Some("package"),
        5 => Some("class"),
        6 => Some("method"),
        7 => Some("property"),
        8 => Some("field"),
        9 => Some("constructor"),
        10 => Some("enum"),
        11 => Some("interface"),
        12 => Some("function"),
        13 => Some("variable"),
        14 => Some("constant"),
        22 => Some("enum_member"),
        23 => Some("struct"),
        25 => Some("operator"),
        26 => Some("type_parameter"),
        _ => None,
    }
}

async fn fallback_symbol_search(
    query: &str,
    cwd: &Path,
    roots: &[PathBuf],
    limit: usize,
) -> Result<Vec<CodeSearchSymbol>, CodeSearchError> {
    let mut matches = grep_text_matches(query, cwd, roots, limit).await?;
    if !matches.is_empty() {
        return Ok(matches
            .drain(..)
            .map(|entry| CodeSearchSymbol {
                name: query.to_string(),
                kind: None,
                path: entry.path,
                range: Some(entry.range),
                container_name: None,
            })
            .collect());
    }

    #[expect(clippy::expect_used)]
    let search_limit = NonZero::new(limit).expect("limit should be non-zero");
    let file_results = codex_file_search::run(
        query,
        roots.to_vec(),
        FileSearchOptions {
            limit: search_limit,
            threads: available_threads(),
            compute_indices: false,
            ..Default::default()
        },
        Some(Arc::new(AtomicBool::new(false))),
    )
    .map_err(|err| {
        CodeSearchError::OperationFailed(format!("file search fallback failed: {err}"))
    })?;

    Ok(file_results
        .matches
        .into_iter()
        .take(limit)
        .map(|entry| {
            let full_path = entry.full_path();
            let name = full_path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or(query)
                .to_string();
            CodeSearchSymbol {
                name,
                kind: Some("file".to_string()),
                path: full_path,
                range: None,
                container_name: None,
            }
        })
        .collect())
}

async fn fallback_location_search(
    operation: CodeSearchOperation,
    path: &Path,
    position: TextPosition,
    cwd: &Path,
    roots: &[PathBuf],
) -> Result<Vec<CodeSearchLocation>, CodeSearchError> {
    let token = token_at_position(path, position).await?;
    if token.is_empty() {
        return Ok(Vec::new());
    }

    let mut matches = grep_text_matches(&token, cwd, roots, MAX_FALLBACK_RESULTS).await?;
    if matches.is_empty() {
        return Ok(Vec::new());
    }

    if operation == CodeSearchOperation::Definition {
        matches.sort_by_key(|entry| {
            let same_file = entry.path != path;
            (same_file, entry.range.start.line, entry.range.start.column)
        });
    }

    Ok(matches
        .into_iter()
        .take(MAX_FALLBACK_RESULTS)
        .map(|entry| CodeSearchLocation {
            path: entry.path,
            range: entry.range,
        })
        .collect())
}

async fn fallback_document_symbols(
    path: &Path,
    language: CodeSearchLanguage,
) -> Result<Vec<CodeSearchDocumentSymbol>, CodeSearchError> {
    let contents =
        tokio::fs::read_to_string(path)
            .await
            .map_err(|source| CodeSearchError::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
    let mut symbols = Vec::new();

    for (index, line) in contents.lines().enumerate() {
        if let Some(symbol) = heuristic_document_symbol(language, line, index + 1) {
            symbols.push(symbol);
        }
        if symbols.len() == MAX_FALLBACK_RESULTS {
            break;
        }
    }

    Ok(symbols)
}

fn heuristic_document_symbol(
    language: CodeSearchLanguage,
    line: &str,
    line_number: usize,
) -> Option<CodeSearchDocumentSymbol> {
    let mut normalized = line.trim_start();
    while let Some(rest) = normalized.strip_prefix("export ") {
        normalized = rest;
    }
    if language == CodeSearchLanguage::Rust {
        if let Some(rest) = normalized.strip_prefix("pub ") {
            normalized = rest;
        } else if let Some(rest) = normalized.strip_prefix("pub(")
            && let Some((_, remainder)) = rest.split_once(") ")
        {
            normalized = remainder;
        }
    }

    for keyword in language.heuristic_keywords() {
        let Some(rest) = normalized.strip_prefix(keyword) else {
            continue;
        };
        let name = extract_identifier(rest, language)?;
        let column = line.find(&name)? + 1;
        let range = TextRange {
            start: TextPosition {
                line: line_number,
                column,
            },
            end: TextPosition {
                line: line_number,
                column: column + name.chars().count(),
            },
        };
        let kind = match *keyword {
            "fn " | "function " | "def " | "func " => Some("function".to_string()),
            "struct " => Some("struct".to_string()),
            "enum " => Some("enum".to_string()),
            "trait " | "interface " | "type " => Some("interface".to_string()),
            "class " => Some("class".to_string()),
            "mod " => Some("module".to_string()),
            "const " | "static " | "let " | "var " => Some("variable".to_string()),
            _ => None,
        };
        return Some(CodeSearchDocumentSymbol {
            name,
            kind,
            range: range.clone(),
            selection_range: range,
            detail: None,
            children: Vec::new(),
        });
    }

    None
}

fn extract_identifier(text: &str, language: CodeSearchLanguage) -> Option<String> {
    let text = if language == CodeSearchLanguage::Go && text.starts_with('(') {
        let (_, rest) = text.split_once(')')?;
        rest.trim_start()
    } else {
        text.trim_start()
    };

    let mut identifier = String::new();
    for character in text.chars() {
        if identifier.is_empty() {
            if character == '_' || character.is_ascii_alphabetic() {
                identifier.push(character);
            } else if !character.is_whitespace() {
                return None;
            }
        } else if character == '_' || character.is_ascii_alphanumeric() {
            identifier.push(character);
        } else {
            break;
        }
    }

    (!identifier.is_empty()).then_some(identifier)
}

async fn token_at_position(path: &Path, position: TextPosition) -> Result<String, CodeSearchError> {
    let contents =
        tokio::fs::read_to_string(path)
            .await
            .map_err(|source| CodeSearchError::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
    let Some(line) = contents.lines().nth(position.line.saturating_sub(1)) else {
        return Err(CodeSearchError::InvalidPosition {
            path: path.to_path_buf(),
            line: position.line,
            column: position.column,
        });
    };
    let characters = line.chars().collect::<Vec<_>>();
    if position.column == 0 || position.column > characters.len() + 1 {
        return Err(CodeSearchError::InvalidPosition {
            path: path.to_path_buf(),
            line: position.line,
            column: position.column,
        });
    }
    if characters.is_empty() {
        return Ok(String::new());
    }

    let mut index = position
        .column
        .saturating_sub(1)
        .min(characters.len().saturating_sub(1));
    if !characters
        .get(index)
        .is_some_and(|character| is_identifier_character(*character))
        && index > 0
        && characters
            .get(index - 1)
            .is_some_and(|character| is_identifier_character(*character))
    {
        index -= 1;
    }

    if !characters
        .get(index)
        .is_some_and(|character| is_identifier_character(*character))
    {
        return Ok(String::new());
    }

    let mut start = index;
    while start > 0 && is_identifier_character(characters[start - 1]) {
        start -= 1;
    }
    let mut end = index;
    while end + 1 < characters.len() && is_identifier_character(characters[end + 1]) {
        end += 1;
    }

    Ok(characters[start..=end].iter().collect())
}

fn is_identifier_character(character: char) -> bool {
    character == '_' || character.is_ascii_alphanumeric()
}

async fn grep_text_matches(
    pattern: &str,
    cwd: &Path,
    roots: &[PathBuf],
    limit: usize,
) -> Result<Vec<TextMatch>, CodeSearchError> {
    let mut command = Command::new("rg");
    command
        .current_dir(cwd)
        .arg("--json")
        .arg("--line-number")
        .arg("--column")
        .arg("--fixed-strings")
        .arg("--no-messages")
        .arg("--regexp")
        .arg(pattern)
        .arg("--")
        .args(roots);

    let output = timeout(SEARCH_TIMEOUT, command.output())
        .await
        .map_err(|_| {
            CodeSearchError::OperationFailed("rg timed out during code-search fallback".to_string())
        })?
        .map_err(|err| {
            CodeSearchError::OperationFailed(format!(
                "failed to launch rg for code-search fallback: {err}"
            ))
        })?;

    match output.status.code() {
        Some(0) | Some(1) => parse_rg_json_matches(&output.stdout, limit),
        _ => Err(CodeSearchError::OperationFailed(format!(
            "rg failed during code-search fallback: {}",
            String::from_utf8_lossy(&output.stderr)
        ))),
    }
}

fn parse_rg_json_matches(stdout: &[u8], limit: usize) -> Result<Vec<TextMatch>, CodeSearchError> {
    let mut matches = Vec::new();
    let mut seen = HashSet::new();
    for line in stdout.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let value = serde_json::from_slice::<Value>(line).map_err(|err| {
            CodeSearchError::OperationFailed(format!("failed to parse ripgrep JSON output: {err}"))
        })?;
        if value.get("type").and_then(Value::as_str) != Some("match") {
            continue;
        }
        let data = value
            .get("data")
            .context("ripgrep JSON match missing data")
            .map_err(|err| CodeSearchError::OperationFailed(err.to_string()))?;
        let path = data
            .get("path")
            .and_then(|value| value.get("text"))
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .ok_or_else(|| {
                CodeSearchError::OperationFailed("ripgrep JSON match missing text path".to_string())
            })?;
        let line_number = data
            .get("line_number")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                CodeSearchError::OperationFailed(
                    "ripgrep JSON match missing line number".to_string(),
                )
            })? as usize;
        let line_text = data
            .get("lines")
            .and_then(|value| value.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(submatch) = data
            .get("submatches")
            .and_then(Value::as_array)
            .and_then(|submatches| submatches.first())
        else {
            continue;
        };
        let start_byte = submatch.get("start").and_then(Value::as_u64).unwrap_or(0) as usize;
        let end_byte = submatch
            .get("end")
            .and_then(Value::as_u64)
            .unwrap_or(start_byte as u64) as usize;
        let start_column = byte_offset_to_column(line_text, start_byte);
        let end_column = byte_offset_to_column(line_text, end_byte).max(start_column + 1);
        let range = TextRange {
            start: TextPosition {
                line: line_number,
                column: start_column,
            },
            end: TextPosition {
                line: line_number,
                column: end_column,
            },
        };
        let dedupe_key = format!(
            "{}:{}:{}:{}",
            path.display(),
            range.start.line,
            range.start.column,
            range.end.column
        );
        if seen.insert(dedupe_key) {
            matches.push(TextMatch { path, range });
        }
        if matches.len() == limit {
            break;
        }
    }
    Ok(matches)
}

fn byte_offset_to_column(line: &str, byte_offset: usize) -> usize {
    line.get(..byte_offset)
        .map_or(1, |prefix| prefix.chars().count() + 1)
}

fn available_threads() -> NonZero<usize> {
    let threads = std::thread::available_parallelism()
        .map(NonZero::get)
        .unwrap_or(1)
        .clamp(1, 12);
    #[expect(clippy::expect_used)]
    NonZero::new(threads).expect("thread count should be non-zero")
}

fn dedupe_symbols(symbols: &mut Vec<CodeSearchSymbol>) {
    let mut seen = HashSet::new();
    symbols.retain(|symbol| {
        let key = format!(
            "{}:{}:{}:{}:{}",
            symbol.name,
            symbol.path.display(),
            symbol.range.as_ref().map_or(0, |range| range.start.line),
            symbol.range.as_ref().map_or(0, |range| range.start.column),
            symbol.range.as_ref().map_or(0, |range| range.end.column),
        );
        seen.insert(key)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::make_session_and_context_with_rx;
    use crate::config::test_config;
    use crate::protocol::EventMsg;
    use pretty_assertions::assert_eq;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn config_with_failing_rust_lsp() -> Config {
        let mut config = test_config();
        config.code_search.enabled = true;
        config.code_search.auto_detect = false;
        config.code_search.lsp.insert(
            "rust".to_string(),
            crate::config::types::CodeSearchLanguageServerConfig {
                command: Some(vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "exit 1".to_string(),
                ]),
            },
        );
        config
    }

    #[cfg(unix)]
    fn write_test_script(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, contents).expect("write test script");
        let mut permissions = std::fs::metadata(&path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("set script permissions");
        path
    }

    #[tokio::test]
    async fn explicit_language_server_wins_over_auto_detect() {
        let mut config = test_config();
        config.code_search.auto_detect = true;
        config.code_search.lsp.insert(
            "rust".to_string(),
            crate::config::types::CodeSearchLanguageServerConfig {
                command: Some(vec!["/bin/custom-rust-lsp".to_string()]),
            },
        );

        let resolution = resolve_language_server(&config, CodeSearchLanguage::Rust).await;

        assert_eq!(
            resolution,
            LanguageServerResolution::MissingCommand {
                language: CodeSearchLanguage::Rust,
                suggested_command: vec!["/bin/custom-rust-lsp".to_string()],
                reason: MissingCommandReason::NotInstalled,
                explicit_command: true,
                install_attempted: false,
            }
        );
    }

    #[tokio::test]
    async fn auto_detect_uses_supported_default_when_unconfigured() {
        let config = test_config();

        let resolution = resolve_language_server(&config, CodeSearchLanguage::Go).await;

        assert_eq!(
            resolution,
            LanguageServerResolution::MissingCommand {
                language: CodeSearchLanguage::Go,
                suggested_command: vec!["gopls".to_string()],
                reason: MissingCommandReason::NotInstalled,
                explicit_command: false,
                install_attempted: false,
            }
        );
    }

    #[test]
    fn auto_install_commands_include_required_dependencies() {
        let config = test_config();
        let managed_bin = managed_bin_dir(&config.codex_home).display().to_string();
        let managed_npm = managed_npm_prefix(&config.codex_home).display().to_string();

        assert_eq!(
            install_command_for_language(
                &config,
                CodeSearchLanguage::Rust,
                &InstallerCommands::default(),
            ),
            InstallCommand {
                argv: vec![
                    "rustup".to_string(),
                    "component".to_string(),
                    "add".to_string(),
                    "rust-analyzer".to_string(),
                    "rust-src".to_string(),
                ],
                env: Vec::new(),
            }
        );
        assert_eq!(
            install_command_for_language(
                &config,
                CodeSearchLanguage::Go,
                &InstallerCommands::default(),
            ),
            InstallCommand {
                argv: vec![
                    "go".to_string(),
                    "install".to_string(),
                    "golang.org/x/tools/gopls@latest".to_string(),
                ],
                env: vec![("GOBIN".to_string(), managed_bin)],
            }
        );
        assert_eq!(
            install_command_for_language(
                &config,
                CodeSearchLanguage::JavaScript,
                &InstallerCommands::default(),
            ),
            InstallCommand {
                argv: vec![
                    "npm".to_string(),
                    "install".to_string(),
                    "--prefix".to_string(),
                    managed_npm.clone(),
                    "typescript".to_string(),
                    "typescript-language-server".to_string(),
                ],
                env: Vec::new(),
            }
        );
        assert_eq!(
            install_command_for_language(
                &config,
                CodeSearchLanguage::TypeScript,
                &InstallerCommands::default(),
            ),
            InstallCommand {
                argv: vec![
                    "npm".to_string(),
                    "install".to_string(),
                    "--prefix".to_string(),
                    managed_npm.clone(),
                    "typescript".to_string(),
                    "typescript-language-server".to_string(),
                ],
                env: Vec::new(),
            }
        );
        assert_eq!(
            install_command_for_language(
                &config,
                CodeSearchLanguage::Python,
                &InstallerCommands::default(),
            ),
            InstallCommand {
                argv: vec![
                    "npm".to_string(),
                    "install".to_string(),
                    "--prefix".to_string(),
                    managed_npm,
                    "pyright".to_string(),
                ],
                env: Vec::new(),
            }
        );
    }

    #[test]
    fn managed_wrapper_resolves_bare_program_names_to_absolute_paths() {
        assert!(
            resolve_wrapper_program_path("sh")
                .as_ref()
                .is_some_and(|path| path.is_absolute())
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_rust_wrapper_executes_with_space_containing_rustup_path() {
        let tempdir = tempdir().expect("tempdir");
        let codex_home = tempdir.path().join("codex-home");
        std::fs::create_dir_all(managed_bin_dir(&codex_home)).expect("create managed bin dir");
        let installer_dir = tempdir.path().join("rustup dir with spaces");
        std::fs::create_dir_all(&installer_dir).expect("create installer dir");
        let invoked_args = tempdir.path().join("rust-analyzer-args");
        let cargo_marker = tempdir.path().join("cargo-ran");
        write_test_script(
            &installer_dir,
            "cargo",
            &format!(
                "#!/bin/sh\nset -eu\nprintf 'cargo from wrapper path\\n' > \"{}\"\n",
                cargo_marker.display()
            ),
        );
        let rust_analyzer = write_test_script(
            &installer_dir,
            "fake-rust-analyzer",
            &format!(
                "#!/bin/sh\nset -eu\ncargo --version >/dev/null\nprintf '%s\\n' \"$*\" > \"{}\"\n",
                invoked_args.display()
            ),
        );
        let rustup = write_test_script(
            &installer_dir,
            "fake-rustup",
            &format!(
                "#!/bin/sh\nset -eu\nif [ \"$1\" = \"which\" ] && [ \"$2\" = \"rust-analyzer\" ]; then\n  printf '%s\\n' \"{}\"\n  exit 0\nfi\nexit 1\n",
                rust_analyzer.display()
            ),
        );

        write_managed_rust_analyzer_wrapper(&codex_home, &rustup.display().to_string())
            .await
            .expect("write wrapper");
        let wrapper = managed_bin_dir(&codex_home).join(managed_script_name("rust-analyzer"));
        let output = std::process::Command::new(wrapper)
            .arg("--version")
            .output()
            .expect("run wrapper");

        assert!(output.status.success());
        assert_eq!(
            std::fs::read_to_string(invoked_args)
                .expect("read invoked args")
                .trim(),
            "--version"
        );
        assert_eq!(
            std::fs::read_to_string(cargo_marker)
                .expect("read cargo marker")
                .trim(),
            "cargo from wrapper path"
        );
        let wrapper = std::fs::read_to_string(
            managed_bin_dir(&codex_home).join(managed_script_name("rust-analyzer")),
        )
        .expect("read wrapper");
        assert!(wrapper.contains(&rust_analyzer.display().to_string()));
        assert!(wrapper.contains("export PATH"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resolve_language_server_refreshes_existing_managed_rust_wrapper() {
        let tempdir = tempdir().expect("tempdir");
        let codex_home = tempdir.path().join("codex-home");
        std::fs::create_dir_all(managed_bin_dir(&codex_home)).expect("create managed bin dir");
        let legacy_wrapper =
            managed_bin_dir(&codex_home).join(managed_script_name("rust-analyzer"));
        std::fs::write(
            &legacy_wrapper,
            "#!/bin/sh\nset -eu\nTARGET=\"$(rustup which rust-analyzer)\"\nexec \"$TARGET\" \"$@\"\n",
        )
        .expect("write legacy wrapper");
        let mut permissions = std::fs::metadata(&legacy_wrapper)
            .expect("legacy wrapper metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&legacy_wrapper, permissions)
            .expect("set legacy wrapper permissions");
        let rust_analyzer =
            write_test_script(tempdir.path(), "fake-rust-analyzer", "#!/bin/sh\nset -eu\n");
        let rustup = write_test_script(
            tempdir.path(),
            "fake-rustup",
            &format!(
                "#!/bin/sh\nset -eu\nif [ \"$1\" = \"which\" ] && [ \"$2\" = \"rust-analyzer\" ]; then\n  printf '%s\\n' \"{}\"\n  exit 0\nfi\nexit 1\n",
                rust_analyzer.display()
            ),
        );
        let mut config = test_config();
        config.codex_home = codex_home.clone();
        config.code_search.auto_detect = false;

        let resolution = resolve_language_server_with_installers(
            &config,
            CodeSearchLanguage::Rust,
            &InstallerCommands {
                rustup: rustup.display().to_string(),
                go: "go".to_string(),
                npm: "npm".to_string(),
            },
        )
        .await;

        assert_eq!(
            resolution,
            LanguageServerResolution::Resolved(ResolvedLanguageServer {
                language: CodeSearchLanguage::Rust,
                command: managed_language_server_command(&config, CodeSearchLanguage::Rust),
                source: LanguageServerSource::ManagedInstall,
                install_attempted: false,
            })
        );
        let contents = std::fs::read_to_string(legacy_wrapper).expect("read refreshed wrapper");
        assert!(contents.contains(&rust_analyzer.display().to_string()));
        assert!(!contents.contains("rustup which rust-analyzer"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resolve_language_server_does_not_downgrade_absolute_wrapper_when_rustup_is_unresolved()
    {
        let tempdir = tempdir().expect("tempdir");
        let codex_home = tempdir.path().join("codex-home");
        std::fs::create_dir_all(managed_bin_dir(&codex_home)).expect("create managed bin dir");
        let wrapper_path = managed_bin_dir(&codex_home).join(managed_script_name("rust-analyzer"));
        let absolute_target = tempdir.path().join("absolute-rust-analyzer");
        let wrapper_contents = format!(
            "#!/bin/sh\nset -eu\nTARGET={}\nexec \"$TARGET\" \"$@\"\n",
            serde_json::to_string(&absolute_target.display().to_string()).expect("json path")
        );
        std::fs::write(&wrapper_path, wrapper_contents).expect("write wrapper");
        let mut permissions = std::fs::metadata(&wrapper_path)
            .expect("wrapper metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&wrapper_path, permissions).expect("set wrapper permissions");
        let mut config = test_config();
        config.codex_home = codex_home;
        config.code_search.auto_detect = false;

        let resolution = resolve_language_server_with_installers(
            &config,
            CodeSearchLanguage::Rust,
            &InstallerCommands {
                rustup: "missing-rustup-command".to_string(),
                go: "go".to_string(),
                npm: "npm".to_string(),
            },
        )
        .await;

        assert_eq!(
            resolution,
            LanguageServerResolution::Resolved(ResolvedLanguageServer {
                language: CodeSearchLanguage::Rust,
                command: managed_language_server_command(&config, CodeSearchLanguage::Rust),
                source: LanguageServerSource::ManagedInstall,
                install_attempted: false,
            })
        );
        let refreshed = std::fs::read_to_string(wrapper_path).expect("read wrapper");
        assert!(refreshed.contains(&absolute_target.display().to_string()));
        assert!(!refreshed.contains("missing-rustup-command"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn auto_install_resolves_managed_rust_server_after_successful_install() {
        let tempdir = tempdir().expect("tempdir");
        let rust_analyzer =
            write_test_script(tempdir.path(), "fake-rust-analyzer", "#!/bin/sh\nset -eu\n");
        let installer = write_test_script(
            tempdir.path(),
            "fake-rustup",
            &format!(
                "#!/bin/sh\nset -eu\nif [ \"$1\" = \"component\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"which\" ] && [ \"$2\" = \"rust-analyzer\" ]; then\n  printf '%s\\n' \"{}\"\n  exit 0\nfi\nexit 1\n",
                rust_analyzer.display()
            ),
        );
        let mut config = test_config();
        config.code_search.auto_detect = false;
        config.code_search.auto_install = true;

        let resolution = resolve_language_server_with_installers(
            &config,
            CodeSearchLanguage::Rust,
            &InstallerCommands {
                rustup: installer.display().to_string(),
                go: "go".to_string(),
                npm: "npm".to_string(),
            },
        )
        .await;

        assert_eq!(
            resolution,
            LanguageServerResolution::Resolved(ResolvedLanguageServer {
                language: CodeSearchLanguage::Rust,
                command: managed_language_server_command(&config, CodeSearchLanguage::Rust),
                source: LanguageServerSource::ManagedInstall,
                install_attempted: true,
            })
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn auto_install_resolves_managed_go_server_after_successful_install() {
        let tempdir = tempdir().expect("tempdir");
        let installer = write_test_script(
            tempdir.path(),
            "fake-go",
            "#!/bin/sh\nset -eu\nmkdir -p \"$GOBIN\"\ncat <<'EOF' > \"$GOBIN/gopls\"\n#!/bin/sh\nexit 0\nEOF\nchmod +x \"$GOBIN/gopls\"\n",
        );
        let mut config = test_config();
        config.code_search.auto_detect = false;
        config.code_search.auto_install = true;

        let resolution = resolve_language_server_with_installers(
            &config,
            CodeSearchLanguage::Go,
            &InstallerCommands {
                rustup: "rustup".to_string(),
                go: installer.display().to_string(),
                npm: "npm".to_string(),
            },
        )
        .await;

        assert_eq!(
            resolution,
            LanguageServerResolution::Resolved(ResolvedLanguageServer {
                language: CodeSearchLanguage::Go,
                command: managed_language_server_command(&config, CodeSearchLanguage::Go),
                source: LanguageServerSource::ManagedInstall,
                install_attempted: true,
            })
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn auto_install_resolves_managed_javascript_and_typescript_servers_after_successful_install()
     {
        let tempdir = tempdir().expect("tempdir");
        let installer = write_test_script(
            tempdir.path(),
            "fake-npm",
            "#!/bin/sh\nset -eu\nprefix=\"$3\"\nmkdir -p \"$prefix/node_modules/.bin\"\ncat <<'EOF' > \"$prefix/node_modules/.bin/typescript-language-server\"\n#!/bin/sh\nexit 0\nEOF\nchmod +x \"$prefix/node_modules/.bin/typescript-language-server\"\n",
        );
        for language in [
            CodeSearchLanguage::JavaScript,
            CodeSearchLanguage::TypeScript,
        ] {
            let mut config = test_config();
            config.code_search.auto_detect = false;
            config.code_search.auto_install = true;

            let resolution = resolve_language_server_with_installers(
                &config,
                language,
                &InstallerCommands {
                    rustup: "rustup".to_string(),
                    go: "go".to_string(),
                    npm: installer.display().to_string(),
                },
            )
            .await;

            assert_eq!(
                resolution,
                LanguageServerResolution::Resolved(ResolvedLanguageServer {
                    language,
                    command: managed_language_server_command(&config, language),
                    source: LanguageServerSource::ManagedInstall,
                    install_attempted: true,
                })
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn auto_install_resolves_managed_python_server_after_successful_install() {
        let tempdir = tempdir().expect("tempdir");
        let installer = write_test_script(
            tempdir.path(),
            "fake-npm",
            "#!/bin/sh\nset -eu\nprefix=\"$3\"\nmkdir -p \"$prefix/node_modules/.bin\"\ncat <<'EOF' > \"$prefix/node_modules/.bin/pyright-langserver\"\n#!/bin/sh\nexit 0\nEOF\nchmod +x \"$prefix/node_modules/.bin/pyright-langserver\"\n",
        );
        let mut config = test_config();
        config.code_search.auto_detect = false;
        config.code_search.auto_install = true;

        let resolution = resolve_language_server_with_installers(
            &config,
            CodeSearchLanguage::Python,
            &InstallerCommands {
                rustup: "rustup".to_string(),
                go: "go".to_string(),
                npm: installer.display().to_string(),
            },
        )
        .await;

        assert_eq!(
            resolution,
            LanguageServerResolution::Resolved(ResolvedLanguageServer {
                language: CodeSearchLanguage::Python,
                command: managed_language_server_command(&config, CodeSearchLanguage::Python),
                source: LanguageServerSource::ManagedInstall,
                install_attempted: true,
            })
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn explicit_command_does_not_trigger_auto_install() {
        let tempdir = tempdir().expect("tempdir");
        let marker = tempdir.path().join("installer-ran");
        let installer = write_test_script(
            tempdir.path(),
            "fake-npm",
            &format!("#!/bin/sh\nset -eu\ntouch \"{}\"\n", marker.display()),
        );
        let mut config = test_config();
        config.code_search.auto_detect = false;
        config.code_search.auto_install = true;
        config.code_search.lsp.insert(
            "typescript".to_string(),
            crate::config::types::CodeSearchLanguageServerConfig {
                command: Some(vec!["/missing/typescript-language-server".to_string()]),
            },
        );

        let resolution = resolve_language_server_with_installers(
            &config,
            CodeSearchLanguage::TypeScript,
            &InstallerCommands {
                rustup: "rustup".to_string(),
                go: "go".to_string(),
                npm: installer.display().to_string(),
            },
        )
        .await;

        assert_eq!(
            resolution,
            LanguageServerResolution::MissingCommand {
                language: CodeSearchLanguage::TypeScript,
                suggested_command: vec!["/missing/typescript-language-server".to_string()],
                reason: MissingCommandReason::NotInstalled,
                explicit_command: true,
                install_attempted: false,
            }
        );
        assert!(
            !marker.exists(),
            "explicit command should short-circuit auto-install"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn auto_install_reports_install_failure() {
        let tempdir = tempdir().expect("tempdir");
        let installer = write_test_script(
            tempdir.path(),
            "fake-npm",
            "#!/bin/sh\nset -eu\necho installer failed >&2\nexit 9\n",
        );
        let mut config = test_config();
        config.code_search.auto_detect = false;
        config.code_search.auto_install = true;

        let resolution = resolve_language_server_with_installers(
            &config,
            CodeSearchLanguage::TypeScript,
            &InstallerCommands {
                rustup: "rustup".to_string(),
                go: "go".to_string(),
                npm: installer.display().to_string(),
            },
        )
        .await;

        let LanguageServerResolution::InstallFailed {
            language,
            install_command,
            error,
        } = resolution
        else {
            panic!("expected install failure resolution");
        };
        assert_eq!(language, CodeSearchLanguage::TypeScript);
        assert_eq!(
            install_command.argv,
            vec![
                installer.display().to_string(),
                "install".to_string(),
                "--prefix".to_string(),
                managed_npm_prefix(&config.codex_home).display().to_string(),
                "typescript".to_string(),
                "typescript-language-server".to_string(),
            ]
        );
        assert!(error.contains("installer failed"));
    }

    #[test]
    fn missing_command_notice_contains_warning_key() {
        let workspace_root = PathBuf::from("/repo");
        let notices = missing_command_notices(
            CodeSearchOperation::Definition,
            &workspace_root,
            CodeSearchLanguage::Rust,
            &["rust-analyzer".to_string()],
            MissingCommandReason::NotInstalled,
            false,
        );

        assert_eq!(notices.info_key, None);
        assert_eq!(
            notices.warning_key,
            Some("warn:/repo:rust:rust-analyzer".to_string())
        );
        assert!(
            notices
                .warning_message
                .as_deref()
                .is_some_and(|message| message.contains("fell back to existing search"))
        );
    }

    #[test]
    fn token_at_position_extracts_identifier() {
        let tempdir = tempdir().expect("tempdir");
        let file_path = tempdir.path().join("sample.rs");
        std::fs::write(&file_path, "fn sample() { let value_name = 1; }\n").expect("write file");

        let token = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(token_at_position(
                &file_path,
                TextPosition {
                    line: 1,
                    column: 21,
                },
            ))
            .expect("token");

        assert_eq!(token, "value_name");
    }

    #[test]
    fn heuristic_document_symbol_extracts_go_method_name() {
        let symbol = heuristic_document_symbol(
            CodeSearchLanguage::Go,
            "func (s *Server) HandleRequest() error {",
            12,
        )
        .expect("symbol");

        assert_eq!(symbol.name, "HandleRequest");
        assert_eq!(symbol.kind.as_deref(), Some("function"));
        assert_eq!(symbol.range.start.line, 12);
    }

    #[tokio::test]
    async fn find_symbols_falls_back_to_grep_when_language_server_is_missing() {
        let tempdir = tempdir().expect("tempdir");
        let workspace_root = tempdir.path();
        let file_path = workspace_root.join("lib.rs");
        std::fs::write(&file_path, "pub struct Widget;\n").expect("write file");

        let mut config = test_config();
        config.code_search.enabled = true;
        config.code_search.auto_detect = false;

        let outcome = find_symbols(
            &config,
            SymbolSearchParams {
                query: "Widget".to_string(),
                cwd: workspace_root.to_path_buf(),
                roots: vec![workspace_root.to_path_buf()],
                language_hint: Some("rust".to_string()),
                limit: 10,
            },
        )
        .await
        .expect("fallback search succeeds");

        assert_eq!(outcome.backend, CodeSearchBackend::GrepFallback);
        assert_eq!(outcome.trace.language.as_deref(), Some("rust"));
        assert_eq!(outcome.trace.resolution_source, None);
        assert!(!outcome.trace.install_attempted);
        assert_eq!(outcome.data.len(), 1);
        assert_eq!(outcome.data[0].path, file_path);
        assert_eq!(
            outcome.data[0].range.as_ref().map(|range| range.start.line),
            Some(1)
        );
        assert!(
            outcome
                .notices
                .warning_message
                .as_deref()
                .is_some_and(|message| message.contains("fell back to existing search"))
        );
    }

    #[tokio::test]
    async fn find_symbols_falls_back_when_language_server_start_fails() {
        let tempdir = tempdir().expect("tempdir");
        let workspace_root = tempdir.path();
        let file_path = workspace_root.join("lib.rs");
        std::fs::write(&file_path, "pub struct Widget;\n").expect("write file");

        let config = config_with_failing_rust_lsp();

        let outcome = find_symbols(
            &config,
            SymbolSearchParams {
                query: "Widget".to_string(),
                cwd: workspace_root.to_path_buf(),
                roots: vec![workspace_root.to_path_buf()],
                language_hint: Some("rust".to_string()),
                limit: 10,
            },
        )
        .await
        .expect("fallback search succeeds");

        assert_eq!(outcome.backend, CodeSearchBackend::GrepFallback);
        assert_eq!(outcome.trace.language.as_deref(), Some("rust"));
        assert_eq!(outcome.trace.resolution_source.as_deref(), Some("explicit"));
        assert!(!outcome.trace.install_attempted);
        assert_eq!(outcome.data.len(), 1);
        assert_eq!(outcome.data[0].path, file_path);
        assert!(
            outcome
                .notices
                .warning_message
                .as_deref()
                .is_some_and(|message| message.contains("`/bin/sh -c exit 1` failed"))
        );
        assert_eq!(
            outcome.notices.warning_key,
            Some(format!(
                "warn-runtime:{}:rust:/bin/sh -c exit 1",
                workspace_root.display()
            ))
        );
    }

    #[tokio::test]
    async fn find_definitions_falls_back_when_language_server_start_fails() {
        let tempdir = tempdir().expect("tempdir");
        let workspace_root = tempdir.path();
        let file_path = workspace_root.join("lib.rs");
        std::fs::write(
            &file_path,
            "pub struct Widget;\nfn use_it() { let _ = Widget; }\n",
        )
        .expect("write file");

        let config = config_with_failing_rust_lsp();

        let outcome = find_definitions(
            &config,
            DefinitionSearchParams {
                path: file_path.clone(),
                position: TextPosition {
                    line: 2,
                    column: 25,
                },
                cwd: workspace_root.to_path_buf(),
                roots: vec![workspace_root.to_path_buf()],
            },
        )
        .await
        .expect("fallback search succeeds");

        assert_eq!(outcome.backend, CodeSearchBackend::GrepFallback);
        assert_eq!(outcome.trace.language.as_deref(), Some("rust"));
        assert_eq!(outcome.trace.resolution_source.as_deref(), Some("explicit"));
        assert!(!outcome.trace.install_attempted);
        assert!(
            outcome
                .data
                .iter()
                .any(|location| location.path == file_path && location.range.start.line == 1)
        );
        assert!(
            outcome
                .notices
                .warning_message
                .as_deref()
                .is_some_and(|message| message.contains("definition lookup fell back"))
        );
    }

    #[tokio::test]
    async fn document_symbols_fall_back_when_language_server_start_fails() {
        let tempdir = tempdir().expect("tempdir");
        let workspace_root = tempdir.path();
        let file_path = workspace_root.join("lib.rs");
        std::fs::write(&file_path, "pub struct Widget;\n").expect("write file");

        let config = config_with_failing_rust_lsp();

        let outcome = document_symbols(
            &config,
            DocumentSymbolsParams {
                path: file_path,
                cwd: workspace_root.to_path_buf(),
            },
        )
        .await
        .expect("fallback search succeeds");

        assert_eq!(outcome.backend, CodeSearchBackend::GrepFallback);
        assert_eq!(outcome.trace.language.as_deref(), Some("rust"));
        assert_eq!(outcome.trace.resolution_source.as_deref(), Some("explicit"));
        assert!(!outcome.trace.install_attempted);
        assert_eq!(outcome.data.len(), 1);
        assert_eq!(outcome.data[0].name, "Widget");
        assert!(
            outcome
                .notices
                .warning_message
                .as_deref()
                .is_some_and(|message| message.contains("document symbol lookup fell back"))
        );
    }

    #[tokio::test]
    async fn emit_session_notices_dedupes_repeated_missing_command_messages() {
        let (session, turn, rx) = make_session_and_context_with_rx().await;
        let notices = missing_command_notices(
            CodeSearchOperation::Definition,
            &turn.cwd,
            CodeSearchLanguage::Rust,
            &["rust-analyzer".to_string()],
            MissingCommandReason::NotInstalled,
            false,
        );

        emit_session_notices(&session, &turn, &notices).await;

        let mut saw_warning = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while !saw_warning {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let event = timeout(remaining, rx.recv())
                .await
                .expect("timeout waiting for dedupe seed events")
                .expect("event");
            if let EventMsg::Warning(event) = event.msg {
                saw_warning = true;
                assert_eq!(Some(event.message), notices.warning_message.clone());
            }
        }

        emit_session_notices(&session, &turn, &notices).await;

        assert!(
            timeout(Duration::from_millis(200), rx.recv())
                .await
                .is_err(),
            "duplicate notice emission should be suppressed"
        );
    }
}
