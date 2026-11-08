//! GPU video presentation for the femtovg (OpenGL) renderer.
//!
//! The player hands frames over as YUV planes (`sa_player::PlanarImage`: the software decoder's own buffers, or the
//! one download of a hardware frame). Each new frame is uploaded into persistent plane textures and converted to RGB
//! by a small shader into a persistent RGBA texture, which `MainBridge.video-frame` shows as a borrowed OpenGL
//! texture. This happens in the window's `BeforeRendering` notifier, so only the newest frame is uploaded, once, at
//! render time: nothing is copied on the UI thread when a frame arrives, no texture or buffer is allocated per frame,
//! and femtovg never re-creates a texture for the video. A new frame asks winit for one redraw directly (not through
//! Slint's frame throttle, which renders twice per request).
//!
//! Where the notifier is unsupported (the software renderer, tests) or the GL setup fails, [`VideoSurface::available`]
//! stays false and the controller keeps the RGBA path.
//!
//! `SUBTITLE_ADDER_FRAME_TRACE=<file>` (diagnostics) appends one line per render of the window to `<file>`:
//! `R <unix time µs> <pts of the video frame on screen, or -> <1 if that frame is new in this render, else 0>
//! <player clock (s), or ->`, written by a background thread.

use std::cell::{Cell, RefCell};
use std::io::Write;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::mpsc;

