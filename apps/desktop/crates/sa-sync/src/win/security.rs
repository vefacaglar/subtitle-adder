//! F6 (required by UNIFIED.md §3.19): the pipe is reachable only by the current user.
//!
//! * Server instances are created with an explicit protected DACL granting `GENERIC_ALL` to the token owner SID,
//!   the token user SID and SYSTEM only, with the owner set to the token owner (exactly what a .NET
//!   `PipeOptions.CurrentUserOnly` client checks: pipe owner == `WindowsIdentity.GetCurrent().Owner`).
//! * The client verifies the server pipe's owner SID equals its own token owner or user SID before sending anything
//!   (a pipe squatted by another user is refused), like .NET `CurrentUserOnly`.

use std::ffi::c_void;
use std::io;
use std::sync::OnceLock;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree, WIN32_ERROR};
use windows::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
use windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, EqualSid, GetLengthSid, GetTokenInformation, OWNER_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_OWNER, TOKEN_QUERY, TOKEN_USER, TokenOwner, TokenUser,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{PCWSTR, PWSTR};

/// The current process token's user and owner SIDs (binary + SDDL string form).
pub(crate) struct TokenSids {
    pub user: Vec<u8>,
    pub owner: Vec<u8>,
    pub user_sddl: String,
    pub owner_sddl: String,
}

fn os_err(e: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error((e.code().0 & 0xFFFF) as i32)
}

unsafe fn pwstr_to_string_and_free(p: PWSTR) -> String {
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe {
        let _ = LocalFree(Some(HLOCAL(p.0 as *mut c_void)));
    }
    s
}

unsafe fn sid_bytes(psid: PSID) -> Vec<u8> {
    let len = unsafe { GetLengthSid(psid) } as usize;
    unsafe { std::slice::from_raw_parts(psid.0 as *const u8, len) }.to_vec()
}

unsafe fn sid_sddl(psid: PSID) -> io::Result<String> {
    let mut out = PWSTR::null();
    unsafe { ConvertSidToStringSidW(psid, &mut out) }.map_err(os_err)?;
    Ok(unsafe { pwstr_to_string_and_free(out) })
}

fn token_info(token: HANDLE, class: windows::Win32::Security::TOKEN_INFORMATION_CLASS) -> io::Result<Vec<u64>> {
    let mut needed = 0u32;
    let _ = unsafe { GetTokenInformation(token, class, None, 0, &mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64 storage keeps the struct aligned.
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    unsafe { GetTokenInformation(token, class, Some(buf.as_mut_ptr() as *mut c_void), needed, &mut needed) }
        .map_err(os_err)?;
    Ok(buf)
}

fn load_token_sids() -> io::Result<TokenSids> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.map_err(os_err)?;
    let result = (|| {
        let ubuf = token_info(token, TokenUser)?;
        let obuf = token_info(token, TokenOwner)?;
        let user_sid = unsafe { (*(ubuf.as_ptr() as *const TOKEN_USER)).User.Sid };
        let owner_sid = unsafe { (*(obuf.as_ptr() as *const TOKEN_OWNER)).Owner };
        Ok(TokenSids {
            user: unsafe { sid_bytes(user_sid) },
            owner: unsafe { sid_bytes(owner_sid) },
            user_sddl: unsafe { sid_sddl(user_sid)? },
            owner_sddl: unsafe { sid_sddl(owner_sid)? },
        })
    })();
    unsafe {
        let _ = CloseHandle(token);
    }
    result
}

/// Cached token SIDs of this process.
pub(crate) fn token_sids() -> Option<&'static TokenSids> {
    static S: OnceLock<Option<TokenSids>> = OnceLock::new();
    S.get_or_init(|| load_token_sids().ok()).as_ref()
}

/// The SDDL of the server instances (owner = token owner; protected DACL: token owner, token user, SYSTEM).
pub(crate) fn server_sddl(sids: &TokenSids) -> String {
    let mut s = format!("O:{}D:P(A;;GA;;;{})", sids.owner_sddl, sids.owner_sddl);
    if sids.user_sddl != sids.owner_sddl {
        s.push_str(&format!("(A;;GA;;;{})", sids.user_sddl));
    }
    s.push_str("(A;;GA;;;SY)");
    s
}

/// A self-relative security descriptor (LocalAlloc'd) + the `SECURITY_ATTRIBUTES` pointing at it.
pub(crate) struct ServerSecurity {
    sd: PSECURITY_DESCRIPTOR,
    attrs: SECURITY_ATTRIBUTES,
}

// The descriptor is immutable after creation and only read by CreateNamedPipeW.
unsafe impl Send for ServerSecurity {}
unsafe impl Sync for ServerSecurity {}

impl ServerSecurity {
    pub(crate) fn new() -> io::Result<Self> {
        let sids = token_sids().ok_or_else(|| io::Error::other("no token SIDs"))?;
        Self::from_sddl(&server_sddl(sids))
    }

    /// A descriptor built from an SDDL string (tests replay another runtime's pipe security with it).
    pub(crate) fn from_sddl(sddl: &str) -> io::Result<Self> {
        let sddl: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(sddl.as_ptr()), SDDL_REVISION_1, &mut sd, None) }
            .map_err(os_err)?;
        let attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        Ok(Self { sd, attrs })
    }

    pub(crate) fn attributes(&self) -> *const SECURITY_ATTRIBUTES {
        &self.attrs
    }
}

impl Drop for ServerSecurity {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.sd.0)));
        }
    }
}

/// True when the pipe's owner SID is this process's token owner or user SID.
pub(crate) fn server_owned_by_current_user(pipe: HANDLE) -> bool {
    let Some(sids) = token_sids() else { return false };
    let mut owner = PSID::default();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let rc: WIN32_ERROR = unsafe {
        GetSecurityInfo(pipe, SE_KERNEL_OBJECT, OWNER_SECURITY_INFORMATION, Some(&mut owner), None, None, None, Some(&mut sd))
    };
    if rc.0 != 0 || owner.0.is_null() {
        return false;
    }
    let matches = |bytes: &[u8]| unsafe { EqualSid(owner, PSID(bytes.as_ptr() as *mut c_void)).is_ok() };
    let ok = matches(&sids.owner) || matches(&sids.user);
    unsafe {
        let _ = LocalFree(Some(HLOCAL(sd.0)));
    }
    ok
}

/// SDDL (owner + DACL) of a kernel object handle (tests of F6).
pub(crate) fn handle_sddl(handle: HANDLE) -> io::Result<String> {
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let info = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let rc = unsafe { GetSecurityInfo(handle, SE_KERNEL_OBJECT, info, None, None, None, None, Some(&mut sd)) };
    if rc.0 != 0 {
        return Err(io::Error::from_raw_os_error(rc.0 as i32));
    }
    let mut out = PWSTR::null();
    let r = unsafe { ConvertSecurityDescriptorToStringSecurityDescriptorW(sd, SDDL_REVISION_1, info, &mut out, None) };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(sd.0)));
    }
    r.map_err(os_err)?;
    Ok(unsafe { pwstr_to_string_and_free(out) })
}
