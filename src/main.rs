#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontFamily, FontId, Pos2, Rect, Sense, Stroke};
use libmpv2::{Mpv, render::{mpv_render_update, RenderContext, OpenGLInitParams, RenderParam, RenderParamApiType}};
use egui_glow::glow::{self, HasContext};
mod adjust;
mod glass;
mod mpv_link;
mod splash;
mod taskbar;
mod thumbs;
mod settings;
mod files;
mod controls;
mod menus;
mod online;
mod visualizer;
use settings::*;
use files::*;
use controls::*;
use menus::*;
use online::*;

use mpv_link::{yn, InfoRow, MpvLink, VideoKind};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::sync::{Arc, Mutex};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

unsafe extern "system" {
    fn wglGetProcAddress(lpszProc: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
    fn GetModuleHandleA(lpModuleName: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
    fn GetProcAddress(hModule: *mut std::ffi::c_void, lpProcName: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegGetValueW(
        hkey: isize,
        sub_key: *const u16,
        value: *const u16,
        flags: u32,
        value_type: *mut u32,
        data: *mut std::ffi::c_void,
        data_len: *mut u32,
    ) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> isize;
    fn TerminateProcess(process: isize, exit_code: u32) -> i32;
    fn K32GetProcessMemoryInfo(process: isize, counters: *mut MemCounters, size: u32) -> i32;
    fn GetProcessTimes(process: isize, created: *mut u64, exited: *mut u64, kernel: *mut u64, user: *mut u64) -> i32;
    fn MultiByteToWideChar(code_page: u32, flags: u32, src: *const u8, src_len: i32, dst: *mut u16, dst_len: i32) -> i32;
    fn LoadLibraryW(name: *const u16) -> isize;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetSystemMetrics(index: i32) -> i32;
    fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
    fn IsZoomed(hwnd: isize) -> i32;
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
}

thread_local! {
    /// How long mpv's last render calls took (smoothed), shown in the info overlay
    static RENDER_MS: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
    static TLS_RENDER_CTX: RefCell<Option<RenderContext<'static>>> = const { RefCell::new(None) };
    /// The last video frame mpv drew, kept so it can be shown again without asking
    /// mpv to redraw it (see `paint_video`)
    static VIDEO_TARGET: RefCell<Option<VideoTarget>> = const { RefCell::new(None) };
}

/// Off-screen texture the video is drawn into. Allocated a little larger than the
/// window, so it isn't remade on every step of a resize.
struct VideoTarget {
    fbo: glow::NativeFramebuffer,
    tex: glow::NativeTexture,
    w: i32,
    h: i32,
}

/// Show the video in the window's (x, y, w, h) pixels.
///
/// mpv only draws a new picture when it has one: a new frame while playing, or a
/// redraw it asks for (a picture setting changed while paused, say). Every other
/// repaint of the window (the pointer moving over a paused video, menus, the
/// controls fading) copies the last picture, which costs the GPU next to nothing.
/// Before, every one of those repaints ran mpv's whole scaling pipeline again,
/// which with a fast mouse meant hundreds of full redraws a second.
unsafe fn paint_video(gl: &glow::Context, rc: &mut RenderContext<'static>, x: i32, y: i32, w: i32, h: i32) -> bool {
    if w <= 0 || h <= 0 {
        return false;
    }
    // Acknowledge mpv's update (its docs ask for this after every update callback);
    // it says whether mpv has a new picture for us
    let new_frame = rc.update().map_or(true, |f| f & mpv_render_update::Frame != 0);
    VIDEO_TARGET.with(|t| {
        let mut t = t.borrow_mut();
        let mut fresh = false;
        let fits = t.as_ref().is_some_and(|v| v.w >= w && v.h >= h && v.w <= w + 512 && v.h <= h + 512);
        if !fits {
            if let Some(old) = t.take() {
                gl.delete_framebuffer(old.fbo);
                gl.delete_texture(old.tex);
            }
            let (tw, th) = ((w + 255) / 256 * 256, (h + 255) / 256 * 256);
            if let (Ok(tex), Ok(fbo)) = (gl.create_texture(), gl.create_framebuffer()) {
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, tw, th, 0, glow::RGBA, glow::UNSIGNED_BYTE, None);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::NEAREST as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::NEAREST as i32);
                gl.bind_texture(glow::TEXTURE_2D, None);
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
                let ok = gl.check_framebuffer_status(glow::FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE;
                gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                if ok {
                    *t = Some(VideoTarget { fbo, tex, w: tw, h: th });
                    fresh = true;
                } else {
                    gl.delete_framebuffer(fbo);
                    gl.delete_texture(tex);
                }
            }
        }
        // egui clips with the scissor test; it mustn't cut into mpv's drawing
        // (egui switches it back on after this callback)
        gl.disable(glow::SCISSOR_TEST);
        let Some(v) = t.as_ref() else {
            // No off-screen texture on this PC: draw straight into the window, as before
            let _ = rc.render::<*mut std::os::raw::c_void>(0, w, h, true);
            return true;
        };
        // The size is part of the picture too: a resize always redraws
        let size_changed = VIDEO_SIZE.with(|s| s.replace((w, h)) != (w, h));
        let drew = fresh || new_frame || size_changed;
        if drew {
            let _ = rc.render::<*mut std::os::raw::c_void>(v.fbo.0.get() as i32, w, h, true);
        }
        if new_frame {
            VIDEO_STALE.with(|s| s.set(false));
        }
        if VIDEO_STALE.with(|s| s.get()) {
            // (the previous file's picture: leave the window black)
            return drew;
        }
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(v.fbo));
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
        gl.blit_framebuffer(0, 0, w, h, x, y, x + w, y + h, glow::COLOR_BUFFER_BIT, glow::NEAREST);
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        drew
    })
}

/// Draws mpv's picture into `rect`
fn video_callback(rect: Rect) -> egui::PaintCallback {
    egui::PaintCallback {
        rect,
        callback: Arc::new(egui_glow::CallbackFn::new(move |info, painter| {
            // Render at the real pixel size so HiDPI scaling stays correct
            let vp = info.viewport_in_pixels();
            let started = std::time::Instant::now();
            let drew = TLS_RENDER_CTX.with(|c| match c.borrow_mut().as_mut() {
                Some(rc) => unsafe { paint_video(painter.gl(), rc, vp.left_px, vp.from_bottom_px, vp.width_px, vp.height_px) },
                None => false,
            });
            // (timings only count real mpv draws, not copies of the last picture)
            if drew {
                let ms = started.elapsed().as_secs_f32() * 1000.0;
                RENDER_MS.with(|r| r.set(r.get() * 0.8 + ms * 0.2));
            }
        })),
    }
}

thread_local! {
    /// The size the cached video picture was drawn at
    static VIDEO_SIZE: std::cell::Cell<(i32, i32)> = const { std::cell::Cell::new((0, 0)) };
    /// A new file was opened and mpv hasn't drawn its first picture yet: the kept
    /// picture is the previous file's (its last frame, or a song's cover), so
    /// nothing is shown until mpv has a new one. (mpv is still asked to draw
    /// every frame meanwhile: if it isn't, it waits up to 0.2 s for each one,
    /// which made new videos start a beat late.)
    static VIDEO_STALE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// End the process immediately, without running any DLL or runtime shutdown code.
/// `std::process::exit` crashed while a video was open: it ran libmpv's DLL shutdown
/// while mpv's decoder/audio threads and the GL context were still alive.
fn terminate_now() -> ! {
    unsafe { TerminateProcess(GetCurrentProcess(), 0); }
    std::process::exit(0)
}

// ── Memory readout for the info overlay ──────────────────────────────────────
#[repr(C)]
#[derive(Default)]
struct MemCounters {
    cb: u32,
    page_fault_count: u32,
    peak_working_set: usize,
    working_set: usize,
    quota_peak_paged_pool: usize,
    quota_paged_pool: usize,
    quota_peak_non_paged_pool: usize,
    quota_non_paged_pool: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_usage: usize,
}

/// (working set, private bytes) of this process in MB
fn process_memory_mb() -> Option<(f64, f64)> {
    let mut c = MemCounters { cb: std::mem::size_of::<MemCounters>() as u32, ..Default::default() };
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) } != 0;
    ok.then(|| (c.working_set as f64 / 1048576.0, c.private_usage as f64 / 1048576.0))
}

/// This process's CPU time so far (kernel + user), in 100 ns units
fn process_cpu_time() -> Option<u64> {
    let (mut c, mut e, mut k, mut u) = (0u64, 0u64, 0u64, 0u64);
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) } != 0;
    ok.then_some(k + u)
}

// ── Palette ───────────────────────────────────────────────────────────────────
const KNOB_RING:  Color32 = Color32::from_rgb(186, 186, 188); // muted light ring around the knob
const TRACK:      Color32 = Color32::from_rgb(190, 184, 180); // unplayed seek track
const VOL_TRACK:  Color32 = Color32::from_rgb(160, 158, 155); // unplayed volume track

// ── Layout (points) ───────────────────────────────────────────────────────────
const TOP_H: f32 = 64.0;              // title strip + gradient height
const BOTTOM_H: f32 = 104.0;          // control area + gradient height
const SIDE: f32 = 20.0;               // left/right margin of the seek bar (and the button row)
const SEEK_FROM_BOTTOM: f32 = 52.0;   // seek bar centre line
const ROW_FROM_BOTTOM: f32 = 27.0;    // button row centre line
/// Control-row buttons: hover highlight size and centre-to-centre spacing
const BTN_BOX: f32 = 28.0;
const BTN_STEP: f32 = 32.0;
/// Volume slider track length
const VOL_W: f32 = 90.0;
/// Space between the volume button and the start of the slider
const VOL_GAP: f32 = 7.0;
const WIN_BTN_W: f32 = 46.0;
const WIN_BTN_H: f32 = 34.0;
/// Resize strip along the top edge over the window buttons, like Windows' own
/// windows. Thin, and none beside Close, so a quick move to the top-right corner
/// still lands on Close (it used to start a resize there instead). The buttons
/// work everywhere else. (Not when maximised: then the buttons reach the edge.)
const BTN_EDGE: f32 = 3.0;
/// What the Deinterlace switch sets: mpv's "auto" deinterlaces only frames the file
/// marks as interlaced. "yes" ran a CPU deinterlacer on every frame of every video,
/// progressive ones included, doubling the frame count. (mpv doesn't make "auto" its
/// own default because a few files mark progressive frames as interlaced; with the
/// switch on, those get deinterlaced as before.)
const DEINTERLACE_ON: &str = "auto";

/// Playback speeds in the speed menu
const SPEEDS: [(f64, &str); 7] = [
    (0.25, "0.25\u{d7}"),
    (0.5, "0.5\u{d7}"),
    (0.75, "0.75\u{d7}"),
    (1.0, "Normal"),
    (1.25, "1.25\u{d7}"),
    (1.5, "1.5\u{d7}"),
    (2.0, "2\u{d7}"),
];
/// Menu button (top left): centre. Its highlight sits 8 px from the left, 7 px from the top.
const MENU_X: f32 = 8.0 + BTN_BOX / 2.0;
const MENU_Y: f32 = 7.0 + BTN_BOX / 2.0;
/// Where the filename starts (right of the menu button)
const TITLE_X: f32 = MENU_X + BTN_BOX / 2.0 + 8.0;

/// The pop-up in the top left after seeking with the arrow keys (the amount,
/// adding up quick repeats). Seeking with the mouse doesn't show it: the seek bar's
/// time preview and thumbnail already show where you are.
#[derive(Clone, Copy)]
enum SeekOsd {
    Step(f64),
}
/// Controls hide this long after the mouse stops (while playing), in seconds
const HIDE_AFTER: f32 = 2.5;
/// Time for a fullscreen switch to settle, during which the overlays are held
const FS_SETTLE: f32 = 0.45;
/// Volume / seek pop-up position (top left, below the menu button and lined up with
/// the filename)
const OSD_LEFT: f32 = TITLE_X;
const OSD_TOP: f32 = MENU_Y + BTN_BOX / 2.0 + 11.0;
/// Picture quality. Since mpv 0.37 its defaults are already its old "high quality"
/// settings (good downscaling, dithering, gamma-correct scaling); the remaining
/// difference in its "high-quality" profile is the upscaler used when the video
/// is smaller than the window: a sharper, cleaner one that costs more GPU time.
const HQ_SCALE: &str = "ewa_lanczossharp";
const NORMAL_SCALE: &str = "lanczos";
/// Decoding methods H cycles through: mpv's hwdec value, and its name on screen.
/// - Hardware: mpv picks the best "copy" method for the GPU (decoded on the GPU,
///   each frame copied back before drawing). The "direct" D3D11 / DXVA2 methods
///   can't work in this app: libmpv only hands Windows GPU-decoded frames straight
///   to OpenGL through ANGLE, which eframe doesn't use.
/// - NVIDIA direct: NVIDIA's own decoder can hand frames to OpenGL directly (on
///   other GPUs mpv falls back to software).
///
/// Settings store the mpv value, so the order here can change freely.
const HW_MODES: [(&str, &str); 3] = [
    ("no", "Software (CPU)"),
    ("auto-copy", "Hardware (GPU)"),
    ("nvdec", "NVIDIA direct (GPU)"),
];
/// Highest volume (percent). mpv allows up to 130 by default.
const VOL_MAX: f64 = 130.0;

/// A background folder scan's result, shared with its thread: (scan number, files)
type ScanResult = Arc<Mutex<Option<(u64, Vec<PathBuf>)>>>;

