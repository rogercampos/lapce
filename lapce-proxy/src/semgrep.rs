use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use lapce_rpc::core::CoreRpcHandler;
use lsp_types::{
    Diagnostic, DiagnosticSeverity, Position, PublishDiagnosticsParams, Range, Url,
};
use parking_lot::Mutex;
use serde::Deserialize;

pub struct SemgrepRunner {
    workspace: PathBuf,
    config_path: PathBuf,
    semgrep_bin: String,
    core_rpc: CoreRpcHandler,
    env: Arc<HashMap<String, String>>,
    /// Per-file file_generation counter — newer scans supersede older ones
    scan_file_generations: Arc<Mutex<HashMap<PathBuf, u64>>>,
    file_generation: Arc<AtomicU64>,
}

#[derive(Deserialize)]
struct SemgrepOutput {
    results: Vec<SemgrepResult>,
}

#[derive(Deserialize)]
struct SemgrepResult {
    #[allow(dead_code)]
    check_id: String,
    start: SemgrepPosition,
    end: SemgrepPosition,
    extra: SemgrepExtra,
}

#[derive(Deserialize)]
struct SemgrepPosition {
    line: u32,
    col: u32,
}

#[derive(Deserialize)]
struct SemgrepExtra {
    message: String,
    severity: String,
}

impl SemgrepRunner {
    /// Creates a new SemgrepRunner if the workspace has a semgrep config
    /// and the `semgrep` binary is available in the shell environment.
    pub fn new(
        workspace: PathBuf,
        core_rpc: CoreRpcHandler,
        env: Arc<HashMap<String, String>>,
    ) -> Option<Self> {
        // Check for semgrep config
        let config_path = if workspace.join(".semgrep.yml").exists() {
            workspace.join(".semgrep.yml")
        } else if workspace.join(".semgrep.yaml").exists() {
            workspace.join(".semgrep.yaml")
        } else if workspace.join(".semgrep").is_dir() {
            workspace.join(".semgrep")
        } else {
            return None;
        };

        // Find semgrep binary in PATH
        let semgrep_bin = find_in_env("semgrep", &env)?;

        tracing::info!(
            "[semgrep] Found config at {:?}, binary at {:?}",
            config_path,
            semgrep_bin
        );

        Some(Self {
            workspace,
            config_path,
            semgrep_bin,
            core_rpc,
            env,
            scan_file_generations: Arc::new(Mutex::new(HashMap::new())),
            file_generation: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Triggers a scan of the given file. If a scan is already in-flight for
    /// this file, it will be superseded (the older scan's results are discarded).
    pub fn scan_file(&self, path: PathBuf) {
        tracing::info!("[semgrep] Queuing scan for {:?}", path);
        let file_gen = self.file_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.scan_file_generations
            .lock()
            .insert(path.clone(), file_gen);

        let workspace = self.workspace.clone();
        let config_path = self.config_path.clone();
        let semgrep_bin = self.semgrep_bin.clone();
        let core_rpc = self.core_rpc.clone();
        let env = self.env.clone();
        let scan_file_generations = self.scan_file_generations.clone();

        thread::spawn(move || {
            tracing::info!("[semgrep] Running scan on {:?}", path);
            let output = std::process::Command::new(&semgrep_bin)
                .args([
                    "scan",
                    "--config",
                    &config_path.to_string_lossy(),
                    "--json",
                    "--no-git-ignore",
                    "--metrics=off",
                    "--quiet",
                    "--disable-version-check",
                    "--timeout",
                    "30",
                    "--jobs",
                    "1",
                ])
                .arg(&path)
                .current_dir(&workspace)
                .envs(env.iter())
                .output();

            // Check if this scan has been superseded
            {
                let file_gens = scan_file_generations.lock();
                if let Some(&current_file_gen) = file_gens.get(&path) {
                    if current_file_gen != file_gen {
                        return; // Superseded by a newer scan
                    }
                }
            }

            let uri = match Url::from_file_path(&path) {
                Ok(u) => u,
                Err(_) => return,
            };

            match output {
                Ok(out) => {
                    let exit_code = out.status.code().unwrap_or(-1);
                    if exit_code == 0 || exit_code == 1 {
                        // exit 0 = no findings, exit 1 = findings present
                        let diagnostics = match serde_json::from_slice::<SemgrepOutput>(
                            &out.stdout,
                        ) {
                            Ok(parsed) => {
                                tracing::info!(
                                    "[semgrep] Scan of {:?} found {} results",
                                    path,
                                    parsed.results.len()
                                );
                                parsed
                                    .results
                                    .into_iter()
                                    .map(|r| to_diagnostic(r))
                                    .collect()
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "[semgrep] Failed to parse JSON output for {:?}: {}",
                                    path,
                                    e
                                );
                                Vec::new()
                            }
                        };
                        core_rpc.publish_diagnostics(PublishDiagnosticsParams {
                            uri,
                            diagnostics,
                            version: None,
                        });
                    } else {
                        // Error exit code — clear diagnostics and log
                        tracing::warn!(
                            "[semgrep] Exit code {} for {:?}: {}",
                            exit_code,
                            path,
                            String::from_utf8_lossy(&out.stderr)
                        );
                        core_rpc.publish_diagnostics(PublishDiagnosticsParams {
                            uri,
                            diagnostics: Vec::new(),
                            version: None,
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "[semgrep] Failed to run semgrep for {:?}: {}",
                        path,
                        e
                    );
                }
            }
        });
    }
}

fn to_diagnostic(result: SemgrepResult) -> Diagnostic {
    let severity = match result.extra.severity.to_uppercase().as_str() {
        "ERROR" => DiagnosticSeverity::ERROR,
        "WARNING" => DiagnosticSeverity::WARNING,
        "INFO" => DiagnosticSeverity::INFORMATION,
        _ => DiagnosticSeverity::WARNING,
    };

    Diagnostic {
        range: Range {
            start: Position {
                line: result.start.line.saturating_sub(1),
                character: result.start.col.saturating_sub(1),
            },
            end: Position {
                line: result.end.line.saturating_sub(1),
                character: result.end.col.saturating_sub(1),
            },
        },
        severity: Some(severity),
        source: Some("semgrep".to_string()),
        message: result.extra.message,
        ..Default::default()
    }
}

/// Find a command binary by searching the PATH from the shell environment.
fn find_in_env(cmd: &str, env: &HashMap<String, String>) -> Option<String> {
    let path_var = env.get("PATH")?;
    for dir in std::env::split_paths(path_var) {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}
