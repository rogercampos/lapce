use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};

/// The kind of project detected by marker file presence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectKind {
    Rust,
    Ruby,
    JavaScript,
    Go,
    Python,
    Elixir,
    Java,
    CSharp,
    Swift,
}

impl ProjectKind {
    /// Filenames whose presence in a directory indicates this project kind.
    pub fn marker_files(&self) -> &'static [&'static str] {
        match self {
            ProjectKind::Rust => &["Cargo.toml"],
            ProjectKind::Ruby => &["Gemfile"],
            ProjectKind::JavaScript => &["package.json"],
            ProjectKind::Go => &["go.mod"],
            ProjectKind::Python => &["pyproject.toml", "setup.py", "setup.cfg"],
            ProjectKind::Elixir => &["mix.exs"],
            ProjectKind::Java => &["pom.xml", "build.gradle", "build.gradle.kts"],
            ProjectKind::CSharp => &["global.json"],
            ProjectKind::Swift => &["Package.swift"],
        }
    }

    /// LSP language identifiers associated with this project kind.
    pub fn lsp_languages(&self) -> &'static [&'static str] {
        match self {
            ProjectKind::Rust => &["rust"],
            ProjectKind::Ruby => &["ruby"],
            ProjectKind::JavaScript => &["javascript", "typescript"],
            ProjectKind::Go => &["go"],
            ProjectKind::Python => &["python"],
            ProjectKind::Elixir => &["elixir"],
            ProjectKind::Java => &["java"],
            ProjectKind::CSharp => &["csharp"],
            ProjectKind::Swift => &["swift"],
        }
    }

    /// Display name for this project kind.
    pub fn label(&self) -> &'static str {
        match self {
            ProjectKind::Rust => "Rust",
            ProjectKind::Ruby => "Ruby",
            ProjectKind::JavaScript => "JavaScript",
            ProjectKind::Go => "Go",
            ProjectKind::Python => "Python",
            ProjectKind::Elixir => "Elixir",
            ProjectKind::Java => "Java",
            ProjectKind::CSharp => "C#",
            ProjectKind::Swift => "Swift",
        }
    }

    /// All known project kinds.
    pub fn all() -> &'static [ProjectKind] {
        &[
            ProjectKind::Rust,
            ProjectKind::Ruby,
            ProjectKind::JavaScript,
            ProjectKind::Go,
            ProjectKind::Python,
            ProjectKind::Elixir,
            ProjectKind::Java,
            ProjectKind::CSharp,
            ProjectKind::Swift,
        ]
    }
}

/// Information about a detected project within the workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectInfo {
    /// Root directory of the project (parent of the marker file).
    pub root: PathBuf,
    /// What kind of project this is.
    pub kind: ProjectKind,
    /// LSP language identifiers this project covers.
    pub languages: Vec<String>,
    /// The marker file that triggered detection (e.g. "Gemfile").
    pub marker_file: String,
    /// Tool versions relevant to this project, extracted from the shell environment.
    /// Each entry is (tool_name, version_string), e.g. ("ruby", "3.2.0").
    pub tool_versions: Vec<(String, String)>,
    /// Detected version manager, if any (e.g. "mise", "rbenv", "asdf").
    pub version_manager: Option<String>,
    /// LSP server commands that would serve this project, if configured.
    pub lsp_servers: Vec<String>,
}