struct FluentMediaPlayer {
    /// Only used (briefly, at startup) to create the render context; everything
    /// else goes through `link`, which talks to mpv on its own thread
    mpv: Arc<Mutex<Mpv>>,
    link: MpvLink,
    /// After a user action, keep our own values for a moment instead of the
    /// (slightly older) snapshot, so buttons and the seek bar don't flicker back
    hold_until: std::time::Instant,
    /// mpv sits on the last frame of a finished video
    eof: bool,
    /// Decoder / video lines from the mpv thread for the info overlay
    mpv_info: Vec<InfoRow>,
    render_ctx_initialized: bool,
    current_file: String,
    is_playing: bool,
    is_fullscreen: bool,
    time_pos: f64,
    duration: f64,
    volume: f64,
    muted: bool,
    repeat: bool,
    on_top: bool,
    speed: f64,
    show_controls: bool,
    /// Fraction of the seek bar while the user is dragging it
    drag_frac: Option<f32>,
    /// Wall-clock instant of last user activity, used for auto-hide
    last_mouse_move: std::time::Instant,
    has_file: bool,
    /// Wakeup sender so mpv can ask for repaints from its own thread
    repaint_tx: std::sync::mpsc::SyncSender<()>,
    /// Video files in the current file's folder, in Explorer-style order
    folder_files: Vec<PathBuf>,
    folder_index: Option<usize>,
    /// The .m3u being played, if any: then `folder_files` holds its entries
    /// instead of the folder's files, and it always plays on to the next entry
    playlist: Option<PathBuf>,
    /// The playlist's entries as the panel lists them (titles, lengths), filled in
    /// on another thread from the songs' tags
    playlist_rows: Vec<adjust::PlaylistRow>,
    playlist_found: Arc<Mutex<Vec<PlaylistFound>>>,
    playlist_gen: Arc<std::sync::atomic::AtomicU64>,
    /// At the end of a playlist, start it again (right-click the Playlist button,
    /// or the switch in its panel); and the "Up next" card (playlist panel)
    playlist_replay: bool,
    playlist_summary: bool,
    /// The playlist is files dropped together (not an .m3u): "Selected playlist",
    /// and it can be shuffled
    playlist_selected: bool,
    playlist_shuffle: bool,
    /// The shuffled order (entry numbers); empty = the list's own order
    play_order: Vec<usize>,
    /// Whether the window showed a video last frame (a new video then keeps
    /// showing the last picture until its own first one: no black flicker)
    video_shown: bool,
    /// Accumulated mouse-wheel scroll (points) not yet converted into volume steps
    scroll_accum: f32,
    /// Windows accent colour (re-read every couple of seconds so changes follow live)
    accent: Color32,
    /// The accent as Windows draws title bars / window borders (not boosted)
    border_color: Color32,
    accent_checked: std::time::Instant,
    /// When the volume was last changed by wheel/keys — drives the volume pop-up
    volume_osd: Option<std::time::Instant>,
    /// Technical info overlay (toggle with I)
    show_info: bool,
    info_rows: Vec<InfoRow>,
    /// Hardware decoding mode, index into HW_MODES (cycle with H)
    hw_decode: usize,
    /// File given on the command line (drop on the .exe / "Open with"), played once ready
    startup_file: Option<String>,
    /// Files arriving from Explorer together (see open_arriving), and when the
    /// last one came
    arriving: Vec<String>,
    arriving_last: Option<std::time::Instant>,
    /// The window was maximized when last closed: re-apply it at startup
    start_max: bool,
    start_max_done: bool,
    start_time: std::time::Instant,
    /// Fullscreen transition under way: overlays are held steady (no fade) until it settles
    fs_settle: Option<std::time::Instant>,
    /// In fullscreen the overlays only appear once the mouse has left the screen edges
    fs_armed: bool,
    /// The top bar (filename, menu) is shown. Windowed it goes with the controls;
    /// in fullscreen only while the pointer is over the top of the screen.
    show_top: bool,
    /// Icon font used for the window buttons
    caption_font: CaptionFont,
    /// Window size last frame, and the end of the current live-resize (if any)
    last_size: egui::Vec2,
    resize_until: Option<std::time::Instant>,
    /// mpv settings saved while the live-resize ones are in use
    /// The live-resize settings are switched on
    resize_mode: bool,
    /// Live-resize statistics: frames, summed / worst frame interval, worst mpv render (ms)
    last_frame: std::time::Instant,
    /// Window redraws per second (smoothed), for the info overlay
    ui_fps: f32,
    /// CPU time and clock at the last CPU reading, and the result (percent of the
    /// whole CPU), for the info overlay
    cpu_mark: Option<(u64, std::time::Instant)>,
    cpu_pct: f32,
    /// Native window handle and the taskbar integration built on it
    hwnd: isize,
    taskbar: Option<taskbar::Taskbar>,
    taskbar_tried: bool,
    /// Volume / mute: last seen, last written, and when they last changed
    settings_last: Settings,
    settings_saved: Settings,
    settings_changed: std::time::Instant,
    /// Online video panel choices (formats asked of yt-dlp)
    online: Online,
    /// The Open URL box, while it's open
    url_box: Option<UrlBox>,
    /// A link being opened (spinner until it plays)
    url_load: Option<UrlLoad>,
    /// A link's video title, once mpv has it (shown instead of the link)
    media_title: String,
    /// A link that couldn't be opened: when, and the message (heading, why)
    link_error: Option<(std::time::Instant, &'static str, String)>,
    /// A link that failed, being tried again once yt-dlp has updated
    link_retry: Option<LinkRetry>,
    /// The link yt-dlp has already been updated for (one retry per link)
    retried_url: Option<String>,
    /// The link whose preview stream the thumbnails are using
    link_thumbs: Option<String>,
    /// What the current file's picture is (a song without one, with a cover, a
    /// video); songs get the visualizer / album-art screen
    video_kind: VideoKind,
    /// A song's title (or file name), artist and album, for the album-art screen
    song_title: String,
    song_artist: String,
    song_album: String,
    song_codec: String,
    song_rate: i64,
    /// The playing song's own details (bit depth, average bitrate...), read on
    /// another thread when it starts: (its path, the details)
    song_info: Arc<Mutex<Option<(String, visualizer::SongInfo)>>>,
    /// The cover's width / height
    cover_aspect: f32,
    /// The music visualizer: the second decoder, the bars, and when they last moved
    scope: visualizer::Scope,
    bars: visualizer::Bars,
    bars_tick: std::time::Instant,
    /// The link playing is a live stream (yt-dlp says so): the seek bar is full and
    /// still, and the time shows "● Live" beside it
    pub(crate) live: bool,
    /// Save video as… / Save clip as…: what to save, waiting for a folder
    save_pending: Option<SavePending>,
    /// Save clip as…: the clip being set up (panel at the top, ends on the seek bar)
    clip: Option<ClipUi>,
    /// The window's title as Windows has it (taskbar thumbnail, Alt+Tab)
    os_title: String,
    /// A right-button press that will open the main menu where it's let go
    right_press: bool,
    /// The pointer is over a blank part of the window (the video, or a bar away
    /// from its buttons and seek bar): this frame so far, and last frame. Only
    /// there does a right-click open the menu.
    pub(crate) blank_hover: bool,
    blank_hover_prev: bool,
    /// A link being saved (or just saved): the card in the top right
    save: Option<SaveJob>,
    /// Current and last-written window geometry
    win_state: WinState,
    win_saved: WinState,
    win_changed: std::time::Instant,
    /// Seek pop-up (top left), and when it was last refreshed
    seek_osd: Option<(std::time::Instant, SeekOsd)>,
    /// Chapters of the current file (start time, title)
    chapters: Arc<Vec<(f64, String)>>,
    /// Seek-bar thumbnails (a second, hidden mpv)
    thumbs: thumbs::Thumbs,
    /// Play the next file in the folder when one ends
    autoplay: bool,
    /// mpv's end-of-file flag last frame (to notice the moment a video ends)
    snap_eof: bool,
    /// The top-left menu is open, and where its button is
    main_menu: bool,
    /// The keyboard shortcuts panel is open
    show_keys: bool,
    /// The playback speed menu is open, and where its button is
    speed_menu: bool,
    speed_anchor: Rect,
    /// Open submenu of the main menu (and its row), and the row hovered (since when)
    menu_sub: Option<(&'static str, Rect)>,
    menu_hover: Option<(&'static str, std::time::Instant)>,
    /// Where the pointer last was on the "Sort folder by" row (for heading into its submenu)
    menu_aim: Option<Pos2>,
    menu_anchor: Rect,
    /// Previous frame, for moving the time display on smoothly
    last_tick: std::time::Instant,
    /// Background folder scan: its number, and its result (number, files)
    scan_gen: u64,
    scan_result: ScanResult,
    egui_ctx: egui::Context,
    /// Order of previous / next in a folder
    sort_by: SortBy,
    sort_desc: bool,
    /// Picture turned clockwise by this many degrees (0 / 90 / 180 / 270), and
    /// mirrored left-right. Both go back to normal for the next file.
    rotation: i32,
    mirror: bool,
    /// Window edges snap to the screen edges
    snap: bool,
    /// Multiple player windows allowed (takes effect for the next file opened from Explorer)
    multi: bool,
    /// Volume normalizer on
    normalize: bool,
    /// Windows' "Show accent colour on title bars and window borders" setting
    accent_borders: bool,
    /// Accent-colour window border (menu)
    border: bool,
    /// Picture & sound adjustments, the last saved copy, and when they last changed
    adjust: adjust::Adjust,
    adjust_saved: adjust::Adjust,
    adjust_changed: std::time::Instant,
    adjust_ui: adjust::AdjustUi,
    /// Filter chains last sent to mpv
    last_vf: Option<String>,
    /// Settings of the GPU picture shader in use (see `apply_shader`)
    last_shader: Option<String>,
    /// The current video's frame rate, and whether smooth motion is active for it
    video_fps: f64,
    smooth_on: bool,
    /// The current video is small enough for Denoise (1080p or less); false while
    /// its size isn't known yet, so a 4K video never starts out denoised
    denoise_ok: bool,
    last_af: Option<String>,
    /// High-quality upscaling (menu)
    hq: bool,
    /// Frosted-glass menus (menu)
    glass: bool,
    /// A right-click on the video opens the main menu (menu)
    right_click_menu: bool,
    /// Where the main menu was opened by a right-click (None: from its button)
    menu_at: Option<Pos2>,
    /// The window has just come back to the front with Tab still held (Alt+Tab):
    /// Tab does nothing until it's let go
    tab_blocked: bool,
    /// Windows 11: Windows draws our rounded corners and border; the colour last
    /// sent to it
    win11_frame: bool,
    border_sent: Option<u32>,
}

/// Buttons fire when the mouse button is released anywhere over them, like Windows'
/// own buttons. (egui's "click" also needs the pointer to have stayed nearly still.)
trait Activated {
    fn activated(&self) -> bool;
}

impl Activated for egui::Response {
    fn activated(&self) -> bool {
        self.clicked()
            || (self.drag_stopped()
                && self.ctx.input(|i| i.pointer.latest_pos()).is_some_and(|p| self.rect.contains(p)))
    }
}

/// OpenGL function lookup for mpv's render contexts
fn gl_proc_address(_ctx: &*mut std::ffi::c_void, name: &str) -> *mut std::ffi::c_void {
    unsafe {
        let c_name = std::ffi::CString::new(name).unwrap();
        let addr = wglGetProcAddress(c_name.as_ptr());
        if !addr.is_null() && (addr as usize) > 3 && (addr as usize) != usize::MAX {
            return addr;
        }
        let h_mod = GetModuleHandleA(c"opengl32.dll".as_ptr());
        GetProcAddress(h_mod, c_name.as_ptr())
    }
}

impl FluentMediaPlayer {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let caption_font = install_fonts(&cc.egui_ctx);
        taskbar::set_snap(load_settings().snap);
        let saved = load_window_state().unwrap_or(WinState::DEFAULT);
        let st = load_settings();
        let (volume, muted) = (st.volume as f64, st.muted);
        // A file passed on the command line: drag a video onto the .exe, double-click an
        // associated file, or "Open with". (Explorer passes the path as the first argument.)
        let startup_files: Vec<String> = std::env::args_os()
            .skip(1)
            .map(PathBuf::from)
            .filter(|p| p.is_file() || is_url(&p.to_string_lossy()))
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let startup_file = startup_files.first().cloned();

        // video-timing-offset = 0 stops mpv from blocking the UI thread inside
        // render() while it waits for each frame's display time (the default 50 ms
        // wait made window moves/resizes follow the video's frame rate).
        let mpv = Mpv::with_initializer(|init| {
            // Draw video ONLY through our render context, never in a window of mpv's
            // own. Older libmpv builds happened to pick this automatically; newer
            // ones default to their own video output (a separate "mpv" window).
            let _ = init.set_property("vo", "libmpv");
            let _ = init.set_property("video-timing-offset", 0.0_f64);
            // Decoding: software by default (mpv's own default, and the newer mpv's
            // hardware decoder garbled frames on one AMD setup). H cycles through the
            // hardware decoding methods; the choice is remembered.
            let _ = init.set_property("hwdec", HW_MODES[st.hwdec].0);
            // Don't decode straight into GPU-mapped buffers (another known source of
            // garbled frames on some drivers).
            let _ = init.set_property("vd-lavc-dr", "no");
            // One decoding thread while hardware decoding. mpv 0.36 did this itself;
            // newer mpv runs the hardware decoder multithreaded, which produced
            // garbled blocks with copy-back decoding on AMD. (Software decoding gets
            // automatic threads again, see toggle_hw_decode.)
            let _ = init.set_property("vd-lavc-threads", if st.hwdec > 0 { "1" } else { "0" });
            // Subtitles start off; the captions button turns them on
            let _ = init.set_property("sid", "no");
            let _ = init.set_property("volume-max", VOL_MAX);
            // Pictures stay on screen until you move on (mpv's default is 1 second)
            let _ = init.set_property("image-display-duration", "inf");
            // Memory: mpv's defaults keep up to 150 MiB of read-ahead plus 50 MiB of
            // already-played packets. For local files a few seconds is plenty.
            let _ = init.set_property("cache", "no");
            let _ = init.set_property("demuxer-max-bytes", "32MiB");
            let _ = init.set_property("demuxer-max-back-bytes", "8MiB");
            Ok(())
        })
        .expect("Failed to initialize libmpv!");
        let _ = mpv.set_property("osc", "no");
        let _ = mpv.set_property("keep-open", "yes");
        if st.hq {
            let _ = mpv.set_property("scale", HQ_SCALE);
        }
        let _ = mpv.set_property("volume", volume);
        let _ = mpv.set_property("mute", muted);
        if st.repeat {
            let _ = mpv.set_property("loop-file", "inf");
        }
        // Frame pacing: mpv's default (video timed to the audio), or timed to the
        // display when Smooth motion is on for a film (see `update_smooth`).
        // Repaints: mpv signals us through a channel when a new frame is ready.

        let mpv = Arc::new(Mutex::new(mpv));
        let link = MpvLink::start(mpv.clone());

        // Channel: mpv render thread → egui repaint
        let (tx, rx) = std::sync::mpsc::sync_channel::<()>(1);
        let ctx_clone = cc.egui_ctx.clone();
        // Spawn a thread that wakes egui whenever mpv signals a new frame
        std::thread::spawn(move || {
            while rx.recv().is_ok() {
                ctx_clone.request_repaint();
            }
        });

        let adjust = adjust::Adjust::load(&adjust_path());
        let mut app = Self {
            mpv: mpv.clone(),
            link,
            hold_until: std::time::Instant::now(),
            eof: false,
            mpv_info: Vec::new(),
            render_ctx_initialized: false,
            current_file: String::new(),
            is_playing: false,
            is_fullscreen: false,
            time_pos: 0.0,
            duration: 0.0,
            volume,
            muted,
            repeat: st.repeat,
            on_top: st.on_top,
            speed: 1.0,
            show_controls: true,
            drag_frac: None,
            last_mouse_move: std::time::Instant::now(),
            has_file: false,
            repaint_tx: tx,
            folder_files: Vec::new(),
            folder_index: None,
            playlist: None,
            playlist_rows: Vec::new(),
            playlist_found: Arc::new(Mutex::new(Vec::new())),
            playlist_gen: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            playlist_replay: st.playlist_replay,
            playlist_summary: st.playlist_summary,
            playlist_selected: false,
            playlist_shuffle: false,
            play_order: Vec::new(),
            video_shown: false,
            scroll_accum: 0.0,
            accent: system_accent(),
            border_color: border_accent(),
            accent_checked: std::time::Instant::now(),
            volume_osd: None,
            show_info: false,
            info_rows: Vec::new(),
            hw_decode: st.hwdec,
            // (the file the player was started with counts as the first to arrive)
            // (several on the command line: a playlist of them too)
            arriving_last: startup_file.as_ref().map(|_| std::time::Instant::now()),
            arriving: startup_files,
            startup_file,
            start_max: saved.maximized,
            start_max_done: false,
            start_time: std::time::Instant::now(),
            fs_settle: None,
            fs_armed: true,
            show_top: true,
            caption_font,
            last_size: egui::Vec2::ZERO,
            resize_until: None,
            resize_mode: false,
            last_frame: std::time::Instant::now(),
            ui_fps: 0.0,
            cpu_mark: None,
            cpu_pct: 0.0,
            hwnd: 0,
            taskbar: None,
            taskbar_tried: false,
            settings_last: st,
            settings_saved: st,
            settings_changed: std::time::Instant::now(),
            online: st.online,
            url_box: None,
            url_load: None,
            media_title: String::new(),
            link_error: None,
            link_retry: None,
            retried_url: None,
            link_thumbs: None,
            video_kind: VideoKind::Unknown,
            song_title: String::new(),
            song_artist: String::new(),
            song_album: String::new(),
            song_codec: String::new(),
            song_rate: 0,
            song_info: Arc::new(Mutex::new(None)),
            cover_aspect: 1.0,
            scope: visualizer::Scope::new(),
            bars: visualizer::Bars::new(),
            bars_tick: std::time::Instant::now(),
            live: false,
            save_pending: None,
            clip: None,
            os_title: String::new(),
            right_press: false,
            blank_hover: false,
            blank_hover_prev: false,
            save: None,
            win_state: saved,
            win_saved: saved,
            win_changed: std::time::Instant::now(),
            seek_osd: None,
            chapters: Arc::new(Vec::new()),
            thumbs: thumbs::Thumbs::new(),
            autoplay: st.autoplay,
            snap_eof: false,
            main_menu: false,
            show_keys: false,
            menu_sub: None,
            speed_menu: false,
            speed_anchor: Rect::NOTHING,
            menu_hover: None,
            menu_aim: None,
            menu_anchor: Rect::NOTHING,
            last_tick: std::time::Instant::now(),
            scan_gen: 0,
            scan_result: Arc::new(Mutex::new(None)),
            egui_ctx: cc.egui_ctx.clone(),
            sort_by: st.sort_by,
            sort_desc: st.sort_desc,
            rotation: 0,
            mirror: false,
            snap: st.snap,
            multi: st.multi,
            normalize: st.normalize,
            accent_borders: accent_borders_on(),
            border: st.border,
            hq: st.hq,
            glass: st.glass,
            right_click_menu: st.right_click_menu,
            menu_at: None,
            tab_blocked: false,
            win11_frame: false,
            border_sent: None,
            adjust: adjust.clone(),
            adjust_saved: adjust,
            adjust_changed: std::time::Instant::now(),
            adjust_ui: adjust::AdjustUi::new(),
            last_vf: None,
            last_shader: None,
            video_fps: 0.0,
            smooth_on: false,
            denoise_ok: false,
            last_af: None,
        };
        app.apply_adjust_all();
        // Links: mpv's built-in yt-dlp support, using the yt-dlp.exe shipped beside
        // the player (otherwise mpv looks for one on the PATH)
        app.link.set("ytdl", "yes");
        if let Some(p) = ytdlp_path() {
            app.link.command("change-list", &["script-opts", "append", &format!("ytdl_hook-ytdl_path={}", p.display())]);
        }
        app.apply_online();
        app
    }

    /// Open a file the user picked (drop, Open dialog, command line). For a playlist,
    /// previous / next go through its entries; otherwise through the files of the
    /// same kind (videos / music / pictures) in the file's folder.
    fn open_file(&mut self, path: &str) {
        if is_url(path) {
            self.open_url(path);
        } else if is_playlist_file(path) {
            let entries = read_playlist(Path::new(path));
            self.start_playlist(PathBuf::from(path), entries, false);
        } else {
            self.playlist = None;
            self.playlist_rows.clear();
            self.play_order.clear();
            self.adjust_ui.playlist_open = false;
            // Previous / next become available once the folder has been read (in the
            // background: a huge folder sorted by date would otherwise freeze the
            // window for a moment)
            self.folder_files = vec![PathBuf::from(path)];
            self.folder_index = Some(0);
            self.start_folder_scan(path);
            self.load_entry(path);
        }
    }

    /// Play a playlist (an .m3u's entries, or files dropped together) from its start
    /// (`selected`: files dropped together, which can be shuffled)
    fn start_playlist(&mut self, name: PathBuf, entries: Vec<PlaylistEntry>, selected: bool) {
        let Some(first) = entries.first().map(|e| e.path.to_string_lossy().into_owned()) else { return };
        self.playlist = Some(name);
        self.playlist_selected = selected;
        self.playlist_shuffle = false;
        self.play_order.clear();
        // (already playing the first one, e.g. the first of several opened from
        // Explorer: carry on, don't start it again)
        let playing_it = self.has_file && self.current_file == first;
        self.start_playlist_rows(&entries);
        self.folder_files = entries.into_iter().map(|e| e.path).collect();
        self.folder_index = Some(0);
        if !playing_it {
            self.load_entry(&first);
        }
        self.scan_gen += 1; // a folder scan still running is no longer wanted
    }

    /// The playlist panel's rows: what the playlist itself says (or the file
    /// names) at once, then the songs' own title tags and lengths, read on
    /// another thread
    fn start_playlist_rows(&mut self, entries: &[files::PlaylistEntry]) {
        use std::sync::atomic::Ordering;
        self.playlist_rows = entries
            .iter()
            .map(|e| adjust::PlaylistRow {
                title: e.title.clone().unwrap_or_else(|| {
                    let p = e.path.to_string_lossy();
                    if is_url(&p) { p.into_owned() } else { e.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default() }
                }),
                secs: e.secs,
            })
            .collect();
        let gen = self.playlist_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let (live, found, ctx) = (self.playlist_gen.clone(), self.playlist_found.clone(), self.egui_ctx.clone());
        let paths: Vec<String> = entries.iter().map(|e| e.path.to_string_lossy().into_owned()).collect();
        let _ = std::thread::Builder::new().name("playlist".into()).spawn(move || {
            for (i, p) in paths.iter().enumerate() {
                if live.load(Ordering::SeqCst) != gen {
                    return; // (another playlist was opened)
                }
                if is_url(p) {
                    continue;
                }
                let visualizer::SongInfo { title, secs, .. } = visualizer::song_info(p);
                if title.is_some() || secs.is_some() {
                    if let Ok(mut f) = found.lock() {
                        f.push((gen, i, title, secs));
                    }
                    if i % 8 == 7 {
                        ctx.request_repaint();
                    }
                }
            }
            ctx.request_repaint();
        });
    }

    /// Play the playlist's (or folder's) entry `i`
    fn play_index(&mut self, i: usize) {
        let Some(path) = self.folder_files.get(i).map(|p| p.to_string_lossy().into_owned()) else { return };
        self.folder_index = Some(i);
        self.load_entry(&path);
    }

    /// Read and sort the file's folder on another thread; `update` picks up the result
    fn start_folder_scan(&mut self, path: &str) {
        self.scan_gen += 1;
        let (gen, slot, ctx) = (self.scan_gen, self.scan_result.clone(), self.egui_ctx.clone());
        let (path, by, desc) = (PathBuf::from(path), self.sort_by, self.sort_desc);
        std::thread::spawn(move || {
            let files = scan_folder(&path, by, desc);
            if let Ok(mut s) = slot.lock() {
                *s = Some((gen, files));
            }
            ctx.request_repaint();
        });
    }

    /// Start playing `path` (keeps the current previous / next list)
    fn load_entry(&mut self, path: &str) {
        self.current_file = path.to_owned();
        self.has_file = true;
        self.media_title.clear();
        if !is_url(path) {
            self.url_load = None;
            self.link_error = None;
        }
        if self.rotation != 0 || self.mirror {
            self.rotation = 0;
            self.mirror = false;
            self.apply_view();
        }
        // Back to "subtitles off" for every new file, even if they were switched on
        // for the previous one
        self.link.set("sid", "no");
        self.link.set("loop-file", if self.wants_loop() { "inf" } else { "no" });
        // Denoise starts off for every file, until its size is known (see update)
        if self.denoise_ok {
            self.denoise_ok = false;
            self.apply_vf();
        }
        // Read-ahead. A file on disk is read in an instant, so mpv keeps only a few
        // seconds of it (little memory). A link gets mpv's network cache: it reads
        // well ahead and pauses to catch up if the connection stalls. (Without it a
        // link had about one second of read-ahead, so any hiccup in the connection
        // stuttered the video, and going back a few seconds downloaded it again.)
        // These apply to the file opened next.
        if is_url(path) {
            self.link.set("cache", "yes");
            self.link.set("demuxer-max-bytes", "64MiB");
            self.link.set("demuxer-max-back-bytes", "16MiB");
        } else {
            self.link.set("cache", "no");
            self.link.set("demuxer-max-bytes", "32MiB");
            self.link.set("demuxer-max-back-bytes", "8MiB");
        }
        // A picture kept from something that wasn't a video (a song's cover) mustn't
        // show while this file opens; after a video, its last frame stays up until
        // the new one's first, so going from video to video doesn't flicker
        VIDEO_STALE.with(|s| s.set(!self.video_shown));
        self.link.command("loadfile", &[path]);
        self.link.set("pause", "no");
        self.is_playing = true;
        self.time_pos = 0.0;
        self.eof = false;
        self.chapters = Arc::new(Vec::new());
        self.link_thumbs = None;
        self.live = false;
        self.video_kind = VideoKind::Unknown;
        if media_kind(path) == Some(MediaKind::Music) && !is_url(path) {
            let (slot, p, ctx) = (self.song_info.clone(), path.to_owned(), self.egui_ctx.clone());
            let _ = std::thread::Builder::new().name("song info".into()).spawn(move || {
                let info = visualizer::song_info(&p);
                if let Ok(mut s) = slot.lock() {
                    *s = Some((p, info));
                }
                ctx.request_repaint();
            });
        }
        // (the bars carry on from the last song and settle by themselves: no
        // jump to nothing between songs)
        self.bars.soften();
        self.clip = None;
        if is_url(path) {
            self.thumbs.forget();
        } else {
            self.thumbs.load(path);
        }
        self.touch();
    }

    /// Open a file handed to us from outside (open dialog, a second launch)
    /// A file opened from outside (Explorer, a later launch). The first one plays
    /// at once; more arriving right after it are collected, and become a playlist
    /// once they stop coming (Explorer's "Open" on several selected files starts
    /// the player once per file, a moment apart).
    fn open_arriving(&mut self, path: String) {
        if !Path::new(&path).is_file() && !is_url(&path) {
            return; // ("": just come to the front)
        }
        let now = std::time::Instant::now();
        let together = self.arriving_last.is_some_and(|t| now.duration_since(t) < std::time::Duration::from_millis(2000));
        self.arriving_last = Some(now);
        if together {
            if !self.arriving.contains(&path) {
                self.arriving.push(path);
            }
            return;
        }
        self.arriving = vec![path.clone()];
        self.open_external(path);
    }

    fn open_external(&mut self, path: String) {
        if !Path::new(&path).is_file() && !is_url(&path) {
            return;
        }
        if self.render_ctx_initialized {
            self.open_file(&path);
        } else {
            self.startup_file = Some(path);
        }
    }

    /// New previous / next order (menu): re-sort the current folder right away
    fn set_sort(&mut self, by: SortBy, desc: bool) {
        self.sort_by = by;
        self.sort_desc = desc;
        if self.playlist.is_none() && self.has_file && !is_url(&self.current_file) {
            let current = self.current_file.clone();
            self.start_folder_scan(&current);
        }
    }

    /// One frame forward / back (pauses, like mpv's own frame stepping)
    fn frame_step(&mut self, forward: bool) {
        if !self.has_file {
            return;
        }
        self.link.command(if forward { "frame-step" } else { "frame-back-step" }, &[]);
        self.is_playing = false;
        // (no touch() here: it holds the displayed time still for a moment, and
        // holding the key down kept renewing that, so the seek bar only caught up
        // once the key was released)
    }

    // ── Picture & sound adjustments ─────────────────────────────────────────
    /// Send every adjustment to mpv (at startup)
    fn apply_adjust_all(&mut self) {
        let a = self.adjust.clone();
        for (name, v) in a.gpu_colour() {
            if v != 0 {
                self.link.set(name, v);
            }
        }
        if a.deband {
            self.link.set("deband", "yes");
        }
        if a.deinterlace {
            self.link.set("deinterlace", DEINTERLACE_ON);
        }
        if a.delay_ms != 0 {
            self.link.set("audio-delay", a.delay_ms as f64 / 1000.0);
        }
        if !a.keep_pitch {
            self.link.set("audio-pitch-correction", "no");
        }
        // (Smooth motion is switched on per video, see `update_smooth`.)
        // Shader files from earlier runs aren't needed any more
        if let Some(dir) = shader_dir() {
            let _ = std::fs::remove_dir_all(dir);
        }
        self.apply_shader();
        self.apply_vf();
        self.apply_af(true);
    }

    /// Send what changed between `old` and the current adjustments to mpv
    fn apply_adjust_changes(&mut self, old: &adjust::Adjust, old_normalize: bool) {
        let a = self.adjust.clone();
        // mpv's own colour controls
        for ((name, before), (_, now)) in old.gpu_colour().into_iter().zip(a.gpu_colour()) {
            if before != now {
                self.link.set(name, now);
            }
        }
        if old.deband != a.deband {
            self.link.set("deband", yn(a.deband));
        }
        if old.deinterlace != a.deinterlace {
            self.link.set("deinterlace", if a.deinterlace { DEINTERLACE_ON } else { "no" });
        }
        if old.delay_ms != a.delay_ms {
            self.link.set("audio-delay", a.delay_ms as f64 / 1000.0);
        }
        if old.keep_pitch != a.keep_pitch {
            self.link.set("audio-pitch-correction", yn(a.keep_pitch));
        }
        // (Smooth motion follows on the next frame, see `update_smooth`)
        self.apply_shader();
        self.apply_vf();
        // Only the equalizer's gains changed: send them to the running filters
        // (no rebuild, so dragging a slider doesn't make the sound drop out)
        let same_shape = old.af_chain(old_normalize, false) == a.af_chain(self.normalize, false);
        if same_shape && a.eq_on {
            if old.eq != a.eq {
                self.link.command("af-command", &["eq", "gain_entry", &a.eq_entries()]);
            }
            if old.preamp != a.preamp {
                self.link.command("af-command", &["eqpre", "volume", &format!("{}dB", a.preamp)]);
            }
            // (kept in step so a later rebuild has the same values)
            self.last_af = a.af_chain(self.normalize, true);
        } else {
            self.apply_af(false);
        }
    }

    /// Picture filters plus mirroring, as one filter chain
    fn apply_vf(&mut self) {
        let chain = self.adjust.vf_chain(self.mirror, self.denoise_ok);
        if chain != self.last_vf {
            match &chain {
                Some(c) => self.link.command("vf", &["set", c]),
                None => self.link.command("vf", &["clr", ""]),
            }
            self.last_vf = chain;
        }
    }

    /// Color temperature, Vibrance and Sharpen: an mpv user shader on the GPU.
    /// Each combination of settings gets its own file (mpv keeps a shader file it
    /// has read for as long as it runs, so a changed file under the same name
    /// wouldn't be read again); they're deleted at the next start.
    fn apply_shader(&mut self) {
        let key = self.adjust.shader_key();
        if key == self.last_shader {
            return;
        }
        self.last_shader = key.clone();
        self.link.command("change-list", &["glsl-shaders", "clr", ""]);
        if let (Some(key), Some(src), Some(dir)) = (key, self.adjust.shader(), shader_dir()) {
            let path = dir.join(format!("picture-{key}.glsl"));
            let _ = std::fs::create_dir_all(&dir);
            if path.exists() || std::fs::write(&path, src).is_ok() {
                self.link.command("change-list", &["glsl-shaders", "append", &path.to_string_lossy()]);
            }
        }
    }

    /// Smooth motion on or off for the current video: only for videos under 40 fps
    /// (films, TV), where the uneven frame timing is visible. 50 / 60 fps video
    /// doesn't need it, and there it costs a lot: every frame then has to be
    /// finished within one refresh of the monitor (7 ms at 144 Hz) rather than one
    /// frame of the video, which 4K video can't always manage.
    fn update_smooth(&mut self) {
        let want = self.adjust.smooth && self.video_fps > 0.0 && self.video_fps < 40.0;
        if want != self.smooth_on {
            self.smooth_on = want;
            self.apply_smooth();
        }
    }

    /// Sound filters (equalizer, night mode, normalizer, …) as one filter chain
    fn apply_af(&mut self, force: bool) {
        let chain = self.adjust.af_chain(self.normalize, true);
        if force || chain != self.last_af {
            match &chain {
                Some(c) => self.link.command("af", &["set", c]),
                None => self.link.command("af", &["clr", ""]),
            }
            self.last_af = chain;
        }
    }

    /// Smooth motion: show frames in step with the monitor's refresh, blending
    /// between them where needed
    ///
    /// (It used to also tell mpv when each frame reached the screen, but once mpv
    /// gets those reports it relies on them for good, so turning smooth motion off
    /// again left playback juddering and slow.)
    fn apply_smooth(&mut self) {
        if self.smooth_on {
            // An embedded player has to tell mpv the monitor's refresh rate
            if let Some(hz) = taskbar::monitor_refresh_hz(self.hwnd) {
                self.link.set("display-fps-override", hz);
                self.link.set("display-fps", hz); // (older mpv's name)
            }
            self.link.set("video-sync", "display-resample");
            self.link.set("interpolation", "yes");
        } else {
            // Everything back to mpv's defaults
            self.link.set("interpolation", "no");
            self.link.set("video-sync", "audio");
            self.link.set("display-fps-override", 0);
            self.link.set("display-fps", 0);
        }
    }

    /// Turn the picture another 90° clockwise
    fn rotate_90(&mut self) {
        if !self.has_file {
            return;
        }
        self.rotation = (self.rotation + 90) % 360;
        self.apply_view();
    }

    /// Mirror the picture left-right (a left-handed golf swing becomes right-handed)
    fn toggle_mirror(&mut self) {
        if !self.has_file {
            return;
        }
        self.mirror = !self.mirror;
        self.apply_view();
    }

    /// Send rotation / mirroring to mpv (and the thumbnail player, so previews match)
    fn apply_view(&mut self) {
        // video-rotate adds to any rotation the file itself asks for (phone videos)
        self.link.set("video-rotate", self.rotation);
        self.apply_vf();
        self.thumbs.set_view(self.rotation, self.mirror);
    }

    /// Windows' "Open" dialog, starting in the current video's folder
    fn show_open_dialog(&mut self) {
        let dir = Path::new(&self.current_file)
            .parent()
            .map(|d| d.to_string_lossy().into_owned())
            .filter(|d| !d.is_empty() && !is_url(&self.current_file));
        taskbar::open_file_dialog(self.hwnd, dir);
    }

    fn toggle_play_pause(&mut self) {
        // Finished playing (mpv sits on the last frame): Play / Space starts it over
        if self.has_file && self.eof {
            self.link.command("seek", &["0", "absolute"]);
            self.link.set("pause", "no");
            self.time_pos = 0.0;
            self.is_playing = true;
            self.eof = false;
            self.touch();
            return;
        }

        self.is_playing = !self.is_playing;
        self.link.set("pause", yn(!self.is_playing));
        self.touch();
    }

    /// Next hardware decoding method (H). Different GPUs / drivers glitch with
    /// different methods; the info overlay opens to show whether it took.
    fn toggle_hw_decode(&mut self) {
        self.hw_decode = (self.hw_decode + 1) % HW_MODES.len();
        // Threads must be set before the decoder is re-opened by the hwdec change
        self.link.set("vd-lavc-threads", if self.hw_decode > 0 { "1" } else { "0" });
        self.link.set("hwdec", HW_MODES[self.hw_decode].0);
        self.show_info = true;
    }

    /// Live-resize settings on/off (applied by the mpv thread, see mpv_link.rs)
    fn set_resize_mode(&mut self, on: bool) {
        if self.resize_mode != on {
            self.resize_mode = on;
            self.link.resize_mode(on);
        }
    }

    /// Keep our own (just changed) values for a moment, see `hold_until`
    fn touch(&mut self) {
        self.hold_until = std::time::Instant::now() + std::time::Duration::from_millis(400);
    }

    fn current_settings(&self) -> Settings {
        Settings {
            volume: self.volume.round() as i32,
            muted: self.muted,
            repeat: self.repeat,
            on_top: self.on_top,
            hwdec: self.hw_decode,
            autoplay: self.autoplay,
            sort_by: self.sort_by,
            sort_desc: self.sort_desc,
            snap: self.snap,
            multi: self.multi,
            normalize: self.normalize,
            border: self.border,
            hq: self.hq,
            glass: self.glass,
            right_click_menu: self.right_click_menu,
            playlist_replay: self.playlist_replay,
            playlist_summary: self.playlist_summary,
            online: self.online,
        }
    }

    /// Remember the window geometry, then end the process at once
    /// The playlist panel and the "Up next" card
    fn draw_playlist(&mut self, ctx: &egui::Context) {
        // Titles / lengths found since the last frame
        let gen = self.playlist_gen.load(std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut f) = self.playlist_found.lock() {
            for (g, i, title, secs) in f.drain(..) {
                if let Some(row) = self.playlist_rows.get_mut(i).filter(|_| g == gen) {
                    if let Some(t) = title {
                        row.title = t;
                    }
                    if secs.is_some() {
                        row.secs = secs;
                    }
                }
            }
        }
        if self.playlist.is_none() || self.playlist_rows.is_empty() {
            self.adjust_ui.playlist_open = false;
            self.adjust_ui.playlist_rect = Rect::NOTHING;
            self.adjust_ui.summary_rect = Rect::NOTHING;
            return;
        }
        let current = self.folder_index.unwrap_or(0);
        let playing = self.is_playing && !self.eof;
        // The playing entry's three-bar mark: the visualizer's bass / middle /
        // treble when it's showing, else a gentle made-up bounce
        let mut mark = [0.35, 0.8, 0.25];
        if playing && self.adjust_ui.playlist_open {
            let lv = self.bars.levels();
            let third = visualizer::BARS / 3;
            let avg = |a: usize, b: usize| lv[a..b].iter().sum::<f32>() / (b - a) as f32;
            let real = [avg(0, third), avg(third, 2 * third), avg(2 * third, visualizer::BARS)];
            if real.iter().any(|v| *v > 0.02) {
                mark = real.map(|v| (v * 1.4).min(1.0));
            } else {
                let t = ctx.input(|i| i.time) as f32;
                mark = [0, 1, 2].map(|k| 0.5 + 0.45 * (t * (6.0 + 1.9 * k as f32) + k as f32 * 1.3).sin());
                ctx.request_repaint_after(std::time::Duration::from_millis(40));
            }
        }
        let (mut replay, mut summary, mut shuffle) = (self.playlist_replay, self.playlist_summary, self.playlist_shuffle);
        let progress = if self.duration > 0.0 { (self.time_pos / self.duration) as f32 } else { 0.0 };
        if playing && self.adjust_ui.playlist_open {
            // (the playing row's progress fill moves on)
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let shuffle_opt = self.playlist_selected.then_some(&mut shuffle);
        let mut action = adjust::draw_playlist(ctx, &mut self.adjust_ui, &self.playlist_rows, current, mark, progress, &mut replay, &mut summary, shuffle_opt, self.accent);
        if summary && !self.adjust_ui.playlist_open {
            let noun = self.playlist_noun();
            // the next three in playing order
            let mut next = Vec::new();
            for d in 1..=3 {
                match self.step_index(d) {
                    Some(k) if k != current && !next.contains(&k) => next.push(k),
                    _ => break,
                }
            }
            let a = adjust::draw_playlist_summary(ctx, &mut self.adjust_ui, &self.playlist_rows, current, &next, noun, self.accent);
            if !matches!(a, adjust::PlaylistAction::None) {
                action = a;
            }
        } else {
            self.adjust_ui.summary_rect = Rect::NOTHING;
        }
        self.playlist_replay = replay;
        self.playlist_summary = summary;
        if shuffle != self.playlist_shuffle {
            self.set_shuffle(shuffle);
        }
        match action {
            adjust::PlaylistAction::Play(i) => self.play_index(i),
            adjust::PlaylistAction::Open => self.toggle_playlist_panel(),
            adjust::PlaylistAction::None => {}
        }
    }

    /// A song is playing (no picture, or just its cover): the visualizer /
    /// album-art screen, and no rotate, mirror or subtitle buttons. While a new
    /// file opens, its extension decides (so going from song to song doesn't
    /// flash anything in between).
    pub(crate) fn is_music(&self) -> bool {
        self.has_file
            && match self.video_kind {
                VideoKind::None | VideoKind::Cover => true,
                VideoKind::Video => false,
                VideoKind::Unknown => media_kind(&self.current_file) == Some(MediaKind::Music),
            }
    }

    /// What previous / next go to: "song", "video", "picture" (or "file")
    pub(crate) fn item_noun(&self) -> &'static str {
        if self.playlist.is_some() {
            let n = self.playlist_noun();
            return n.strip_suffix('s').unwrap_or(n);
        }
        match media_kind(&self.current_file) {
            Some(MediaKind::Music) => "song",
            Some(MediaKind::Video) => "video",
            Some(MediaKind::Image) => "picture",
            None => "file",
        }
    }

    /// What the playlist holds: "songs", "videos", or "files" if it's a mix
    pub(crate) fn playlist_noun(&self) -> &'static str {
        let one = self.folder_files.len() == 1;
        let all = |k: MediaKind| self.folder_files.iter().all(|f| media_kind(&f.to_string_lossy()) == Some(k));
        match (all(MediaKind::Music), all(MediaKind::Video)) {
            (true, _) => if one { "song" } else { "songs" },
            (_, true) => if one { "video" } else { "videos" },
            _ => if one { "file" } else { "files" },
        }
    }

    /// "3 of 12" while a playlist plays
    fn playlist_position(&self) -> Option<String> {
        self.playlist.as_ref()?;
        Some(format!("{} of {}", self.folder_index? + 1, self.folder_files.len()))
    }

    /// A song is playing: the visualizer, or the album-art screen
    /// (Picture & Sound → Sound → Show)
    fn draw_music(&mut self, ui: &egui::Ui, rect: Rect) {
        use adjust::MusicShow;
        let now = std::time::Instant::now();
        let dt = now.saturating_duration_since(self.bars_tick).as_secs_f32();
        self.bars_tick = now;

        // The song's details: its title tag (else the file name), artist, album
        let title = if self.song_title.is_empty() { self.file_name() } else { self.song_title.clone() };
        let position = self.playlist_position();
        let name = visualizer::SongName { song: &title, artist: &self.song_artist, album: &self.song_album, playlist: position.as_deref() };
        let p = ui.painter();
        let playing = self.is_playing && !self.eof;

        if self.adjust.music == MusicShow::AlbumArt {
            // Album art: the cover with the song's details under it; nothing
            // moves, so nothing more to decode or redraw
            self.scope.stop();
            let has_cover = self.video_kind == VideoKind::Cover && self.render_ctx_initialized;
            if let Some(c) = visualizer::Bars::cover_rect(rect, self.cover_aspect, name.lines()).filter(|_| has_cover) {
                self.bars.draw_cover_back(p, rect, c, self.accent);
                p.add(video_callback(c));
                self.bars.draw_cover_front(p, rect, name);
            } else {
                // No cover (or no room for one): the details drift round in their box
                if self.bars.draw_floating(p, rect, name, self.accent, playing, dt) {
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
                }
            }
            return;
        }

        self.scope.follow(&self.current_file, self.time_pos);
        let moving = self.bars.update(&self.scope, &self.current_file, self.time_pos, playing, dt);
        if self.bars.state == visualizer::State::Unsupported {
            // (a file the bars can't read: WMA, a link...): the drifting box
            if self.bars.draw_floating(p, rect, name, self.accent, playing, dt) {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
            }
            return;
        }
        self.bars.draw_visualizer(p, rect, name, self.accent);
        // Keep the bars moving (at the screen's refresh rate; the 6 ms is only a
        // limit in case vsync is off). Paused, they settle and the redrawing stops.
        if moving {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(6));
        }
    }

