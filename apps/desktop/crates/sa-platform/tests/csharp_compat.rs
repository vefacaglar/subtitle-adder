//! Compatibility with the C# WinUI app (Windows), replayed from golden data frozen from the C# code at commit
//! `2f4bc96` (`tests/fixtures/csharp/`, see the README there): the single-instance pipe in both directions (wire
//! bytes, what the C# server parsed out of them, the pipe security the .NET `CurrentUserOnly` client accepted), the
//! user-name sanitization of the mutex and pipe names, the file-association registry tree (written, re-written and
//! removed), the font-list rules (the `SortedSet(CurrentCultureIgnoreCase)` order under fixed cultures, the vertical
//! and empty name filter, the fallback list) and the path rules (`GetPathRoot`, `HasExtension`,
//! `GetFileNameWithoutExtension`, `NormalizeDirectoryPath`, drive-root readiness).
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sa_core::platform::{ExternalOpenSource, InstanceRole};
use sa_platform::win::single_instance::{InstanceNames, SingleInstanceCoordinator, encode_paths, sanitize_token};
use serde_json::Value;

const TIMEOUT: Duration = Duration::from_secs(10);

fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/csharp").join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap()
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or_else(|| panic!("not a string: {v}"))
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|x| s(x).to_owned()).collect()
}

fn unique(tag: &str) -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    format!("{tag} {}-{n}", std::process::id())
}

