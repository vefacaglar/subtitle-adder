//! Labels back-to-back `downloading_model` runs during `transcribe --diarize` (mac-enginekit §4 = win-enginekit §7).

use sa_core::engine_types::EngineStage;

/// Which model a download run belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DownloadKind {
    Transcription,
    Diarization,
}

/// Value type (Clone, PartialEq).
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadPhaseTracker {
    expected: Vec<DownloadKind>,
    phase: i64,
    last_fraction: Option<f64>,
    in_download: bool,
}

impl DownloadPhaseTracker {
    pub fn new(transcription_downloaded: bool, diarize: bool, diarization_downloaded: bool) -> Self {
        let mut expected = Vec::new();
        if !transcription_downloaded {
            expected.push(DownloadKind::Transcription);
        }
        if diarize && !diarization_downloaded {
            expected.push(DownloadKind::Diarization);
        }
        Self { expected, phase: -1, last_fraction: None, in_download: false }
    }

    /// Feeds a stage event; returns the kind for `downloading_model`, `None` for any other stage.
    pub fn update(&mut self, stage: EngineStage, fraction: Option<f64>) -> Option<DownloadKind> {
        if stage != EngineStage::DownloadingModel {
            self.in_download = false;
            self.last_fraction = None;
            return None;
        }
        let last = self.last_fraction.unwrap_or(0.0);
        let dropped_back = last >= 0.99 && fraction.unwrap_or(0.0) < last;
        if !self.in_download || dropped_back {
            self.phase += 1;
        }
        self.in_download = true;
        self.last_fraction = fraction;
        let phase = usize::try_from(self.phase).unwrap_or(0);
        Some(self.expected.get(phase).copied().unwrap_or(DownloadKind::Transcription))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DownloadKind::{Diarization as D, Transcription as T};
    use EngineStage::{DownloadingModel as Dl, LoadingModel as Lm};

    fn run(mut tracker: DownloadPhaseTracker, events: &[(EngineStage, Option<f64>)]) -> Vec<Option<DownloadKind>> {
        events.iter().map(|&(s, f)| tracker.update(s, f)).collect()
    }

    #[test]
    fn both_missing_back_to_back() {
        let t = DownloadPhaseTracker::new(false, true, false);
        let got = run(
            t,
            &[
                (EngineStage::ExtractingAudio, None),
                (Dl, Some(0.0)),
                (Dl, Some(0.5)),
                (Dl, Some(1.0)),
                (Dl, Some(0.0)),
                (Dl, Some(0.5)),
                (Dl, Some(1.0)),
                (Lm, None),
            ],
        );
        assert_eq!(got, vec![None, Some(T), Some(T), Some(T), Some(D), Some(D), Some(D), None]);
    }

    #[test]
    fn only_speaker_model_missing() {
        let got = run(DownloadPhaseTracker::new(true, true, false), &[(Dl, Some(0.0)), (Dl, Some(1.0)), (Lm, None)]);
        assert_eq!(got, vec![Some(D), Some(D), None]);
    }

    #[test]
    fn only_transcription_model_missing() {
        let got = run(DownloadPhaseTracker::new(false, true, true), &[(Dl, Some(0.0)), (Dl, Some(1.0)), (Lm, None)]);
        assert_eq!(got, vec![Some(T), Some(T), None]);
    }

    #[test]
    fn nothing_missing_still_labels_an_unexpected_download() {
        assert_eq!(run(DownloadPhaseTracker::new(true, true, true), &[(Dl, Some(0.3))]), vec![Some(T)]);
    }

    #[test]
    fn phases_separated_by_another_stage() {
        let got = run(DownloadPhaseTracker::new(false, true, false), &[(Dl, Some(0.4)), (Lm, None), (Dl, Some(0.1))]);
        assert_eq!(got, vec![Some(T), None, Some(D)]);
    }

    #[test]
    fn diarize_off_expects_no_speaker_phase() {
        let mut t = DownloadPhaseTracker::new(true, false, false);
        assert_eq!(t.update(Dl, Some(0.2)), Some(T));
    }
}
