"""Make a local MP4 preview for players that can't open the source container."""

from contextlib import nullcontext
from fractions import Fraction
from pathlib import Path
from typing import BinaryIO

import av

from .i18n import t


def make_preview(source: Path | BinaryIO, output: Path) -> None:
    # Open only the selected file. Don't let a demuxer resolve external references.
    stream = source.open("rb") if isinstance(source, Path) else nullcontext(source)
    with stream as media, av.open(media, options={"protocol_whitelist": "pipe"}) as reader:
        video = next((s for s in reader.streams.video if not s.disposition & s.disposition.attached_pic), None)
        audio = next(iter(reader.streams.audio), None)
        if video is None and audio is None:
            raise ValueError(t("preview_no_streams"))

        # Reserve the output exclusively so a caller can't overwrite the source or another file.
        with output.open("xb") as destination:
            try:
                with av.open(destination, "w", format="mp4") as writer:
                    _convert(reader, writer, video, audio)
            except Exception:
                output.unlink(missing_ok=True)
                raise


def _convert(reader, writer, video, audio):
    copied = {}
    encoders = {}
    resampler = None
    decoded = set()
    decode_errors = {}
    is_windows_media = reader.format.name == "asf"
    if video is not None:
        codec = video.codec_context
        # Some demuxers (notably Matroska) omit the first decode timestamps of reordered video.
        # Re-encode those tracks rather than dropping their initial keyframe or guessing DTS.
        if codec.name == "h264" and codec.pix_fmt in ("yuv420p", "yuvj420p") and not codec.has_b_frames:
            copied[video.index] = writer.add_stream_from_template(video)
        else:
            rate = video.average_rate or Fraction(30)
            track = writer.add_stream("libx264", rate=rate, options={"preset": "ultrafast", "crf": "21"})
            scale = min(1, 1920 / codec.width, 1080 / codec.height)
            track.width = max(2, int(codec.width * scale) // 2 * 2)
            track.height = max(2, int(codec.height * scale) // 2 * 2)
            track.pix_fmt = "yuv420p"
            track.time_base = track.codec_context.time_base = Fraction(1, 90_000)
            encoders[video.index] = track
    if audio is not None:
        if audio.codec_context.name == "aac" and audio.codec_context.channels in (1, 2):
            copied[audio.index] = writer.add_stream_from_template(audio)
        else:
            track = writer.add_stream("aac", rate=48_000, layout="stereo")
            encoders[audio.index] = track
            resampler = av.AudioResampler(format="fltp", layout="stereo", rate=48_000)

    for packet in reader.demux(*[stream for stream in (video, audio) if stream is not None]):
        index = packet.stream.index
        if index in copied:
            if packet.size == 0:
                continue  # decoder flush packet
            if packet.dts is None:
                packet.dts = packet.pts  # copied codecs have no frame reordering
            packet.stream = copied[index]
            writer.mux(packet)
            continue
        track = encoders[index]
        try:
            frames = packet.decode()
        except av.error.InvalidDataError as exc:
            if not is_windows_media:
                raise
            # A damaged ASF packet can be followed by valid frames. Keep their original timestamps
            # and fail if a track was entirely undecodable.
            decode_errors.setdefault(index, exc)
            continue
        if frames:
            decoded.add(index)
        for frame in frames:
            if isinstance(frame, av.VideoFrame):
                converted = frame.reformat(width=track.width, height=track.height, format=track.pix_fmt)
                # The flush packet can leave a decoded frame without a time base.
                converted.time_base = frame.time_base or packet.stream.time_base
                writer.mux(track.encode(converted))
            else:
                for converted in resampler.resample(frame):
                    writer.mux(track.encode(converted))

    for index, error in decode_errors.items():
        if index not in decoded:
            raise error
    if resampler is not None:
        for frame in resampler.resample(None):
            writer.mux(encoders[audio.index].encode(frame))
    for track in encoders.values():
        writer.mux(track.encode())
