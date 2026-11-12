# Desktop app architecture notes

The crate layout is described in the repository's `CLAUDE.md`. This file records dependency and feature decisions
that `Cargo.toml` asks to be written down (every external version is pinned; adding a crate or a feature to a crate is
noted here).

## §7 `sa-player` (video player)

Video path: demux → decode (D3D11VA / VideoToolbox through FFmpeg's hwaccel, else software) → planes for the UI's
OpenGL shader (`sa-app/src/main_window/video_gl.rs`), or RGBA for the software renderer (swscale, with tone mapping
and deinterlacing on the CPU in `sa_player::soft`).

- Every pixel format reaches the GPU path: planar / semi-planar / gray YUV (any subsampling up to 4:1:0, 8–16-bit
  little-endian, read from the pixel format descriptor) as the decoder's own planes, everything else (RGB, palette,
  packed YUV, big-endian) as swscale's RGBA in one packed plane.
- Colour (`sa_player::color`, shared by the shader and its unit tests): HDR PQ / HLG is tone-mapped to SDR (BT.2390
  EETF from MaxCLL / mastering peak / 1000 nits to 203-nit reference white), BT.2020 and P3 are converted to BT.709;
  SDR BT.709 content takes the plain matrix conversion unchanged.
- Interlaced frames are handed over once per field; the shader deinterlaces (motion-adaptive against the previous
  frame, bob where it moved). `SUBTITLE_ADDER_DEINTERLACE=0` shows them woven.
- The RGBA path (software renderer: `SLINT_BACKEND=winit-software`, a GL setup or render failure, renderer teardown)
  shows the same picture in pure Rust (`sa_player::soft`): HDR / wide-gamut frames go through `color::ToneLuts`
  (`ColorParams::apply` as lookup tables, within one code of it) in a striped kernel that mirrors the shader's
  `to_rgb` (luma direct, chroma bilinear, the sample-aspect stretch per row); interlaced frames are deinterlaced per
  plane with the shader's rule (against the previous frame as decoded, `soft::previous_frame_follows` shared with the
  shader path) and handed over field by field, the second field dropped before its conversion when it is already
  late. Interlaced SDR frames with a planar description and no alpha are converted by swscale after deinterlacing;
  interlaced SDR formats without a planar description (swscale's RGBA, deinterlaced) and with alpha (shown opaque,
  like the shader) go through the kernel; plain SDR progressive frames take swscale alone, as before. Frames above
  1440p (2560 × 1440 pixels) on the kernel route are converted at half width and height (2 × 2 luma averaged, chroma
  at the output resolution, one tone mapping per output pixel), so 4K costs what 1080p costs; `VideoFrame::video_size`
  keeps the video's size for `natural_size` / `SizeChanged`, and the UI scales the buffer with the video's aspect.
  Large interlaced SDR frames keep full size through swscale (interlaced video above 1080i does not exist in
  practice). Stripes run on persistent workers with the decode thread's priority (one per 0.25 Mpx, up to the
  hardware threads minus two, at most 8); a panicking stripe converts the frame again on the decode thread. Known
  deviations from the shader (motion measured per plane, chroma deinterlaced before upsampling) are listed in
  `soft.rs`. Measured (release, 12 hardware threads, 8 stripes): PQ 3.9-4.9 ns/px at 1080p (22-24 on one thread),
  HLG 6.8-10, SDR 1.4-2.4; at half size 3.4-3.6 ns per source pixel (HLG 5.1-5.4), so a 4K HDR frame takes ~30 ms.
  1080i25, 1080p24 HDR10 and 2160p24 HDR10 play with no dropped frames; 2160p60 HDR10 drops up to ~4 % (testsrc2
  clips, which decode far faster than real 4K content). libavfilter (`bwdif`/`yadif`), swscale's and
  zscale's tone mapping, a 3D LUT and an RGB48 + LUT pass were rejected: other curves than the shader's, the `filter`
  feature, or a frame of latency.
