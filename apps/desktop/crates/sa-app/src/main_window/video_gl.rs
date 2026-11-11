//! GPU video presentation for the femtovg (OpenGL) renderer.
//!
//! The player hands frames over as planes (`sa_player::PlanarImage`: the software decoder's own YUV buffers, the one
//! download of a hardware frame, or swscale's RGBA for pixel formats without a planar description). Each new frame is
//! uploaded into persistent plane textures and converted to RGB by a small shader into a persistent RGBA texture,
//! which `MainBridge.video-frame` shows as a borrowed OpenGL texture. This happens in the window's `BeforeRendering`
//! notifier, so only the newest frame is uploaded, once, at render time: nothing is copied on the UI thread when a
//! frame arrives, no texture or buffer is allocated per frame, and femtovg never re-creates a texture for the video. A
//! new frame asks winit for one redraw directly (not through Slint's frame throttle, which renders twice per request).
//!
//! The shader converts YUV with the frame's matrix and range, then (only for non-SDR-BT.709 content) linearizes,
//! tone-maps HDR (PQ / HLG) to SDR and converts the gamut to BT.709 (`sa_player::color`, whose GLSL it includes).
//! Interlaced frames arrive once per field; the shader shows the field due: its own rows, and for the other field's
//! rows a motion-adaptive choice between weaving them in (where nothing changed since the previous frame, whose
//! planes stay in a second texture set) and the average of the shown rows above and below (where it moved: no comb
//! teeth). The second field of a frame reuses the uploaded planes.
//!
//! Where the notifier is unsupported (the software renderer, tests) or the GL setup fails, [`VideoSurface::available`]
//! stays false and the controller keeps the RGBA path.
//!
//! Diagnostics:
//! * `SUBTITLE_ADDER_FRAME_TRACE=<file>` appends one line per render of the window to `<file>`:
//!   `R <unix time µs> <pts of the video frame on screen, or -> <1 if that frame is new in this render, else 0>
//!   <player clock (s), or ->`, written by a background thread.
//! * `SUBTITLE_ADDER_FRAME_DUMP=<dir>` writes the converted picture (the RGBA target, read back after the shader ran)
//!   as `<dir>/frame-<n>-<pts>[-field2].png` for the first 3 frames after every load or jump, and one frame every
//!   2 s, so the shader's output can be checked without a screen capture. Each dump logs one `[video] dump` line with
//!   the frame's format to stderr.

use std::cell::{Cell, RefCell};
use std::io::Write;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use glow::HasContext;
use sa_player::color::{ColorMode, ColorParams};
use sa_player::{PlanarImage, PlaneLayout, VideoFrame};
use slint::ComponentHandle;

use crate::ui::{MainBridge, MainWindow};

/// What the surface needs from the window it draws into.
pub struct SurfaceHost {
    /// Shows `image` as the video frame (`MainBridge.video-frame`).
    pub set_image: Box<dyn Fn(slint::Image)>,
    /// Renders the window once, soon ([`request_winit_redraw`]).
    pub redraw: Box<dyn Fn()>,
    /// Called after every render of the window (measurements).
    pub after_render: Option<Box<dyn Fn()>>,
}

/// The GPU video path of one window.
pub struct VideoSurface {
    inner: Rc<RefCell<Inner>>,
    available: Rc<Cell<bool>>,
    uploads: Rc<Cell<u64>>,
    renders: Rc<Cell<u64>>,
    redraw: Rc<dyn Fn()>,
    installed: Cell<bool>,
}

struct Inner {
    set_image: Box<dyn Fn(slint::Image)>,
    /// The newest frame not uploaded yet.
    pending: Option<VideoFrame>,
    /// The frame on screen (uploaded again after the GL context was re-created).
    current: Option<VideoFrame>,
    gl: Option<Gl>,
    /// A GL error happened: the RGBA path from now on.
    failed: bool,
    /// The borrowed texture `video-frame` shows (`None`: the bridge shows something else).
    shown: Option<(NonZeroU32, u32, u32)>,
    trace: Option<FrameTrace>,
    dump: Option<FrameDump>,
}

/// `SUBTITLE_ADDER_FRAME_TRACE`: render lines for a writer thread (see the module docs).
struct FrameTrace {
    lines: mpsc::Sender<String>,
    /// The player clock, when the controller provided it.
    clock: Option<Box<dyn Fn() -> Option<f64>>>,
}

impl FrameTrace {
    fn from_env() -> Option<Self> {
        let path = std::env::var_os("SUBTITLE_ADDER_FRAME_TRACE").filter(|p| !p.is_empty())?;
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok()?;
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::Builder::new()
            .name("video-trace".into())
            .spawn(move || {
                while let Ok(first) = rx.recv() {
                    let mut batch = first;
                    while let Ok(more) = rx.try_recv() {
                        batch.push_str(&more);
                    }
                    if file.write_all(batch.as_bytes()).is_err() {
                        return;
                    }
                }
            })
            .ok()?;
        Some(Self { lines: tx, clock: None })
    }

    fn record(&self, pts: Option<f64>, new: bool) {
        let us = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_micros());
        let pts = pts.map_or_else(|| "-".to_owned(), |p| format!("{p:.6}"));
        let clock = self.clock.as_ref().and_then(|c| c()).map_or_else(|| "-".to_owned(), |c| format!("{c:.6}"));
        let _ = self.lines.send(format!("R {us} {pts} {} {clock}\n", u8::from(new)));
    }
}

