//! Seek-bar thumbnails.
//!
//! A second mpv, never shown and never playing, opens the same file as the player.
//! While the pointer is over the seek bar it jumps to the time under the pointer
//! (to the nearest keyframe, which is quick), draws that frame small into an
//! off-screen texture, and the texture is copied into the window above the seek bar.
//!
//! The thumbnail mpv is started the first time the seek bar is hovered. From then on,
//! each new file is also opened in it a moment after it starts playing (`warm`), so
//! the first hover over its seek bar only has to seek, not open the file. (The short
//! wait keeps it from competing with the main player while that one is starting.)
//! Until thumbnails have been used once, nothing is opened. Like the main mpv it is
//! only ever talked to through its own background thread (mpv_link.rs).

use crate::mpv_link::MpvLink;
use eframe::egui;
use egui_glow::glow::{self, HasContext};
use libmpv2::render::{mpv_render_update, OpenGLInitParams, RenderContext, RenderParam, RenderParamApiType};
use libmpv2::Mpv;
use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The thumbnail's width in points (16:9 video: 200 × 112); the height follows
/// the video's shape, up to THUMB_MAX_H (tall video is narrower instead)
pub const THUMB_W: f32 = 200.0;
pub const THUMB_MAX_H: f32 = 150.0;

/// How long a new file plays before the thumbnail player opens it as well
const WARM_DELAY: Duration = Duration::from_millis(1200);

/// GL objects; they live on the window's (drawing) thread
struct Gl {
    rc: RenderContext<'static>,
    target: Option<Target>,
}

/// Off-screen texture the thumbnail is drawn into
struct Target {
    fbo: glow::NativeFramebuffer,
    tex: glow::NativeTexture,
    w: i32,
    h: i32,
}

thread_local! {
    static GL: RefCell<Option<Gl>> = const { RefCell::new(None) };
}

pub struct Thumbs {
    /// Started on first use (`load`); None until then, or if it couldn't start
    mpv: Option<(Arc<Mutex<Mpv>>, MpvLink)>,
    gl_ready: bool,
    gl_failed: bool,
    /// The current file, not yet opened in the thumbnail player (that happens on
    /// the first hover; mpv must have its render context first, or it turns the
    /// video off)
    pending: Option<String>,
    /// The seek bar has been hovered: start the thumbnail player
    wanted: bool,
    /// The opened file has finished loading and been sent the latest time
    synced: bool,
    /// The time last asked for
    requested: Option<f64>,
    /// When the pending file was handed over (it's opened ahead of time once it has
    /// been playing for WARM_DELAY)
    pending_since: Option<Instant>,
    /// For a link's preview stream: the user agent and other HTTP headers the site
    /// wants (None / empty for files)
    user_agent: Option<String>,
    headers: Vec<String>,
}

impl Thumbs {
    pub fn new() -> Self {
        Self {
            mpv: None,
            gl_ready: false,
            gl_failed: false,
            pending: None,
            wanted: false,
            synced: false,
            requested: None,
            pending_since: None,
            user_agent: None,
            headers: Vec::new(),
        }
    }

    /// A new file is playing: remember it for the thumbnail player (opened on the
    /// first hover), and let go of the previous one
    pub fn load(&mut self, path: &str) {
        self.user_agent = None;
        self.headers.clear();
        self.pending = Some(path.to_owned());
        self.pending_since = Some(Instant::now());
        self.requested = None;
        if let Some((_, link)) = &self.mpv {
            link.command("stop", &[]);
        }
    }

