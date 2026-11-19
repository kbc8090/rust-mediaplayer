//! Frosted glass behind menus, panels and tooltips (like CSS `backdrop-filter`).
//!
//! Windows' own acrylic / mica only blur what is *behind the window* (the desktop),
//! not the video inside it, so the blur is done here on the graphics card: when a
//! glass panel is drawn, the pixels already on screen under it (the video and
//! anything drawn before) are copied at quarter size, blurred, tinted dark, and
//! drawn back with rounded corners. Then egui draws the panel's contents on top.
//!
//! If anything about it can't be set up on a PC (old OpenGL), panels quietly fall
//! back to their solid background.

use eframe::egui::{self, Color32, Rect};
use egui_glow::glow::{self, HasContext};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

/// The menu setting
static ENABLED: AtomicBool = AtomicBool::new(true);
/// Set false if the shaders can't be built on this PC
static GL_OK: AtomicBool = AtomicBool::new(true);
/// Panel edge colour while the glass is on (the window-border accent), as RGBA
static EDGE: AtomicU32 = AtomicU32::new(0x5866_34FF);

/// Dark tint over the blur: (grey level, strength)
const TINT: (f32, f32) = (30.0 / 255.0, 0.70);
/// Captured area is shrunk by this before blurring (cheaper, and a wider blur)
const DOWN: i32 = 4;
/// Extra pixels captured around the panel so the blur has something at its edges
const MARGIN: i32 = 40;

pub fn set_style(enabled: bool, edge: Color32) {
    ENABLED.store(enabled, Ordering::Relaxed);
    EDGE.store(u32::from_be_bytes([edge.r(), edge.g(), edge.b(), 255]), Ordering::Relaxed);
}

/// Glass is in use (on in the menu, and working on this PC)
pub fn active() -> bool {
    ENABLED.load(Ordering::Relaxed) && GL_OK.load(Ordering::Relaxed)
}

/// The panel edge colour: the accent with glass on, `solid` (the usual grey) without
pub fn edge(solid: Color32) -> Color32 {
    if active() {
        let [r, g, b, _] = EDGE.load(Ordering::Relaxed).to_be_bytes();
        Color32::from_rgb(r, g, b)
    } else {
        solid
    }
}

/// Fill a panel's background: frosted glass, or the solid colour when glass is off.
/// `alpha` fades the whole thing (tooltips fade in).
pub fn fill(p: &egui::Painter, rect: Rect, radius: f32, solid: Color32, alpha: f32) {
    if !active() {
        p.rect_filled(rect, radius, solid.gamma_multiply(alpha));
        return;
    }
    p.add(egui::PaintCallback {
        rect,
        callback: Arc::new(egui_glow::CallbackFn::new(move |info, painter| {
            let vp = info.viewport_in_pixels();
            let [sw, sh] = info.screen_size_px;
            let r = radius * info.pixels_per_point;
            let gl = painter.gl();
            GL.with(|g| {
                let mut g = g.borrow_mut();
                if g.is_none() {
                    match unsafe { Gl::new(gl) } {
                        Some(new) => *g = Some(new),
                        None => {
                            GL_OK.store(false, Ordering::Relaxed);
                            return;
                        }
                    }
                }
                if let Some(state) = g.as_mut() {
                    unsafe {
                        state.draw(gl, [vp.left_px, vp.from_bottom_px, vp.width_px, vp.height_px], [sw as i32, sh as i32], r, alpha)
                    };
                }
            });
        })),
    });
}

/// A soft shadow that stays outside the panel (a filled shadow under the panel
/// would get picked up by the glass and darken it)
pub fn shadow(p: &egui::Painter, rect: Rect, radius: f32, alpha: f32) {
    if active() {
        for (e, a) in [(1.0, 40.0), (2.5, 22.0), (4.5, 10.0)] {
            p.rect_stroke(rect.expand(e), radius + e, egui::Stroke::new(2.0_f32, Color32::from_black_alpha((a * alpha) as u8)));
        }
    } else {
        for (e, a) in [(1.0, 40.0), (3.0, 22.0), (6.0, 10.0)] {
            p.rect_filled(
                rect.expand(e).translate(egui::vec2(0.0, 2.0)),
                radius + e,
                Color32::from_black_alpha((a * alpha) as u8),
            );
        }
    }
}