/// A converted picture read back for `SUBTITLE_ADDER_FRAME_DUMP`.
struct DumpImage {
    name: String,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// Which frames [`FrameDump`] writes: the first [`DumpPolicy::BURST`] after every load or jump, then one every
/// [`DumpPolicy::INTERVAL`].
#[derive(Debug, Default)]
struct DumpPolicy {
    since_reset: u32,
    last_pts: Option<f64>,
    last_dump: Option<Instant>,
}

impl DumpPolicy {
    const BURST: u32 = 3;
    const INTERVAL: Duration = Duration::from_secs(2);
    /// A pts step larger than this (either way) counts as a jump: a new source or a seek.
    const JUMP: f64 = 1.0;

    fn reset(&mut self) {
        *self = Self::default();
    }

    fn wants(&mut self, pts: f64, now: Instant) -> bool {
        if self.last_pts.is_some_and(|p| (pts - p).abs() > Self::JUMP) {
            self.since_reset = 0;
        }
        self.last_pts = Some(pts);
        let burst = self.since_reset < Self::BURST;
        self.since_reset = self.since_reset.saturating_add(1);
        let due = burst || self.last_dump.is_none_or(|t| now.duration_since(t) >= Self::INTERVAL);
        if due {
            self.last_dump = Some(now);
        }
        due
    }
}

/// `SUBTITLE_ADDER_FRAME_DUMP`: PNG writer thread and the dump policy (see the module docs).
struct FrameDump {
    images: mpsc::Sender<DumpImage>,
    policy: DumpPolicy,
    count: u32,
}

impl FrameDump {
    fn from_env() -> Option<Self> {
        let dir = std::path::PathBuf::from(std::env::var_os("SUBTITLE_ADDER_FRAME_DUMP").filter(|p| !p.is_empty())?);
        std::fs::create_dir_all(&dir).ok()?;
        let (tx, rx) = mpsc::channel::<DumpImage>();
        std::thread::Builder::new()
            .name("video-dump".into())
            .spawn(move || {
                while let Ok(img) = rx.recv() {
                    let path = dir.join(&img.name);
                    if let Err(e) = image::save_buffer(&path, &img.rgba, img.width, img.height, image::ExtendedColorType::Rgba8) {
                        eprintln!("[video] dump {}: {e}", path.display());
                    }
                }
            })
            .ok()?;
        Some(Self { images: tx, policy: DumpPolicy::default(), count: 0 })
    }
}

impl VideoSurface {
    /// The surface of a main window (installed before the window is shown).
    pub fn for_main_window(window: &MainWindow) -> Rc<Self> {
        let image_target = window.as_weak();
        let redraw_target = window.as_weak();
        Self::install(
            window.window(),
            SurfaceHost {
                set_image: Box::new(move |image| {
                    if let Some(w) = image_target.upgrade() {
                        w.global::<MainBridge>().set_video_frame(image);
                    }
                }),
                redraw: Box::new(move || {
                    if let Some(w) = redraw_target.upgrade() {
                        request_winit_redraw(w.window());
                    }
                }),
                after_render: None,
            },
        )
    }

    /// Installs the rendering notifier on `window` (before it is shown). Without GPU rendering the surface stays
    /// unavailable.
    pub fn install(window: &slint::Window, host: SurfaceHost) -> Rc<Self> {
        let SurfaceHost { set_image, redraw, after_render } = host;
        let surface = Rc::new(Self {
            inner: Rc::new(RefCell::new(Inner {
                set_image,
                pending: None,
                current: None,
                gl: None,
                failed: false,
                shown: None,
                trace: FrameTrace::from_env(),
                dump: FrameDump::from_env(),
            })),
            available: Rc::new(Cell::new(false)),
            uploads: Rc::new(Cell::new(0)),
            renders: Rc::new(Cell::new(0)),
            redraw: Rc::from(redraw),
            installed: Cell::new(false),
        });
        let inner = surface.inner.clone();
        let available = surface.available.clone();
        let uploads = surface.uploads.clone();
        let renders = surface.renders.clone();
        let installed = window.set_rendering_notifier(move |state, api| {
            let Ok(mut s) = inner.try_borrow_mut() else { return };
            match state {
                slint::RenderingState::RenderingSetup => {
                    if let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = api {
                        // SAFETY: the renderer's context is current during the notifier.
                        let gl = unsafe { glow::Context::from_loader_function_cstr(|name| get_proc_address(name)) };
                        match unsafe { Gl::new(gl) } {
                            Ok(g) => {
                                s.gl = Some(g);
                                // A frame shown before the context was (re-)created is uploaded again.
                                if s.pending.is_none() {
                                    s.pending = s.current.take();
                                }
                                s.shown = None;
                            }
                            Err(e) => {
                                eprintln!("[video] GPU video path unavailable: {e}");
                                s.failed = true;
                            }
                        }
                    }
                    available.set(s.gl.is_some() && !s.failed);
                }
                slint::RenderingState::BeforeRendering => {
                    renders.set(renders.get() + 1);
                    let mut new = false;
                    if s.gl.is_some() && !s.failed {
                        if s.render() {
                            uploads.set(uploads.get() + 1);
                            new = true;
                        }
                        if s.failed {
                            available.set(false);
                        }
                    }
                    if let Some(t) = &s.trace {
                        t.record(s.shown.and(s.current.as_ref()).map(|f| f.pts), new);
                    }
                }
                slint::RenderingState::AfterRendering => {
                    drop(s);
                    if let Some(f) = &after_render {
                        f();
                    }
                }
                slint::RenderingState::RenderingTeardown => {
                    if let Some(g) = s.gl.take() {
                        unsafe { g.destroy() };
                    }
                    s.shown = None;
                    available.set(false);
                }
                _ => {}
            }
        });
        match installed {
            Ok(()) => surface.installed.set(true),
            // The software renderer has no notifier. Another one installed first was replaced (femtovg keeps the
            // last), so this one is in place; whoever installed it is a wiring bug.
            Err(slint::SetRenderingNotifierError::AlreadySet) => {
                eprintln!("[video] replaced an existing rendering notifier");
                surface.installed.set(true);
            }
            Err(_) => {}
        }
        surface
    }