    /// Start the thumbnail player and its drawing side once there is a file for it.
    /// Call from `update`, where the window's GL context is current.
    pub fn init_gl(&mut self, ctx: &egui::Context, get_proc_address: fn(&*mut c_void, &str) -> *mut c_void) {
        if self.gl_ready || self.gl_failed || !self.wanted {
            return;
        }
        if self.mpv.is_none() {
            match start_mpv() {
                Some(m) => {
                    let m = Arc::new(Mutex::new(m));
                    let link = MpvLink::start(m.clone());
                    self.mpv = Some((m, link));
                }
                None => {
                    self.gl_failed = true;
                    return;
                }
            }
        }
        let Some((mpv, _)) = &self.mpv else { return };
        let params = vec![
            RenderParam::ApiType(RenderParamApiType::OpenGl),
            RenderParam::InitParams(OpenGLInitParams { get_proc_address, ctx: std::ptr::null_mut::<c_void>() }),
        ];
        // The Mpv lives (in its Arc) until the process ends, like the main player's
        let mpv_ref = unsafe { std::mem::transmute::<&Mpv, &'static Mpv>(&*mpv.lock().unwrap()) };
        match mpv_ref.create_render_context(params) {
            Ok(mut rc) => {
                // A new thumbnail frame is ready: draw again
                let ctx = ctx.clone();
                rc.set_update_callback(move || ctx.request_repaint());
                GL.with(|g| *g.borrow_mut() = Some(Gl { rc, target: None }));
                self.gl_ready = true;
            }
            Err(_) => self.gl_failed = true,
        }
    }

    /// A link is playing: its thumbnails come from a small separate stream of the
    /// same video (240p or so, video only), found by yt-dlp along with the main
    /// one. Opened like a file; a seek only fetches a little of it.
    pub fn load_stream(&mut self, url: &str, user_agent: Option<String>, headers: Vec<String>) {
        self.load(url);
        self.user_agent = user_agent;
        self.headers = headers;
    }

    /// A link is playing whose preview stream isn't known yet (or there isn't
    /// one): let go of the previous file and don't open anything
    pub fn forget(&mut self) {
        self.pending = None;
        self.pending_since = None;
        self.requested = None;
        if let Some((_, link)) = &self.mpv {
            link.command("stop", &[]);
        }
    }

    /// Call every frame with the playing position. Once thumbnails have been used and
    /// the current file has been playing for a moment, open it in the thumbnail
    /// player too (at that position), so the first hover only needs a seek.
    pub fn warm(&mut self, ctx: &egui::Context, secs: f64) {
        if !self.gl_ready || self.pending.is_none() {
            return;
        }
        let Some(since) = self.pending_since else { return };
        let wait = WARM_DELAY.saturating_sub(since.elapsed());
        if wait.is_zero() {
            self.open_pending(secs);
        } else {
            ctx.request_repaint_after(wait);
        }
    }

    /// Open the waiting file in the thumbnail player, right at `secs`
    fn open_pending(&mut self, secs: f64) {
        let Some((_, link)) = &self.mpv else { return };
        let Some(path) = self.pending.take() else { return };
        self.pending_since = None;
        // (headers go one at a time: a user agent has commas in it)
        link.command("change-list", &["http-header-fields", "clr", ""]);
        for h in &self.headers {
            link.command("change-list", &["http-header-fields", "append", h]);
        }
        link.set("user-agent", self.user_agent.as_deref().unwrap_or("libmpv"));
        link.set("start", format!("{secs:.2}"));
        link.command("loadfile", &[&path]);
        self.requested = Some(secs);
        self.synced = false;
    }

    /// Turn / mirror the thumbnails like the main picture
    pub fn set_view(&self, rotation: i32, mirror: bool) {
        if let Some((_, link)) = &self.mpv {
            link.set("video-rotate", rotation);
            if mirror {
                link.command("vf", &["set", "lavfi=[hflip]"]);
            } else {
                link.command("vf", &["clr", ""]);
            }
        }
    }

    /// Width / height of the current video, once the thumbnail player has it
    /// (None for audio-only files, or while still opening)
    pub fn aspect(&self) -> Option<f32> {
        if !self.gl_ready {
            return None;
        }
        let s = self.mpv.as_ref()?.1.snapshot();
        (s.dwidth > 0 && s.dheight > 0).then(|| s.dwidth as f32 / s.dheight as f32)
    }

    /// Ask for the frame at `secs` (the pointer is over the seek bar). Cheap to call
    /// every frame: mpv itself merges seeks that arrive faster than it can carry
    /// them out.
    pub fn request(&mut self, ctx: &egui::Context, secs: f64) {
        if !self.gl_ready {
            // First hover: the player starts in the next frame (see init_gl)
            if !self.gl_failed && !self.wanted {
                self.wanted = true;
                ctx.request_repaint();
            }
            return;
        }
        if self.pending.is_some() {
            // Not opened ahead of time yet: open it right at the hovered time
            self.open_pending(secs);
            return;
        }
        let Some((_, link)) = &self.mpv else { return };
        let at = format!("{secs:.2}");
        // Once it has loaded, catch up with wherever the pointer went meanwhile
        let resend = !self.synced && {
            let s = link.snapshot();
            s.duration > 0.0 && s.dwidth > 0
        };
        if resend {
            self.synced = true;
        }
        if resend || self.requested.is_none_or(|r| (r - secs).abs() > 0.2) {
            self.requested = Some(secs);
            link.command("seek", &[&at, "absolute+keyframes"]);
        }
    }

    /// Draw the latest thumbnail into `rect`, with corners rounded by `radius` points
    pub fn paint(&self, painter: &egui::Painter, rect: egui::Rect, radius: f32) {
        painter.add(egui::PaintCallback {
            rect,
            callback: Arc::new(egui_glow::CallbackFn::new(move |info, painter| {
                let vp = info.viewport_in_pixels();
                let r_px = (radius * info.pixels_per_point).round() as i32;
                let gl = painter.gl();
                GL.with(|g| {
                    if let Some(g) = g.borrow_mut().as_mut() {
                        unsafe { draw(gl, g, vp.left_px, vp.from_bottom_px, vp.width_px, vp.height_px, r_px) };
                    }
                });
            })),
        });
    }
}

unsafe fn draw(gl: &glow::Context, g: &mut Gl, x: i32, y: i32, w: i32, h: i32, radius: i32) {
    if w <= 0 || h <= 0 {
        return;
    }
    // (Re)create the off-screen texture at exactly the on-screen pixel size
    let mut fresh = false;
    if g.target.as_ref().is_none_or(|t| t.w != w || t.h != h) {
        if let Some(old) = g.target.take() {
            gl.delete_framebuffer(old.fbo);
            gl.delete_texture(old.tex);
        }
        let (Ok(tex), Ok(fbo)) = (gl.create_texture(), gl.create_framebuffer()) else { return };
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, w, h, 0, glow::RGBA, glow::UNSIGNED_BYTE, None);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        gl.bind_texture(glow::TEXTURE_2D, None);
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        g.target = Some(Target { fbo, tex, w, h });
        fresh = true;
    }
    let Some(t) = g.target.as_ref() else { return };

