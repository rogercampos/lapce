use std::path::PathBuf;

use anyhow::{Error, Result, anyhow};
use lapce_core::directory::Directory;
use lapce_rpc::{
    RpcMessage,
    file::{LineCol, PathObject},
    proxy::{ProxyMessage, ProxyNotification},
};

/// Parses a CLI path argument that may include line and column numbers in the
/// format `path:line:column`. Splits from the RIGHT using `:` to handle paths
/// that may contain colons (e.g., Windows drive letters like `C:\foo`).
/// Falls back gracefully: if the rightmost segments aren't valid numbers,
/// they're treated as part of the path.
pub fn parse_file_line_column(path: &str) -> Result<PathObject, Error> {
    if let Ok(path) = PathBuf::from(path).canonicalize() {
        return Ok(PathObject {
            is_dir: path.is_dir(),
            path,
            linecol: None,
        });
    }

    let pwd = std::env::current_dir().unwrap_or_default();

    let mut splits = path.rsplit(':').peekable();
    let (path, linecol) = if let Some(first_rhs) =
        splits.peek().and_then(|s| s.parse::<usize>().ok())
    {
        splits.next();
        if let Some(second_rhs) = splits.peek().and_then(|s| s.parse::<usize>().ok())
        {
            splits.next();
            let remaning: Vec<&str> = splits.rev().collect();
            let path = remaning.join(":");
            let path = PathBuf::from(path);
            let path = if let Ok(path) = path.canonicalize() {
                path
            } else {
                pwd.join(&path)
            };
            (
                path,
                Some(LineCol {
                    line: second_rhs,
                    column: first_rhs,
                }),
            )
        } else {
            let remaning: Vec<&str> = splits.rev().collect();
            let path = remaning.join(":");
            let path = PathBuf::from(path);
            let path = if let Ok(path) = path.canonicalize() {
                path
            } else {
                pwd.join(&path)
            };
            (
                path,
                Some(LineCol {
                    line: first_rhs,
                    column: 1,
                }),
            )
        }
    } else {
        (pwd.join(path), None)
    };

    Ok(PathObject {
        is_dir: path.is_dir(),
        path,
        linecol,
    })
}

/// Attempts to send an "open paths" message to an already-running Lapce instance
/// via a local socket (Unix domain socket / named pipe). This enables the
/// `lapce-proxy path/to/file` CLI workflow where a second invocation hands off
/// to the first. If no instance is listening, the connection fails and the caller
/// should exit.
pub fn try_open_in_existing_process(paths: &[PathObject]) -> Result<()> {
    let local_socket = Directory::local_socket()
        .ok_or_else(|| anyhow!("can't get local socket folder"))?;
    let mut socket =
        interprocess::local_socket::LocalSocketStream::connect(local_socket)?;

    let msg: ProxyMessage = RpcMessage::Notification(ProxyNotification::OpenPaths {
        paths: paths.to_vec(),
    });
    lapce_rpc::stdio::write_msg(&mut socket, msg)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{env, path::PathBuf};

    use super::parse_file_line_column;
    use crate::cli::PathObject;

    #[test]
    #[cfg(windows)]
    fn test_absolute_path() {
        assert_eq!(
            parse_file_line_column("C:\\Cargo.toml:55").unwrap(),
            PathObject::new(PathBuf::from("C:\\Cargo.toml"), false, 55, 1),
        );
    }

    #[test]
    #[cfg(windows)]
    fn test_relative_path() {
        assert_eq!(
            parse_file_line_column(".\\..\\Cargo.toml:55").unwrap(),
            PathObject::new(
                PathBuf::from(".\\..\\Cargo.toml").canonicalize().unwrap(),
                false,
                55,
                1
            ),
        );
    }

    #[test]
    #[cfg(windows)]
    fn test_directory_looking_like_file() {
        assert_eq!(
            parse_file_line_column(".\\Cargo.toml\\").unwrap(),
            PathObject::from_path(
                env::current_dir().unwrap().join("Cargo.toml"),
                false
            ),
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_absolute_path() {
        assert_eq!(
            parse_file_line_column("/tmp/Cargo.toml:55").unwrap(),
            PathObject::new(PathBuf::from("/tmp/Cargo.toml"), false, 55, 1),
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_relative_path() {
        assert_eq!(
            parse_file_line_column("./../Cargo.toml").unwrap(),
            PathObject::from_path(
                PathBuf::from("./../Cargo.toml").canonicalize().unwrap(),
                false,
            ),
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_directory_looking_like_file() {
        assert_eq!(
            parse_file_line_column("./Cargo.toml/").unwrap(),
            PathObject::from_path(
                env::current_dir().unwrap().join("Cargo.toml"),
                false
            ),
        );
    }

    #[test]
    fn test_current_dir() {
        assert_eq!(
            parse_file_line_column(".").unwrap(),
            PathObject::from_path(
                env::current_dir().unwrap().canonicalize().unwrap(),
                true
            ),
        );
    }

    #[test]
    fn test_relative_path_with_line() {
        assert_eq!(
            parse_file_line_column("Cargo.toml:55").unwrap(),
            PathObject::new(
                PathBuf::from("Cargo.toml").canonicalize().unwrap(),
                false,
                55,
                1
            ),
        );
    }

    #[test]
    fn test_relative_path_with_linecol() {
        assert_eq!(
            parse_file_line_column("Cargo.toml:55:3").unwrap(),
            PathObject::new(
                PathBuf::from("Cargo.toml").canonicalize().unwrap(),
                false,
                55,
                3
            ),
        );
    }

    #[test]
    fn test_relative_path_with_none() {
        assert_eq!(
            parse_file_line_column("Cargo.toml:12:623:352").unwrap(),
            PathObject::new(
                env::current_dir()
                    .unwrap()
                    .join(PathBuf::from("Cargo.toml:12")),
                false,
                623,
                352
            ),
        );
    }
}