- Hardware frames on Windows (`ffmpeg_backend/staging.rs`): the decode thread copies the decoder's array slice into a
  staging texture of its own (a ring of up to 10 per decoder, NV12 / P010 at the decoder texture's size, created as
  needed) and maps it; the frame's planes point into the mapping (Y at `pData`, chroma at `pData + RowPitch × texture
  height`, FFmpeg's own `fill_texture_ptrs` rule; byte-identical to `av_hwframe_transfer_data` in the unit test, also
  for 1920×800 / 3840×1600 frames in padded textures), which stays mapped until the frame is dropped. That removes
  the CPU copy `av_hwframe_transfer_data` makes (~25 MB per 4K P010 frame); the UI uploads straight from the mapping
  and the RGBA path reads it in place. D3D calls run only on the decode thread under FFmpeg's device lock (a dropped
  frame just marks its slot returned; only a frame that outlives its decoder unmaps its slot itself). No free slot,
  another format or any D3D failure: that frame goes through `av_hwframe_transfer_data`. `SUBTITLE_ADDER_ZEROCOPY=0`
  (`off` / `no` / `false`) turns the ring off. Measured (RTX 5070, maximized window, process CPU over 30 s, 3–6 runs):
  4K60 HEVC 10-bit 0.62 → 0.47 cores (spread 0.56–0.67 → 0.43–0.52), HDR10 0.68 → 0.51, AV1 10-bit 0.66 → 0.50,
  4K60 H.264 0.48 → 0.44, 1440p120 0.64 → 0.56, a 2160p24 10-bit film 0.33 → 0.26; no dropped frames either way. At
  4K60 10-bit the transfer costs ~0.01 core afterwards and the GL upload ~0.10, so the GL interop route
  (`WGL_NV_DX_interop2`, planes copied on the GPU) was not built: what it could still save is below the ~0.2 core that
  would justify its risk. `[player-stats]` (`SUBTITLE_ADDER_PLAYER_STATS=1`) carries the attribution (`perf.rs`).
- `SUBTITLE_ADDER_FRAME_DUMP=<dir>` dumps the GPU path's converted pictures and, as `...-cpu.png`, the RGBA path's.
- `SUBTITLE_ADDER_FULL_SCREEN=1` makes the main window enter full screen once, when the first video frame arrives
  (as the `F` key would), so full screen can be measured without input. `SUBTITLE_ADDER_PLAYER_STATS=1` ends the
  `[player-stats]` line with the window state: `full_screen=` (the controller's), `cursor_hidden=` (the pointer
  hidden over the playing full-screen video) and, on Windows, `window=x,y,WxH client=x,y,WxH monitor=x,y,WxH`
  (physical px, the client rectangle in screen coordinates, the monitor the window is mostly on), `maximized=` and
  `native_full_screen=` (the chrome's own flag). In full screen `client` must equal `monitor`.
- Full screen on Windows is the window chrome's (`sa_platform::win::chrome`, `WindowChrome::set_full_screen`), not
  winit's borderless mode: winit kept `WS_MAXIMIZE` and changed the styles after the size messages, so the chrome's
  `WM_NCCALCSIZE` still applied the maximized top inset (the client area was 9 px short of the monitor at 125 %).
  The chrome sets its flag before touching the window; full screen drops only the caption and sizing frame (so
  Windows keeps its maximized / snapped bookkeeping), covers the monitor the window is mostly on (kept there through
  monitor, resolution, DPI and work-area changes), squares the corners, removes the DWM border, switches Mica off
  and marks the window full screen for the shell (`ITaskbarList2`); leaving puts the frame and the previous
  rectangle back.
- AV1: the default decoder (`libdav1d`) has no hardware config, so hardware decoding uses the native `av1` decoder;
  a stream the GPU cannot decode goes back to `libdav1d`.
- Thread priorities (`ffmpeg_backend/priority.rs`): presenter and audio decode in MMCSS "Playback", the cpal output
  callback in "Pro Audio", video decode above normal, embedded subtitle reads in background mode (Windows); pthread
  QoS classes (macOS). Off with `SUBTITLE_ADDER_THREAD_PRIORITY=0`.

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
- Reading an embedded text subtitle track (`stream_text_subtitles`; `extract_text_subtitles` without snapshots)
  uses its own demuxer, with every other stream set to `AVDISCARD_ALL`, so it never touches the playing session. It
  runs on a worker thread of the app (never the UI or player threads) at background priority (`priority.rs`
  `Role::BackgroundRead`: Windows `THREAD_MODE_BACKGROUND_BEGIN`, which also lowers the thread's I/O and memory
  priority so the player's reads of the same disk go first, left again when the read ends; macOS the background QoS
  class, whose disk I/O is throttled too, and the thread's previous class restored when it ends), is cancellable through FFmpeg's interrupt callback (another video, the
  window closing; a blocking read returns at its next I/O call) and stops with an error past 100 000 events or 16 MiB
  of text (`ExtractLimits`). Packets go through FFmpeg's subtitle decoder (ASS events for every text codec: SubRip,
  ASS/SSA, mov_text, WebVTT). Their text is made plain here, the one clean-up there is (override blocks and drawing
  events dropped, FFmpeg's escapes undone; a tagless `{…}` block is kept as text except in ASS/SSA tracks, where it is
  a comment), and the events become the app's sentences directly (`sentences_of`: sorted by start, open ends at the
  next later start, ids from 1, millisecond timings), bit for bit what importing them as the app's JSON sentence list
  gave (tested under both import flavors); no file is written any more (the former JSON file in the work folder was
  only the bridge to the importer). While it reads it hands over snapshots of every line so far: the first line at
  once, then at most every 250 ms (20 µs per line when longer: a 50 000-event track gets one a second, since each is
  applied to the whole list on the UI thread). The app keeps only the newest snapshot not yet applied (one post in
  flight; the one it replaces is freed outside the slot's lock), and a read that does not fill the list as it goes
  gets no snapshot callback at all (`extract_text_subtitles`: no list built every 250 ms). Matroska interleaves subtitles with the video by time, so the film's first lines arrive within moments.
- The app side (`sa_viewmodels::media_tracks`, `subtitle/embedded.rs`, `controller.rs`): when a video opens and the
  session is empty and idle, with no subtitle file next to it loaded or still being looked for (the sidecar lookup
  counts itself on the session, through a guard that also counts down if its thread dies, and reports its end as a
  status change; the controller asks again then and from its 100 ms timer), no job, the video not a batch item nor
  waiting in the batch list (a run of other videos does not matter) and "Off" not chosen for it in this run of the
  app (`State::embedded_off`),
  `auto_track` picks the UI language's text track (non-forced first), else the default one. A read into an empty
  list or over an unedited embedded track is progressive: each snapshot goes in through `apply_embedded_snapshot`,
  merged by `merge_sentences_preserving_uid` like the engine's `partial` snapshots (rows keep their keys,
  `sync_keyed` grows the list in place), and only while the session still has the mark the previous snapshot left
  (`media_tracks::SessionMark`); an edit stops the read there and says so, anything else replacing the lines stops it
  silently. The merge matches the leading run of lines that are equal one to one by index and only the rest by the
  matching (the same result: those lines get exactly the uids the matching would give them), so a growing prefix
  costs O(n) per snapshot instead of a scan of the list per new line (50 000 lines in 60 snapshots stay linear,
  `session.rs` `merge_tests`).
- While a progressive read fills the foreground's list, the view model knows it (`set_embedded_read`; the
  controller reports the start, every 2 % of progress and every end, cancel or detach). As long as the list is
  still only what the read delivered (empty, or the track's lines nobody changed), translating (all, missing or
  selected), saving and exporting are unavailable and refuse, and the status shows `Okunuyor… %N` after the
  summary; once the user changes the lines (a typed draft is enough) they are theirs and can be saved at once.
  Generating stays available. The session mark's working part is the session's job or its being in a batch run
  (`embedded::in_batch`: a batch item, or queued in the list of a run in progress), so a job starting on this
  video, or this video's turn in a run, cancels its read silently (never "the subtitles changed"), dropping lines
  nobody touched; a run of other videos leaves it alone. A read over the user's lines applies the complete track at
  the end through the open-subtitle rules (`request_embedded_lines`: macOS asks when edits would be lost or a job
  stopped) unless the lines changed meanwhile.
- Embedded lines carry `Session::embedded` (`EmbeddedLines { stream, pristine, complete }`) and what the read had
  delivered (`Session::embedded_delivered`: every line by start and text, and the lines at its last start with the
  end they were given). `embedded_pristine` (no committed edit, translation or save, and no typed draft that
  differs from its line) decides the interactive rules: loading leaves the session clean, macOS evicts such a
  session like a rebuildable one, the Windows reopen of a video looks for a sidecar again and a subtitle file found
  then replaces them. Automatic writes and batch runs go by `embedded_unchanged` (the same without drafts), so a
  draft never makes the track's own text writable: `Session::may_write_automatically` guards auto-save's
  `save_subtitle(automatic: true)` and stage 3 (`hand_off`, which fails the row with `Videodan okunan altyazı
  değiştirilmeden yazılmaz.` instead of leaving it working; `save_automatically` and `put_track_lines_for_tests`
  are hidden test hooks for these backstops). Before a batch run processes a video (`start_playlist`,
  `stage2_begin`, also for a video added mid-run or whose track was chosen by hand after the run started), a draft
  typed into its foreground lines is committed (the typing is the user's work; the lines are then theirs and are
  translated like any edited lines) and unchanged track lines are dropped, so the video is transcribed. The first
  committed edit, translation, save or export makes them the user's for good (`settle_embedded` after every
  view-model edit). With auto-save on, translating the video's own track writes the translation next to the video:
  the user asked for that translation, so it is their work like any other.
- A track chosen again after its read stopped early (an edit) adds only its rest: the read runs whole, without
  snapshots (`ReadMode::resume`); `append_embedded_rest` takes the lines the stopped read had not delivered
  (`TrackRest`: by start and text, so lines FFmpeg delivered late and that sort earlier are not lost), gives the
  lines still at that read's last start the end the track gives them (unless the user re-timed them), merges the
  rest in by start time while the user's lines keep their order (`merge_by_start`), renumbers and records it as one
  undo step (`Kalan Satırları Ekle`) that also restores the "stopped early" state, so the rest can be added again
  after an undo. With every line deleted, the whole track goes in. Choosing a track commits a draft first; "Off"
  commits it too, then clears only unedited lines, and is remembered per video for the run. It is checked whenever
  no embedded track is in the list (an imported or generated subtitle included: the video's own tracks are off
  then, and choosing it keeps one from being chosen for the user). A read that fails part-way drops a track chosen
  for the user (quietly) and keeps one the user chose (`Videodaki altyazının yalnızca bir kısmı yüklendi: …`); a
  read that fails before its first lines also drops what an earlier, replaced read left half done. When a typed
  draft stopped a read and is then taken back (or an undo leaves the track's lines as read), the read goes on
  (`continue_unfinished_track`), except after a failure of that read. A half-read track is removed when another
  video opens (`abandon_embedded_read`), so it is read again next time. Image tracks (PGS, VobSub, DVB) are listed
  but disabled; more than three collapse into one summary row.
- The read opens the file directly (`MediaSource::File`), not through the Windows `BufferedVideoStream`: it bypasses
  the encrypted SSD chunk cache and reads Matroska through to the end, at background I/O priority so playback keeps
  its share of a slow drive.
- Test hooks (`#[doc(hidden)]`): `use_audio_tap` replaces the device with a virtual real-time output that records
  `(pts, samples)` blocks, and `simulate_audio_decoder_failure` makes opening a given audio stream fail.