/// Detect the active version manager for a specific project kind.
/// Only checks version managers relevant to that language.
pub fn detect_version_manager(
    kind: &ProjectKind,
    env: &HashMap<String, String>,
) -> Option<String> {
    // mise and asdf are polyglot — relevant to any language, but only report
    // them if they actually manage a tool for this project's language.
    let mise_key = match kind {
        ProjectKind::Ruby => "MISE_RUBY_VERSION",
        ProjectKind::JavaScript => "MISE_NODE_VERSION",
        ProjectKind::Python => "MISE_PYTHON_VERSION",
        ProjectKind::Go => "MISE_GO_VERSION",
        ProjectKind::Rust => "MISE_RUST_VERSION",
        ProjectKind::Elixir => "MISE_ELIXIR_VERSION",
        ProjectKind::Java => "MISE_JAVA_VERSION",
        ProjectKind::Swift => "MISE_SWIFT_VERSION",
        ProjectKind::CSharp => return None,
    };

    if env.contains_key(mise_key) {
        return Some("mise".to_string());
    }

    // asdf — check ASDF_DIR exists and the relevant tool version is set
    if env.contains_key("ASDF_DIR") || env.contains_key("ASDF_DATA_DIR") {
        // asdf doesn't set per-tool env vars reliably, so if asdf is present
        // and we found a tool version, it's likely asdf managing it.
        return Some("asdf".to_string());
    }

    // Language-specific version managers
    match kind {
        ProjectKind::Ruby => {
            if env.contains_key("RBENV_ROOT") || env.contains_key("RBENV_VERSION") {
                return Some("rbenv".to_string());
            }
            if env.contains_key("RVM_PATH") || env.contains_key("MY_RUBY_HOME") {
                return Some("rvm".to_string());
            }
            // chruby sets RUBY_ROOT
            if env.contains_key("RUBY_ROOT") {
                return Some("chruby".to_string());
            }
        }
        ProjectKind::JavaScript => {
            if env.contains_key("NVM_DIR") || env.contains_key("NVM_BIN") {
                return Some("nvm".to_string());
            }
            if env.contains_key("FNM_DIR") || env.contains_key("FNM_MULTISHELL_PATH")
            {
                return Some("fnm".to_string());
            }
        }
        ProjectKind::Python => {
            if env.contains_key("PYENV_ROOT") || env.contains_key("PYENV_VERSION") {
                return Some("pyenv".to_string());
            }
        }
        ProjectKind::Go => {
            if env.contains_key("GOENV_ROOT") {
                return Some("goenv".to_string());
            }
        }
        ProjectKind::Rust => {
            if env.contains_key("RUSTUP_HOME")
                || env.contains_key("RUSTUP_TOOLCHAIN")
            {
                return Some("rustup".to_string());
            }
        }
        ProjectKind::Java => {
            if env.contains_key("SDKMAN_DIR") {
                return Some("sdkman".to_string());
            }
        }
        ProjectKind::Swift => {
            if env.contains_key("SWIFTENV_ROOT") {
                return Some("swiftenv".to_string());
            }
        }
        ProjectKind::Elixir | ProjectKind::CSharp => {}
    }

    None
}

/// Extract tool versions relevant to the given project kind from environment
/// variables. Only returns versions that matter for the project type — a Ruby
/// project gets ruby/bundler info, not rust/go/node.
pub fn extract_tool_versions(
    kind: &ProjectKind,
    env: &HashMap<String, String>,
) -> Vec<(String, String)> {
    match kind {
        ProjectKind::Ruby => extract_ruby_versions(env),
        ProjectKind::JavaScript => extract_js_versions(env),
        ProjectKind::Python => extract_python_versions(env),
        ProjectKind::Go => extract_go_versions(env),
        ProjectKind::Rust => extract_rust_versions(env),
        ProjectKind::Elixir => extract_elixir_versions(env),
        ProjectKind::Java => extract_java_versions(env),
        ProjectKind::Swift => extract_swift_versions(env),
        ProjectKind::CSharp => extract_dotnet_versions(env),
    }
}