    /// The playing song's format for the top bar: "FLAC", then its details
    /// (longest first, shorter ones for less room)
    pub(crate) fn song_format(&self) -> Option<(String, Vec<String>)> {
        if !self.is_music() {
            return None;
        }
        let info = self.song_info.lock().ok().and_then(|s| s.as_ref().filter(|(p, _)| *p == self.current_file).map(|(_, i)| i.clone()));
        visualizer::song_format(&self.current_file, &self.song_codec, u32::try_from(self.song_rate).ok(), info.as_ref())
    }

    fn quit(&mut self) -> ! {
        if self.win_state != self.win_saved {
            save_window_state(&self.win_state);
        }
        let cur = self.current_settings();
        if cur != self.settings_saved {
            save_settings(&cur);
        }
        if self.adjust != self.adjust_saved {
            self.adjust.save(&adjust_path());
        }
        // A save still running is cancelled (and its half-downloaded pieces left
        // for yt-dlp to carry on from if the same video is saved there again)
        self.save = None;
        terminate_now()
    }

    fn seek_to(&mut self, secs: f64) {
        self.link.set("time-pos", secs);
        self.time_pos = secs;
        self.touch();
    }

    fn set_volume(&mut self, vol: f64) {
        self.volume = vol.clamp(0.0, VOL_MAX);
        self.link.set("volume", self.volume);
        self.touch();
    }

