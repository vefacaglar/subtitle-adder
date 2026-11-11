//! Scheduling priority of the player's threads, so pacing holds while other processes load the CPU.
//!
//! * Windows: the presenter and the audio decode thread join MMCSS ("Playback"), the audio output callback thread
//!   ("Pro Audio"; cpal registers it only with its `realtime` feature, which is not enabled), and the video decode
//!   thread runs one step above normal. Without MMCSS (service stopped) the MMCSS roles fall back to
//!   `THREAD_PRIORITY_HIGHEST` / `TIME_CRITICAL`. A background read (an embedded subtitle track read through the
//!   whole file) enters background mode (`THREAD_MODE_BACKGROUND_BEGIN`: low CPU, very low I/O and memory priority,
//!   so the player's own reads of the same disk go first) and leaves it when the guard is dropped.
//! * macOS: pthread QoS classes (user-interactive for the presenter, user-initiated for the decoders, background for a
//!   background read, which also puts its disk I/O in the throttled tier); CoreAudio's IO thread is real-time
//!   already.
//! * Elsewhere: nothing.
//!
//! The priority is set by the thread itself ([`raise`]) and lasts until the returned guard is dropped (MMCSS
//! registrations are undone) or the thread exits.

/// What a thread does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// Shows frames when they are due (`present_main`): needs to wake on time.
    Presenter,
    /// Decodes and resamples audio into the ring.
    AudioDecode,
    /// The audio device callback (cpal's thread).
    AudioOutput,
    /// Decodes video (the GPU or FFmpeg's frame threads do most of the work).
    VideoDecode,
    /// Reads through a media file that may be playing (embedded subtitle tracks): must not slow playback down.
    BackgroundRead,
}

/// Undoes [`raise`] on drop (MMCSS registrations; the plain priorities end with the thread).
#[must_use = "the priority is reverted when the guard is dropped"]
pub(crate) struct Guard {
    #[cfg(windows)]
    mmcss: Option<windows::Win32::Foundation::HANDLE>,
    /// The thread is in background mode (left on drop: it must be left by the same thread).
    #[cfg(windows)]
    background: bool,
}

impl Drop for Guard {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(h) = self.mmcss.take() {
            // SAFETY: `h` is the registration of this thread (guards are not `Send`).
            let _ = unsafe { windows::Win32::System::Threading::AvRevertMmThreadCharacteristics(h) };
        }
        #[cfg(windows)]
        if std::mem::take(&mut self.background) {
            use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_END};
            // SAFETY: the pseudo handle of the calling thread, the one that entered background mode.
            let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_END) };
        }
    }
}

/// Raises the calling thread's priority for `role` (nothing when `SUBTITLE_ADDER_THREAD_PRIORITY=0`, for
/// comparisons).
pub(crate) fn raise(role: Role) -> Guard {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let enabled = *ENABLED.get_or_init(|| {
        let v = std::env::var("SUBTITLE_ADDER_THREAD_PRIORITY").unwrap_or_default().to_ascii_lowercase();
        !matches!(v.trim(), "0" | "off" | "no" | "false")
    });
    if !enabled {
        return Guard {
            #[cfg(windows)]
            mmcss: None,
            #[cfg(windows)]
            background: false,
        };
    }
    imp::raise(role)
}

/// Raises the calling thread once (for callbacks that run on a thread someone else owns): later calls on the same
/// thread do nothing. The registration lasts until the thread exits.
pub(crate) fn raise_once_on_this_thread(role: Role) {
    thread_local! {
        static RAISED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    RAISED.with(|r| {
        if !r.replace(true) {
            std::mem::forget(raise(role));
        }
    });
}

#[cfg(windows)]
mod imp {
    use super::{Guard, Role};
    use windows::Win32::System::Threading::{
        AvSetMmThreadCharacteristicsW, GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN, THREAD_PRIORITY,
        THREAD_PRIORITY_ABOVE_NORMAL, THREAD_PRIORITY_HIGHEST, THREAD_PRIORITY_TIME_CRITICAL,
    };
    use windows::core::{PCWSTR, w};

    pub(super) fn raise(role: Role) -> Guard {
        if role == Role::BackgroundRead {
            // SAFETY: the pseudo handle of the calling thread. Fails (and is not left on drop) when the thread is in
            // background mode already.
            let background = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) }.is_ok();
            return Guard { mmcss: None, background };
        }
        let (task, fallback): (Option<PCWSTR>, THREAD_PRIORITY) = match role {
            Role::Presenter | Role::AudioDecode => (Some(w!("Playback")), THREAD_PRIORITY_HIGHEST),
            Role::AudioOutput => (Some(w!("Pro Audio")), THREAD_PRIORITY_TIME_CRITICAL),
            Role::VideoDecode => (None, THREAD_PRIORITY_ABOVE_NORMAL),
            Role::BackgroundRead => unreachable!("handled above"),
        };
        let mut mmcss = None;
        if let Some(task) = task {
            let mut index = 0u32;
            // SAFETY: registers the calling thread; `index` is an out parameter.
            if let Ok(h) = unsafe { AvSetMmThreadCharacteristicsW(task, &mut index) }
                && !h.is_invalid()
            {
                mmcss = Some(h);
            }
        }
        if mmcss.is_none() {
            // SAFETY: the pseudo handle of the calling thread.
            let _ = unsafe { SetThreadPriority(GetCurrentThread(), fallback) };
        }
        Guard { mmcss, background: false }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{Guard, Role};

    pub(super) fn raise(role: Role) -> Guard {
        let class = match role {
            Role::Presenter => Some(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE),
            Role::AudioDecode | Role::VideoDecode => Some(libc::qos_class_t::QOS_CLASS_USER_INITIATED),
            // Also throttles the thread's disk I/O behind the player's. Like the other classes it lasts until the
            // thread ends: reads run on a worker thread of their own.
            Role::BackgroundRead => Some(libc::qos_class_t::QOS_CLASS_BACKGROUND),
            // CoreAudio's IO thread is a real-time thread already; a QoS class would only lower it.
            Role::AudioOutput => None,
        };
        if let Some(class) = class {
            // SAFETY: applies to the calling thread only.
            let _ = unsafe { libc::pthread_set_qos_class_self_np(class, 0) };
        }
        Guard {}
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use super::{Guard, Role};

    pub(super) fn raise(_role: Role) -> Guard {
        Guard {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_role_can_be_raised_and_reverted_on_a_thread_of_its_own() {
        for role in [Role::Presenter, Role::AudioDecode, Role::AudioOutput, Role::VideoDecode, Role::BackgroundRead] {
            std::thread::spawn(move || {
                let g = raise(role);
                drop(g);
                raise_once_on_this_thread(role);
                raise_once_on_this_thread(role);
            })
            .join()
            .unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_video_decode_thread_runs_above_normal() {
        use windows::Win32::System::Threading::{GetCurrentThread, GetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL};
        std::thread::spawn(|| {
            let _g = raise(Role::VideoDecode);
            assert_eq!(unsafe { GetThreadPriority(GetCurrentThread()) }, THREAD_PRIORITY_ABOVE_NORMAL.0);
        })
        .join()
        .unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_background_read_enters_background_mode_until_its_guard_is_dropped() {
        use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN, THREAD_MODE_BACKGROUND_END};
        // Entering background mode twice fails, which tells whether the thread is in it.
        let enter_again = || unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) }.is_ok();
        std::thread::spawn(move || {
            let g = raise(Role::BackgroundRead);
            assert!(!enter_again(), "in background mode");
            drop(g);
            assert!(enter_again(), "left it");
            let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_END) };
        })
        .join()
        .unwrap();
    }
}
