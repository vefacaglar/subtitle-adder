//! The audio and subtitle streams of an opened input ([`crate::MediaTracks`]).

use std::ffi::{CStr, c_char};

use ffmpeg_next::ffi;
use ffmpeg_next::format::context::Input;
use ffmpeg_next::media::Type;

use crate::{AudioTrack, MediaTracks, SubtitleKind, SubtitleTrack};

/// Every audio stream (cover art and other attached pictures are video streams and not listed) and every subtitle
/// stream, in container order. `active_audio` is left `None` (the session sets it).
pub(crate) fn enumerate(ictx: &Input) -> MediaTracks {
    let mut out = MediaTracks::default();
    for s in ictx.streams() {
        let params = s.parameters();
        let medium = params.medium();
        if medium != Type::Audio && medium != Type::Subtitle {
            continue;
        }
        let meta = s.metadata();
        let tag = |k: &str| meta.get(k).map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned);
        let language = tag("language").filter(|l| !l.eq_ignore_ascii_case("und"));
        let title = tag("title");
        let (disposition, par) = unsafe { ((*s.as_ptr()).disposition, (*s.as_ptr()).codecpar) };
        let codec_id = unsafe { (*par).codec_id };
        let codec = codec_name(codec_id);
        let default = disposition & ffi::AV_DISPOSITION_DEFAULT != 0;
        if medium == Type::Audio {
            let (channels, rate, layout) = unsafe { ((*par).ch_layout.nb_channels, (*par).sample_rate, describe_layout(&(*par).ch_layout)) };
            out.audio.push(AudioTrack {
                stream_index: s.index(),
                language,
                title,
                codec,
                channels: channels.clamp(0, u16::MAX as i32) as u16,
                channel_layout: layout,
                sample_rate: rate.max(0) as u32,
                default,
            });
        } else {
            out.subtitles.push(SubtitleTrack {
                stream_index: s.index(),
                language,
                title,
                kind: subtitle_kind(codec_id),
                codec,
                default,
                forced: disposition & ffi::AV_DISPOSITION_FORCED != 0,
            });
        }
    }
    out
}

/// FFmpeg's short codec name (`aac`, `subrip`, ...).
pub(crate) fn codec_name(id: ffi::AVCodecID) -> String {
    unsafe {
        let p = ffi::avcodec_get_name(id);
        if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
    }
}

/// Text when the codec descriptor says the stream carries text events and not pictures (subrip, ass, ssa, mov_text,
/// webvtt, text, ...) and this build has a decoder for it; a text codec without one is [`SubtitleKind::Unsupported`];
/// everything else (PGS, VobSub, DVB, XSUB, unknown codecs) is an image track for now.
pub(crate) fn subtitle_kind(id: ffi::AVCodecID) -> SubtitleKind {
    let props = unsafe {
        let d = ffi::avcodec_descriptor_get(id);
        if d.is_null() { 0 } else { (*d).props }
    };
    let text = props & ffi::AV_CODEC_PROP_TEXT_SUB as i32 != 0;
    let bitmap = props & ffi::AV_CODEC_PROP_BITMAP_SUB as i32 != 0;
    match (text && !bitmap, has_decoder(id)) {
        (true, true) => SubtitleKind::Text,
        (true, false) => SubtitleKind::Unsupported,
        (false, _) => SubtitleKind::Image,
    }
}

fn has_decoder(id: ffi::AVCodecID) -> bool {
    unsafe { !ffi::avcodec_find_decoder(id).is_null() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_codecs_need_a_decoder() {
        let _ = ffmpeg_next::init();
        assert_eq!(subtitle_kind(ffi::AVCodecID::AV_CODEC_ID_SUBRIP), SubtitleKind::Text);
        assert_eq!(subtitle_kind(ffi::AVCodecID::AV_CODEC_ID_WEBVTT), SubtitleKind::Text);
        assert_eq!(subtitle_kind(ffi::AVCodecID::AV_CODEC_ID_HDMV_PGS_SUBTITLE), SubtitleKind::Image);
        // FFmpeg has no decoder for HDMV text subtitles (Blu-ray TextST) at all.
        assert_eq!(subtitle_kind(ffi::AVCodecID::AV_CODEC_ID_HDMV_TEXT_SUBTITLE), SubtitleKind::Unsupported);
    }
}

/// `av_channel_layout_describe` (`mono`, `stereo`, `5.1(side)`); `None` for an unspecified layout.
unsafe fn describe_layout(layout: &ffi::AVChannelLayout) -> Option<String> {
    if layout.nb_channels <= 0 || layout.order == ffi::AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC {
        return None;
    }
    let mut buf = [0 as c_char; 64];
    let r = unsafe { ffi::av_channel_layout_describe(layout, buf.as_mut_ptr(), buf.len()) };
    if r <= 0 {
        return None;
    }
    let text = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned();
    (!text.is_empty()).then_some(text)
}