    /// Whether the window's renderer took the notifier (a GPU renderer; the software renderer has none). Nothing
    /// else may set a rendering notifier on this window afterwards: it would replace this one.
    pub fn installed(&self) -> bool {
        self.installed.get()
    }

    /// Whether planar frames can be shown (the GL context is up and working).
    pub fn available(&self) -> bool {
        self.available.get()
    }

    /// Frames uploaded and converted so far, i.e. frames that reached the screen (diagnostics).
    pub fn uploads(&self) -> u64 {
        self.uploads.get()
    }

    /// Renders of the window so far (diagnostics).
    pub fn renders(&self) -> u64 {
        self.renders.get()
    }

    /// The pts of the frame on screen (diagnostics).
    pub fn shown_pts(&self) -> Option<f64> {
        self.inner.try_borrow().ok()?.current.as_ref().map(|f| f.pts)
    }

    /// The player clock for `SUBTITLE_ADDER_FRAME_TRACE` lines (no effect without the variable).
    pub fn set_trace_clock(&self, clock: Box<dyn Fn() -> Option<f64>>) {
        if let Ok(mut s) = self.inner.try_borrow_mut()
            && let Some(t) = &mut s.trace
        {
            t.clock = Some(clock);
        }
    }

    /// Shows a planar frame with the next render (replacing a frame not rendered yet).
    pub fn submit(&self, frame: VideoFrame) {
        {
            let Ok(mut s) = self.inner.try_borrow_mut() else { return };
            s.pending = Some(frame);
        }
        (self.redraw)();
    }