    /// Change the volume by `delta` percent and show the volume pop-up
    fn nudge_volume(&mut self, delta: f64) {
        self.set_volume(self.volume + delta);
        if delta > 0.0 && self.muted { self.set_mute(false); }
        self.volume_osd = Some(std::time::Instant::now());
        self.seek_osd = None;
    }

    /// Gather the details for the info overlay (Tab)
    fn refresh_info(&mut self) {
        let mut rows = Vec::new();
        for row in self.mpv_info.iter().cloned() {
            let after_decoder = matches!(row, InfoRow::Item("Decoder", _));
            rows.push(row);
            if after_decoder {
                rows.push(InfoRow::Item("Method", HW_MODES[self.hw_decode].1.to_owned()));
            }
        }
        rows.push(InfoRow::Section("Performance"));
        let render_ms = RENDER_MS.with(|r| r.get());
        rows.push(InfoRow::Item(
            "Render",
            format!(
                "{render_ms:.1} ms per frame  \u{b7}  {:.0} redraws/s{}",
                self.ui_fps,
                if self.resize_mode { "  \u{b7}  resizing" } else { "" }
            ),
        ));
        // CPU use of the whole player, as a share of all cores (100 % = every core busy)
        if let Some(cpu) = process_cpu_time() {
            let now = std::time::Instant::now();
            if let Some((c0, t0)) = self.cpu_mark {
                let secs = now.duration_since(t0).as_secs_f64();
                if secs >= 0.5 {
                    let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
                    self.cpu_pct = ((cpu.saturating_sub(c0)) as f64 / 1e7 / secs / cores * 100.0) as f32;
                    self.cpu_mark = Some((cpu, now));
                }
            } else {
                self.cpu_mark = Some((cpu, now));
            }
            rows.push(InfoRow::Item("CPU", format!("{:.0} % of all cores", self.cpu_pct)));
        }
        if let Some((ws, private)) = process_memory_mb() {
            rows.push(InfoRow::Item("Memory", format!("{ws:.0} MB  \u{b7}  {private:.0} MB private")));
        }
        self.info_rows = rows;
    }