// ── OpenGL side ───────────────────────────────────────────────────────────────

thread_local! {
    static GL: RefCell<Option<Gl>> = const { RefCell::new(None) };
}

struct Target {
    fbo: glow::NativeFramebuffer,
    tex: glow::NativeTexture,
}

struct Gl {
    blur: glow::NativeProgram,
    blur_tex: Option<glow::NativeUniformLocation>,
    blur_step: Option<glow::NativeUniformLocation>,
    comp: glow::NativeProgram,
    comp_tex: Option<glow::NativeUniformLocation>,
    comp_uv: Option<glow::NativeUniformLocation>,
    comp_size: Option<glow::NativeUniformLocation>,
    comp_radius: Option<glow::NativeUniformLocation>,
    comp_tint: Option<glow::NativeUniformLocation>,
    comp_alpha: Option<glow::NativeUniformLocation>,
    vbo: glow::NativeBuffer,
    vao: Option<glow::NativeVertexArray>,
    /// Two work textures (ping-pong) and their current size
    a: Target,
    b: Target,
    size: (i32, i32),
    /// Half-size step of the shrink, and its size
    half: Target,
    half_size: (i32, i32),
}

const VERTEX: &str = r#"
IN vec2 a_pos;
OUT vec2 v_uv;
void main() {
    v_uv = a_pos * 0.5 + 0.5;
    gl_Position = vec4(a_pos, 0.0, 1.0);
}
"#;

// 9-tap Gaussian done in 5 samples (using the hardware's linear filtering)
const BLUR: &str = r#"
uniform sampler2D u_tex;
uniform vec2 u_step;
IN vec2 v_uv;
void main() {
    vec4 c = TEX(u_tex, v_uv) * 0.227027;
    c += TEX(u_tex, v_uv + u_step * 1.384615) * 0.316216;
    c += TEX(u_tex, v_uv - u_step * 1.384615) * 0.316216;
    c += TEX(u_tex, v_uv + u_step * 3.230769) * 0.070270;
    c += TEX(u_tex, v_uv - u_step * 3.230769) * 0.070270;
    FRAG = c;
}
"#;

// Blurred backdrop: a little extra colour, the dark tint, and the rounded-corner
// shape (anti-aliased)
const COMPOSITE: &str = r#"
uniform sampler2D u_tex;
uniform vec4 u_uv;
uniform vec2 u_size;
uniform float u_radius;
uniform vec4 u_tint;
uniform float u_alpha;
IN vec2 v_uv;
void main() {
    vec3 b = TEX(u_tex, u_uv.xy + v_uv * u_uv.zw).rgb;
    float l = dot(b, vec3(0.299, 0.587, 0.114));
    b = clamp(mix(vec3(l), b, 1.5), 0.0, 1.0);
    vec3 c = b * (1.0 - u_tint.a) + u_tint.rgb;
    vec2 p = v_uv * u_size;
    vec2 h = u_size * 0.5;
    vec2 q = abs(p - h) - (h - vec2(u_radius));
    float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - u_radius;
    float a = clamp(0.5 - d, 0.0, 1.0) * u_alpha;
    FRAG = vec4(c * a, a);
}
"#;

unsafe fn compile(gl: &glow::Context, vs: &str, fs: &str) -> Option<glow::NativeProgram> {
    let v = egui_glow::ShaderVersion::get(gl);
    let head = v.version_declaration();
    let new = v.is_new_shader_interface();
    let common = if new {
        "#define TEX texture\n"
    } else {
        "#define TEX texture2D\n"
    };
    let vs_src = format!(
        "{head}{}{common}{vs}",
        if new { "#define IN in\n#define OUT out\n" } else { "#define IN attribute\n#define OUT varying\n" }
    );
    let fs_src = format!(
        "{head}{}{}{common}{fs}",
        if v.is_embedded() { "precision mediump float;\n" } else { "" },
        if new { "#define IN in\nout vec4 frag_out;\n#define FRAG frag_out\n" } else { "#define IN varying\n#define FRAG gl_FragColor\n" }
    );
    let program = gl.create_program().ok()?;
    let mut shaders = Vec::new();
    for (kind, src) in [(glow::VERTEX_SHADER, vs_src), (glow::FRAGMENT_SHADER, fs_src)] {
        let s = gl.create_shader(kind).ok()?;
        gl.shader_source(s, &src);
        gl.compile_shader(s);
        if !gl.get_shader_compile_status(s) {
            gl.delete_shader(s);
            gl.delete_program(program);
            return None;
        }
        gl.attach_shader(program, s);
        shaders.push(s);
    }
    gl.bind_attrib_location(program, 0, "a_pos");
    gl.link_program(program);
    for s in shaders {
        gl.detach_shader(program, s);
        gl.delete_shader(s);
    }
    if !gl.get_program_link_status(program) {
        gl.delete_program(program);
        return None;
    }
    Some(program)
}

