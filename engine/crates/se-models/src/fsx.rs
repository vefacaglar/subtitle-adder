//! Small file-system helpers with Python `pathlib` / `os` semantics used by the cache code (symlinks,
//! lexical normalization, Windows case-insensitive path equality, `shutil.rmtree(ignore_errors=True)`,
//! lock files).

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use se_core::paths::{normcase, normpath, os_to_py_string, py_path_str};

/// `Path.is_symlink()` (lstat; errors → false).
pub fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

/// `stat.S_ISREG(path.lstat().st_mode)` (errors → false).
pub fn is_regular_nofollow(path: &Path) -> bool {
    fs::symlink_metadata(path).map(|m| m.file_type().is_file()).unwrap_or(false)
}

/// `stat.S_ISDIR(path.lstat().st_mode)` (errors → false).
pub fn is_dir_nofollow(path: &Path) -> bool {
    fs::symlink_metadata(path).map(|m| m.file_type().is_dir()).unwrap_or(false)
}

/// `Path.exists()` (follows symlinks).
pub fn exists(path: &Path) -> bool {
    fs::metadata(path).is_ok()
}

/// `Path.is_dir()` (follows symlinks).
pub fn is_dir(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
}

/// `Path.is_file()` (follows symlinks).
pub fn is_file(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
}

/// Python `Path.is_dir()` error policy: "not found"-like errors are `false`, others (permission) propagate.
pub fn is_dir_strict(path: &Path) -> io::Result<bool> {
    match fs::metadata(path) {
        Ok(m) => Ok(m.is_dir()),
        Err(e) if ignorable(&e) => Ok(false),
        Err(e) => Err(e),
    }
}

fn ignorable(e: &io::Error) -> bool {
    use io::ErrorKind::*;
    if matches!(e.kind(), NotFound | NotADirectory | InvalidFilename) {
        return true;
    }
    // ELOOP / Windows 21 (not ready), 123 (invalid name), 1921 (cannot be resolved).
    matches!(e.raw_os_error(), Some(21) | Some(123) | Some(1921)) && cfg!(windows)
        || (cfg!(unix) && e.raw_os_error() == Some(40))
}

/// `os.readlink(link)` with a Windows `\\?\` prefix stripped (as Python returns the print name).
pub fn read_link(link: &Path) -> io::Result<PathBuf> {
    let target = fs::read_link(link)?;
    Ok(dunce::simplified(&target).to_path_buf())
}

/// `Path(os.path.normpath(path))` (lexical).
pub fn norm(path: &Path) -> PathBuf {
    PathBuf::from(normpath(&os_to_py_string(path.as_os_str())))
}

/// `Path(os.path.normpath(link.parent / os.readlink(link)))`.
pub fn link_target(link: &Path) -> io::Result<PathBuf> {
    let target = read_link(link)?;
    let parent = link.parent().unwrap_or(Path::new(""));
    Ok(norm(&parent.join(target)))
}

/// Python `Path ==` (Windows: case-insensitive; separators normalized).
pub fn same_path(a: &Path, b: &Path) -> bool {
    path_key(a) == path_key(b)
}

/// A hashable comparison key with `Path ==` semantics.
pub fn path_key(path: &Path) -> String {
    let s = py_path_str(path);
    if cfg!(windows) { normcase(&s) } else { s }
}

/// `path.relative_to(base)` succeeds (component-wise, case-insensitive on Windows).
pub fn is_relative_to(path: &Path, base: &Path) -> bool {
    let p: Vec<String> = components(path);
    let b: Vec<String> = components(base);
    p.len() >= b.len() && p[..b.len()] == b[..]
}

/// The components of `path.relative_to(base)`, if any.
pub fn relative_parts(path: &Path, base: &Path) -> Option<Vec<String>> {
    let p_raw: Vec<String> = path.components().map(|c| os_to_py_string(c.as_os_str())).collect();
    let p: Vec<String> = components(path);
    let b: Vec<String> = components(base);
    if p.len() >= b.len() && p[..b.len()] == b[..] { Some(p_raw[b.len()..].to_vec()) } else { None }
}

fn components(path: &Path) -> Vec<String> {
    path.components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .map(|c| {
            let s = os_to_py_string(c.as_os_str()).replace('/', "\\");
            if cfg!(windows) { normcase(&s) } else { os_to_py_string(c.as_os_str()) }
        })
        .collect()
}

/// Creates a symbolic link `link` → `target` (a file link on Windows).
pub fn symlink_file(target: &Path, link: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link)
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (target, link);
        Err(io::Error::new(io::ErrorKind::Unsupported, "symlinks are not supported"))
    }
}