    fn set_mute(&mut self, mute: bool) {
        self.muted = mute;
        self.link.set("mute", yn(mute));
        self.touch();
    }

    /// Open / close the playlist panel (at the playing entry)
    pub(crate) fn toggle_playlist_panel(&mut self) {
        self.adjust_ui.playlist_open = !self.adjust_ui.playlist_open;
        self.adjust_ui.playlist_scroll = self.adjust_ui.playlist_open;
    }

    fn toggle_repeat(&mut self) {
        self.repeat = !self.repeat;
        self.link.set("loop-file", if self.wants_loop() { "inf" } else { "no" });
    }

    /// Repeat is on, or the file is a GIF (those always loop, like in a browser).
    /// Not in a playlist: that plays on (its button is the Playlist button then).
    fn wants_loop(&self) -> bool {
        (self.repeat && self.playlist.is_none()) || has_ext(&self.current_file, &["gif"])
    }

    fn cycle_captions(&mut self) {
        self.link.command("cycle", &["sub"]);
    }

    fn set_speed(&mut self, speed: f64) {
        self.speed = speed;
        self.link.set("speed", speed);
    }

    /// A playlist set to replay goes round: next after the last is the first
    fn wraps(&self) -> bool {
        self.playlist.is_some() && self.playlist_replay && self.folder_files.len() > 1
    }

    fn has_prev(&self) -> bool {
        self.step_index(-1).is_some()
    }

    fn has_next(&self) -> bool {
        self.step_index(1).is_some()
    }

    /// The entry `delta` steps from the playing one: in the shuffled order when
    /// shuffle is on, going round when the playlist replays
    pub(crate) fn step_index(&self, delta: isize) -> Option<usize> {
        let i = self.folder_index?;
        let n = self.folder_files.len() as isize;
        let shuffled = self.play_order.len() == n as usize;
        let pos = if shuffled { self.play_order.iter().position(|&k| k == i)? as isize } else { i as isize };
        let mut target = pos + delta;
        if self.wraps() {
            target = target.rem_euclid(n);
        }
        if target < 0 || target >= n {
            return None;
        }
        Some(if shuffled { self.play_order[target as usize] } else { target as usize })
    }

    /// Shuffle on: a random order, starting from the playing entry. Off: the
    /// list's own order.
    fn set_shuffle(&mut self, on: bool) {
        self.playlist_shuffle = on;
        self.play_order.clear();
        if !on {
            return;
        }
        let n = self.folder_files.len();
        let cur = self.folder_index.unwrap_or(0);
        let mut rest: Vec<usize> = (0..n).filter(|&k| k != cur).collect();
        // (xorshift seeded from the clock: no need for a random-number library)
        let mut x = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0x9E37_79B9, |d| d.as_nanos() as u64) | 1;
        for k in (1..rest.len()).rev() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            rest.swap(k, (x % (k as u64 + 1)) as usize);
        }
        self.play_order = std::iter::once(cur).chain(rest).collect();
    }

    /// Open the previous (-1) or next (+1) entry: of the playlist when one is
    /// playing, else of the current file's folder
    fn play_adjacent(&mut self, delta: isize) {
        if let Some(target) = self.step_index(delta) {
            self.play_index(target);
        }
    }

    /// Skip with the keys / taskbar buttons, and show "+5 sec" (adding up quick repeats
    /// in the same direction)
    fn step(&mut self, secs: f64) {
        if !self.has_file {
            return;
        }
        // Count only what the jump really moved: near the start or end of the
        // video a 5 sec step can be shorter, and at the very start / end nothing
        let from = self.time_pos;
        let to = (from + secs).clamp(0.0, self.duration.max(0.0));
        let moved = to - from;
        // Pressed at (or within a moment of) the start / end: go there, but don't
        // add the tiny distance to the pop-up. (The video keeps playing between
        // presses, so each press at the start moved back a fraction of a second,
        // which slowly added up to "−1 sec", "−2 sec"…)
        let clamped = (to - (from + secs)).abs() > 1e-9;
        if moved.abs() < 0.05 || (clamped && moved.abs() < 1.0) {
            if moved.abs() >= 0.05 {
                self.seek_to(to);
            }
            if let Some((t0, _)) = self.seek_osd.as_mut() {
                *t0 = std::time::Instant::now(); // keep the pop-up up a moment longer
            }
            return;
        }
        self.seek_to(to);
        let total = match self.seek_osd {
            Some((t0, SeekOsd::Step(prev))) if t0.elapsed().as_secs_f32() < 1.2 && prev.signum() == moved.signum() => prev + moved,
            _ => moved,
        };
        self.seek_osd = Some((std::time::Instant::now(), SeekOsd::Step(total)));
        self.volume_osd = None;
    }

    fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        if self.is_fullscreen {
            self.exit_fullscreen(ctx);
        } else {
            self.enter_fullscreen(ctx);
        }
    }

    /// Our window has no frame, so "fullscreen" is just: cover the whole monitor in one
    /// single resize. The OS fullscreen mode switch (several style + size changes in a
    /// row) is what made the picture flicker and lag. Windows hides the taskbar by
    /// itself for a frameless window that covers its monitor.
    fn enter_fullscreen(&mut self, ctx: &egui::Context) {
        // Everything hides at once (like most players); the controls come back when
        // the pointer goes to the bottom of the screen, the filename / menu at the top
        self.fs_settle = Some(std::time::Instant::now());
        self.fs_armed = false;
        self.show_controls = false;
        self.show_top = false;
        self.is_fullscreen = true;
        if self.hwnd != 0 {
            // Posted, so the resize happens between two frames (see taskbar.rs)
            taskbar::post_fullscreen(self.hwnd, true);
            ctx.request_repaint();
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        }
    }

    fn exit_fullscreen(&mut self, ctx: &egui::Context) {
        self.fs_settle = Some(std::time::Instant::now());
        self.show_controls = true;
        self.is_fullscreen = false;
        if self.hwnd != 0 {
            taskbar::post_fullscreen(self.hwnd, false);
            ctx.request_repaint();
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
    }

    fn toggle_maximize(&mut self, ctx: &egui::Context) {
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }

    fn toggle_on_top(&mut self, ctx: &egui::Context) {
        self.on_top = !self.on_top;
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(if self.on_top {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        }));
    }

    fn file_name(&self) -> String {
        if is_url(&self.current_file) {
            return if self.media_title.is_empty() { short_url(&self.current_file) } else { self.media_title.clone() };
        }
        std::path::Path::new(&self.current_file)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_owned()
    }
}

impl eframe::App for FluentMediaPlayer {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.blank_hover_prev = std::mem::take(&mut self.blank_hover);
        // No button ever keeps keyboard focus. egui moves focus between buttons with
        // Tab (our info key), and a focused button also "clicks" on Space / Enter,
        // so Tab then Space would have paused *and* pressed some button.
        // (The Open URL box's text field, and a clip time being typed, are the
        // exceptions while they're open.)
        let typing = if self.url_box.is_some() {
            Some(egui::Id::new(URL_FIELD))
        } else if self.clip.as_ref().is_some_and(|c| c.editing.is_some()) {
            Some(egui::Id::new(CLIP_FIELD))
        } else {
            None
        };
        ctx.memory_mut(|m| {
            if let Some(id) = m.focused() {
                if Some(id) != typing {
                    m.surrender_focus(id);
                }
            }
        });
        // ── Folder list from the background scan ──────────────────────────────
        let scanned = self.scan_result.lock().ok().and_then(|mut s| s.take());
        if let Some((gen, files)) = scanned {
            if gen == self.scan_gen && self.playlist.is_none() {
                let current = PathBuf::from(&self.current_file);
                self.folder_index = files.iter().position(|p| *p == current);
                self.folder_files = files;
            }
        }