unsafe fn target(gl: &glow::Context, w: i32, h: i32) -> Option<Target> {
    let tex = gl.create_texture().ok()?;
    gl.bind_texture(glow::TEXTURE_2D, Some(tex));
    gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, w, h, 0, glow::RGBA, glow::UNSIGNED_BYTE, None);
    for (k, v) in [
        (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
        (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
        (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
        (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
    ] {
        gl.tex_parameter_i32(glow::TEXTURE_2D, k, v as i32);
    }
    let fbo = gl.create_framebuffer().ok()?;
    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
    gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
    let ok = gl.check_framebuffer_status(glow::FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE;
    gl.bind_framebuffer(glow::FRAMEBUFFER, None);
    gl.bind_texture(glow::TEXTURE_2D, None);
    ok.then_some(Target { fbo, tex })
}

impl Gl {
    unsafe fn new(gl: &glow::Context) -> Option<Self> {
        let blur = compile(gl, VERTEX, BLUR)?;
        let comp = compile(gl, VERTEX, COMPOSITE)?;
        let vbo = gl.create_buffer().ok()?;
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        let quad: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
        let bytes: Vec<u8> = quad.iter().flat_map(|f| f.to_ne_bytes()).collect();
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &bytes, glow::STATIC_DRAW);
        gl.bind_buffer(glow::ARRAY_BUFFER, None);
        let vao = gl.create_vertex_array().ok();
        let size = (64, 64);
        let a = target(gl, size.0, size.1)?;
        let b = target(gl, size.0, size.1)?;
        let half = target(gl, size.0, size.1)?;
        Some(Self {
            blur_tex: gl.get_uniform_location(blur, "u_tex"),
            blur_step: gl.get_uniform_location(blur, "u_step"),
            blur,
            comp_tex: gl.get_uniform_location(comp, "u_tex"),
            comp_uv: gl.get_uniform_location(comp, "u_uv"),
            comp_size: gl.get_uniform_location(comp, "u_size"),
            comp_radius: gl.get_uniform_location(comp, "u_radius"),
            comp_tint: gl.get_uniform_location(comp, "u_tint"),
            comp_alpha: gl.get_uniform_location(comp, "u_alpha"),
            comp,
            vbo,
            vao,
            a,
            b,
            size,
            half,
            half_size: size,
        })
    }

    /// Make the work textures exactly w × h (so the blur's edge clamping works at
    /// the edges of the captured area, not of some larger, stale texture)
    unsafe fn ensure_size(&mut self, gl: &glow::Context, w: i32, h: i32, hw: i32, hh: i32) {
        if (hw, hh) != self.half_size {
            gl.bind_texture(glow::TEXTURE_2D, Some(self.half.tex));
            gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, hw, hh, 0, glow::RGBA, glow::UNSIGNED_BYTE, None);
            gl.bind_texture(glow::TEXTURE_2D, None);
            self.half_size = (hw, hh);
        }
        if (w, h) == self.size {
            return;
        }
        let (nw, nh) = (w, h);
        for t in [&self.a, &self.b] {
            gl.bind_texture(glow::TEXTURE_2D, Some(t.tex));
            gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, nw, nh, 0, glow::RGBA, glow::UNSIGNED_BYTE, None);
        }
        gl.bind_texture(glow::TEXTURE_2D, None);
        self.size = (nw, nh);
    }

    unsafe fn quad(&self, gl: &glow::Context) {
        if let Some(vao) = self.vao {
            gl.bind_vertex_array(Some(vao));
        }
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.disable_vertex_attrib_array(0);
        gl.bind_buffer(glow::ARRAY_BUFFER, None);
        if self.vao.is_some() {
            gl.bind_vertex_array(None);
        }
    }

    /// `vp`: the panel in screen pixels (x, y from the bottom, w, h)
    unsafe fn draw(&mut self, gl: &glow::Context, vp: [i32; 4], screen: [i32; 2], radius: f32, alpha: f32) {
        let [x, y, w, h] = vp;
        if w <= 0 || h <= 0 {
            return;
        }
        // What's on screen under the panel, plus a margin, at quarter size
        let cx0 = (x - MARGIN).max(0);
        let cy0 = (y - MARGIN).max(0);
        let cx1 = (x + w + MARGIN).min(screen[0]);
        let cy1 = (y + h + MARGIN).min(screen[1]);
        let (cw, ch) = (cx1 - cx0, cy1 - cy0);
        if cw <= 0 || ch <= 0 {
            return;
        }
        let (lw, lh) = ((cw + DOWN - 1) / DOWN, (ch + DOWN - 1) / DOWN);
        let (hw, hh) = ((cw + 1) / 2, (ch + 1) / 2);
        self.ensure_size(gl, lw, lh, hw, hh);
        let (tw, th) = (self.size.0 as f32, self.size.1 as f32);

        gl.disable(glow::SCISSOR_TEST);
        gl.disable(glow::BLEND);
        // Screen → half size → quarter size: each halving averages 2×2 pixels
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(self.half.fbo));
        gl.blit_framebuffer(cx0, cy0, cx1, cy1, 0, 0, hw, hh, glow::COLOR_BUFFER_BIT, glow::LINEAR);
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.half.fbo));
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(self.a.fbo));
        gl.blit_framebuffer(0, 0, hw, hh, 0, 0, lw, lh, glow::COLOR_BUFFER_BIT, glow::LINEAR);
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);

        // Blur: horizontal + vertical, twice (the second time wider)
        gl.use_program(Some(self.blur));
        gl.active_texture(glow::TEXTURE0);
        gl.uniform_1_i32(self.blur_tex.as_ref(), 0);
        gl.viewport(0, 0, lw, lh);
        for spread in [1.3_f32, 2.5] {
            for (from, to, dx, dy) in [(&self.a, &self.b, 1.0, 0.0), (&self.b, &self.a, 0.0, 1.0)] {
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(to.fbo));
                gl.bind_texture(glow::TEXTURE_2D, Some(from.tex));
                gl.uniform_2_f32(self.blur_step.as_ref(), dx * spread / tw, dy * spread / th);
                self.quad(gl);
            }
        }

        // Back onto the screen over the panel, blended like egui's own drawing
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.viewport(x, y, w, h);
        gl.enable(glow::BLEND);
        gl.blend_equation(glow::FUNC_ADD);
        gl.blend_func_separate(glow::ONE, glow::ONE_MINUS_SRC_ALPHA, glow::ONE_MINUS_DST_ALPHA, glow::ONE);
        gl.use_program(Some(self.comp));
        gl.bind_texture(glow::TEXTURE_2D, Some(self.a.tex));
        gl.uniform_1_i32(self.comp_tex.as_ref(), 0);
        // Where the panel sits inside the captured (and shrunk) texture
        let sx = lw as f32 / tw / cw as f32;
        let sy = lh as f32 / th / ch as f32;
        gl.uniform_4_f32(
            self.comp_uv.as_ref(),
            (x - cx0) as f32 * sx,
            (y - cy0) as f32 * sy,
            w as f32 * sx,
            h as f32 * sy,
        );
        gl.uniform_2_f32(self.comp_size.as_ref(), w as f32, h as f32);
        gl.uniform_1_f32(self.comp_radius.as_ref(), radius);
        let (grey, k) = TINT;
        gl.uniform_4_f32(self.comp_tint.as_ref(), grey * k, grey * k, grey * k, k);
        gl.uniform_1_f32(self.comp_alpha.as_ref(), alpha);
        self.quad(gl);
        gl.bind_texture(glow::TEXTURE_2D, None);
        // (egui puts back its own program, buffers, viewport and scissor after this)
    }
}
