//! LibraryKit `MediaPath` (mac-librarykit §2.1): standardized POSIX paths, NFC, case-insensitive keys.

use crate::text::{lower_key, nfc};

/// Lexical standardization: absolute against the CWD when relative, `.`/`..` resolved (`/..` stays `/`), `//`
/// collapsed, no trailing `/` except for the root. No symlink resolution, no case change.
pub fn standardize(path: &str) -> String {
    let absolute = if path.starts_with('/') {
        path.to_owned()
    } else {
        let cwd = std::env::current_dir().map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_else(|_| "/".into());
        format!("{cwd}/{path}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in absolute.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    if parts.is_empty() { "/".into() } else { format!("/{}", parts.join("/")) }
}

/// `MediaPath.normalized`: `""` for `""`, else [`standardize`] then NFC.
pub fn normalized(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    nfc(&standardize(path))
}

/// `MediaPath.key`: the case-insensitive identity of a folder.
pub fn key(path: &str) -> String {
    lower_key(&normalized(path))
}

/// `MediaPath.child(name, of: directory)`.
pub fn child(name: &str, directory: &str) -> String {
    format!("{}/{}", if directory == "/" { "" } else { directory }, nfc(name))
}

/// `MediaPath.parent(of:)`: `None` for `/` or a path without `/`.
pub fn parent(path: &str) -> Option<String> {
    if path == "/" {
        return None;
    }
    let slash = path.rfind('/')?;
    Some(if slash == 0 { "/".into() } else { path[..slash].to_owned() })
}

/// A `&Path` argument as the string the index works with (`""` for a non-UTF-8 path).
pub fn from_path(p: &std::path::Path) -> String {
    p.to_str().map(|s| s.replace('\\', "/")).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_standardized_without_trailing_slash_and_nfc() {
        assert_eq!(normalized("/"), "/");
        assert_eq!(normalized("/a/b/"), "/a/b");
        assert_eq!(normalized("/a/b/../c/./d"), "/a/c/d");
        assert_eq!(normalized("/.."), "/");
        assert_eq!(normalized("//a//b"), "/a/b");
        assert_eq!(normalized(""), "");
        assert_eq!(normalized("/tmp/x/"), "/tmp/x");
        let nfd: String = unicode_normalization::UnicodeNormalization::nfd("/a/Çalışma").collect();
        assert_eq!(normalized(&nfd), nfc("/a/Çalışma"));
        assert_eq!(key("/A/B/"), key("/a/b"));
        assert_eq!(parent("/a/b").as_deref(), Some("/a"));
        assert_eq!(parent("/a").as_deref(), Some("/"));
        assert_eq!(parent("/"), None);
        assert_eq!(child("x", "/"), "/x");
        assert_eq!(child("x", "/a"), "/a/x");
    }
}