        // ── Player state from the mpv thread (never waits for mpv) ────────────
        let snap = self.link.snapshot();
        let now = std::time::Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64().min(0.25);
        self.last_tick = now;
        if snap.valid {
            self.track_url_load(ctx, &snap);
            // A link's thumbnails: from the small preview stream yt-dlp found
            if is_url(&self.current_file) && self.link_thumbs.as_deref() != Some(self.current_file.as_str()) {
                if let Some(li) = snap.link_info.as_ref().filter(|li| li.url == self.current_file) {
                    if let Some(pv) = &li.preview {
                        self.thumbs.load_stream(&pv.url, pv.user_agent.clone(), pv.headers.clone());
                    }
                    self.link_thumbs = Some(self.current_file.clone());
                }
            }
            self.live = is_url(&self.current_file)
                && snap.link_info.as_ref().is_some_and(|li| li.url == self.current_file && li.live);
            if is_url(&self.current_file) && snap.path == self.current_file && !snap.media_title.is_empty() {
                self.media_title = snap.media_title.clone();
            }
            self.duration = snap.duration;
            self.video_fps = snap.fps;
            // (A link can turn into other addresses, e.g. a YouTube playlist into
            // its videos: then whatever mpv is playing counts. While a link opens
            // the screen stays black anyway, see url_load.)
            if snap.path == self.current_file || (is_url(&self.current_file) && !snap.path.is_empty()) {
                self.video_kind = snap.video_kind;
                for (mine, theirs) in [
                    (&mut self.song_title, &snap.media_title),
                    (&mut self.song_artist, &snap.artist),
                    (&mut self.song_album, &snap.album),
                ] {
                    if mine != theirs {
                        mine.clone_from(theirs);
                    }
                }
                if self.song_codec != snap.audio_codec {
                    self.song_codec.clone_from(&snap.audio_codec);
                }
                self.song_rate = snap.audio_rate;
                if snap.dwidth > 0 && snap.dheight > 0 {
                    self.cover_aspect = snap.dwidth as f32 / snap.dheight as f32;
                }
            }
            self.update_smooth();
            // Denoise only up to 1080p, counted in pixels (so a 1080x1920 phone
            // video counts too); larger videos play without it. (Sizes still
            // from the previous file, just after switching, don't count.)
            let same_file = snap.path == self.current_file;
            let pixels = if same_file { snap.dwidth.max(0) * snap.dheight.max(0) } else { 0 };
            let ok = pixels > 0 && pixels <= 1920 * 1088;
            if ok != self.denoise_ok {
                self.denoise_ok = ok;
                self.apply_vf();
            }
            self.adjust_ui.denoise_skipped = self.adjust.denoise > 0 && pixels > 1920 * 1088;
            if now >= self.hold_until {
                // The mpv thread reads the time only ~10x a second. While playing,
                // move on by the elapsed time every frame and ease toward mpv's value,
                // so the seek bar glides instead of jumping in 100 ms steps.
                let playing = !snap.paused && !snap.eof;
                let age = snap.taken.map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64());
                let target = if playing { snap.time_pos + age * self.speed } else { snap.time_pos };
                if playing && (target - self.time_pos).abs() < 0.5 {
                    self.time_pos += dt * self.speed;
                    self.time_pos += (target - self.time_pos) * 0.15;
                } else {
                    self.time_pos = target;
                }
                // (a live stream's "duration" is only what's buffered so far, and
                // moves: the time isn't held to it)
                if self.duration > 0.0 && !self.live {
                    self.time_pos = self.time_pos.clamp(0.0, self.duration);
                }
                self.volume = snap.volume;
                self.is_playing = !snap.paused;
                self.muted = snap.muted;
                self.eof = snap.eof;
            } else if self.is_playing {
                self.time_pos += dt * self.speed;
            }
            self.mpv_info = snap.info;
            self.chapters = snap.chapters;
            // A video just ended: go on to the next one in the folder if wanted
            // (a playlist always plays on)
            let play_on = self.autoplay || self.playlist.is_some();
            if snap.eof && !self.snap_eof && self.has_file && play_on && !self.wants_loop() && self.clip.is_none() {
                if self.has_next() {
                    self.play_adjacent(1);
                } else if self.playlist.is_some() && self.playlist_replay {
                    self.play_index(0); // (the playlist again from the start)
                }
            }
            self.snap_eof = snap.eof;
        }
        // Setting up a clip with "Play the clip on repeat": back to its start each
        // time playback runs past its end (not when sent past it on purpose)
        if let Some((start, end)) = self.clip.as_ref().filter(|c| c.repeat && c.dragging.is_none()).map(|c| (c.start, c.end)) {
            let ran_past = self.is_playing && self.time_pos >= end && self.time_pos < end + 1.5;
            let ended = self.eof && end >= self.duration - 0.5;
            if self.has_file && (ran_past || ended) {
                self.seek_to(start);
                if ended {
                    self.link.set("pause", "no");
                    self.is_playing = true;
                    self.eof = false;
                }
            }
        }
        self.link.want_info(self.show_info);

        // ── Native window handle → taskbar progress + thumbnail buttons ───────
        if self.hwnd == 0 {
            if let Ok(h) = frame.window_handle() {
                if let RawWindowHandle::Win32(w) = h.as_raw() {
                    self.hwnd = w.hwnd.get();
                }
            }
        }
        if self.hwnd != 0 && !self.taskbar_tried {
            self.taskbar_tried = true;
            taskbar::hook_window(self.hwnd);
            self.taskbar = taskbar::Taskbar::new(self.hwnd);
            self.win11_frame = taskbar::round_corners(self.hwnd);
            // (smooth motion needs the window to find the monitor's refresh rate)
            if self.adjust.smooth {
                self.apply_smooth();
            }
        }
        if taskbar::take_fullscreen_failed() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        }

        // Re-apply "maximized" from last time if the window didn't start that way
        if self.start_max && !self.start_max_done && self.hwnd != 0 && self.start_time.elapsed().as_secs_f32() > 0.5 {
            self.start_max_done = true;
            unsafe {
                if IsZoomed(self.hwnd) == 0 {
                    ShowWindow(self.hwnd, 3); // SW_MAXIMIZE
                }
            }
        }
        if let Some(id) = taskbar::take_click() {
            match id {
                taskbar::PREV => self.play_adjacent(-1),
                taskbar::BACK => self.step(-10.0),
                taskbar::PLAY => self.toggle_play_pause(),
                taskbar::FWD => self.step(10.0),
                taskbar::NEXT => self.play_adjacent(1),
                _ => {}
            }
        }
        // Keyboard media keys
        if let Some(key) = taskbar::take_media_key() {
            use taskbar::MediaKey;
            match key {
                MediaKey::PlayPause if self.has_file => self.toggle_play_pause(),
                MediaKey::Play if self.has_file && !self.is_playing => self.toggle_play_pause(),
                MediaKey::Pause | MediaKey::Stop if self.is_playing => self.toggle_play_pause(),
                MediaKey::Next => self.play_adjacent(1),
                MediaKey::Previous => self.play_adjacent(-1),
                _ => {}
            }
        }
        // yt-dlp has updated after a link failed: try it again
        self.poll_link_retry();
        // A folder for Save As…
        if let Some(folder) = taskbar::take_picked_folder() {
            self.start_save(ctx, &folder);
        }
        // A file from the open dialog, or from a second launch of the player
        if let Some(path) = taskbar::take_picked_file() {
            self.open_external(path);
        }
        for path in taskbar::take_forwarded() {
            self.open_arriving(path);
        }
        // Several files opened from Explorer together have all arrived: they
        // become a selected playlist (like dropping them in together)
        if let Some(t) = self.arriving_last {
            if t.elapsed() > std::time::Duration::from_millis(450) && self.render_ctx_initialized {
                self.arriving_last = None;
                let mut files: Vec<PathBuf> = std::mem::take(&mut self.arriving).into_iter().filter(|s| is_media_file(s)).map(PathBuf::from).collect();
                if files.len() > 1 {
                    files.sort_by(|a, b| natural_cmp(&a.file_name().unwrap_or_default().to_string_lossy(), &b.file_name().unwrap_or_default().to_string_lossy()));
                    let entries: Vec<PlaylistEntry> = files.into_iter().map(|path| PlaylistEntry { path, title: None, secs: None }).collect();
                    self.start_playlist(PathBuf::from("Selected files"), entries, true);
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        let status = taskbar::Status {
            has_file: self.has_file,
            playing: self.is_playing,
            has_prev: self.has_prev(),
            has_next: self.has_next(),
        };
        if let Some(tb) = self.taskbar.as_mut() {
            tb.sync(status, self.time_pos, self.duration);
        }

        // Follow the Windows accent colour (changes with wallpaper / Settings)
        if self.accent_checked.elapsed() > std::time::Duration::from_secs(2) {
            self.accent = system_accent();
            self.border_color = border_accent();
            self.accent_borders = accent_borders_on();
            self.accent_checked = std::time::Instant::now();
        }
        if self.show_info {
            self.refresh_info();
        }

        // ── Track window geometry so it can be restored next launch ───────────
        if !self.is_fullscreen {
            let (outer, inner, maximized, minimized) = ctx.input(|i| {
                let v = i.viewport();
                (v.outer_rect, v.inner_rect, v.maximized.unwrap_or(false), v.minimized.unwrap_or(false))
            });
            // egui's own "maximized" flag is unreliable for a frameless window, so ask
            // Windows. For the first moments after starting maximized, trust the saved
            // state: the window may not have been maximized yet.
            let maximized = if self.start_max && self.start_time.elapsed().as_secs_f32() < 3.0 {
                true
            } else if self.hwnd != 0 {
                unsafe { IsZoomed(self.hwnd) != 0 }
            } else {
                maximized
            };
            if let (Some(o), Some(i), false) = (outer, inner, minimized) {
                // (a minimised window reports a position around -32000)
                if o.min.x > -10000.0 && o.min.y > -10000.0 {
                    let mut s = self.win_state;
                    s.maximized = maximized;
                    if !maximized {
                        s.x = o.min.x.round();
                        s.y = o.min.y.round();
                        s.w = i.width().round();
                        s.h = i.height().round();
                        s.ppp = ctx.pixels_per_point();
                    }
                    if s != self.win_state {
                        self.win_state = s;
                        self.win_changed = std::time::Instant::now();
                    }
                }
            }
        }
        // Write it once the window has stopped moving/resizing for a second
        if self.win_state != self.win_saved {
            if self.win_changed.elapsed() > std::time::Duration::from_secs(1) {
                save_window_state(&self.win_state);
                self.win_saved = self.win_state;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(1100));
            }
        }

        // Picture & sound adjustments: saved a second after the last change
        if self.adjust != self.adjust_saved {
            if self.adjust_changed.elapsed() > std::time::Duration::from_secs(1) {
                self.adjust.save(&adjust_path());
                self.adjust_saved = self.adjust.clone();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(1100));
            }
        }

        // Same for volume / mute / repeat / always-on-top
        let cur = self.current_settings();
        if cur != self.settings_last {
            self.settings_last = cur;
            self.settings_changed = std::time::Instant::now();
        }
        if self.settings_last != self.settings_saved {
            if self.settings_changed.elapsed() > std::time::Duration::from_secs(1) {
                save_settings(&self.settings_last);
                self.settings_saved = self.settings_last;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(1100));
            }
        }

        // ── Initialize mpv OpenGL render context once GL is ready ─────────────
        if !self.render_ctx_initialized && frame.gl().is_some() {
            let init_params = OpenGLInitParams {
                get_proc_address: gl_proc_address,
                ctx: std::ptr::null_mut::<std::os::raw::c_void>(),
            };
            let params = vec![
                RenderParam::ApiType(RenderParamApiType::OpenGl),
                RenderParam::InitParams(init_params),
            ];
            let mpv_ref = unsafe {
                std::mem::transmute::<&Mpv, &'static Mpv>(&*self.mpv.lock().unwrap())
            };
            if let Ok(mut rc) = mpv_ref.create_render_context(params) {
                // Register mpv's frame-update callback → send wakeup to our repaint thread
                let tx = self.repaint_tx.clone();
                rc.set_update_callback(move || { let _ = tx.try_send(()); });
                TLS_RENDER_CTX.with(|c| *c.borrow_mut() = Some(rc));
                self.render_ctx_initialized = true;
            }
        }

        if self.render_ctx_initialized {
            if let Some(path) = self.startup_file.take() {
                self.open_file(&path);
            }
            if frame.gl().is_some() {
                self.thumbs.init_gl(ctx, gl_proc_address);
            }
            // Videos only: music and pictures have no thumbnails to get ready
            if matches!(media_kind(&self.current_file), Some(MediaKind::Video)) {
                self.thumbs.warm(ctx, self.time_pos);
            }
        }

        // Minimised (the taskbar timer still wakes us): nothing to draw
        let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        let sc = ctx.screen_rect();
        if minimized || sc.width() < 64.0 || sc.height() < 64.0 {
            return;
        }

        // ── Live resize: pause dithering while the size keeps changing ────────
        // Newer mpv (seen in 0.42) writes the window HEIGHT as a constant into its
        // dithering shader, so every 1 px height change forces a shader recompile
        // before the next frame can be drawn (mpv 0.36 didn't). That made resizing
        // jerky. With dithering off the shader doesn't depend on the size, so it is
        // compiled once; 300 ms after the resize stops, dithering is switched back
        // on (one compile at the final size).
        let now = std::time::Instant::now();
        let frame_ms = now.duration_since(self.last_frame).as_secs_f32() * 1000.0;
        self.last_frame = now;
        if frame_ms > 0.0 {
            self.ui_fps = self.ui_fps * 0.95 + (1000.0 / frame_ms) * 0.05;
        }
        let size = sc.size();
        if size != self.last_size {
            let first = self.last_size == egui::Vec2::ZERO;
            self.last_size = size;
            if !first && self.has_file && self.fs_settle.is_none() {
                self.resize_until = Some(std::time::Instant::now() + std::time::Duration::from_millis(300));
                self.set_resize_mode(true);
            }
        }

        if let Some(until) = self.resize_until {
            if std::time::Instant::now() >= until {
                self.resize_until = None;
                self.set_resize_mode(false);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(60));
            }
        }

        // ── Read all input first (no viewport commands inside the closure) ────
        let screen = ctx.screen_rect();
        let mut inp = ctx.input(|i| InputSnapshot {
            dropped: i.raw.dropped_files.iter()
                .filter_map(|f| f.path.as_ref().map(|p| p.to_string_lossy().into_owned()))
                .collect(),
            space: i.key_pressed(egui::Key::Space),
            right: i.key_pressed(egui::Key::ArrowRight),
            left:  i.key_pressed(egui::Key::ArrowLeft),
            up:    i.key_pressed(egui::Key::ArrowUp),
            down:  i.key_pressed(egui::Key::ArrowDown),
            f:     i.key_pressed(egui::Key::F),
            d:     i.key_pressed(egui::Key::D),
            enter: i.key_pressed(egui::Key::Enter),
            m:     i.key_pressed(egui::Key::M),
            n:     i.key_pressed(egui::Key::N),
            p:     i.key_pressed(egui::Key::P),
            // (not Alt+Tab, and not the key repeating while held)
            info:  !i.modifiers.alt && i.events.iter().any(|e| matches!(e,
                       egui::Event::Key { key: egui::Key::Tab, pressed: true, repeat: false, .. })),
            focus_back: i.events.iter().any(|e| matches!(e, egui::Event::WindowFocused(true))),
            tab_down: i.key_down(egui::Key::Tab),
            hw:    i.key_pressed(egui::Key::H),
            esc:   i.key_pressed(egui::Key::Escape),
            open:  i.modifiers.ctrl && i.key_pressed(egui::Key::O),
            open_url: i.modifiers.ctrl && i.key_pressed(egui::Key::U),
            save: i.modifiers.ctrl && !i.modifiers.shift && i.key_pressed(egui::Key::S),
            clip: i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(egui::Key::S),
            // ([ and ] by the character typed: egui 0.27 has no key codes for them)
            clip_start: i.events.iter().any(|e| matches!(e, egui::Event::Text(t) if t == "[")),
            clip_end: i.events.iter().any(|e| matches!(e, egui::Event::Text(t) if t == "]")),
            zoom: !i.modifiers.ctrl && i.key_pressed(egui::Key::Z),
            pasted: i.events.iter().find_map(|e| if let egui::Event::Paste(t) = e { Some(t.clone()) } else { None }),
            rotate: !i.modifiers.ctrl && i.key_pressed(egui::Key::R),
            adjust: !i.modifiers.ctrl && i.key_pressed(egui::Key::E),
            playlist: !i.modifiers.ctrl && i.key_pressed(egui::Key::L),
            scroll_y: i.raw_scroll_delta.y,
            pressed: i.pointer.primary_pressed(),
            moved: i.pointer.delta().length_sq() > 0.5,
            active: i.pointer.any_down()
                || i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. })),
            pointer: i.pointer.hover_pos(),
        });
        if self.url_box.is_some() || self.clip.as_ref().is_some_and(|c| c.editing.is_some()) {
            inp.keys_for_typing();
        }
        // Alt+Tab back to the player, letting go of Alt a moment before Tab: Windows
        // hands the window a Tab press as it comes to the front. Ignore Tab from
        // then until it has been let go.
        if inp.focus_back {
            self.tab_blocked = true;
        }
        if self.tab_blocked {
            inp.info = false;
            if !inp.tab_down {
                self.tab_blocked = false;
            }
        }

        // ── Controls visibility logic ─────────────────────────────────────────
        if inp.moved || inp.active || inp.scroll_y != 0.0 {
            self.last_mouse_move = std::time::Instant::now();
        }
        let near_edge = inp.pointer
            .is_some_and(|p| p.y < TOP_H || p.y > screen.bottom() - BOTTOM_H || self.adjust_ui.contains(p));
        let menu_open = self.main_menu || self.show_keys || self.speed_menu;
        let idle = self.last_mouse_move.elapsed().as_secs_f32();
        let settling = self.fs_settle.is_some_and(|t0| t0.elapsed().as_secs_f32() < FS_SETTLE);
        if !settling {
            self.fs_settle = None;
        }
        // Fullscreen ended from outside the app (Win+Down, snapping, a monitor
        // change...): the window no longer covers its monitor. Leave fullscreen mode
        // too, or the window would keep refusing to be dragged or resized.
        if self.is_fullscreen && !settling && self.hwnd != 0 && !taskbar::still_fullscreen(self.hwnd) {
            self.is_fullscreen = false;
            self.show_controls = true;
            taskbar::forget_fullscreen(self.hwnd);
        }
        if self.is_fullscreen && self.has_file {
            // Fullscreen: the controls show while the pointer is over the bottom of
            // the screen, the filename / menu only while it's over the top. Right after
            // switching the pointer position is stale, so neither reacts to it until
            // the mouse has moved (or left the edges).
            if !settling && (!near_edge || inp.moved) {
                self.fs_armed = true;
            }
            let armed = !settling && self.fs_armed;
            let over_top = inp.pointer.is_some_and(|p| p.y < TOP_H);
            let over_bottom = inp.pointer.is_some_and(|p| p.y > screen.bottom() - BOTTOM_H);
            let speed_menu = self.speed_menu;
            // (setting up a clip: its ends are on the seek bar, so it stays)
            self.show_controls = speed_menu || self.clip.is_some() || (armed && over_bottom);
            self.show_top = self.main_menu || (armed && over_top);
        } else if settling {
            self.show_controls = true;
            self.show_top = true;
        } else {
            self.show_controls = !self.has_file || !self.is_playing || menu_open || near_edge || idle < HIDE_AFTER || self.clip.is_some();
            self.show_top = self.show_controls;
        }
        // Hide the mouse cursor too once it has been still for a moment
        if !self.show_controls && !self.show_top && self.has_file && idle > 2.0 {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }

        // ── Actions ───────────────────────────────────────────────────────────
        // Several files dropped at once: a playlist of them, in Explorer's name order
        let media: Vec<&String> = inp.dropped.iter().filter(|s| is_media_file(s)).collect();
        if media.len() > 1 {
            let mut files: Vec<PathBuf> = media.iter().map(PathBuf::from).collect();
            files.sort_by(|a, b| natural_cmp(&a.file_name().unwrap_or_default().to_string_lossy(), &b.file_name().unwrap_or_default().to_string_lossy()));
            let entries: Vec<PlaylistEntry> = files.into_iter().map(|path| PlaylistEntry { path, title: None, secs: None }).collect();
            self.start_playlist(PathBuf::from("Dropped files"), entries, true);
        } else {
            for s in &inp.dropped {
                if is_media_file(s) || is_playlist_file(s) { self.open_file(s); }
            }
        }
        if inp.space { self.toggle_play_pause(); }
        if inp.right { self.step(5.0); }
        if inp.left  { self.step(-5.0); }
        if inp.up    { self.nudge_volume(5.0); }
        if inp.down  { self.nudge_volume(-5.0); }
        if inp.m     { let m = !self.muted; self.set_mute(m); self.volume_osd = Some(std::time::Instant::now()); self.seek_osd = None; }
        if inp.n     { self.play_adjacent(1); }
        if inp.p     { self.play_adjacent(-1); }
        if inp.info {
            self.show_info = !self.show_info;
            // (it opens where Picture & Sound / Online video settings do)
            if self.show_info {
                self.adjust_ui.open = false;
                self.adjust_ui.eq_open = false;
                self.adjust_ui.online_open = false;
            }
        }
        if inp.hw    { self.toggle_hw_decode(); }
        if inp.open  { self.main_menu = false; self.show_open_dialog(); }
        if inp.open_url { self.show_url_box(); }
        if inp.save { self.ask_save_folder(); }
        if inp.clip { self.start_clip(); }
        if let Some(c) = self.clip.as_mut() {
            if inp.clip_start {
                c.set(ClipEnd::Start, self.time_pos, self.duration);
            }
            if inp.clip_end {
                c.set(ClipEnd::End, self.time_pos, self.duration);
            }
            if inp.zoom {
                c.zoom = !c.zoom;
                c.refit(self.duration);
            }
        }
        // Ctrl+V on the player with a link on the clipboard plays it straight away
        if let Some(url) = inp.pasted.as_deref().and_then(normalize_url) {
            self.open_url(&url);
        }
        if inp.rotate { self.rotate_90(); }
        if inp.playlist && self.playlist.is_some() {
            self.toggle_playlist_panel();
        }
        if inp.adjust {
            self.adjust_ui.open = !self.adjust_ui.open;
            if self.adjust_ui.open {
                self.show_info = false;
            }
            self.adjust_ui.online_open = false;
        }

        // Mouse wheel anywhere in the window = volume, 5 % per notch
        // (egui reports one wheel notch as 50 points). Not over the Picture & sound
        // panels (the wheel adjusts the setting there) or the shortcuts list.
        let over_panels = inp.pointer.is_some_and(|p| self.adjust_ui.contains(p));
        if !over_panels && !self.show_keys {
            self.scroll_accum += inp.scroll_y;
        }
        let steps = (self.scroll_accum / 50.0).trunc();
        if steps != 0.0 {
            self.scroll_accum -= steps * 50.0;
            self.nudge_volume(steps as f64 * 5.0);
        }

        if inp.f     { self.frame_step(true); }
        if inp.d     { self.frame_step(false); }
        if inp.enter { self.toggle_fullscreen(ctx); }
        if inp.esc {
            if let Some(b) = self.url_box.as_mut().filter(|b| b.menu.is_some()) {
                b.menu = None;
            } else if self.url_box.is_some() {
                self.url_box = None;
            } else if let Some(c) = self.clip.as_mut().filter(|c| c.editing.is_some()) {
                c.editing = None; // (stop typing, keep the old time)
            } else if self.clip.is_some() {
                self.clip = None;
            } else if self.show_keys {
                self.show_keys = false;
            } else if self.speed_menu {
                self.speed_menu = false;
            } else if self.main_menu {
                self.main_menu = false;
            } else if self.adjust_ui.playlist_open {
                self.adjust_ui.playlist_open = false;
            } else if self.adjust_ui.open || self.adjust_ui.eq_open || self.adjust_ui.online_open {
                self.adjust_ui.open = false;
                self.adjust_ui.eq_open = false;
                self.adjust_ui.online_open = false;
            } else if self.show_info {
                self.show_info = false;
            } else if self.is_fullscreen {
                self.exit_fullscreen(ctx);
            }
        }

        // ── Resize from the window edges (we have no OS frame) ────────────────
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        let edge = if self.is_fullscreen || maximized {
            None
        } else {
            inp.pointer.and_then(|p| hit_edge(p, screen))
        };
        if let Some((dir, _)) = edge {
            if inp.pressed {
                ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
            }
        }

        // ── Video panel: full-bleed, the controls float on top of it ──────────
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(Color32::BLACK))
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                let resp = ui.allocate_rect(rect, Sense::click_and_drag());
                if resp.hovered() {
                    self.blank_hover = true;
                }

                // Double-click → toggle fullscreen
                if resp.double_clicked() {
                    self.toggle_fullscreen(ctx);
                }

                // Drag anywhere on the video (left button) → move the window
                if resp.drag_started_by(egui::PointerButton::Primary) && !self.is_fullscreen && edge.is_none() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }

                let music = self.is_music();
                if !music {
                    self.scope.stop();
                }
                self.video_shown = false;
                if !self.has_file {
                    splash::draw(ui.painter(), rect, self.accent);
                } else if music {
                    self.draw_music(ui, rect);
                } else if self.render_ctx_initialized && self.url_load.is_none() {
                    // (While a link is opening: just black. See VIDEO_STALE for a
                    // new file.)
                    ui.painter().add(video_callback(rect));
                    self.video_shown = self.video_kind == VideoKind::Video;
                }
            });

        // ── Overlay: title bar + controls (fade in/out) ───────────────────────
        // (0 s fade while switching fullscreen on/off: the bars must not visibly fade out)
        let fade = if self.fs_settle.is_some() { 0.0 } else { 0.2 };
        let t = ctx.animate_bool_with_time(egui::Id::new("overlay_vis"), self.show_controls, fade);
        let t_top = ctx.animate_bool_with_time(egui::Id::new("overlay_top"), self.show_top, fade);
        if self.fs_settle.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        if t_top > 0.0 {
            draw_top_bar(self, ctx, t_top);
        }
        // (a right-click menu doesn't wait for the title bar to fade in)
        if t_top > 0.0 || self.menu_at.is_some() {
            draw_main_menu(self, ctx);
        } else {
            self.main_menu = false;
        }
        if t > 0.0 {
            draw_bottom_bar(self, ctx, t);
            draw_speed_menu(self, ctx);
        } else {
            self.speed_menu = false;
        }
        glass::set_style(self.glass);

        // Right-click on the video, or on the title / control bars over it: the
        // main menu, where the button is let go (like Windows' own right-click
        // menus; holding it and moving first works too)
        let (r_down, r_up, at) = ctx.input(|i| {
            (
                i.pointer.button_pressed(egui::PointerButton::Secondary),
                i.pointer.button_released(egui::PointerButton::Secondary),
                i.pointer.latest_pos(),
            )
        });
        if r_down {
            // (only on a blank part: not on buttons, the seek bar, sliders, panels)
            self.right_press = self.right_click_menu && (self.blank_hover || self.blank_hover_prev);
        }
        if r_up && std::mem::take(&mut self.right_press) {
            if let Some(p) = at {
                self.main_menu = true;
                self.menu_at = Some(p);
                self.menu_sub = None;
                self.speed_menu = false;
            }
        }

        // A click on the video itself (not on the bars, menus or panels) closes the
        // menu and the Picture & Sound / equalizer panels
        let on_video = ctx.input(|i| i.pointer.any_pressed().then(|| i.pointer.press_origin()).flatten())
            .and_then(|p| ctx.layer_id_at(p))
            .is_some_and(|l| l.order == egui::Order::Background);
        if on_video {
            self.main_menu = false;
            self.speed_menu = false;
            self.adjust_ui.open = false;
            self.adjust_ui.eq_open = false;
            self.adjust_ui.online_open = false;
            self.url_box = None;
        }

        // Picture & sound panel and the equalizer
        let (before, before_norm) = (self.adjust.clone(), self.normalize);
        let hq_before = self.hq;
        adjust::draw_panel(ctx, &mut self.adjust_ui, &mut self.adjust, &mut self.normalize, &mut self.hq, self.accent);
        if self.hq != hq_before {
            self.link.set("scale", if self.hq { HQ_SCALE } else { NORMAL_SCALE });
        }
        adjust::draw_eq(ctx, &mut self.adjust_ui, &mut self.adjust, self.accent);
        if self.adjust != before || self.normalize != before_norm {
            self.adjust_changed = std::time::Instant::now();
            self.apply_adjust_changes(&before, before_norm);
        }
        draw_shortcuts(self, ctx);

        // Online video settings panel, the Open URL box, and a link opening / failed
        let online_before = self.online;
        adjust::draw_online(ctx, &mut self.adjust_ui, &mut self.online, self.accent);
        self.draw_playlist(ctx);
        if self.online != online_before {
            self.apply_online();
        }
        // Save clip as…: its panel at the top
        if let Some(c) = self.clip.as_mut() {
            match adjust::draw_clip_panel(ctx, c, self.duration, self.time_pos, self.accent) {
                adjust::ClipAction::Save => self.ask_clip_folder(),
                adjust::ClipAction::Close => self.clip = None,
                adjust::ClipAction::None => {}
            }
        }
        if let Some(b) = self.url_box.as_mut() {
            match adjust::draw_url_box(ctx, b, self.accent) {
                adjust::UrlBoxAction::Open(text) => {
                    if let Some(url) = normalize_url(&text) {
                        self.open_url(&url);
                    }
                }
                adjust::UrlBoxAction::Cancel => self.url_box = None,
                adjust::UrlBoxAction::None => {}
            }
        }
        if self.has_file || self.link_error.is_some() || self.link_retry.is_some() {
            draw_link_status(self, ctx);
        }

        // The window's title as Windows shows it (taskbar thumbnail, Alt+Tab)
        let title = if self.has_file { format!("{} - RustMediaPlayer", self.file_name()) } else { "RustMediaPlayer".to_owned() };
        if title != self.os_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.os_title = title;
        }

        // The save card: with the title bar while saving; on its own for a few
        // seconds once it has saved (or failed), then it goes
        if let Some(job) = self.save.as_mut() {
            const NEWS_SECS: f32 = 8.0;
            let stage = job.stage();
            if matches!(stage, SaveStage::Done(_) | SaveStage::Failed(_)) && job.ended.is_none() {
                job.ended = Some(std::time::Instant::now());
            }
            let news = job.ended.map_or(0.0, |t0| ((NEWS_SECS - t0.elapsed().as_secs_f32()) / 0.4).clamp(0.0, 1.0));
            if job.ended.is_some() {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
            // (stays up the whole time it's saving, whatever the bars are doing)
            let alpha = if job.ended.is_some() { news } else { 1.0 };
            match adjust::draw_save_card(ctx, &job.title, &stage, job.progress_note(), alpha, self.accent) {
                // (closing it while it's saving cancels: see SaveJob's drop)
                adjust::SaveCardAction::Close => self.save = None,
                adjust::SaveCardAction::ShowInFolder => {
                    if let SaveStage::Done(path) = &stage {
                        show_in_folder(path);
                    }
                }
                adjust::SaveCardAction::None => {
                    if job.ended.is_some() && news <= 0.0 {
                        self.save = None;
                    }
                }
            }
        }

        // Resize cursor wins over any other cursor set this frame
        if let Some((_, icon)) = edge {
            ctx.set_cursor_icon(icon);
        }

        // Volume / seek pop-up (top left) for a moment after a change
        let osd_alpha = |t0: std::time::Instant| {
            let age = t0.elapsed().as_secs_f32();
            (age < 1.6).then(|| if age > 1.2 { 1.0 - (age - 1.2) / 0.4 } else { 1.0 })
        };
        if let Some(t0) = self.volume_osd {
            if let Some(alpha) = osd_alpha(t0) {
                draw_volume_osd(ctx, self.volume, self.muted, self.accent, alpha);
                ctx.request_repaint_after(std::time::Duration::from_millis(30));
            } else {
                self.volume_osd = None;
            }
        }
        if let Some((t0, kind)) = self.seek_osd {
            if let Some(alpha) = osd_alpha(t0) {
                let target = self.drag_frac.map_or(self.time_pos, |f| f as f64 * self.duration);
                draw_seek_osd(ctx, kind, target, self.duration, self.accent, alpha);
                ctx.request_repaint_after(std::time::Duration::from_millis(30));
            } else {
                self.seek_osd = None;
            }
        }

        // Window border: the menu's "Accent color border" decides
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        let accent_border = self.border;
        if self.win11_frame {
            // Windows 11 draws it (with the rounded corners): accent colour while
            // active when the option is on; otherwise the usual grey, as on a window
            // with Windows' accent-border setting off; none in fullscreen
            let want = if self.is_fullscreen {
                taskbar::BORDER_NONE
            } else if accent_border && focused {
                let c = self.border_color;
                c.r() as u32 | (c.g() as u32) << 8 | (c.b() as u32) << 16
            } else if self.accent_borders {
                // Windows' own setting would colour it: ask for plain grey instead
                taskbar::BORDER_GREY
            } else {
                taskbar::BORDER_DEFAULT
            };
            if self.border_sent != Some(want) {
                taskbar::set_border_color(self.hwnd, want);
                self.border_sent = Some(want);
            }
        } else if accent_border && !self.is_fullscreen && !(self.hwnd != 0 && unsafe { IsZoomed(self.hwnd) != 0 }) {
            // Windows 10: our own 1 px line (Windows doesn't draw one for a window
            // without a standard frame); grey while another window is active
            let col = if focused { self.border_color } else { Color32::from_rgb(70, 70, 72) };
            let px = 1.0 / ctx.pixels_per_point(); // exactly one screen pixel
            ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("window_border")))
                .rect_stroke(screen.shrink(px / 2.0), 0.0, Stroke::new(px, col));
        }

        // Technical info overlay (Tab). It appears once mpv's details have arrived
        // (a moment after Tab), so it never shows up half filled, and fades in/out.
        let info_visible = self.show_info && self.has_file && !self.mpv_info.is_empty();
        let info_t = ctx.animate_bool_with_time(egui::Id::new("info_fade"), info_visible, 0.12);
        if info_t > 0.0 {
            draw_info_overlay(ctx, &self.info_rows, info_t);
            if info_visible {
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
        } else {
            ctx.data_mut(|d| d.remove::<f32>(egui::Id::new("info_value_w")));
        }

        // Schedule a time-display refresh every ~250ms while playing.
        // mpv's update callback handles actual frame repaints — we only need
        // this slow tick for the seek-bar clock and the auto-hide timer.
        if self.is_playing {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    /// Reached when the window is closed some way other than our own close button
    /// (Alt+F4, taskbar). Normal teardown of mpv / GL / audio is slow and crashed
    /// while a video was open, so save the window position and end the process.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.quit();
    }
}