/// Raw Win32 helpers: a pipe client that behaves like the .NET `CurrentUserOnly` client, a spy pipe server that keeps
/// the exact bytes, the security descriptor of a pipe instance, and an HKCU subtree dumper in the C# tool's format.
mod raw {
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{
        CloseHandle, ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_NO_MORE_ITEMS, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, ERROR_SUCCESS, GENERIC_WRITE, HANDLE, HLOCAL,
        INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
        ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1, SE_KERNEL_OBJECT,
    };
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
        TOKEN_INFORMATION_CLASS, TOKEN_QUERY, TokenOwner, TokenUser,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_READ_ATTRIBUTES, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_INBOUND, READ_CONTROL,
        ReadFile, WriteFile,
    };
    use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_TYPE_BYTE};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_ALL_ACCESS, KEY_READ, REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE, RegCloseKey,
        RegCreateKeyExW, RegDeleteTreeW, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW, RegQueryInfoKeyW, RegSetValueExW,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::{PCWSTR, PWSTR};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn take_pwstr(p: PWSTR) -> String {
        let s = unsafe { p.to_string() }.unwrap();
        unsafe {
            let _ = LocalFree(Some(HLOCAL(p.0.cast())));
        }
        s
    }

    fn sid_string(sid: PSID) -> String {
        let mut p = PWSTR::null();
        unsafe { ConvertSidToStringSidW(sid, &mut p) }.unwrap();
        take_pwstr(p)
    }

    fn token_sid(class: TOKEN_INFORMATION_CLASS) -> String {
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).unwrap();
            let mut len = 0u32;
            let _ = GetTokenInformation(token, class, None, 0, &mut len);
            let mut buf = vec![0u8; len as usize];
            GetTokenInformation(token, class, Some(buf.as_mut_ptr().cast()), len, &mut len).unwrap();
            let _ = CloseHandle(token);
            // TOKEN_USER and TOKEN_OWNER both start with the SID pointer.
            sid_string(std::ptr::read_unaligned(buf.as_ptr().cast::<PSID>()))
        }
    }

    /// The token owner SID (.NET `WindowsIdentity.GetCurrent().Owner`, what `CurrentUserOnly` clients compare the
    /// pipe owner with).
    pub fn owner_sid() -> String {
        token_sid(TokenOwner)
    }

    pub fn user_sid() -> String {
        token_sid(TokenUser)
    }

    /// Replaces the token SIDs with placeholders: the owner field (`O:`) becomes `{owner}`, an ACE trustee `{user}`
    /// (or `{owner}` when it is the token owner but not the user), so the template also fits an account whose token
    /// owner differs from its user (an elevated administrator).
    pub fn mask_sddl(sddl: &str) -> String {
        let owner = owner_sid();
        let user = user_sid();
        let pipe_owner = sddl_owner(sddl);
        let mut out = format!("O:{}", if pipe_owner == owner { "{owner}" } else { pipe_owner });
        let mut rest = &sddl[2 + pipe_owner.len()..];
        // Token by token, so a SID that is a prefix of another (`...-1001` / `...-10011`) is never half-replaced.
        while !rest.is_empty() {
            let end = rest.find([':', ';', ')', '(']).map_or(rest.len(), |i| i.max(1));
            let (tok, next) = rest.split_at(end);
            out.push_str(if tok == user { "{user}" } else if tok == owner { "{owner}" } else { tok });
            rest = next;
        }
        out
    }

    pub fn unmask_sddl(template: &str) -> String {
        template.replace("{owner}", &owner_sid()).replace("{user}", &user_sid())
    }

    fn handle_sddl(h: HANDLE) -> String {
        unsafe {
            let mut sd = PSECURITY_DESCRIPTOR::default();
            let info = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
            let r = GetSecurityInfo(h, SE_KERNEL_OBJECT, info, None, None, None, None, Some(&mut sd));
            assert_eq!(r, ERROR_SUCCESS, "GetSecurityInfo");
            let mut p = PWSTR::null();
            let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(sd, SDDL_REVISION_1, info, &mut p, None);
            let _ = LocalFree(Some(HLOCAL(sd.0)));
            ok.unwrap();
            take_pwstr(p)
        }
    }

    /// The pipe owner from an SDDL string (`O:<sid>`).
    pub fn sddl_owner(sddl: &str) -> &str {
        let rest = sddl.strip_prefix("O:").expect("SDDL with an owner");
        &rest[..rest.find(':').map_or(rest.len(), |i| i - 1)]
    }

    /// A pipe client like .NET's `NamedPipeClientStream(".", pipe, PipeDirection.Out, PipeOptions.CurrentUserOnly)`:
    /// it opens the pipe (retrying until a server instance exists, within `timeout`), reads the instance's security
    /// descriptor, requires its owner to be the token owner (`ValidateRemotePipeUser`), writes `bytes` and closes.
    /// Returns the instance's SDDL.
    pub fn dotnet_like_client(pipe: &str, bytes: &[u8], timeout: Duration) -> String {
        let path = wide(&format!(r"\\.\pipe\{pipe}"));
        let deadline = Instant::now() + timeout;
        let access = GENERIC_WRITE.0 | READ_CONTROL.0 | FILE_READ_ATTRIBUTES.0;
        let h = loop {
            match unsafe {
                CreateFileW(PCWSTR(path.as_ptr()), access, FILE_SHARE_NONE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None)
            } {
                Ok(h) => break h,
                Err(e) => {
                    assert!(Instant::now() < deadline, "connecting to {pipe}: {e}");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        };
        let sddl = handle_sddl(h);
        assert_eq!(sddl_owner(&sddl), owner_sid(), "a CurrentUserOnly client rejects this pipe owner: {sddl}");
        let mut written = 0u32;
        let ok = bytes.is_empty() || unsafe { WriteFile(h, Some(bytes), Some(&mut written), None) }.is_ok();
        unsafe {
            let _ = CloseHandle(h);
        }
        assert!(ok && written as usize == bytes.len(), "writing to {pipe}");
        sddl
    }

    /// One inbound pipe instance (optionally with an SDDL); `accept` serves one client and returns every byte it
    /// wrote. The instance exists when `new` returns.
    pub struct SpyServer(usize);

    impl SpyServer {
        pub fn new(pipe: &str, sddl: Option<&str>) -> SpyServer {
            let path = wide(&format!(r"\\.\pipe\{pipe}"));
            unsafe {
                let mut sd = PSECURITY_DESCRIPTOR::default();
                let sa = sddl.map(|sddl| {
                    let w = wide(sddl);
                    ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(w.as_ptr()), SDDL_REVISION_1, &mut sd, None).unwrap();
                    SECURITY_ATTRIBUTES {
                        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                        lpSecurityDescriptor: sd.0,
                        bInheritHandle: false.into(),
                    }
                });
                let h = CreateNamedPipeW(
                    PCWSTR(path.as_ptr()),
                    PIPE_ACCESS_INBOUND,
                    PIPE_TYPE_BYTE,
                    1,
                    0,
                    0,
                    0,
                    sa.as_ref().map(|sa| sa as *const SECURITY_ATTRIBUTES),
                );
                if !sd.0.is_null() {
                    let _ = LocalFree(Some(HLOCAL(sd.0)));
                }
                assert!(h != INVALID_HANDLE_VALUE && !h.is_invalid(), "CreateNamedPipeW {pipe}");
                SpyServer(h.0 as usize)
            }
        }

        /// Blocks until a client connected, wrote and closed.
        pub fn accept(self) -> Vec<u8> {
            let h = HANDLE(self.0 as *mut _);
            let mut data = Vec::new();
            unsafe {
                if let Err(e) = ConnectNamedPipe(h, None) {
                    // Connected before this call, or connected and already closed (what it wrote is still readable).
                    let early = [ERROR_PIPE_CONNECTED, ERROR_NO_DATA].map(|c| c.to_hresult());
                    assert!(early.contains(&e.code()), "ConnectNamedPipe: {e}");
                }
                let mut buf = [0u8; 4096];
                loop {
                    let mut n = 0u32;
                    match ReadFile(h, Some(&mut buf), Some(&mut n), None) {
                        Ok(()) => data.extend_from_slice(&buf[..n as usize]),
                        Err(e) if [ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED].map(|c| c.to_hresult()).contains(&e.code()) => {
                            break;
                        }
                        Err(e) => panic!("ReadFile: {e}"),
                    }
                }
                let _ = CloseHandle(h);
            }
            data
        }
    }

    // ---- registry ---------------------------------------------------------------------------------------------

    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    fn open(path: &str) -> Option<Key> {
        let w = wide(path);
        let mut hk = HKEY::default();
        (unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(w.as_ptr()), None, KEY_READ, &mut hk) } == ERROR_SUCCESS).then_some(Key(hk))
    }

    fn create(path: &str) -> Key {
        let w = wide(path);
        let mut hk = HKEY::default();
        let r = unsafe {
            RegCreateKeyExW(HKEY_CURRENT_USER, PCWSTR(w.as_ptr()), None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_ALL_ACCESS, None, &mut hk, None)
        };
        assert_eq!(r, ERROR_SUCCESS, "RegCreateKeyExW {path}");
        Key(hk)
    }

    pub fn delete_tree(path: &str) {
        let w = wide(path);
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(w.as_ptr()));
        }
    }

    /// .NET `RegistryValueKind` names.
    fn kind_name(t: REG_VALUE_TYPE) -> String {
        match t.0 {
            0 => "None".into(),
            1 => "String".into(),
            2 => "ExpandString".into(),
            3 => "Binary".into(),
            4 => "DWord".into(),
            7 => "MultiString".into(),
            11 => "QWord".into(),
            n => format!("Unknown({n})"),
        }
    }

    struct Value {
        name: String,
        kind: REG_VALUE_TYPE,
        data: Vec<u8>,
    }

    fn values_and_subkeys(k: &Key) -> (Vec<Value>, Vec<String>) {
        let (mut subkeys, mut max_sub, mut values, mut max_name, mut max_data) = (0u32, 0u32, 0u32, 0u32, 0u32);
        let r = unsafe {
            RegQueryInfoKeyW(
                k.0,
                None,
                None,
                None,
                Some(&mut subkeys),
                Some(&mut max_sub),
                None,
                Some(&mut values),
                Some(&mut max_name),
                Some(&mut max_data),
                None,
                None,
            )
        };
        assert_eq!(r, ERROR_SUCCESS);
        let mut out = Vec::new();
        for i in 0.. {
            let mut name = vec![0u16; max_name as usize + 1];
            let mut name_len = name.len() as u32;
            let mut data = vec![0u8; max_data as usize + 2];
            let mut data_len = data.len() as u32;
            let mut kind = 0u32;
            let r = unsafe {
                RegEnumValueW(k.0, i, Some(PWSTR(name.as_mut_ptr())), &mut name_len, None, Some(&mut kind), Some(data.as_mut_ptr()), Some(&mut data_len))
            };
            if r == ERROR_NO_MORE_ITEMS {
                break;
            }
            assert_eq!(r, ERROR_SUCCESS, "RegEnumValueW");
            data.truncate(data_len as usize);
            out.push(Value { name: String::from_utf16(&name[..name_len as usize]).unwrap(), kind: REG_VALUE_TYPE(kind), data });
        }
        let mut subs = Vec::new();
        for i in 0.. {
            let mut name = vec![0u16; max_sub as usize + 1];
            let mut len = name.len() as u32;
            let r = unsafe { RegEnumKeyExW(k.0, i, Some(PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None) };
            if r == ERROR_NO_MORE_ITEMS {
                break;
            }
            assert_eq!(r, ERROR_SUCCESS, "RegEnumKeyExW");
            subs.push(String::from_utf16(&name[..len as usize]).unwrap());
        }
        (out, subs)
    }

    /// .NET `GetValue` of a string value: the UTF-16 text up to the first NUL.
    fn text(data: &[u8]) -> String {
        let units: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
        String::from_utf16_lossy(&units)
    }

    fn walk(path: &str, relative: &str, lines: &mut Vec<String>, sizes: &mut Vec<String>) {
        let Some(k) = open(path) else { return };
        lines.push(format!("{relative}|"));
        let (values, subs) = values_and_subkeys(&k);
        for v in values {
            let kind = kind_name(v.kind);
            let data = match v.kind.0 {
                1 | 2 => text(&v.data),
                _ => v.data.iter().map(|b| format!("{b:02x}")).collect(),
            };
            lines.push(format!("{relative}|{}|{kind}|{data}", v.name));
            sizes.push(format!("{relative}|{}|{kind}|{}", v.name, v.data.len()));
        }
        for sub in subs {
            let rel = if relative.is_empty() { sub.clone() } else { format!(r"{relative}\{sub}") };
            walk(&format!(r"{path}\{sub}"), &rel, lines, sizes);
        }
    }

    fn ordinal_sort(v: &mut [String]) {
        v.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    }

    /// `HKCU\<root>` in the C# tool's `dump` format: `key|` per key and `key|name|kind|data` per value, relative to
    /// the root, ordinal order. Also returns `key|name|kind|<byte size>` per value (the raw size, which the .NET
    /// dump does not show: REG_SZ includes its terminating NUL).
    pub fn dump(root: &str) -> (Vec<String>, Vec<String>) {
        let (mut lines, mut sizes) = (Vec::new(), Vec::new());
        walk(root, "", &mut lines, &mut sizes);
        ordinal_sort(&mut lines);
        ordinal_sort(&mut sizes);
        (lines, sizes)
    }

    /// Writes a dumped tree (string values only) below `HKCU\<root>`, the way .NET `SetValue(string)` does.
    pub fn materialize(root: &str, lines: &[String]) {
        for line in lines {
            let parts: Vec<&str> = line.splitn(4, '|').collect();
            let path = if parts[0].is_empty() { root.to_owned() } else { format!(r"{root}\{}", parts[0]) };
            let k = create(&path);
            if parts.len() == 2 && parts[1].is_empty() {
                continue;
            }
            assert!(parts.len() == 4 && parts[2] == "String", "only string values: {line}");
            let data: Vec<u8> = wide(parts[3]).iter().flat_map(|u| u.to_le_bytes()).collect();
            let n = wide(parts[1]);
            let r = unsafe { RegSetValueExW(k.0, PCWSTR(n.as_ptr()), None, REG_SZ, Some(&data)) };
            assert_eq!(r, ERROR_SUCCESS, "{line}");
        }
    }
}

