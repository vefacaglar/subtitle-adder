//! Standard folders, quick locations and mounted volumes (LibraryKit `QuickLocations`, mac-librarykit §16).

use std::path::{Path, PathBuf};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use sa_core::platform::QuickLocation;

use super::ffi::{array_items, ns_array, ns_string, rust_string, url_path};
use crate::mac_rules::{VolumeEntry, build_quick_locations, normalized};

const NS_DESKTOP_DIRECTORY: usize = 12;
const NS_DOWNLOADS_DIRECTORY: usize = 15;
const NS_MOVIES_DIRECTORY: usize = 17;
const NS_APPLICATION_SUPPORT_DIRECTORY: usize = 14;
const NS_USER_DOMAIN_MASK: usize = 1;
const NS_VOLUME_ENUMERATION_SKIP_HIDDEN_VOLUMES: usize = 1 << 1;

fn file_manager() -> Retained<AnyObject> {
    unsafe { msg_send![class!(NSFileManager), defaultManager] }
}

/// `FileManager.urls(for:in: .userDomainMask).first`.
pub fn user_directory(directory: usize) -> Option<PathBuf> {
    autoreleasepool(|_| unsafe {
        let fm = file_manager();
        let urls: Option<Retained<AnyObject>> = msg_send![&*fm, URLsForDirectory: directory, inDomains: NS_USER_DOMAIN_MASK];
        let urls = urls?;
        let first = array_items(&urls).into_iter().next()?;
        url_path(&first)
    })
}

/// `~/Library/Application Support`.
pub fn application_support() -> PathBuf {
    user_directory(NS_APPLICATION_SUPPORT_DIRECTORY).unwrap_or_else(|| {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        home.join("Library/Application Support")
    })
}

/// `mountedVolumeURLs` (optionally skipping hidden volumes) with their names.
pub fn mounted_volumes(skip_hidden: bool) -> Vec<VolumeEntry> {
    autoreleasepool(|_| unsafe {
        let fm = file_manager();
        let name_key = ns_string("NSURLVolumeNameKey");
        let localized_key = ns_string("NSURLVolumeLocalizedNameKey");
        let keys = ns_array(&[&*name_key, &*localized_key]);
        let options = if skip_hidden { NS_VOLUME_ENUMERATION_SKIP_HIDDEN_VOLUMES } else { 0 };
        let urls: Option<Retained<AnyObject>> =
            msg_send![&*fm, mountedVolumeURLsIncludingResourceValuesForKeys: &*keys, options: options];
        let Some(urls) = urls else { return Vec::new() };
        array_items(&urls)
            .iter()
            .filter_map(|url| {
                let path = url_path(url)?;
                let name = resource_string(url, &name_key)
                    .or_else(|| resource_string(url, &localized_key))
                    .unwrap_or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                Some(VolumeEntry { path, name })
            })
            .collect()
    })
}

unsafe fn resource_string(url: &AnyObject, key: &AnyObject) -> Option<String> {
    unsafe {
        let mut value: *mut AnyObject = std::ptr::null_mut();
        let ok: bool = msg_send![url, getResourceValue: &mut value, forKey: key, error: std::ptr::null_mut::<*mut AnyObject>()];
        if !ok || value.is_null() {
            return None;
        }
        let s = rust_string(&*value);
        (!s.is_empty()).then_some(s)
    }
}

/// Swift `ExplorerServices.system.isVolumeMounted`: listed among the mounted volumes, or a link (the startup disk's
/// `/Volumes/Macintosh HD` → `/`).
pub fn is_volume_mounted(root: &str) -> bool {
    if mounted_volumes(false).iter().any(|v| normalized(&v.path.to_string_lossy()) == root) {
        return true;
    }
    std::fs::read_link(root).is_ok()
}

/// Movies, Downloads, Desktop (when they exist), then visible volumes except the startup disk.
pub fn quick_locations() -> Vec<QuickLocation> {
    build_quick_locations(
        user_directory(NS_MOVIES_DIRECTORY),
        user_directory(NS_DOWNLOADS_DIRECTORY),
        user_directory(NS_DESKTOP_DIRECTORY),
        &mounted_volumes(true),
        &|p: &Path| p.is_dir(),
    )
}
