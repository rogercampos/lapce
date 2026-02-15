use std::path::PathBuf;

use tracing::{Level, event};
use url::Url;

/// Converts an LSP file URI to a PathBuf, handling platform-specific quirks.
///
/// On Windows, rust-analyzer returns URIs like "file:///C:/..." which URL::to_file_path()
/// sometimes misparses. This function handles various edge cases:
/// - Encoded drive letters (e.g., "%3A" instead of ":")
/// - Extra leading slashes before the drive letter
/// - Percent-encoded path segments
///
/// On Unix, this is straightforward but still handles percent-encoding fallbacks.
#[cfg(windows)]
pub fn path_from_url(url: &Url) -> PathBuf {
    use percent_encoding::percent_decode_str;

    event!(Level::DEBUG, "Converting `{:?}` to path", url);

    if let Ok(path) = url.to_file_path() {
        return path;
    }

    let path = url.path();

    let path = if path.contains('%') {
        percent_decode_str(path)
            .decode_utf8()
            .unwrap_or(std::borrow::Cow::from(path))
    } else {
        std::borrow::Cow::from(path)
    };

    if let Some(path) = path.strip_prefix('/') {
        event!(Level::DEBUG, "Found `/` prefix");
        if let Some((maybe_drive_letter, path_second_part)) =
            path.split_once(['/', '\\'])
        {
            event!(Level::DEBUG, maybe_drive_letter);
            event!(Level::DEBUG, path_second_part);

            let b = maybe_drive_letter.as_bytes();

            if !b.is_empty() && !b[0].is_ascii_alphabetic() {
                event!(Level::ERROR, "First byte is not ascii alphabetic: {b:?}");
            }

            match maybe_drive_letter.len() {
                2 => match maybe_drive_letter.chars().nth(1) {
                    Some(':') => {
                        event!(Level::DEBUG, "Returning path `{:?}`", path);
                        return PathBuf::from(path);
                    }
                    v => {
                        event!(
                            Level::ERROR,
                            "Unhandled 'maybe_drive_letter' chars: {v:?}"
                        );
                    }
                },
                4 => {
                    if maybe_drive_letter.contains("%3A") {
                        let path = path.replace("%3A", ":");
                        event!(Level::DEBUG, "Returning path `{:?}`", path);
                        return PathBuf::from(path);
                    } else {
                        event!(
                            Level::ERROR,
                            "Unhandled 'maybe_drive_letter' pattern: {maybe_drive_letter:?}"
                        );
                    }
                }
                v => {
                    event!(
                        Level::ERROR,
                        "Unhandled 'maybe_drive_letter' length: {v}"
                    );
                }
            }
        }
    }

    event!(Level::DEBUG, "Returning unmodified path `{:?}`", path);
    PathBuf::from(path.into_owned())
}

#[cfg(not(windows))]
pub fn path_from_url(url: &Url) -> PathBuf {
    event!(Level::DEBUG, "Converting `{:?}` to path", url);
    url.to_file_path().unwrap_or_else(|_| {
        let path = url.path();
        if let Ok(path) = percent_encoding::percent_decode_str(path).decode_utf8() {
            return PathBuf::from(path.into_owned());
        }
        PathBuf::from(path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_from_url_simple_unix_path() {
        let url = Url::parse("file:///home/user/project/main.rs").unwrap();
        assert_eq!(
            path_from_url(&url),
            PathBuf::from("/home/user/project/main.rs")
        );
    }

    #[test]
    fn path_from_url_root_path() {
        let url = Url::parse("file:///").unwrap();
        assert_eq!(path_from_url(&url), PathBuf::from("/"));
    }

    #[test]
    fn path_from_url_percent_encoded_spaces() {
        let url = Url::parse("file:///home/user/my%20project/main.rs").unwrap();
        assert_eq!(
            path_from_url(&url),
            PathBuf::from("/home/user/my project/main.rs")
        );
    }

    #[test]
    fn path_from_url_percent_encoded_special_chars() {
        let url = Url::parse("file:///home/user/project%23name/file.rs").unwrap();
        assert_eq!(
            path_from_url(&url),
            PathBuf::from("/home/user/project#name/file.rs")
        );
    }

    #[test]
    fn path_from_url_deeply_nested() {
        let url = Url::parse("file:///a/b/c/d/e/f/g.txt").unwrap();
        assert_eq!(path_from_url(&url), PathBuf::from("/a/b/c/d/e/f/g.txt"));
    }

    #[test]
    fn path_from_url_unicode_path() {
        let url =
            Url::parse("file:///home/user/%E4%B8%AD%E6%96%87/file.rs").unwrap();
        assert_eq!(
            path_from_url(&url),
            PathBuf::from("/home/user/中文/file.rs")
        );
    }
}