// ---- single instance: names (win-ui-main §2) ------------------------------------------------------------------------

#[test]
fn user_name_sanitization_matches_dotnet() {
    let f = fixture("single-instance.json");
    for row in f["names"].as_array().unwrap() {
        let user = s(&row["user"]);
        let names = InstanceNames::for_user(user);
        assert_eq!(sanitize_token(user), s(&row["token"]), "token of {user:?}");
        assert_eq!(names.mutex, s(&row["mutex"]), "mutex of {user:?}");
        assert_eq!(names.pipe, s(&row["pipe"]), "pipe of {user:?}");
    }
}

// ---- single instance: the pipe in both directions ---------------------------------------------------------------

/// The files and folders the recorded payloads name, recreated under a fresh `{dir}`.
struct PipeDir {
    dir: tempfile::TempDir,
}

impl PipeDir {
    fn new(f: &Value) -> PipeDir {
        let dir = tempfile::tempdir().unwrap();
        for file in strings(&f["files"]) {
            std::fs::write(dir.path().join(file), b"x").unwrap();
        }
        for sub in strings(&f["directories"]) {
            std::fs::create_dir(dir.path().join(sub)).unwrap();
        }
        PipeDir { dir }
    }

    fn expand(&self, template: &str) -> String {
        template.replace("{dir}", &self.dir.path().display().to_string())
    }

