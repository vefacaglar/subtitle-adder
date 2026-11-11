# Desktop app architecture notes

The crate layout is described in the repository's `CLAUDE.md`. This file records dependency and feature decisions
that `Cargo.toml` asks to be written down (every external version is pinned; adding a crate or a feature to a crate is
noted here).

## §7 `sa-player` (video player)

Video path: demux → decode (D3D11VA / VideoToolbox through FFmpeg's hwaccel, else software) → planes for the UI's
OpenGL shader (`sa-app/src/main_window/video_gl.rs`), or swscale RGBA for the software renderer.

- Every pixel format reaches the GPU path: planar / semi-planar / gray YUV (any subsampling up to 4:1:0, 8–16-bit
  little-endian, read from the pixel format descriptor) as the decoder's own planes, everything else (RGB, palette,
  packed YUV, big-endian) as swscale's RGBA in one packed plane.
- Colour (`sa_player::color`, shared by the shader and its unit tests): HDR PQ / HLG is tone-mapped to SDR (BT.2390
  EETF from MaxCLL / mastering peak / 1000 nits to 203-nit reference white), BT.2020 and P3 are converted to BT.709;
  SDR BT.709 content takes the plain matrix conversion unchanged.
- Interlaced frames are handed over once per field; the shader deinterlaces (motion-adaptive against the previous
  frame, bob where it moved). `SUBTITLE_ADDER_DEINTERLACE=0` shows them woven.
- AV1: the default decoder (`libdav1d`) has no hardware config, so hardware decoding uses the native `av1` decoder;
  a stream the GPU cannot decode goes back to `libdav1d`.
- Thread priorities (`ffmpeg_backend/priority.rs`): presenter and audio decode in MMCSS "Playback", the cpal output
  callback in "Pro Audio", video decode above normal (Windows); pthread QoS classes (macOS). Off with
  `SUBTITLE_ADDER_THREAD_PRIORITY=0`.

Tracks (`ffmpeg_backend/tracks.rs`, `subtitles.rs`; the menu model is `sa_viewmodels::media_tracks`). After the input
opens, the demux thread lists every audio and subtitle stream (language/title tags, codec, channels, default/forced,
and for subtitles text or image from the codec descriptor's `TEXT_SUB`/`BITMAP_SUB` properties; a text codec this
FFmpeg build has no decoder for is listed as unsupported). It emits `PlayerEvent::TracksChanged`. `open_streams`
falls back to the next audio stream when the best one cannot be decoded. `MediaPlayerBackend::tracks` and
`select_audio_track` have no default bodies, so a wrapper backend (the Windows `CachedPlayer`) that forgets to forward
them does not build.

- Switching audio (`Player::select_audio_track`): the request goes into a slot that the demux thread handles at the top
  of its loop (a newer request replaces one not yet handled, and asking for the active stream does nothing, so
  A → B → A ends on A). It opens the new decoder first; on failure it emits `AudioTrackFailed` and keeps the current
  track. Otherwise the new decoder and time base wait in an `AudioSwap` slot keyed on the stream, and the switch reuses
  the seek machinery: `restart_decoding(position)` bumps the serial, flushes the queues and restarts demuxing at the
  current position. Every queued packet carries its stream index, and the audio thread changes decoders exactly at
  the first packet of a stream it does not decode yet (a packet whose stream has no waiting decoder is skipped), so
  no packet of one stream reaches the other's decoder, whatever a seek racing the switch or a second switch does to
  the serials. The output device stays open, because swresample is rebuilt per input format (another rate or layout
  needs no reopen). A paused player stays paused, and the audio clock (master) resumes from the restarted packets. At
  EOF the switch takes effect at the next seek. With no output device only the active track changes.
- Reading an embedded text subtitle track (`extract_text_subtitles`) uses its own demuxer, with every other stream
  set to `AVDISCARD_ALL`, so it never touches the playing session. It runs on a worker thread of the app (never the
  UI or player threads) and is cancellable through FFmpeg's interrupt callback; it stops with an error past 100 000
  events or 16 MiB of text (`ExtractLimits`). Packets go through FFmpeg's subtitle decoder (ASS events for every text
  codec: SubRip, ASS/SSA, mov_text, WebVTT). Their text is made plain here (override blocks and drawing events
  dropped, FFmpeg's escapes undone; a tagless `{…}` block is kept as text except in ASS/SSA tracks, where it is a
  comment) and written as the app's JSON sentence list under `embedded-subtitles/` in the app's temp work folder
  (registered as a session scratch file, deleted on exit). JSON, because both other importers strip `{…}` as markup.
  The app then imports that file through the same path as a subtitle file the user picked (`sa_core::subtitle_import`,
  same confirmation and replacement rules), unless the session's lines, translations or job changed while the read
  ran (`media_tracks::SessionMark`): then it reports that and leaves the lines alone. Image tracks (PGS, VobSub, DVB)
  are listed but disabled; more than three collapse into one summary row.
- The read opens the file directly (`MediaSource::File`), not through the Windows `BufferedVideoStream`: it bypasses
  the encrypted SSD chunk cache and, on a slow drive, competes with playback for I/O while it runs (Matroska is read
  through to the end). Left as is: reads are user-started, one at a time and cancellable.
- Test hooks (`#[doc(hidden)]`): `use_audio_tap` replaces the device with a virtual real-time output that records
  `(pts, samples)` blocks, and `simulate_audio_decoder_failure` makes opening a given audio stream fail.

Dependencies of `sa-player` (feature `ffmpeg`):

| Crate | Where | Why |
|---|---|---|
| `ffmpeg-next` (workspace features `codec`, `format`, `software-resampling`, `software-scaling`) | all | decode, swscale, swresample; no `filter` feature (deinterlacing runs in the shader) |
| `cpal` | all | audio output |
| `windows` (`Win32_Foundation`, `Win32_System_Threading`) | Windows | `SetThreadPriority`, `AvSetMmThreadCharacteristicsW` |
| `libc` | macOS | `pthread_set_qos_class_self_np` |

`windows` and `libc` were already workspace dependencies (no new crate, no version change).
