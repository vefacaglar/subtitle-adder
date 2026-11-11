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

Dependencies of `sa-player` (feature `ffmpeg`):

| Crate | Where | Why |
|---|---|---|
| `ffmpeg-next` (workspace features `codec`, `format`, `software-resampling`, `software-scaling`) | all | decode, swscale, swresample; no `filter` feature (deinterlacing runs in the shader) |
| `cpal` | all | audio output |
| `windows` (`Win32_Foundation`, `Win32_System_Threading`) | Windows | `SetThreadPriority`, `AvSetMmThreadCharacteristicsW` |
| `libc` | macOS | `pthread_set_qos_class_self_np` |

`windows` and `libc` were already workspace dependencies (no new crate, no version change).
