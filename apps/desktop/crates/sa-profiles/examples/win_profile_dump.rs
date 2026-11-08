//! Prints what a Windows profile folder holds for one user, in the line format of the former C# `E2EData dump`
//! tool ([`sa_profiles::win::e2e_dump_lines`], pinned by sa-app's `csharp_e2e_data` test against the frozen output
//! `crates/sa-app/tests/fixtures/csharp/e2e-data/csharp-dump.txt`):
//!
//! ```text
//! user=<name>
//! history|<file name>|views=<n>|liked=<True|False>|rating=<n>
//! list|<list name>
//! ```
//!
//! `scripts/e2e-windows.ps1` runs it on the end-to-end data folder after the app ran on it:
//!
//! ```text
//! win_profile_dump.exe <dataDir> <user> <password>   (built by e2e-windows.ps1: cargo build --release -p sa-profiles --example win_profile_dump)
//! ```
//!
//! It logs in (read side only: no engagement is recorded), prints and logs out. Uses its own in-process broker scope,
//! so it never talks to a running app.

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use std::path::Path;
    use std::process::ExitCode;
    use std::time::Duration;

    use sa_core::secrets::SecretString;
    use sa_profiles::win::WinProfileStore;
    use sa_profiles::ProfileStore;
    use sa_sync::{InProcessBus, ProfileWriteBroker};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let [data_dir, user, password] = args.as_slice() else {
        eprintln!("usage: win_profile_dump <dataDir> <user> <password>");
        return ExitCode::from(64);
    };
    let broker: std::sync::Arc<dyn ProfileWriteBroker> = InProcessBus::with_scope(&format!("e2e-dump-{}", sa_core::text::guid_n()));
    let store = WinProfileStore::open(Path::new(data_dir).join("profiles.json"), broker);
    if let Err(e) = store.login(user, &SecretString::new(password.as_str()), false) {
        eprintln!("login {user} failed: {e:?}");
        return ExitCode::from(2);
    }
    for line in sa_profiles::win::e2e_dump_lines(store.as_ref()) {
        println!("{line}");
    }
    let _ = store.logout();
    store.flush_pending_writes(Duration::from_secs(10));
    store.dispose();
    ExitCode::SUCCESS
}

#[cfg(not(windows))]
fn main() {
    eprintln!("win_profile_dump reads the Windows profile format and runs on Windows only");
    std::process::exit(64);
}