use glow::HasContext;
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
        let result = unsafe { gl.draw(&img, w, h) };
        match result {
            Ok(texture) => {
                if self.shown != Some((texture, w, h)) {
                    self.shown = Some((texture, w, h));
                    // SAFETY: the texture lives until the next size change or teardown; the bridge is updated before
                    // either deletes it (deletion is deferred by one frame).
                    let image = unsafe { slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(texture, [w, h].into()).build() };
                    // Setting the property also schedules a render, in case this one already read the old image.
                    (self.set_image)(image);
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
    semi: Option<glow::UniformLocation>,
    wide: Option<glow::UniformLocation>,
    sample_scale: Option<glow::UniformLocation>,
    range: Option<glow::UniformLocation>,
    coeffs: Option<glow::UniformLocation>,
    rot: Option<glow::UniformLocation>,
    size_y: Option<glow::UniformLocation>,
    size_c: Option<glow::UniformLocation>,
}

struct Gl {
    gl: glow::Context,
    program: Program,
    vao: glow::VertexArray,
    planes: [Option<PlaneTex>; 3],
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

// Planes: 8-bit samples are filtered by the GPU (R8 / RG8); 16-bit samples arrive as byte pairs (RG8, or RGBA8 for
// interleaved chroma) and are reassembled and filtered by hand (`texelFetch`), since filtering bytes separately would
// be wrong. Output row 0 is the picture's top row (the texture is shown with a top-left origin).
const FRAGMENT: &str = r#"
in vec2 v_uv;
out vec4 frag;
uniform sampler2D u_y;
uniform sampler2D u_u;
uniform sampler2D u_v;
uniform int u_semi;
uniform int u_wide;
uniform float u_sample_scale;
uniform vec4 u_range;   // y offset, y gain, chroma offset, chroma gain
uniform vec4 u_coeffs;  // r.v, g.u, g.v, b.u
uniform int u_rot;      // clockwise quarter turns from the decoded picture to the display
uniform ivec2 u_size_y;
uniform ivec2 u_size_c;

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

void main() {
    vec2 o = v_uv;
    vec2 s = o;
    if (u_rot == 1) { s = vec2(o.y, 1.0 - o.x); }
    else if (u_rot == 2) { s = vec2(1.0 - o.x, 1.0 - o.y); }
    else if (u_rot == 3) { s = vec2(1.0 - o.y, o.x); }
    float y;
    vec2 c;
    if (u_wide == 0) {
        y = texture(u_y, s).r;
        if (u_semi == 1) { c = texture(u_u, s).rg; }
        else { c = vec2(texture(u_u, s).r, texture(u_v, s).r); }
    } else {
        y = sample_wide(u_y, u_size_y, s).x;
        if (u_semi == 1) {
            c = sample_wide(u_u, u_size_c, s);
        } else {
            c = vec2(sample_wide(u_u, u_size_c, s).x, sample_wide(u_v, u_size_c, s).x);
        }
    }
    y = (y - u_range.x) * u_range.y;
    c = (c - u_range.z) * u_range.w;
    vec3 rgb = vec3(y + u_coeffs.x * c.y, y - u_coeffs.y * c.x - u_coeffs.z * c.y, y + u_coeffs.w * c.x);
    frag = vec4(clamp(rgb, 0.0, 1.0), 1.0);
}
"#;

/// GL state the conversion touches, restored afterwards so femtovg finds what it left.
struct Saved {
    fbo: Option<glow::Framebuffer>,
    viewport: [i32; 4],
    program: Option<glow::Program>,
    vao: Option<glow::VertexArray>,
    active: i32,
    textures: [Option<glow::Texture>; 3],
    unpack_buffer: Option<glow::Buffer>,
    unpack_alignment: i32,
    unpack_row_length: i32,
    enabled: [(u32, bool); 5],
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
            for (kind, src) in [(glow::VERTEX_SHADER, VERTEX), (glow::FRAGMENT_SHADER, FRAGMENT)] {
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
                semi: u("u_semi"),
                wide: u("u_wide"),
                sample_scale: u("u_sample_scale"),
                range: u("u_range"),
                coeffs: u("u_coeffs"),
                rot: u("u_rot"),
                size_y: u("u_size_y"),
                size_c: u("u_size_c"),
            };
            let samplers = [u("u_y"), u("u_u"), u("u_v")];
            let prev = gl.get_parameter_program(glow::CURRENT_PROGRAM);
            gl.use_program(Some(program));
            for (i, s) in samplers.iter().enumerate() {
                gl.uniform_1_i32(s.as_ref(), i as i32);
            }
            gl.use_program(prev);
            let vao = gl.create_vertex_array()?;
            Ok(Self { gl, program: Program { program, loc }, vao, planes: [None, None, None], target: None, retired: Vec::new(), scratch: Vec::new() })
        }
    }

    unsafe fn destroy(self) {
        unsafe {
            let gl = &self.gl;
            gl.delete_program(self.program.program);
            gl.delete_vertex_array(self.vao);
            for p in self.planes.into_iter().flatten() {
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
            let mut textures = [None; 3];
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
            for (cap, on) in s.enabled {
                if on {
                    gl.enable(cap);
                } else {
                    gl.disable(cap);
                }
            }
        }
    }

    /// Uploads `img` and converts it into the output texture (`w` × `h`, as shown); returns that texture.
    unsafe fn draw(&mut self, img: &PlanarImage, w: u32, h: u32) -> Result<NonZeroU32, String> {
        unsafe {
            // Textures replaced last frame: the bridge shows the new one by now.
            for (f, t) in std::mem::take(&mut self.retired) {
                self.gl.delete_framebuffer(f);
                self.gl.delete_texture(t);
            }
            let saved = self.save();
            let r = self.convert(img, w, h);
            self.restore(&saved);
            let err = self.gl.get_error();
            let tex = r?;
            if err != glow::NO_ERROR {
                return Err(format!("GL error 0x{err:x}"));
            }
            Ok(tex)
        }
    }

    unsafe fn convert(&mut self, img: &PlanarImage, w: u32, h: u32) -> Result<NonZeroU32, String> {
        unsafe {
            let wide = img.bits > 8;
            let semi = img.layout == PlaneLayout::SemiPlanar;
            self.gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
            self.gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            let mut sizes = [(0i32, 0i32); 3];
            for i in 0..img.plane_count() {
                let Some((data, info)) = img.plane(i) else { return Err(format!("plane {i} missing")) };
                // Texel = one sample (8-bit) or one byte pair (16-bit); interleaved chroma has two per texel.
                let comps = if semi && i == 1 { 2 } else { 1 };
                let bytes = comps * img.sample_bytes();
                let (internal, format) = match bytes {
                    1 => (glow::R8, glow::RED),
                    2 => (glow::RG8, glow::RG),
                    _ => (glow::RGBA8, glow::RGBA),
                };
                let (pw, ph) = (info.width as i32, info.rows as i32);
                sizes[i] = (pw, ph);
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
                self.gl.active_texture(glow::TEXTURE0 + i as u32);
                let spec = (pw, ph, internal);
                let filter = if wide { glow::NEAREST } else { glow::LINEAR } as i32;
                match &self.planes[i] {
                    Some(p) if p.spec == spec => {
                        self.gl.bind_texture(glow::TEXTURE_2D, Some(p.tex));
                        self.gl.tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, 0, pw, ph, format, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(Some(data)));
                    }
                    _ => {
                        if let Some(p) = self.planes[i].take() {
                            self.gl.delete_texture(p.tex);
                        }
                        let tex = self.gl.create_texture()?;
                        self.gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                        self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, filter);
                        self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, filter);
                        self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
                        self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
                        self.gl.tex_image_2d(glow::TEXTURE_2D, 0, internal as i32, pw, ph, 0, format, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(Some(data)));
                        self.planes[i] = Some(PlaneTex { tex, spec });
                    }
                }
                // A filter change between 8- and 16-bit sources of the same size.
                self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, filter);
                self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, filter);
            }
            if !semi && img.plane_count() < 3 {
                return Err("three planes expected".into());
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
                self.gl.bind_texture(glow::TEXTURE_2D, self.planes[0].as_ref().map(|p| p.tex));
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
            let (range, coeffs, scale) = conversion(img);
            self.gl.uniform_1_i32(p.loc.semi.as_ref(), semi as i32);
            self.gl.uniform_1_i32(p.loc.wide.as_ref(), wide as i32);
            self.gl.uniform_1_f32(p.loc.sample_scale.as_ref(), scale);
            self.gl.uniform_4_f32(p.loc.range.as_ref(), range[0], range[1], range[2], range[3]);
            self.gl.uniform_4_f32(p.loc.coeffs.as_ref(), coeffs[0], coeffs[1], coeffs[2], coeffs[3]);
            self.gl.uniform_1_i32(p.loc.rot.as_ref(), (img.quarter_turns % 4) as i32);
            self.gl.uniform_2_i32(p.loc.size_y.as_ref(), sizes[0].0, sizes[0].1);
            self.gl.uniform_2_i32(p.loc.size_c.as_ref(), sizes[1].0, sizes[1].1);
            if semi {
                // The shader never reads `u_v` for two planes; keep the unit bound to something valid.
                self.gl.active_texture(glow::TEXTURE2);
                self.gl.bind_texture(glow::TEXTURE_2D, self.planes[1].as_ref().map(|p| p.tex));
            }
            self.gl.bind_vertex_array(Some(self.vao));
            self.gl.draw_arrays(glow::TRIANGLES, 0, 3);
            Ok(target.tex.0)
        }
    }
}