    /// Forgets the frames (a new source, or the bridge shows an RGBA frame now).
    pub fn clear(&self) {
        if let Ok(mut s) = self.inner.try_borrow_mut() {
            s.pending = None;
            s.current = None;
            s.shown = None;
            if let Some(d) = &mut s.dump {
                d.policy.reset();
            }
        }
    }
}

/// One render without going through Slint's frame throttle (`TimerBasedFrameThrottle` renders twice per request and
/// paces at `1000 / refresh` ms); winit coalesces the request into the next paint.
pub fn request_winit_redraw(window: &slint::Window) {
    use slint::winit_030::WinitWindowAccessor;
    if window.with_winit_window(|ww| ww.request_redraw()).is_none() {
        window.request_redraw();
    }
}

impl Inner {
    /// Uploads and converts the pending frame; true when one reached the output texture.
    fn render(&mut self) -> bool {
        let Some(frame) = self.pending.take() else { return false };
        let Some(img) = frame.planar.clone() else { return false };
        let Some(gl) = self.gl.as_mut() else { return false };
        let (w, h) = (frame.width.max(1), frame.height.max(1));
        let dump = self.dump.as_mut().is_some_and(|d| d.policy.wants(frame.pts, Instant::now()));
        let result = unsafe { gl.draw(&img, frame.second_field, w, h, dump) };
        match result {
            Ok((texture, pixels)) => {
                if self.shown != Some((texture, w, h)) {
                    self.shown = Some((texture, w, h));
                    // SAFETY: the texture lives until the next size change or teardown; the bridge is updated before
                    // either deletes it (deletion is deferred by one frame).
                    let image = unsafe { slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(texture, [w, h].into()).build() };
                    // Setting the property also schedules a render, in case this one already read the old image.
                    (self.set_image)(image);
                }
                if let (Some(d), Some(rgba)) = (self.dump.as_mut(), pixels) {
                    d.count += 1;
                    let field = if frame.second_field { "-field2" } else { "" };
                    let name = format!("frame-{:04}-{:09.3}{field}.png", d.count, frame.pts);
                    eprintln!("[video] dump {name}: {}", describe(&img, w, h));
                    let _ = d.images.send(DumpImage { name, width: w, height: h, rgba });
                }
                self.current = Some(frame);
                true
            }
            Err(e) => {
                eprintln!("[video] GPU video path failed: {e}");
                self.failed = true;
                self.shown = None;
                (self.set_image)(slint::Image::default());
                false
            }
        }
    }
}

/// One line about a frame for the dump log.
fn describe(img: &PlanarImage, w: u32, h: u32) -> String {
    format!(
        "{w}x{h} from {}x{} {:?} {}-bit{} chroma>>{:?}{} {:?} {} {:?} {:?} peak={} {}",
        img.coded_width,
        img.coded_height,
        img.layout,
        img.bits,
        if img.msb_aligned { " (msb)" } else { "" },
        img.chroma_shift,
        if img.swap_uv { " vu" } else { "" },
        img.matrix,
        if img.full_range { "full" } else { "limited" },
        img.transfer,
        img.primaries,
        img.peak_nits,
        if img.interlaced { if img.top_field_first { "interlaced tff" } else { "interlaced bff" } } else { "progressive" },
    )
}

struct PlaneTex {
    tex: glow::Texture,
    /// (width, height, internal format) of its storage.
    spec: (i32, i32, u32),
}

struct Target {
    fbo: glow::Framebuffer,
    tex: glow::Texture,
    size: (u32, u32),
}

struct Program {
    program: glow::Program,
    loc: Uniforms,
}

struct Uniforms {
    layout: Option<glow::UniformLocation>,
    swap: Option<glow::UniformLocation>,
    wide: Option<glow::UniformLocation>,
    sample_scale: Option<glow::UniformLocation>,
    range: Option<glow::UniformLocation>,
    coeffs: Option<glow::UniformLocation>,
    rot: Option<glow::UniformLocation>,
    size_y: Option<glow::UniformLocation>,
    size_c: Option<glow::UniformLocation>,
    deint: Option<glow::UniformLocation>,
    field: Option<glow::UniformLocation>,
    chroma_fields: Option<glow::UniformLocation>,
    temporal: Option<glow::UniformLocation>,
    color: Option<glow::UniformLocation>,
    gamut: Option<glow::UniformLocation>,
    tone: Option<glow::UniformLocation>,
    luma: Option<glow::UniformLocation>,
}

struct Gl {
    gl: glow::Context,
    program: Program,
    vao: glow::VertexArray,
    /// Two sets of plane textures: interlaced frames alternate between them, so the previous frame stays available
    /// for motion detection; progressive frames keep using one.
    planes: [[Option<PlaneTex>; 3]; 2],
    /// The set the current image is in.
    cur: usize,
    /// The image `planes[cur]` holds (its second field is drawn without uploading again).
    uploaded: Option<Arc<PlanarImage>>,
    /// The interlaced image the other set holds (the frame before `uploaded`).
    previous: Option<Arc<PlanarImage>>,
    target: Option<Target>,
    /// Output textures replaced by a size change; deleted with the next frame, once the bridge shows the new one.
    retired: Vec<(glow::Framebuffer, glow::Texture)>,
    scratch: Vec<u8>,
}

const VERTEX: &str = r#"
out vec2 v_uv;
void main() {
    // One triangle covering the viewport: (0,0), (2,0), (0,2).
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    v_uv = p;
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

// Planes: 8-bit samples are filtered by the GPU (R8 / RG8 / RGBA8); 16-bit samples arrive as byte pairs (RG8, or
// RGBA8 for interleaved chroma) and are reassembled and filtered by hand (`texelFetch`), since filtering bytes
// separately would be wrong. Output row 0 is the picture's top row (the texture is shown with a top-left origin).
// `color_map` and its uniforms come from `sa_player::color::glsl()`, inserted before `main`.
const FRAGMENT: &str = r#"
in vec2 v_uv;
out vec4 frag;
uniform sampler2D u_y;
uniform sampler2D u_u;
uniform sampler2D u_v;
// The previous frame's planes (interlaced sources: motion detection).
uniform sampler2D u_py;
uniform sampler2D u_pu;
uniform sampler2D u_pv;
uniform int u_layout;   // 0 Y + U + V, 1 Y + interleaved UV, 2 Y only, 3 packed R'G'B'A
uniform int u_swap;     // interleaved chroma is V, U
uniform int u_wide;
uniform float u_sample_scale;
uniform vec4 u_range;   // y offset, y gain, chroma offset, chroma gain
uniform vec4 u_coeffs;  // r.v, g.u, g.v, b.u
uniform int u_rot;      // clockwise quarter turns from the decoded picture to the display
uniform ivec2 u_size_y;
uniform ivec2 u_size_c;
uniform int u_deint;    // an interlaced frame: show field u_field (0 = top: even rows, 1 = bottom: odd rows)
uniform int u_field;
uniform int u_chroma_fields; // interlaced 4:2:0: chroma rows alternate between the fields as well
uniform int u_temporal; // the previous frame's planes are bound (same format and size)

vec2 word(vec4 t) {
    return vec2(dot(t.rg, vec2(255.0, 65280.0)), dot(t.ba, vec2(255.0, 65280.0))) * u_sample_scale;
}

vec2 fetch_wide(sampler2D t, ivec2 size, ivec2 p) {
    return word(texelFetch(t, clamp(p, ivec2(0), size - 1), 0));
}

vec2 sample_wide(sampler2D t, ivec2 size, vec2 uv) {
    vec2 p = uv * vec2(size) - 0.5;
    ivec2 i = ivec2(floor(p));
    vec2 f = p - floor(p);
    vec2 a = fetch_wide(t, size, i);
    vec2 b = fetch_wide(t, size, i + ivec2(1, 0));
    vec2 c = fetch_wide(t, size, i + ivec2(0, 1));
    vec2 d = fetch_wide(t, size, i + ivec2(1, 1));
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

// Raw samples (Y, U, V) of the planes `ty`, `tu`, `tv` with luma at `sy` and chroma at `sc` (normalized source
// coordinates), or R'G'B' (packed).
vec3 fetch_from(sampler2D ty, sampler2D tu, sampler2D tv, vec2 sy, vec2 sc) {
    if (u_layout == 3) {
        return texture(ty, sy).rgb;
    }
    float y;
    vec2 c;
    if (u_wide == 0) {
        y = texture(ty, sy).r;
        if (u_layout == 1) { c = texture(tu, sc).rg; }
        else if (u_layout == 0) { c = vec2(texture(tu, sc).r, texture(tv, sc).r); }
        else { c = vec2(u_range.z); }
    } else {
        y = sample_wide(ty, u_size_y, sy).x;
        if (u_layout == 1) {
            c = sample_wide(tu, u_size_c, sc);
        } else if (u_layout == 0) {
            c = vec2(sample_wide(tu, u_size_c, sc).x, sample_wide(tv, u_size_c, sc).x);
        } else {
            c = vec2(u_range.z);
        }
    }
    if (u_swap == 1) { c = c.yx; }
    return vec3(y, c);
}

vec3 fetch(vec2 sy, vec2 sc) {
    return fetch_from(u_y, u_u, u_v, sy, sc);
}

// Luma row `row` (texel index) at `x`, of the current frame or the previous one; interlaced 4:2:0 chroma comes from
// the nearest chroma row of the same field.
vec3 fetch_row(float x, float row, bool previous) {
    vec2 sy = vec2(x, (row + 0.5) / float(u_size_y.y));
    vec2 sc = sy;
    if (u_chroma_fields == 1) {
        float i = floor(row * 0.5);
        float p = row - 2.0 * i;
        float crow = min(2.0 * floor(i * 0.5) + p, float(u_size_c.y - 1));
        sc = vec2(x, (crow + 0.5) / float(u_size_c.y));
    }
    return previous ? fetch_from(u_py, u_pu, u_pv, sy, sc) : fetch_from(u_y, u_u, u_v, sy, sc);
}

float largest(vec3 v) {
    return max(v.x, max(v.y, v.z));
}

// Motion-adaptive deinterlacing: the shown field's rows as they are; a row of the other field (captured at another
// time) is woven in where the picture did not change since the previous frame (full vertical detail), and replaced by
// the average of the shown rows above and below it where it did (no comb teeth). Without a previous frame: always
// the average (bob).
vec3 deinterlaced(vec2 s) {
    float h = float(u_size_y.y);
    float row = clamp(floor(s.y * h), 0.0, h - 1.0);
    vec3 own = fetch_row(s.x, row, false);
    if (mod(row, 2.0) == float(u_field)) {
        return own;
    }
    float up = row - 1.0;
    float dn = row + 1.0;
    if (up < 0.0) { up = dn; }
    if (dn > h - 1.0) { dn = up; }
    vec3 a = fetch_row(s.x, up, false);
    vec3 b = fetch_row(s.x, dn, false);
    vec3 interp = 0.5 * (a + b);
    if (u_temporal == 0) {
        return interp;
    }
    // Change of this row (the other field) and of the rows around it (the shown field) over one frame.
    float m = largest(abs(own - fetch_row(s.x, row, true)));
    m = max(m, 0.5 * (largest(abs(a - fetch_row(s.x, up, true))) + largest(abs(b - fetch_row(s.x, dn, true)))));
    return mix(own, interp, smoothstep(0.012, 0.04, m));
}

vec3 to_rgb(vec3 v) {
    if (u_layout == 3) {
        return v;
    }
    float y = (v.x - u_range.x) * u_range.y;
    vec2 c = (v.yz - u_range.z) * u_range.w;
    return vec3(y + u_coeffs.x * c.y, y - u_coeffs.y * c.x - u_coeffs.z * c.y, y + u_coeffs.w * c.x);
}

void main() {
    vec2 o = v_uv;
    vec2 s = o;
    if (u_rot == 1) { s = vec2(o.y, 1.0 - o.x); }
    else if (u_rot == 2) { s = vec2(1.0 - o.x, 1.0 - o.y); }
    else if (u_rot == 3) { s = vec2(1.0 - o.y, o.x); }
    vec3 rgb = to_rgb(u_deint == 1 ? deinterlaced(s) : fetch(s, s));
    if (u_color != 0) {
        rgb = color_map(rgb);
    }
    frag = vec4(clamp(rgb, 0.0, 1.0), 1.0);
}
"#;

/// The fragment shader with the colour functions inserted after the declarations of `FRAGMENT`.
fn fragment_source() -> String {
    let color = sa_player::color::glsl();
    match FRAGMENT.find("vec2 word(") {
        Some(i) => format!("{}{color}\n{}", &FRAGMENT[..i], &FRAGMENT[i..]),
        None => format!("{color}{FRAGMENT}"),
    }
}

/// GL state the conversion touches, restored afterwards so femtovg finds what it left.
struct Saved {
    fbo: Option<glow::Framebuffer>,
    viewport: [i32; 4],
    program: Option<glow::Program>,
    vao: Option<glow::VertexArray>,
    active: i32,
    textures: [Option<glow::Texture>; UNITS],
    unpack_buffer: Option<glow::Buffer>,
    unpack_alignment: i32,
    unpack_row_length: i32,
    pack_buffer: Option<glow::Buffer>,
    pack_alignment: i32,
    enabled: [(u32, bool); 5],
}

/// Texture units the conversion uses: the current frame's planes (0..3) and the previous frame's (3..6).
const UNITS: usize = 6;

/// The shader's layout code for `layout`.
fn layout_code(layout: PlaneLayout) -> i32 {
    match layout {
        PlaneLayout::Planar => 0,
        PlaneLayout::SemiPlanar => 1,
        PlaneLayout::Gray => 2,
        PlaneLayout::Packed => 3,
    }
}

impl Gl {
    unsafe fn new(gl: glow::Context) -> Result<Self, String> {
        unsafe {
            let version = gl.version();
            let header = if version.is_embedded {
                "#version 300 es\nprecision highp float;\nprecision highp int;\nprecision highp sampler2D;\n"
            } else {
                "#version 150\n"
            };
            if !version.is_embedded && (version.major, version.minor) < (3, 2) {
                return Err(format!("OpenGL {}.{} (3.2 needed)", version.major, version.minor));
            }
            if version.is_embedded && version.major < 3 {
                return Err(format!("OpenGL ES {}.{} (3.0 needed)", version.major, version.minor));
            }
            let program = gl.create_program()?;
            let mut shaders = Vec::new();
            let fragment = fragment_source();
            for (kind, src) in [(glow::VERTEX_SHADER, VERTEX), (glow::FRAGMENT_SHADER, fragment.as_str())] {
                let sh = gl.create_shader(kind)?;
                gl.shader_source(sh, &format!("{header}{src}"));
                gl.compile_shader(sh);
                if !gl.get_shader_compile_status(sh) {
                    let log = gl.get_shader_info_log(sh);
                    gl.delete_shader(sh);
                    gl.delete_program(program);
                    return Err(format!("shader: {log}"));
                }
                gl.attach_shader(program, sh);
                shaders.push(sh);
            }
            gl.link_program(program);
            for sh in shaders {
                gl.detach_shader(program, sh);
                gl.delete_shader(sh);
            }
            if !gl.get_program_link_status(program) {
                let log = gl.get_program_info_log(program);
                gl.delete_program(program);
                return Err(format!("link: {log}"));
            }
            let u = |name: &str| gl.get_uniform_location(program, name);
            let loc = Uniforms {
                layout: u("u_layout"),
                swap: u("u_swap"),
                wide: u("u_wide"),
                sample_scale: u("u_sample_scale"),
                range: u("u_range"),
                coeffs: u("u_coeffs"),
                rot: u("u_rot"),
                size_y: u("u_size_y"),
                size_c: u("u_size_c"),
                deint: u("u_deint"),
                field: u("u_field"),
                chroma_fields: u("u_chroma_fields"),
                temporal: u("u_temporal"),
                color: u("u_color"),
                gamut: u("u_gamut"),
                tone: u("u_tone"),
                luma: u("u_luma"),
            };
            let samplers = [u("u_y"), u("u_u"), u("u_v"), u("u_py"), u("u_pu"), u("u_pv")];
            let prev = gl.get_parameter_program(glow::CURRENT_PROGRAM);
            gl.use_program(Some(program));
            for (i, s) in samplers.iter().enumerate() {
                gl.uniform_1_i32(s.as_ref(), i as i32);
            }
            gl.use_program(prev);
            let vao = gl.create_vertex_array()?;
            Ok(Self {
                gl,
                program: Program { program, loc },
                vao,
                planes: Default::default(),
                cur: 0,
                uploaded: None,
                previous: None,
                target: None,
                retired: Vec::new(),
                scratch: Vec::new(),
            })
        }
    }

    unsafe fn destroy(self) {
        unsafe {
            let gl = &self.gl;
            gl.delete_program(self.program.program);
            gl.delete_vertex_array(self.vao);
            for p in self.planes.into_iter().flatten().flatten() {
                gl.delete_texture(p.tex);
            }
            if let Some(t) = self.target {
                gl.delete_framebuffer(t.fbo);
                gl.delete_texture(t.tex);
            }
            for (f, t) in self.retired {
                gl.delete_framebuffer(f);
                gl.delete_texture(t);
            }
        }
    }

    unsafe fn save(&self) -> Saved {
        unsafe {
            let gl = &self.gl;
            let mut viewport = [0; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            let active = gl.get_parameter_i32(glow::ACTIVE_TEXTURE);
            let mut textures = [None; UNITS];
            for (i, t) in textures.iter_mut().enumerate() {
                gl.active_texture(glow::TEXTURE0 + i as u32);
                *t = gl.get_parameter_texture(glow::TEXTURE_BINDING_2D);
            }
            gl.active_texture(active as u32);
            let caps = [glow::SCISSOR_TEST, glow::BLEND, glow::DEPTH_TEST, glow::STENCIL_TEST, glow::CULL_FACE];
            Saved {
                fbo: gl.get_parameter_framebuffer(glow::FRAMEBUFFER_BINDING),
                viewport,
                program: gl.get_parameter_program(glow::CURRENT_PROGRAM),
                vao: gl.get_parameter_vertex_array(glow::VERTEX_ARRAY_BINDING),
                active,
                textures,
                unpack_buffer: gl.get_parameter_buffer(glow::PIXEL_UNPACK_BUFFER_BINDING),
                unpack_alignment: gl.get_parameter_i32(glow::UNPACK_ALIGNMENT),
                unpack_row_length: gl.get_parameter_i32(glow::UNPACK_ROW_LENGTH),
                pack_buffer: gl.get_parameter_buffer(glow::PIXEL_PACK_BUFFER_BINDING),
                pack_alignment: gl.get_parameter_i32(glow::PACK_ALIGNMENT),
                enabled: caps.map(|c| (c, gl.is_enabled(c))),
            }
        }
    }

    unsafe fn restore(&self, s: &Saved) {
        unsafe {
            let gl = &self.gl;
            gl.bind_framebuffer(glow::FRAMEBUFFER, s.fbo);
            gl.viewport(s.viewport[0], s.viewport[1], s.viewport[2], s.viewport[3]);
            gl.use_program(s.program);
            gl.bind_vertex_array(s.vao);
            for (i, t) in s.textures.iter().enumerate() {
                gl.active_texture(glow::TEXTURE0 + i as u32);
                gl.bind_texture(glow::TEXTURE_2D, *t);
            }
            gl.active_texture(s.active as u32);
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, s.unpack_buffer);
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, s.unpack_alignment);
            gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, s.unpack_row_length);
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, s.pack_buffer);
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, s.pack_alignment);
            for (cap, on) in s.enabled {
                if on {
                    gl.enable(cap);
                } else {
                    gl.disable(cap);
                }
            }
        }
    }

    /// Uploads `img` (unless it is the image the planes hold already: the second field of an interlaced frame) and
    /// converts it into the output texture (`w` × `h`, as shown); returns that texture, and the converted pixels
    /// (RGBA, top row first) when `read_back`.
    unsafe fn draw(&mut self, img: &Arc<PlanarImage>, second_field: bool, w: u32, h: u32, read_back: bool) -> Result<(NonZeroU32, Option<Vec<u8>>), String> {
        unsafe {
            // Textures replaced last frame: the bridge shows the new one by now.
            for (f, t) in std::mem::take(&mut self.retired) {
                self.gl.delete_framebuffer(f);
                self.gl.delete_texture(t);
            }
            let saved = self.save();
            let r = self.convert(img, second_field, w, h, read_back);
            self.restore(&saved);
            let err = self.gl.get_error();
            let r = r?;
            if err != glow::NO_ERROR {
                return Err(format!("GL error 0x{err:x}"));
            }
            Ok(r)
        }
    }

    unsafe fn convert(&mut self, img: &Arc<PlanarImage>, second_field: bool, w: u32, h: u32, read_back: bool) -> Result<(NonZeroU32, Option<Vec<u8>>), String> {
        unsafe {
            let wide = img.bits > 8;
            let format = img.format();
            let count = img.plane_count();
            let upload = !self.uploaded.as_ref().is_some_and(|u| Arc::ptr_eq(u, img));
            if upload {
                // An interlaced frame goes into the other set, so the one before it stays for motion detection.
                let prev = self.uploaded.take();
                if img.interlaced && prev.as_ref().is_some_and(|p| p.interlaced) {
                    self.cur ^= 1;
                    self.previous = prev;
                } else {
                    self.previous = None;
                }
            }
            // Forgotten until the upload succeeded (a failure leaves the planes undefined).
            self.uploaded = None;
            let set = self.cur;
            self.gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
            self.gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            let mut sizes = [(0i32, 0i32); 3];
            for i in 0..count {
                let Some((data, info)) = img.plane(i) else { return Err(format!("plane {i} missing")) };
                // Texel = one sample (8-bit) or one byte pair (16-bit); interleaved chroma has two per texel, packed
                // RGBA four.
                let bytes = format.components(i) * img.sample_bytes();
                let (internal, gl_format) = match bytes {
                    1 => (glow::R8, glow::RED),
                    2 => (glow::RG8, glow::RG),
                    4 => (glow::RGBA8, glow::RGBA),
                    n => return Err(format!("{n} bytes per texel")),
                };
                let (pw, ph) = (info.width as i32, info.rows as i32);
                sizes[i] = (pw, ph);
                self.gl.active_texture(glow::TEXTURE0 + i as u32);
                let spec = (pw, ph, internal);
                let filter = if wide { glow::NEAREST } else { glow::LINEAR } as i32;
                if !upload && self.planes[set][i].as_ref().is_some_and(|p| p.spec == spec) {
                    self.gl.bind_texture(glow::TEXTURE_2D, self.planes[set][i].as_ref().map(|p| p.tex));
                    continue;
                }
                let data: &[u8] = if info.stride % bytes == 0 {
                    self.gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, (info.stride / bytes) as i32);
                    data
                } else {
                    // Rows not a whole number of texels apart (unusual): pack them.
                    self.gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
                    self.scratch.clear();
                    for r in 0..info.rows as usize {
                        self.scratch.extend_from_slice(&data[r * info.stride..r * info.stride + info.row_bytes]);
                    }
                    &self.scratch
                };
                match &self.planes[set][i] {
                    Some(p) if p.spec == spec => {
                        self.gl.bind_texture(glow::TEXTURE_2D, Some(p.tex));
                        self.gl.tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, pw, ph, gl_format, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(Some(data)));
                    }
                    _ => {
                        if let Some(p) = self.planes[set][i].take() {
                            self.gl.delete_texture(p.tex);
                        }
                        let tex = self.gl.create_texture()?;
                        self.gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                        self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
                        self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
                        self.gl.tex_image_2d(glow::TEXTURE_2D, 0, internal as i32, pw, ph, 0, gl_format, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(Some(data)));
                        self.planes[set][i] = Some(PlaneTex { tex, spec });
                    }
                }
                // Also a filter change between 8- and 16-bit sources of the same size.
                self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, filter);
                self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, filter);
            }
            // The previous frame's planes are usable when they hold the same format at the same size.
            let other = set ^ 1;
            let temporal = img.interlaced
                && self.previous.as_ref().is_some_and(|p| p.format() == format && (p.coded_width, p.coded_height) == (img.coded_width, img.coded_height))
                && (0..count).all(|i| matches!((&self.planes[other][i], &self.planes[set][i]), (Some(a), Some(b)) if a.spec == b.spec));
            // Units the shader does not read stay bound to a valid texture.
            let fallback = self.planes[set][0].as_ref().map(|p| p.tex);
            for i in count..3 {
                self.gl.active_texture(glow::TEXTURE0 + i as u32);
                self.gl.bind_texture(glow::TEXTURE_2D, self.planes[set][i].as_ref().map(|p| p.tex).or(fallback));
            }
            for i in 0..3 {
                let tex = if temporal && i < count { self.planes[other][i].as_ref().map(|p| p.tex) } else { None };
                self.gl.active_texture(glow::TEXTURE3 + i as u32);
                self.gl.bind_texture(glow::TEXTURE_2D, tex.or(fallback));
            }            if count == 1 {
                sizes[1] = sizes[0];
            }

            // Output texture + framebuffer at the display size.
            if self.target.as_ref().map(|t| t.size) != Some((w, h)) {
                if let Some(t) = self.target.take() {
                    self.retired.push((t.fbo, t.tex));
                }
                let tex = self.gl.create_texture()?;
                self.gl.active_texture(glow::TEXTURE0);
                self.gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
                self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
                self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
                self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
                self.gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, w as i32, h as i32, 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
                // Unit 0 holds the luma plane again for the draw.
                self.gl.bind_texture(glow::TEXTURE_2D, self.planes[set][0].as_ref().map(|p| p.tex));
                let fbo = self.gl.create_framebuffer()?;
                self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                self.gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
                let status = self.gl.check_framebuffer_status(glow::FRAMEBUFFER);
                self.target = Some(Target { fbo, tex, size: (w, h) });
                if status != glow::FRAMEBUFFER_COMPLETE {
                    return Err(format!("framebuffer incomplete 0x{status:x}"));
                }
            }
            let target = self.target.as_ref().expect("created above");
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(target.fbo));
            self.gl.viewport(0, 0, w as i32, h as i32);
            for cap in [glow::SCISSOR_TEST, glow::BLEND, glow::DEPTH_TEST, glow::STENCIL_TEST, glow::CULL_FACE] {
                self.gl.disable(cap);
            }
            let p = &self.program;
            self.gl.use_program(Some(p.program));
            let (range, coeffs, scale) = sa_player::color::yuv_uniforms(img);
            let color = ColorParams::for_image(img);
            let chroma_fields = img.interlaced && img.chroma_shift.1 == 1 && matches!(img.layout, PlaneLayout::Planar | PlaneLayout::SemiPlanar);
            self.gl.uniform_1_i32(p.loc.layout.as_ref(), layout_code(img.layout));
            self.gl.uniform_1_i32(p.loc.swap.as_ref(), img.swap_uv as i32);
            self.gl.uniform_1_i32(p.loc.wide.as_ref(), wide as i32);
            self.gl.uniform_1_f32(p.loc.sample_scale.as_ref(), scale);
            self.gl.uniform_4_f32(p.loc.range.as_ref(), range[0], range[1], range[2], range[3]);
            self.gl.uniform_4_f32(p.loc.coeffs.as_ref(), coeffs[0], coeffs[1], coeffs[2], coeffs[3]);
            self.gl.uniform_1_i32(p.loc.rot.as_ref(), (img.quarter_turns % 4) as i32);
            self.gl.uniform_2_i32(p.loc.size_y.as_ref(), sizes[0].0, sizes[0].1);
            self.gl.uniform_2_i32(p.loc.size_c.as_ref(), sizes[1].0, sizes[1].1);
            self.gl.uniform_1_i32(p.loc.deint.as_ref(), img.interlaced as i32);
            self.gl.uniform_1_i32(p.loc.field.as_ref(), img.shown_field_is_bottom(second_field) as i32);
            self.gl.uniform_1_i32(p.loc.chroma_fields.as_ref(), chroma_fields as i32);
            self.gl.uniform_1_i32(p.loc.temporal.as_ref(), temporal as i32);
            self.gl.uniform_1_i32(p.loc.color.as_ref(), color.mode as i32);
            if color.mode != ColorMode::None {
                self.gl.uniform_matrix_3_f32_slice(p.loc.gamut.as_ref(), false, &color.gamut_column_major());
                self.gl.uniform_4_f32(p.loc.tone.as_ref(), color.tone[0], color.tone[1], color.tone[2], color.tone[3]);
                self.gl.uniform_3_f32(p.loc.luma.as_ref(), color.luma[0], color.luma[1], color.luma[2]);
            }
            self.gl.bind_vertex_array(Some(self.vao));
            self.gl.draw_arrays(glow::TRIANGLES, 0, 3);
            let pixels = if read_back {
                let mut buf = vec![0u8; w as usize * h as usize * 4];
                self.gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
                self.gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
                self.gl.read_pixels(0, 0, w as i32, h as i32, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(Some(&mut buf)));
                Some(buf)
            } else {
                None
            };
            self.uploaded = Some(img.clone());
            Ok((target.tex.0, pixels))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bt709_limited_range_coefficients_match_the_standard() {
        let (kr, kb) = sa_player::YuvMatrix::Bt709.coefficients();
        let kg = 1.0 - kr - kb;
        let rv = 2.0 * (1.0 - kr);
        let bu = 2.0 * (1.0 - kb);
        assert!((rv - 1.5748).abs() < 1e-4);
        assert!((bu - 1.8556).abs() < 1e-4);
        assert!((bu * kb / kg - 0.18732).abs() < 1e-4);
        assert!((rv * kr / kg - 0.46812).abs() < 1e-4);
    }

    #[test]
    fn the_colour_functions_are_inserted_before_the_shader_uses_them() {
        let src = fragment_source();
        let color = src.find("vec3 color_map(").expect("color_map");
        assert!(color < src.find("void main()").unwrap());
        assert!(src.find("uniform int u_deint;").unwrap() < color);
        assert_eq!(src.matches("void main()").count(), 1);
    }

    #[test]
    fn dumps_take_a_burst_after_every_jump_then_one_every_two_seconds() {
        let mut p = DumpPolicy::default();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let wanted: Vec<bool> = (0..10).map(|i| p.wants(i as f64 * 0.04, at(i * 40))).collect();
        assert_eq!(wanted, [true, true, true, false, false, false, false, false, false, false]);
        assert!(!p.wants(0.4, at(1900)));
        assert!(p.wants(0.44, at(2100)), "two seconds after the last dump");
        assert!(!p.wants(0.48, at(2140)));
        // A seek (pts jump) starts a new burst.
        assert!(p.wants(30.0, at(2180)) && p.wants(30.04, at(2220)) && p.wants(30.08, at(2260)));
        assert!(!p.wants(30.12, at(2300)));
        p.reset();
        assert!(p.wants(30.16, at(2340)), "a new load starts a new burst");
    }

    #[test]
    fn every_layout_has_a_shader_code() {
        let codes = [PlaneLayout::Planar, PlaneLayout::SemiPlanar, PlaneLayout::Gray, PlaneLayout::Packed].map(layout_code);
        assert_eq!(codes, [0, 1, 2, 3]);
    }
}