/// A playlist entry's title tag and length, read on another thread: (which
/// playlist, entry, title, length)
type PlaylistFound = (u64, usize, Option<String>, Option<f64>);

struct InputSnapshot {
    dropped: Vec<String>,
    space: bool, right: bool, left: bool, up: bool, down: bool,
    f: bool, d: bool, enter: bool, m: bool, n: bool, p: bool, info: bool, hw: bool, esc: bool, open: bool, rotate: bool, adjust: bool,
    /// L: the playlist panel (when a playlist is playing)
    playlist: bool,
    open_url: bool,
    save: bool,
    /// Save clip as… (Ctrl+Shift+S), and while setting one up: [ / ] set the
    /// start / end to now, Z zooms the seek bar in or out
    clip: bool,
    clip_start: bool,
    clip_end: bool,
    zoom: bool,
    /// The window came back to the front this frame, and whether Tab is held
    focus_back: bool,
    tab_down: bool,
    /// Text pasted with Ctrl+V this frame
    pasted: Option<String>,
    scroll_y: f32,
    pressed: bool,
    moved: bool, active: bool,
    pointer: Option<Pos2>,
}

impl InputSnapshot {
    /// While typing in the Open URL box, the keys are the text field's, not the
    /// player's (Esc still closes the box)
    fn keys_for_typing(&mut self) {
        self.space = false;
        self.right = false;
        self.left = false;
        self.up = false;
        self.down = false;
        self.f = false;
        self.d = false;
        self.enter = false;
        self.m = false;
        self.n = false;
        self.p = false;
        self.info = false;
        self.hw = false;
        self.open = false;
        self.rotate = false;
        self.adjust = false;
        self.playlist = false;
        self.open_url = false;
        self.save = false;
        self.clip = false;
        self.clip_start = false;
        self.clip_end = false;
        self.zoom = false;
        self.pasted = None;
    }
}

