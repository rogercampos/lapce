use std::{collections::HashMap, path::Path};

/// Resolves the user's login shell environment by spawning an interactive
/// login shell with an explicit `cd` into the workspace directory. This
/// ensures:
///
/// - Login profiles are sourced (`-l` → `.zprofile`, `.bash_profile`)
/// - Interactive rc files are sourced (`-i` → `.zshrc`, `.bashrc`) where
///   version managers like `eval "$(mise activate zsh)"` are configured
/// - The explicit `cd` triggers directory-change hooks (mise's `chpwd`/
///   `precmd`) so project-local `.mise.toml` / `.tool-versions` are resolved
///
/// On non-Unix platforms, returns an empty map (Windows GUI apps already
/// inherit the full user environment).
///
/// On any failure, logs a warning and returns an empty map (graceful
/// degradation — falls back to the current inherited-env behavior).
pub fn resolve_shell_env(workspace: Option<&Path>) -> HashMap<String, String> {
    #[cfg(unix)]
    {
        resolve_shell_env_unix(workspace)
    }
    #[cfg(not(unix))]
    {
        let _ = workspace;
        HashMap::new()
    }
}

#[cfg(unix)]
fn resolve_shell_env_unix(workspace: Option<&Path>) -> HashMap<String, String> {
    use std::process::{Command, Stdio};

    let shell = match std::env::var("SHELL") {
        Ok(s) if !s.is_empty() => s,
        _ => {
            tracing::warn!("$SHELL not set, skipping shell env resolution");
            return HashMap::new();
        }
    };

    // Build a shell command that cd's into the workspace (triggering
    // directory-change hooks like mise's chpwd) then dumps the env.
    let script = match workspace {
        Some(dir) if dir.is_dir() => {
            format!(
                "cd {} && env -0",
                shell_escape(dir.to_string_lossy().as_ref())
            )
        }
        _ => "env -0".to_string(),
    };

    let mut cmd = Command::new(&shell);
    // -i (interactive) sources .zshrc/.bashrc where version managers are activated.
    // -l (login) sources profile files for base PATH setup.
    cmd.args(["-i", "-l", "-c", &script]);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());

    let output = match cmd.output() {
        Ok(o) => o,
        Err(err) => {
            tracing::warn!("failed to spawn login shell for env resolution: {err}");
            return HashMap::new();
        }
    };

    if !output.status.success() {
        tracing::warn!(
            "login shell exited with status {} during env resolution",
            output.status
        );
        return HashMap::new();
    }

    let stdout = match String::from_utf8(output.stdout) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!("non-UTF-8 output from login shell env: {err}");
            return HashMap::new();
        }
    };

    // Parse null-delimited env output. Any garbage printed by shell rc files
    // before `env -0` runs will land in the first segment; we filter it out
    // by requiring keys to be valid env var names (alphanumeric + underscore).
    let env: HashMap<String, String> = stdout
        .split('\0')
        .filter(|s| !s.is_empty())
        .filter_map(|entry| {
            let (key, value) = entry.split_once('=')?;
            if key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                && !key.is_empty()
            {
                Some((key.to_string(), value.to_string()))
            } else {
                None
            }
        })
        .collect();

    tracing::info!(
        shell = shell,
        workspace = ?workspace,
        vars = env.len(),
        "resolved shell environment"
    );
    if let Some(path) = env.get("PATH") {
        tracing::debug!(PATH = path, "shell env PATH");
    }

    env
}

/// Single-quote a string for safe embedding in a shell command.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_escape_simple_string() {
        assert_eq!(shell_escape("hello"), "'hello'");
    }

    #[test]
    fn shell_escape_empty_string() {
        assert_eq!(shell_escape(""), "''");
    }

    #[test]
    fn shell_escape_with_spaces() {
        assert_eq!(shell_escape("hello world"), "'hello world'");
    }

    #[test]
    fn shell_escape_with_single_quote() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn shell_escape_with_multiple_single_quotes() {
        assert_eq!(shell_escape("a'b'c"), "'a'\\''b'\\''c'");
    }

    #[test]
    fn shell_escape_with_double_quotes() {
        // Double quotes are not special inside single quotes
        assert_eq!(shell_escape("say \"hi\""), "'say \"hi\"'");
    }

    #[test]
    fn shell_escape_with_special_shell_chars() {
        // All these are safely quoted inside single quotes
        assert_eq!(shell_escape("$HOME"), "'$HOME'");
        assert_eq!(shell_escape("foo;bar"), "'foo;bar'");
        assert_eq!(shell_escape("a && b"), "'a && b'");
        assert_eq!(shell_escape("$(cmd)"), "'$(cmd)'");
        assert_eq!(shell_escape("`cmd`"), "'`cmd`'");
    }

    #[test]
    fn shell_escape_with_path() {
        assert_eq!(
            shell_escape("/home/user/my project"),
            "'/home/user/my project'"
        );
    }

    #[test]
    fn shell_escape_with_newline() {
        assert_eq!(shell_escape("line1\nline2"), "'line1\nline2'");
    }

    #[test]
    fn shell_escape_with_backslash() {
        assert_eq!(shell_escape("back\\slash"), "'back\\slash'");
    }

    #[test]
    fn shell_escape_only_single_quote() {
        assert_eq!(shell_escape("'"), "''\\'''");
    }
}