/// `os.path.relpath(path, start)` for absolute, normalized paths; `None` when they are on different drives.
pub fn relpath(path: &Path, start: &Path) -> Option<PathBuf> {
    let p: Vec<_> = path.components().collect();
    let s: Vec<_> = start.components().collect();
    let key = |c: &std::path::Component<'_>| {
        let t = os_to_py_string(c.as_os_str());
        if cfg!(windows) { normcase(&t) } else { t }
    };
    if p.is_empty() || s.is_empty() || key(&p[0]) != key(&s[0]) {
        return None;
    }
    let mut common = 0;
    while common < p.len() && common < s.len() && key(&p[common]) == key(&s[common]) {
        common += 1;
    }
    let mut out = PathBuf::new();
    for _ in common..s.len() {
        out.push("..");
    }
    for c in &p[common..] {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    Some(out)
}

/// Removes a symlink or file (`os.unlink`): on Windows a directory symlink needs `remove_dir`.
pub fn remove_link_or_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) => {
            if cfg!(windows) && is_symlink(path) {
                fs::remove_dir(path)
            } else {
                Err(e)
            }
        }
    }
}

/// `Path.unlink(missing_ok=True)`.
pub fn unlink_missing_ok(path: &Path) -> io::Result<()> {
    match remove_link_or_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// `shutil.rmtree(path, ignore_errors=True)`: never follows symlinks/junctions (they are unlinked), keeps going
/// past errors.
pub fn rmtree_ignore_errors(path: &Path) {
    let Ok(meta) = fs::symlink_metadata(path) else { return };
    if meta.file_type().is_symlink() || !meta.is_dir() {
        // rmtree refuses a symlink root (OSError, ignored); a plain file root is an error too.
        return;
    }
    rmtree_inner(path);
}

fn rmtree_inner(dir: &Path) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            match fs::symlink_metadata(&p) {
                Ok(m) if m.file_type().is_symlink() => {
                    let _ = remove_link_or_file(&p);
                }
                Ok(m) if m.is_dir() => rmtree_inner(&p),
                Ok(_) => {
                    let _ = fs::remove_file(&p);
                }
                Err(_) => {}
            }
        }
    }
    let _ = fs::remove_dir(dir);
}

/// Recursive listing like `Path.glob("**/*")` in Python 3.11: every entry below `root` (directories included),
/// not descending into symlinked directories; unreadable directories are skipped silently.
pub fn walk_all(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !is_dir(root) {
        return out;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries.into_iter().rev() {
            let link = is_symlink(&p);
            if !link && is_dir(&p) {
                stack.push(p.clone());
            }
            out.push(p);
        }
    }
    out
}

/// 8 random lowercase hex characters (`uuid.uuid4().hex[:8]` / `secrets.token_hex(4)`).
pub fn rand_hex8() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// An exclusive lock on `path` (created if missing, never deleted), released on drop.
pub struct FileLock {
    file: File,
}