/// `(range, coefficients, sample scale)` uniforms: normalized sample = bytes × scale (16-bit) or the texel (8-bit);
/// `(v - offset) × gain` per limited/full range; RGB from the matrix's `(kr, kb)`.
fn conversion(img: &PlanarImage) -> ([f32; 4], [f32; 4], f32) {
    let bits = img.bits.clamp(8, 16) as u32;
    // The value range the normalized sample covers.
    let (max, scale) = if img.bits <= 8 {
        (255.0f32, 1.0)
    } else if img.msb_aligned {
        // The value sits in the high bits: normalized against the 16-bit range, same ratios.
        (65535.0, 1.0 / 65535.0)
    } else {
        let m = ((1u32 << bits) - 1) as f32;
        (m, 1.0 / m)
    };
    let range = if img.full_range {
        [0.0, 1.0, 0.5 * (max + 1.0) / max, 1.0]
    } else {
        // 16..235 / 16..240 at 8 bits, shifted up for deeper samples.
        let unit = (max + 1.0) / 256.0;
        [16.0 * unit / max, max / (219.0 * unit), 128.0 * unit / max, max / (224.0 * unit)]
    };
    let (kr, kb) = img.matrix.coefficients();
    let kg = 1.0 - kr - kb;
    let rv = 2.0 * (1.0 - kr);
    let bu = 2.0 * (1.0 - kb);
    let gu = bu * kb / kg;
    let gv = rv * kr / kg;
    (range, [rv, gu, gv, bu], scale)
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
}