fn extract_ruby_versions(env: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut versions = Vec::new();

    // Try multiple sources for the ruby version, in priority order
    let ruby_version = None
        // MISE_RUBY_VERSION (mise manages ruby)
        .or_else(|| env.get("MISE_RUBY_VERSION").cloned())
        // RUBY_VERSION env var (some version managers set this)
        .or_else(|| env.get("RUBY_VERSION").cloned())
        // RBENV_VERSION (rbenv shell)
        .or_else(|| env.get("RBENV_VERSION").cloned())
        // MY_RUBY_HOME (rvm): e.g. /usr/local/rvm/rubies/ruby-3.2.0
        .or_else(|| {
            env.get("MY_RUBY_HOME").and_then(|home| {
                home.rsplit('/')
                    .next()
                    .and_then(|s| s.strip_prefix("ruby-").map(|v| v.to_string()))
            })
        })
        // GEM_HOME: e.g. ~/.gem/ruby/3.2.0 or ~/.local/share/gem/ruby/3.2.0
        .or_else(|| {
            env.get("GEM_HOME").and_then(|home| {
                home.rsplit('/').next().and_then(|s| {
                    if s.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                        Some(s.to_string())
                    } else {
                        None
                    }
                })
            })
        })
        // Parse PATH for rbenv/rvm version directories
        // e.g. ~/.rbenv/versions/3.2.0/bin or ~/.rvm/rubies/ruby-3.2.0/bin
        .or_else(|| {
            env.get("PATH").and_then(|path| {
                for entry in path.split(':') {
                    // rbenv: ~/.rbenv/versions/3.2.0/bin
                    if entry.contains("/versions/") && entry.contains("ruby")
                        || entry.contains("rbenv")
                    {
                        for segment in entry.split('/') {
                            if segment
                                .chars()
                                .next()
                                .is_some_and(|c| c.is_ascii_digit())
                                && segment.contains('.')
                            {
                                return Some(segment.to_string());
                            }
                        }
                    }
                    // rvm: ~/.rvm/rubies/ruby-3.2.0/bin
                    if entry.contains("/rubies/ruby-") {
                        for segment in entry.split('/') {
                            if let Some(v) = segment.strip_prefix("ruby-") {
                                return Some(v.to_string());
                            }
                        }
                    }
                }
                None
            })
        });

    if let Some(v) = ruby_version {
        versions.push(("ruby".to_string(), v));
    }

    // GEM_HOME path (useful context — shows where gems are installed)
    if let Some(gem_home) = env.get("GEM_HOME") {
        versions.push(("GEM_HOME".to_string(), gem_home.clone()));
    }

    // Bundler version from BUNDLER_VERSION env var
    if let Some(v) = env.get("BUNDLER_VERSION") {
        versions.push(("bundler".to_string(), v.clone()));
    }

    // RAILS_ENV if set
    if let Some(v) = env.get("RAILS_ENV") {
        versions.push(("RAILS_ENV".to_string(), v.clone()));
    }

    versions
}

fn extract_js_versions(env: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut versions = Vec::new();

    let node_version = None
        .or_else(|| env.get("MISE_NODE_VERSION").cloned())
        .or_else(|| env.get("NODE_VERSION").cloned())
        .or_else(|| {
            // NVM: parse from NVM_BIN, e.g. ~/.nvm/versions/node/v20.1.0/bin
            env.get("NVM_BIN").and_then(|bin| {
                bin.split('/')
                    .find_map(|s| s.strip_prefix('v').map(|v| v.to_string()))
            })
        })
        .or_else(|| {
            // FNM: parse from FNM_MULTISHELL_PATH
            env.get("FNM_MULTISHELL_PATH").and_then(|p| {
                p.split('/').find_map(|s| {
                    s.strip_prefix("node-v")
                        .or_else(|| s.strip_prefix('v'))
                        .map(|v| v.to_string())
                })
            })
        });

    if let Some(v) = node_version {
        versions.push(("node".to_string(), v));
    }

    // npm/yarn/pnpm from MISE
    if let Some(v) = env.get("MISE_YARN_VERSION") {
        versions.push(("yarn".to_string(), v.clone()));
    }
    if let Some(v) = env.get("MISE_PNPM_VERSION") {
        versions.push(("pnpm".to_string(), v.clone()));
    }
    if let Some(v) = env.get("MISE_BUN_VERSION") {
        versions.push(("bun".to_string(), v.clone()));
    }

    versions
}

fn extract_python_versions(env: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut versions = Vec::new();

    let python_version = None
        .or_else(|| env.get("MISE_PYTHON_VERSION").cloned())
        .or_else(|| env.get("PYTHON_VERSION").cloned())
        .or_else(|| env.get("PYENV_VERSION").cloned());

    if let Some(v) = python_version {
        versions.push(("python".to_string(), v));
    }

    if let Some(venv) = env.get("VIRTUAL_ENV") {
        let name = venv.rsplit('/').next().unwrap_or(venv);
        versions.push(("venv".to_string(), name.to_string()));
    }

    if let Some(v) = env.get("CONDA_DEFAULT_ENV") {
        versions.push(("conda".to_string(), v.clone()));
    }

    versions
}

