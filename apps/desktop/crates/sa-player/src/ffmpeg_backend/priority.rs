//! Scheduling priority of the player's threads, so pacing holds while other processes load the CPU.
//!
//! * Windows: the presenter and the audio decode thread join MMCSS ("Playback"), the audio output callback thread
//!   ("Pro Audio"; cpal registers it only with its `realtime` feature, which is not enabled), and the video decode
//!   thread runs one step above normal. Without MMCSS (service stopped) the MMCSS roles fall back to
//!   `THREAD_PRIORITY_HIGHEST` / `TIME_CRITICAL`.
//! * macOS: pthread QoS classes (user-interactive for the presenter, user-initiated for the decoders); CoreAudio's
//!   IO thread is real-time already.
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
}

/// Undoes [`raise`] on drop (MMCSS registrations; the plain priorities end with the thread).
#[must_use = "the priority is reverted when the guard is dropped"]
pub(crate) struct Guard {
    #[cfg(windows)]
    mmcss: Option<windows::Win32::Foundation::HANDLE>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(h) = self.mmcss.take() {
            // SAFETY: `h` is the registration of this thread (guards are not `Send`).
            let _ = unsafe { windows::Win32::System::Threading::AvRevertMmThreadCharacteristics(h) };
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
        AvSetMmThreadCharacteristicsW, GetCurrentThread, SetThreadPriority, THREAD_PRIORITY, THREAD_PRIORITY_ABOVE_NORMAL,
        THREAD_PRIORITY_HIGHEST, THREAD_PRIORITY_TIME_CRITICAL,
    };
    use windows::core::{PCWSTR, w};

    pub(super) fn raise(role: Role) -> Guard {
        let (task, fallback): (Option<PCWSTR>, THREAD_PRIORITY) = match role {
            Role::Presenter | Role::AudioDecode => (Some(w!("Playback")), THREAD_PRIORITY_HIGHEST),
            Role::AudioOutput => (Some(w!("Pro Audio")), THREAD_PRIORITY_TIME_CRITICAL),
            Role::VideoDecode => (None, THREAD_PRIORITY_ABOVE_NORMAL),
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
        Guard { mmcss }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{Guard, Role};

    pub(super) fn raise(role: Role) -> Guard {
        let class = match role {
            Role::Presenter => Some(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE),
            Role::AudioDecode | Role::VideoDecode => Some(libc::qos_class_t::QOS_CLASS_USER_INITIATED),
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
        for role in [Role::Presenter, Role::AudioDecode, Role::AudioOutput, Role::VideoDecode] {
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
}