    /// A recorded payload: text segments (`{dir}` expanded) and `{"hex": ".."}` segments of raw bytes.
    fn bytes(&self, segments: &Value) -> Vec<u8> {
        let mut out = Vec::new();
        for seg in segments.as_array().unwrap() {
            match seg {
                Value::String(t) => out.extend_from_slice(self.expand(t).as_bytes()),
                _ => {
                    let hex = s(&seg["hex"]);
                    out.extend((0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()));
                }
            }
        }
        out
    }
}

/// C# → Rust: every payload the C# server was fed (the C# client's own output for recorded arguments, and edge
/// cases), sent by a client that does what the .NET `CurrentUserOnly` client does, must reach the Rust sink as the
/// same list the C# server made of it. The Rust pipe security must still be the one the .NET client accepted.
#[test]
fn csharp_payloads_reach_the_rust_server_as_csharp_parsed_them() {
    let f = fixture("single-instance.json");
    let d = PipeDir::new(&f);
    let names = InstanceNames::for_user(&unique("sa compat server"));
    let mut owner = SingleInstanceCoordinator::with_names(names.clone());
    assert_eq!(owner.acquire_or_forward(&[]), InstanceRole::Primary);
    let (tx, rx) = mpsc::channel();
    owner.set_sink(Box::new(move |p| {
        let _ = tx.send(p);
    }));
    let cases = f["csharp_server"].as_array().unwrap();
    assert!(cases.len() >= 10);
    for case in cases {
        let name = s(&case["name"]);
        let bytes = d.bytes(&case["bytes"]);
        let sddl = raw::dotnet_like_client(&names.pipe, &bytes, TIMEOUT);
        let expected: Vec<PathBuf> = strings(&case["batch"]).iter().map(|p| PathBuf::from(d.expand(p))).collect();
        assert_eq!(rx.recv_timeout(TIMEOUT).unwrap(), expected, "case {name}");
        assert_eq!(raw::mask_sddl(&sddl), s(&f["rust_server_sddl"]), "the Rust pipe security changed (case {name})");
    }
    owner.dispose();
    // Nobody listens any more: forwarding fails within the client timeout (the C# client failed there too).
    assert_eq!(s(&f["csharp_client_without_server"]), "fail");
    let started = Instant::now();
    assert!(!SingleInstanceCoordinator::forward(&names, &[PathBuf::from("x")]));
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// Rust → C#: for every argument list the C# client forwarded, the Rust client writes exactly the bytes the C#
/// client wrote (and the C# server parsed, see the case of the same name), to a pipe with the C# server's security.
#[test]
fn the_rust_client_writes_the_csharp_client_bytes() {
    let f = fixture("single-instance.json");
    let d = PipeDir::new(&f);
    let csharp_sddl = raw::unmask_sddl(s(&f["csharp_server_sddl"]));
    let server_cases: Vec<&str> = f["csharp_server"].as_array().unwrap().iter().map(|c| s(&c["name"])).collect();
    let cases = f["csharp_client"].as_array().unwrap();
    assert!(cases.len() >= 4);
    for case in cases {
        let name = s(&case["name"]);
        assert!(server_cases.contains(&name), "{name}: its payload is also a C# server case");
        let args: Vec<PathBuf> = strings(&case["args"]).iter().map(|a| PathBuf::from(d.expand(a))).collect();
        let expected = d.bytes(&case["bytes"]);
        assert_eq!(encode_paths(&args), expected, "case {name}");
        let pipe = unique("sa compat client").replace(' ', "_");
        let spy = raw::SpyServer::new(&pipe, Some(&csharp_sddl));
        let reader = std::thread::spawn(move || spy.accept());
        let names = InstanceNames { mutex: String::new(), pipe };
        assert!(SingleInstanceCoordinator::forward(&names, &args), "case {name}");
        assert_eq!(reader.join().unwrap(), expected, "case {name}");
    }
}

// ---- file associations (win-ui-main §3) -----------------------------------------------------------------------------

/// `HKCU\<scratch>` for one test, removed (with everything below it) on drop.
struct Scratch(String);

impl Scratch {
    fn new(tag: &str) -> Self {
        Scratch(format!(r"Software\SubtitleAdderDesktopTests-compat-{}", unique(tag).replace(' ', "-")))
    }
    fn dump(&self) -> Vec<String> {
        raw::dump(&self.0).0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        raw::delete_tree(&self.0);
    }
}

/// The dump lines (and value sizes) the Rust registrar writes beyond the C# tree: for every accepted media extension
/// the C# app did not have, a `SupportedTypes` value and a `Capabilities\FileAssociations` value (no
/// `Software\Classes\.<ext>` key). Both lists are in the dump's ordinal order.
fn open_with_only_additions() -> (Vec<String>, Vec<String>) {
    use sa_platform::win::associations::{MEDIA_PROG_ID, OPEN_WITH_ONLY_MEDIA_EXTENSIONS};
    let types = r"Software\Classes\Applications\SubtitleAdder.exe\SupportedTypes";
    let assoc = r"Software\SubtitleAdder\Capabilities\FileAssociations";
    let prog_id_size = (MEDIA_PROG_ID.encode_utf16().count() + 1) * 2;
    let (mut lines, mut sizes) = (Vec::new(), Vec::new());
    for ext in OPEN_WITH_ONLY_MEDIA_EXTENSIONS {
        lines.push(format!("{types}|.{ext}|String|"));
        lines.push(format!("{assoc}|.{ext}|String|{MEDIA_PROG_ID}"));
        sizes.push(format!("{types}|.{ext}|String|2"));
        sizes.push(format!("{assoc}|.{ext}|String|{prog_id_size}"));
    }
    for v in [&mut lines, &mut sizes] {
        v.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    }
    assert_eq!(lines.len(), 81 * 2);
    (lines, sizes)
}

/// `dump` without the `expected` lines, which must all be in it (exactly once); the rest keeps its order.
fn split_off(dump: Vec<String>, expected: &[String], what: &str) -> Vec<String> {
    let set: std::collections::HashSet<&String> = expected.iter().collect();
    let (removed, rest): (Vec<String>, Vec<String>) = dump.into_iter().partition(|l| set.contains(l));
    assert_eq!(removed, expected, "the {what} the additions write");
    rest
}

#[test]
fn file_associations_write_and_remove_the_csharp_registry_tree() {
    use sa_core::platform::FileAssociations;
    use sa_platform::win::associations::FileAssociationRegistrar;
    let f = fixture("file-associations.json");
    let exe = s(&f["exe"]);
    let cases = f["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 2);
    for case in cases {
        let icon = case["icon"].as_str().map(str::to_owned);
        let seed = strings(&case["seed"]);
        let registered = strings(&case["registered"]);
        let unregistered = strings(&case["unregistered"]);
        let rust = |s: &Scratch| FileAssociationRegistrar::with_root(&s.0, PathBuf::from(exe), icon.clone());

        // Rust writes the C# tree, value for value (and byte size for byte size), plus exactly the "Open with"
        // entries of the additions, and registering again changes nothing.
        let rs = Scratch::new("rs");
        raw::materialize(&rs.0, &seed);
        assert!(!rust(&rs).is_registered());
        assert!(rust(&rs).register());
        let (extra_lines, extra_sizes) = open_with_only_additions();
        let (lines, sizes) = raw::dump(&rs.0);
        let lines = split_off(lines, &extra_lines, "lines");
        let sizes = split_off(sizes, &extra_sizes, "sizes");
        assert_eq!(lines, registered, "Rust and C# registrations differ (icon {icon:?})");
        assert_eq!(sizes, strings(&case["registered_sizes"]), "value sizes (icon {icon:?})");
        assert!(rust(&rs).register());
        let (lines, sizes) = raw::dump(&rs.0);
        assert_eq!(split_off(lines, &extra_lines, "lines"), registered);
        assert_eq!(split_off(sizes, &extra_sizes, "sizes"), strings(&case["registered_sizes"]), "sizes after registering again");
        // Removing it leaves exactly what C# left after removing its own tree (the additions' values live in trees
        // that are deleted); removing twice is fine.
        assert!(rust(&rs).unregister());
        assert_eq!(rs.dump(), unregistered);
        assert!(!rust(&rs).is_registered());
        assert!(rust(&rs).unregister());
        assert_eq!(rs.dump(), strings(&case["unregistered_twice"]));

        // The tree C# wrote: Rust recognizes it and removes it the way C# did.
        let cs = Scratch::new("cs");
        raw::materialize(&cs.0, &registered);
        assert_eq!(cs.dump(), registered);
        assert_eq!(rust(&cs).is_registered(), case["is_registered"].as_bool().unwrap());
        assert!(rust(&cs).unregister());
        assert_eq!(cs.dump(), unregistered);
        assert_eq!(rust(&cs).is_registered(), case["is_registered_after_unregister"].as_bool().unwrap());

        // Upgrade: Rust registering over the tree C# wrote yields the C# tree plus exactly the additions.
        let up = Scratch::new("up");
        raw::materialize(&up.0, &registered);
        assert!(rust(&up).register());
        let (lines, sizes) = raw::dump(&up.0);
        assert_eq!(split_off(lines, &extra_lines, "lines"), registered, "upgrade over the C# tree (icon {icon:?})");
        assert_eq!(split_off(sizes, &extra_sizes, "sizes"), strings(&case["registered_sizes"]), "upgrade sizes (icon {icon:?})");
    }
}

// ---- fonts (win-ui-main §20.2) -------------------------------------------------------------------------------------

/// The C# list is `SortedSet<string>(StringComparer.CurrentCultureIgnoreCase)` filled from DirectWrite, skipping
/// empty and vertical (`@…`) names. The installed fonts differ per machine, so the fixture holds fixed names in a
/// fixed enumeration order and the C# result for several cultures.
#[test]
fn font_family_rules_match_the_csharp_sorted_set() {
    use sa_platform::win::fonts::{CultureCollator, FALLBACK, sort_names_by};
    let f = fixture("fonts.json");
    assert_eq!(FALLBACK.to_vec(), strings(&f["fallback"]));
    let inputs = strings(&f["inputs"]);
    let pairs: Vec<(String, String)> =
        f["pairs"].as_array().unwrap().iter().map(|p| (s(&p[0]).to_owned(), s(&p[1]).to_owned())).collect();
    let cultures = f["cultures"].as_object().unwrap();
    assert!(cultures.len() >= 4);
    let mut differences = Vec::new();
    for (culture, expected) in cultures {
        let Some(collator) = CultureCollator::for_locale(culture) else {
            panic!("no ICU collator for {culture:?} (icu.dll is part of Windows 10 1903+)");
        };
        let sorted = sort_names_by(inputs.clone(), |a, b| collator.compare(a, b));
        assert_eq!(sorted, strings(&expected["sorted"]), "SortedSet order under {culture:?}");
        let compared: String = pairs
            .iter()
            .map(|(a, b)| match collator.compare(a, b) {
                std::cmp::Ordering::Less => '<',
                std::cmp::Ordering::Equal => '=',
                std::cmp::Ordering::Greater => '>',
            })
            .collect();
        let want = s(&expected["compare"]);
        let diff: Vec<_> = pairs
            .iter()
            .zip(compared.chars().zip(want.chars()))
            .filter(|(_, (got, want))| got != want)
            .map(|((a, b), (got, want))| format!("{culture:?}: {a:?} vs {b:?}: Rust {got}, C# {want}"))
            .collect();
        for pair in KNOWN_FONT_ORDER_DIVERGENCES {
            assert!(
                diff.iter().any(|d| d.contains(&format!("{:?} vs {:?}:", pair.0, pair.1))),
                "{pair:?} now compares like C# under {culture:?}: remove it from KNOWN_FONT_ORDER_DIVERGENCES"
            );
        }
        differences.extend(diff.into_iter().filter(|d| {
            !KNOWN_FONT_ORDER_DIVERGENCES.iter().any(|(a, b)| d.contains(&format!("{a:?} vs {b:?}:")))
        }));
    }
    assert!(differences.is_empty(), "CurrentCultureIgnoreCase: {differences:#?}");
}

/// Pairs the Rust collator (ICU at secondary strength) finds equal while .NET's `IgnoreCase` keeps width
/// (`Ａ`/`A`, `ｱ`/`ア`) and kana type (`あ`/`ア`) differences, under every recorded culture. None of these occur in
/// the fixed font names (the sorted lists match), but a machine with two family names differing only that way would
/// list one of them in Rust and both in C#.
const KNOWN_FONT_ORDER_DIVERGENCES: [(&str, &str); 3] = [("Ａrial", "Arial"), ("ｱ", "ア"), ("あ", "ア")];

// ---- path rules: roots, extensions, explorer folders, storage readiness ----------------------------------------------

#[test]
fn path_rules_match_dotnet() {
    use sa_platform::dialogs::{DialogFlavor, has_extension, suggested_save_name};
    use sa_platform::win::folders::{normalize_directory_path, path_root, storage_available};
    let f = fixture("paths.json");
    let cwd = std::env::current_dir().unwrap().display().to_string();
    assert!(cwd.as_bytes().get(1) == Some(&b':'), "the test runs from a drive-letter directory: {cwd}");
    let drive = cwd[..2].to_owned();
    // A drive letter that does not exist here (DriveInfo.IsReady false).
    let missing = (b'D'..=b'Z').map(|c| format!("{}:", c as char)).find(|r| !Path::new(&format!(r"{r}\")).exists()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let tmp_s = tmp.path().display().to_string();
    let parent = Path::new(&cwd).parent().unwrap().display().to_string();
    let tmp_root = tmp_s[..3].to_owned();
    let expand = |t: &str| {
        t.replace("{cwd}", &cwd)
            .replace("{parent}", &parent)
            .replace("{drive}", &drive)
            .replace("{missing}", &missing)
            .replace("{tmp_root}", &tmp_root)
            .replace("{tmp}", &tmp_s)
    };
    let rows = f["rows"].as_array().unwrap();
    assert!(rows.len() >= 20);
    for row in rows {
        let input = expand(s(&row["input"]));
        let input = input.as_str();
        assert_eq!(path_root(input), expand(s(&row["root"])), "GetPathRoot({input:?})");
        assert_eq!(has_extension(Path::new(input)), row["hasExtension"].as_bool().unwrap(), "HasExtension({input:?})");
        assert_eq!(
            suggested_save_name(input, DialogFlavor::Windows),
            expand(s(&row["withoutExtension"])),
            "GetFileNameWithoutExtension({input:?})"
        );
        assert_eq!(normalize_directory_path(input), expand(s(&row["normalizedDirectory"])), "NormalizeDirectoryPath({input:?})");
        assert_eq!(storage_available(Path::new(input)), row["storageAvailable"].as_bool().unwrap(), "storage of {input:?}");
    }
}