    // egui clips with the scissor test; it mustn't cut into mpv's drawing in the
    // texture (egui switches it back on after this callback)
    gl.disable(glow::SCISSOR_TEST);
    let new_frame = g.rc.update().map_or(true, |f| f & mpv_render_update::Frame != 0);
    if fresh || new_frame {
        let _ = g.rc.render::<*mut c_void>(t.fbo.0.get() as i32, w, h, true);
    }
    // Copy the texture into the window. A copy can only be rectangular, so the
    // rounded corners are made by copying the top and bottom few rows a little
    // narrower (the border egui draws around it smooths the edge).
    gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(t.fbo));
    gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
    let copy = |y0: i32, y1: i32, inset: i32| {
        gl.blit_framebuffer(inset, y0, w - inset, y1, x + inset, y + y0, x + w - inset, y + y1, glow::COLOR_BUFFER_BIT, glow::NEAREST);
    };
    let r = radius.clamp(0, h / 2);
    for row in 0..r {
        let d = r as f32 - row as f32 - 0.5;
        let inset = (r as f32 - (r as f32 * r as f32 - d * d).max(0.0).sqrt()).round() as i32;
        copy(row, row + 1, inset); // bottom rows
        copy(h - row - 1, h - row, inset); // top rows
    }
    copy(r, h - r, 0);
    gl.bind_framebuffer(glow::FRAMEBUFFER, None);
}

fn start_mpv() -> Option<Mpv> {
    Mpv::with_initializer(|init| {
        for (k, v) in [
            ("vo", "libmpv"),
            // Picture only, never playing
            ("aid", "no"),
            ("sid", "no"),
            ("pause", "yes"),
            ("keep-open", "yes"),
            ("audio-display", "no"),
            // Fast over pretty: nearest-keyframe seeks, quick decoding, cheap scaling
            ("hr-seek", "no"),
            ("hwdec", "no"),
            ("vd-lavc-threads", "2"),
            ("vd-lavc-fast", "yes"),
            ("vd-lavc-skiploopfilter", "all"),
            ("scale", "bilinear"),
            ("dscale", "bilinear"),
            ("cscale", "bilinear"),
            ("dither-depth", "no"),
            ("correct-downscaling", "no"),
            ("linear-downscaling", "no"),
            ("sigmoid-upscaling", "no"),
            ("deband", "no"),
            ("video-timing-offset", "0"),
            // Little memory: it only ever needs the packets around one keyframe
            ("cache", "no"),
            ("demuxer-max-bytes", "4MiB"),
            ("demuxer-max-back-bytes", "1MiB"),
            ("osc", "no"),
            ("load-scripts", "no"),
            ("ytdl", "no"),
        ] {
            let _ = init.set_property(k, v);
        }
        Ok(())
    })
    .ok()
}