fn extract_go_versions(env: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut versions = Vec::new();

    let go_version = None
        .or_else(|| env.get("MISE_GO_VERSION").cloned())
        .or_else(|| env.get("GOVERSION").cloned());

    if let Some(v) = go_version {
        versions.push(("go".to_string(), v));
    }

    if let Some(v) = env.get("GOPATH") {
        versions.push(("GOPATH".to_string(), v.clone()));
    }

    versions
}

fn extract_rust_versions(env: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut versions = Vec::new();

    if let Some(v) = env.get("RUSTUP_TOOLCHAIN") {
        versions.push(("toolchain".to_string(), v.clone()));
    }

    if let Some(v) = env.get("CARGO_HOME") {
        versions.push(("CARGO_HOME".to_string(), v.clone()));
    }

    versions
}

fn extract_elixir_versions(env: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut versions = Vec::new();

    let elixir_version = None
        .or_else(|| env.get("MISE_ELIXIR_VERSION").cloned())
        .or_else(|| env.get("ELIXIR_VERSION").cloned());

    if let Some(v) = elixir_version {
        versions.push(("elixir".to_string(), v));
    }

    let erlang_version = None
        .or_else(|| env.get("MISE_ERLANG_VERSION").cloned())
        .or_else(|| env.get("ERLANG_VERSION").cloned());

    if let Some(v) = erlang_version {
        versions.push(("erlang".to_string(), v));
    }

    if let Some(v) = env.get("MIX_ENV") {
        versions.push(("MIX_ENV".to_string(), v.clone()));
    }

    versions
}

fn extract_java_versions(env: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut versions = Vec::new();

    let java_version = None
        .or_else(|| env.get("MISE_JAVA_VERSION").cloned())
        .or_else(|| {
            env.get("JAVA_HOME").and_then(|java_home| {
                for segment in java_home.rsplit('/') {
                    if let Some(rest) = segment.strip_prefix("jdk-").or_else(|| {
                        segment
                            .strip_prefix("jdk")
                            .filter(|s| s.starts_with(|c: char| c.is_ascii_digit()))
                    }) {
                        return Some(rest.to_string());
                    }
                    if let Some(rest) = segment.strip_prefix("java-") {
                        if rest.starts_with(|c: char| c.is_ascii_digit()) {
                            return Some(
                                rest.split('-').next().unwrap_or(rest).to_string(),
                            );
                        }
                    }
                }
                None
            })
        });

    if let Some(v) = java_version {
        versions.push(("java".to_string(), v));
    }

    if let Some(v) = env.get("JAVA_HOME") {
        versions.push(("JAVA_HOME".to_string(), v.clone()));
    }

    versions
}

fn extract_swift_versions(env: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut versions = Vec::new();

    if let Some(v) = env.get("MISE_SWIFT_VERSION") {
        versions.push(("swift".to_string(), v.clone()));
    }

    versions
}

fn extract_dotnet_versions(env: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut versions = Vec::new();

    if let Some(v) = env.get("DOTNET_ROOT") {
        versions.push(("DOTNET_ROOT".to_string(), v.clone()));
    }

    versions
}

#[cfg(test)]
mod tests {
    use super::ProjectKind;

    #[test]
    fn all_project_kinds_have_non_empty_marker_files() {
        let all_variants = [
            ProjectKind::Rust,
            ProjectKind::Ruby,
            ProjectKind::JavaScript,
            ProjectKind::Go,
            ProjectKind::Python,
            ProjectKind::Elixir,
            ProjectKind::Java,
            ProjectKind::CSharp,
            ProjectKind::Swift,
        ];

        for kind in &all_variants {
            assert!(
                !kind.marker_files().is_empty(),
                "{:?} has empty marker_files()",
                kind
            );
        }
    }
}
