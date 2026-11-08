//! The macOS folder lister (LibraryKit `MediaFolderScanner.list`, mac-librarykit §15.6 steps 1–2): `stat` tells a
//! vanished folder from one we may not read, entries are gathered with `lstat`/`stat` (a symbolic link counts as its
//! target, a dangling one is skipped) and handed to [`super::listing::build_listing`].
//!
//! `isHidden` and `isPackage` come from the same resource values Foundation uses (`CFURLCopyResourcePropertyForKey`
//! with `kCFURLIsHiddenKey` / `kCFURLIsPackageKey`: the BSD `UF_HIDDEN` flag, Finder-hidden, Launch Services
//! packages). Only if CoreFoundation cannot answer, the `UF_HIDDEN` flag and a list of the package extensions every Mac
//! registers are used instead.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;

use super::listing::{RawEntry, build_listing};
use crate::ScanResult;
use crate::dates::{TICKS_PER_SECOND, UNIX_EPOCH_TICKS};

/// `UF_HIDDEN` (`<sys/stat.h>`).
const UF_HIDDEN: u32 = 0x0000_8000;

/// Directory extensions macOS treats as packages (bundles, libraries, documents).
const PACKAGE_EXTENSIONS: &[&str] = &[
    "app", "appex", "bundle", "framework", "plugin", "kext", "xpc", "qlgenerator", "mdimporter", "prefpane", "saver",
    "wdgt", "photoslibrary", "photolibrary", "migratedphotolibrary", "aplibrary", "musiclibrary", "tvlibrary",
    "imovielibrary", "theater", "fcpbundle", "logicx", "band", "rtfd", "pkg", "mpkg", "playground", "xcodeproj",
    "xcworkspace", "xcarchive", "docarchive", "scptd", "key", "pages", "numbers", "sparsebundle", "dvdmedia",
    "photoslibrarybackup", "lrlibrary", "lrdata", "dSYM",
];

fn stat(path: &[u8], follow: bool) -> Result<libc::stat, i32> {
    let c = CString::new(path).map_err(|_| libc::EINVAL)?;
    // SAFETY: `c` is a valid NUL-terminated path; `st` is fully written by a successful call.
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        let rc = if follow { libc::stat(c.as_ptr(), &mut st) } else { libc::lstat(c.as_ptr(), &mut st) };
        if rc == 0 { Ok(st) } else { Err(*libc::__error()) }
    }
}

fn is_dir(st: &libc::stat) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFDIR
}

fn mtime_ticks(st: &libc::stat) -> i64 {
    UNIX_EPOCH_TICKS + st.st_mtime * TICKS_PER_SECOND + st.st_mtime_nsec / 100
}

/// CoreFoundation resource values (`NSURLResourceKey` twins), queried per entry like `URL.resourceValues(forKeys:)`.
mod resource {
    use std::ffi::c_void;
    use std::path::Path;

    use core_foundation::base::{Boolean, CFRelease, CFTypeRef, TCFType};
    use core_foundation::error::CFErrorRef;
    use core_foundation::number::kCFBooleanTrue;
    use core_foundation::string::CFStringRef;
    use core_foundation::url::{CFURL, CFURLRef, kCFURLIsHiddenKey, kCFURLIsPackageKey};

    unsafe extern "C" {
        // CoreFoundation (linked by `core-foundation`); not declared by core-foundation-sys 0.8.
        fn CFURLCopyResourcePropertyForKey(url: CFURLRef, key: CFStringRef, value: *mut CFTypeRef, error: *mut CFErrorRef) -> Boolean;
    }

    fn flag(url: &CFURL, key: CFStringRef) -> Option<bool> {
        let mut value: CFTypeRef = std::ptr::null();
        let mut error: CFErrorRef = std::ptr::null_mut();
        // SAFETY: `url` and `key` are valid CF objects; on success `value` is a +1 reference (or NULL) that we
        // release; on failure `error` is a +1 reference that we release.
        unsafe {
            let ok = CFURLCopyResourcePropertyForKey(url.as_concrete_TypeRef(), key, &mut value, &mut error);
            if !error.is_null() {
                CFRelease(error as *const c_void);
            }
            if ok == 0 {
                if !value.is_null() {
                    CFRelease(value);
                }
                return None;
            }
            let result = !value.is_null() && value == kCFBooleanTrue as CFTypeRef;
            if !value.is_null() {
                CFRelease(value);
            }
            Some(result)
        }
    }

    /// `URLResourceValues.isHidden` of the entry itself (a link's own flag).
    pub fn is_hidden(path: &Path, is_dir: bool) -> Option<bool> {
        let url = CFURL::from_path(path, is_dir)?;
        // SAFETY: a framework constant.
        flag(&url, unsafe { kCFURLIsHiddenKey })
    }

    /// `URLResourceValues.isPackage` (Launch Services), of the link target for a symbolic link.
    pub fn is_package(path: &Path) -> Option<bool> {
        let url = CFURL::from_path(path, true)?;
        // SAFETY: a framework constant.
        flag(&url, unsafe { kCFURLIsPackageKey })
    }
}

fn is_package_name(name: &str) -> bool {
    name.rfind('.').is_some_and(|dot| dot > 0 && PACKAGE_EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(&name[dot + 1..])))
}

/// One level of the already normalized folder `path`.
pub fn list(path: &str, now: i64) -> ScanResult {
    match stat(path.as_bytes(), true) {
        Err(e) if e == libc::ENOENT || e == libc::ENOTDIR => return ScanResult::Missing,
        Err(_) => return ScanResult::Unreadable,
        Ok(st) if !is_dir(&st) => return ScanResult::Missing,
        Ok(_) => {}
    }
    let Ok(read) = std::fs::read_dir(path) else { return ScanResult::Unreadable };
    let mut entries = Vec::new();
    for entry in read {
        // `contentsOfDirectory` fails as a whole when reading the folder fails part way: keep the cached entry.
        let Ok(entry) = entry else { return ScanResult::Unreadable };
        let name = entry.file_name();
        let Some(name) = name.to_str().map(str::to_owned) else { continue };
        if name.starts_with('.') {
            continue;
        }
        let full = entry.path();
        let Ok(link) = stat(full.as_os_str().as_bytes(), false) else { continue };
        let is_link = link.st_mode & libc::S_IFMT == libc::S_IFLNK;
        let hidden = resource::is_hidden(&full, is_dir(&link)).unwrap_or(link.st_flags & UF_HIDDEN != 0);
        let (values, dangling) = if is_link {
            match stat(full.as_os_str().as_bytes(), true) {
                Ok(target) => (target, false),
                Err(_) => (link, true),
            }
        } else {
            (link, false)
        };
        let dir = is_dir(&values);
        // A symbolic link counts as its target (`resolvingSymlinksInPath().resourceValues`), the package flag included.
        let is_package = dir && {
            let target = if is_link { std::fs::canonicalize(&full).ok() } else { None };
            let probe = target.as_deref().unwrap_or(&full);
            resource::is_package(probe).unwrap_or_else(|| {
                probe.file_name().and_then(|n| n.to_str()).is_some_and(is_package_name) || is_package_name(&name)
            })
        };
        entries.push(RawEntry {
            is_package,
            name,
            hidden,
            is_dir: dir,
            dangling,
            size: (!dir).then_some(values.st_size),
            modified: Some(mtime_ticks(&values)),
        });
    }
    ScanResult::Listed(build_listing(path, &entries, now))
}