// ── Fonts: Golos Text (built in), then Segoe UI / egui's defaults as fallback ─
/// Which Windows icon font supplies the minimize / maximize / close glyphs
#[derive(Clone, Copy, PartialEq)]
enum CaptionFont {
    /// Segoe Fluent Icons (Windows 11)
    Fluent,
    /// Segoe MDL2 Assets (Windows 10)
    Mdl2,
    /// Neither found: draw the glyphs with lines
    None,
}

fn install_fonts(ctx: &egui::Context) -> CaptionFont {
    let mut fonts = egui::FontDefinitions::default();

    // The exact icon font Windows draws its own title-bar buttons with
    let mut caption = CaptionFont::None;
    for (path, kind) in [
        (r"C:\Windows\Fonts\SegoeIcons.ttf", CaptionFont::Fluent),
        (r"C:\Windows\Fonts\segmdl2.ttf", CaptionFont::Mdl2),
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert("caption".into(), egui::FontData::from_owned(bytes));
            caption = kind;
            break;
        }
    }

    if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
        fonts.font_data.insert("segoe".into(), egui::FontData::from_owned(bytes));
        if let Some(fam) = fonts.families.get_mut(&FontFamily::Proportional) {
            fam.insert(0, "segoe".into());
        }
    }
    // Emoji and symbols in titles and file names (🟢, ★, ✓, …): Windows' own emoji
    // and symbol fonts, after Segoe UI. (egui draws text in one colour, so emoji
    // come out as white shapes rather than in colour; egui's built-in emoji font is
    // too old for newer ones, which were drawn blank.)
    let mut at = 1;
    for (name, path) in [("segoe_emoji", r"C:\Windows\Fonts\seguiemj.ttf"), ("segoe_symbol", r"C:\Windows\Fonts\seguisym.ttf")] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(name.into(), egui::FontData::from_owned(bytes));
            if let Some(fam) = fonts.families.get_mut(&FontFamily::Proportional) {
                let i = at.min(fam.len());
                fam.insert(i, name.into());
                at = i + 1;
            }
        }
    }

    // Golos Text (Paratype; free under the SIL Open Font License, whose notice is
    // stored inside the font files) is built into the .exe and used for all text.
    // Segoe UI stays behind it as a fallback for characters Golos doesn't have.
    fonts.font_data.insert(
        "golos".into(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/GolosText-Regular.ttf")),
    );
    fonts.font_data.insert(
        "golos_sb".into(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/GolosText-SemiBold.ttf")),
    );
    if let Some(fam) = fonts.families.get_mut(&FontFamily::Proportional) {
        fam.insert(0, "golos".into());
    }

    // "semibold" family always exists (falls back to the proportional fonts)
    let mut semibold = fonts.families
        .get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\seguisb.ttf") {
        fonts.font_data.insert("segoe_sb".into(), egui::FontData::from_owned(bytes));
        semibold.insert(0, "segoe_sb".into());
    }
    semibold.insert(0, "golos_sb".into());
    fonts.families.insert(FontFamily::Name("semibold".into()), semibold);

    // The icon font has no ordinary characters, and egui refuses (panics) to build
    // a font family that lacks a "?" / "◻" glyph. So the caption family is the icon
    // font first, then the normal fonts as fallback.
    if caption != CaptionFont::None {
        let mut family: Vec<String> = vec!["caption".into()];
        family.extend(fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default());
        fonts.families.insert(FontFamily::Name("caption".into()), family);
    }

    ctx.set_fonts(fonts);
    caption
}

// ── Window edge resizing ──────────────────────────────────────────────────────
/// Which resize edge/corner (if any) the pointer is on, with its cursor icon
fn hit_edge(p: Pos2, screen: Rect) -> Option<(egui::viewport::ResizeDirection, egui::CursorIcon)> {
    use egui::viewport::ResizeDirection as D;
    use egui::CursorIcon as C;
    const EDGE: f32 = 6.0;
    const CORNER: f32 = 14.0;

    let (dl, dr) = (p.x - screen.left(), screen.right() - p.x);
    let (dt, db) = (p.y - screen.top(), screen.bottom() - p.y);

    // Over the minimize / maximize / close buttons only a thin strip along the edge
    // resizes, so the buttons stay easy to hit
    if p.y < WIN_BTN_H && p.x > screen.right() - WIN_BTN_W * 3.0 {
        if dt < BTN_EDGE && dr < CORNER {
            return Some((D::NorthEast, C::ResizeNeSw));
        }
        if dt < BTN_EDGE {
            return Some((D::North, C::ResizeVertical));
        }
        return None;
    }

    if dl < CORNER && dt < CORNER { return Some((D::NorthWest, C::ResizeNwSe)); }
    if dr < CORNER && dt < CORNER { return Some((D::NorthEast, C::ResizeNeSw)); }
    if dl < CORNER && db < CORNER { return Some((D::SouthWest, C::ResizeNeSw)); }
    if dr < CORNER && db < CORNER { return Some((D::SouthEast, C::ResizeNwSe)); }
    if dl < EDGE { return Some((D::West,  C::ResizeHorizontal)); }
    if dr < EDGE { return Some((D::East,  C::ResizeHorizontal)); }
    if dt < EDGE { return Some((D::North, C::ResizeVertical)); }
    if db < EDGE { return Some((D::South, C::ResizeVertical)); }
    None
}

// ── Windows accent colour ─────────────────────────────────────────────────────
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn reg_dword(sub_key: &str, value: &str) -> Option<u32> {
    let (key, val) = (wide(sub_key), wide(value));
    let mut data: u32 = 0;
    let mut len: u32 = 4;
    // HKEY_CURRENT_USER = 0x80000001 (sign-extended to pointer size); RRF_RT_REG_DWORD = 0x10
    let rc = unsafe {
        RegGetValueW(
            0x8000_0001_u32 as i32 as isize,
            key.as_ptr(),
            val.as_ptr(),
            0x10,
            std::ptr::null_mut(),
            &mut data as *mut u32 as *mut std::ffi::c_void,
            &mut len,
        )
    };
    (rc == 0).then_some(data)
}

/// The accent colour the controls use (seek and volume bars, switches, selected
/// items): halfway between Windows' calm title-bar accent (the window border's
/// colour) and a fully vivid version of it, so it stands out on the dark control
/// bar without glaring.
fn system_accent() -> Color32 {
    let c = border_accent();
    let (h, s, v) = rgb_to_hsv(c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0);
    // The vivid end: more saturated, near full brightness (genuinely grey accents stay grey)
    let s_vivid = if s < 0.12 { s } else { (s * 1.25).clamp(0.65, 1.0) };
    let v_vivid = v.max(0.96);
    let (r, g, b) = hsv_to_rgb(h, (s + s_vivid) / 2.0, (v + v_vivid) / 2.0);
    Color32::from_rgb((r * 255.0).round() as u8, (g * 255.0).round() as u8, (b * 255.0).round() as u8)
}

/// Windows' "Show accent colour on title bars and window borders" (Settings →
/// Personalisation → Colours)
fn accent_borders_on() -> bool {
    reg_dword(r"Software\Microsoft\Windows\DWM", "ColorPrevalence") == Some(1)
}

/// The window border colour: Windows' title-bar accent (what Explorer's title bar
/// shows), lifted slightly the way Windows draws its own 1 px borders.
fn border_accent() -> Color32 {
    const ACCENT_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent";
    let abgr = reg_dword(ACCENT_KEY, "AccentColorMenu")
        .or_else(|| reg_dword(r"Software\Microsoft\Windows\DWM", "AccentColor"));
    let Some(v) = abgr else { return Color32::from_rgb(0, 95, 184) };
    let ch = |c: u32| ((c & 0xFF) as f32 * 0.9 + 223.0 * 0.1).round() as u8;
    Color32::from_rgb(ch(v), ch(v >> 8), ch(v >> 16))
}

/// h in 0..360, s and v in 0..1
fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, if max == 0.0 { 0.0 } else { d / max }, max)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 % 6 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (r + m, g + m, b + m)
}

fn fmt_time(secs: f64) -> String {
    let s = if secs.is_finite() && secs > 0.0 { secs as u64 } else { 0 };
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// Load libmpv's DLL from the player's folder. If it can't be loaded, say why in
/// a message box and return false (the player then closes).
fn libmpv_loads() -> bool {
    let name = option_env!("MPV_DLL_NAME").unwrap_or("libmpv-2.dll");
    if unsafe { LoadLibraryW(wide(name).as_ptr()) } != 0 {
        return true;
    }
    let there = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(name)))
        .is_some_and(|p| p.is_file());
    let text = if there {
        format!(
            "{name} is in the player's folder but Windows couldn't load it. It may be damaged, \
             or be a build for a different kind of PC.\n\nDownload the player again, and keep \
             {name} in the same folder as RustMediaPlayer.exe."
        )
    } else {
        format!(
            "{name} is missing.\n\nRust Media Player plays videos with libmpv, which comes as \
             {name} in the same download as the player. Put it back in the same folder as \
             RustMediaPlayer.exe and start the player again."
        )
    };
    let (text, title) = (wide(&text), wide("Rust Media Player"));
    unsafe { MessageBoxW(0, text.as_ptr(), title.as_ptr(), 0x10 /* MB_ICONERROR */) };
    false
}

fn main() -> eframe::Result<()> {
    // If anything ever panics, say so (and keep a log) instead of vanishing silently
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("{info}");
        if let Some(path) = settings_path().map(|p| p.with_file_name("crash.log")) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, &msg);
        }
        let text = wide(&format!("Rust Media Player hit an error and has to close:\n\n{msg}"));
        let title = wide("Rust Media Player");
        unsafe { MessageBoxW(0, text.as_ptr(), title.as_ptr(), 0x10 /* MB_ICONERROR */); }
    }));

    // libmpv is delay-loaded (see build.rs): load it now, before anything uses it,
    // so a missing DLL gets a clear message rather than a crash
    if !libmpv_loads() {
        return Ok(());
    }
    // Anything the player starts (yt-dlp and the Deno it runs, ffmpeg; also
    // mpv's own yt-dlp runs) ends when the player does, however it closes
    taskbar::end_children_with_player();

    // Single window: if the player is already running, hand it the file (it plays
    // it in place of the current one) and leave
    let arg_files: Vec<String> = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .filter(|p| p.is_file() || is_url(&p.to_string_lossy()))
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    // (unless multiple windows are allowed in the menu)
    if taskbar::forward_to_running_instance(&arg_files, !load_settings().multi) {
        return Ok(());
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("RustMediaPlayer")
        .with_inner_size([960.0, 540.0])
        .with_min_inner_size([480.0, 300.0])
        // We draw our own title bar and window buttons
        .with_decorations(false)
        .with_drag_and_drop(true);
    // Reopen where (and how big) the window was last time
    if let Some(w) = load_window_state() {
        viewport = viewport
            .with_inner_size([w.w.max(480.0), w.h.max(300.0)])
            .with_position([w.x, w.y]);
        if w.maximized {
            viewport = viewport.with_maximized(true);
        }
    }
    if load_settings().on_top {
        viewport = viewport.with_window_level(egui::WindowLevel::AlwaysOnTop);
    }
    // Window / taskbar icon
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "Rust Media Player",
        options,
        Box::new(|cc| Box::new(FluentMediaPlayer::new(cc))),
    )
}