Dependencies of `sa-player` (feature `ffmpeg`):

| Crate | Where | Why |
|---|---|---|
| `ffmpeg-next` (workspace features `codec`, `format`, `software-resampling`, `software-scaling`) | all | decode, swscale, swresample; no `filter` feature (deinterlacing runs in the shader, or in `sa_player::soft` for the RGBA path) |
| `cpal` | all | audio output |
| `windows` (`Win32_Foundation`, `Win32_System_Threading`) | Windows | `SetThreadPriority` (incl. `THREAD_MODE_BACKGROUND_BEGIN/END`), `AvSetMmThreadCharacteristicsW`, `GetThreadTimes` / `GetProcessTimes` (`perf.rs`) |
| `windows` (`Win32_Graphics_Direct3D`, `Win32_Graphics_Direct3D11`, `Win32_Graphics_Dxgi`, `Win32_Graphics_Dxgi_Common`) | Windows | the mapped staging ring for D3D11VA frames (`staging.rs`: `CreateTexture2D`, `CopySubresourceRegion`, `Map` / `Unmap`) |
| `libc` | macOS | `pthread_set_qos_class_self_np` (incl. `QOS_CLASS_BACKGROUND`) |

`windows` and `libc` were already workspace dependencies (no new crate, no version change; the Direct3D features are
new features of the same `windows` 0.62.2).