impl FileLock {
    /// Blocks until the lock is held.
    pub fn acquire(path: &Path) -> io::Result<FileLock> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        file.lock()?;
        Ok(FileLock { file })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// `pathlib.Path.expanduser()` / `os.path.expanduser` for a leading `~` (Windows: `USERPROFILE`, else
/// `HOMEDRIVE`+`HOMEPATH`; POSIX: `HOME`).
pub fn expanduser(text: &str) -> String {
    if !text.starts_with('~') {
        return text.to_string();
    }
    let is_sep = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    let end = text[1..].find(is_sep).map(|i| i + 1).unwrap_or(text.len());
    let Some(home) = home_dir_text() else { return text.to_string() };
    if end != 1 {
        // `~user`: Windows replaces the last component of the home path; POSIX uses the password database.
        if cfg!(windows) {
            let user = &text[1..end];
            let parent = Path::new(&home).parent().map(|p| os_to_py_string(p.as_os_str())).unwrap_or_default();
            let current = Path::new(&home).file_name().map(|n| os_to_py_string(n)).unwrap_or_default();
            let base = if current == user { home.clone() } else { se_core::paths::py_join(&parent, user) };
            return format!("{base}{}", &text[end..]);
        }
        return text.to_string();
    }
    let rest = &text[end..];
    let home = if rest.is_empty() { home } else { home.trim_end_matches(is_sep).to_string() };
    format!("{home}{rest}")
}

/// The user's home directory as Python computes it for `expanduser("~")` / `Path.home()`.
pub fn home_dir_text() -> Option<String> {
    let get = |k: &str| std::env::var(k).ok();
    if cfg!(windows) {
        if let Some(p) = get("USERPROFILE") {
            return Some(p);
        }
        let path = get("HOMEPATH")?;
        let drive = get("HOMEDRIVE").unwrap_or_default();
        Some(format!("{drive}{path}"))
    } else {
        get("HOME").filter(|h| !h.is_empty()).or_else(|| Some("/".to_string()))
    }
}

/// `Path.home()`.
pub fn home_dir() -> PathBuf {
    PathBuf::from(home_dir_text().unwrap_or_default())
}

/// `os.path.expandvars` (Windows `ntpath`: `%VAR%`, `$VAR`, `${VAR}`, `'...'` literal, `%%`/`$$`; POSIX: `$VAR`,
/// `${VAR}`). Unknown variables are left as is.
pub fn expandvars(text: &str) -> String {
    if !text.contains('$') && !(cfg!(windows) && text.contains('%')) {
        return text.to_string();
    }
    let var = |name: &str| std::env::var(name).ok();
    let chars: Vec<char> = text.chars().collect();
    let is_name = |c: char| c.is_ascii_alphanumeric() || c == '_' || (cfg!(windows) && c == '-');
    let mut out = String::new();
    let mut i = 0;
    if cfg!(windows) {
        while i < chars.len() {
            let c = chars[i];
            if c == '\'' {
                match chars[i + 1..].iter().position(|&x| x == '\'') {
                    Some(j) => {
                        out.extend(&chars[i..i + 2 + j]);
                        i += 2 + j;
                    }
                    None => {
                        out.extend(&chars[i..]);
                        i = chars.len();
                    }
                }
            } else if c == '%' {
                if chars.get(i + 1) == Some(&'%') {
                    out.push('%');
                    i += 2;
                } else {
                    match chars[i + 1..].iter().position(|&x| x == '%') {
                        Some(j) => {
                            let name: String = chars[i + 1..i + 1 + j].iter().collect();
                            match var(&name) {
                                Some(v) => out.push_str(&v),
                                None => out.push_str(&format!("%{name}%")),
                            }
                            i += j + 2;
                        }
                        None => {
                            out.extend(&chars[i..]);
                            i = chars.len();
                        }
                    }
                }
            } else if c == '$' {
                if chars.get(i + 1) == Some(&'$') {
                    out.push('$');
                    i += 2;
                } else if chars.get(i + 1) == Some(&'{') {
                    match chars[i + 2..].iter().position(|&x| x == '}') {
                        Some(j) => {
                            let name: String = chars[i + 2..i + 2 + j].iter().collect();
                            match var(&name) {
                                Some(v) => out.push_str(&v),
                                None => out.push_str(&format!("${{{name}}}")),
                            }
                            i += j + 3;
                        }
                        None => {
                            out.extend(&chars[i..]);
                            i = chars.len();
                        }
                    }
                } else {
                    let mut j = i + 1;
                    while j < chars.len() && is_name(chars[j]) {
                        j += 1;
                    }
                    let name: String = chars[i + 1..j].iter().collect();
                    match (name.is_empty(), var(&name)) {
                        (false, Some(v)) => out.push_str(&v),
                        _ => out.push_str(&format!("${name}")),
                    }
                    i = j;
                }
            } else {
                out.push(c);
                i += 1;
            }
        }
        return out;
    }
    while i < chars.len() {
        let c = chars[i];
        if c == '$' {
            if chars.get(i + 1) == Some(&'{') {
                if let Some(j) = chars[i + 2..].iter().position(|&x| x == '}') {
                    let name: String = chars[i + 2..i + 2 + j].iter().collect();
                    match var(&name) {
                        Some(v) => out.push_str(&v),
                        None => out.extend(&chars[i..i + 3 + j]),
                    }
                    i += j + 3;
                    continue;
                }
            } else {
                let mut j = i + 1;
                while j < chars.len() && is_name(chars[j]) {
                    j += 1;
                }
                if j > i + 1 {
                    let name: String = chars[i + 1..j].iter().collect();
                    match var(&name) {
                        Some(v) => out.push_str(&v),
                        None => out.extend(&chars[i..j]),
                    }
                    i = j;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relpath_walks_up() {
        let base = if cfg!(windows) { "C:\\c\\models--a--b" } else { "/c/models--a--b" };
        let blob = Path::new(base).join("blobs").join("x");
        let dir = Path::new(base).join("snapshots").join("1").join("sub");
        let rel = relpath(&blob, &dir).unwrap();
        assert_eq!(rel, Path::new("..").join("..").join("..").join("blobs").join("x"));
    }

    #[test]
    fn relative_to_is_component_wise() {
        let a = Path::new("/x/blobsy/z");
        assert!(!is_relative_to(a, Path::new("/x/blobs")));
        assert!(is_relative_to(Path::new("/x/blobs/z"), Path::new("/x/blobs")));
    }
}
