#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontFamily, FontId, Pos2, Rect, Sense, Stroke};
use libmpv2::{Mpv, render::{RenderContext, OpenGLInitParams, RenderParam, RenderParamApiType}};
mod mpv_link;
mod splash;
mod taskbar;
mod thumbs;

use mpv_link::{yn, MpvLink};

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
    fn MultiByteToWideChar(code_page: u32, flags: u32, src: *const u8, src_len: i32, dst: *mut u16, dst_len: i32) -> i32;
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
    /// The most recent single render call, unsmoothed (for resize statistics)
    static RENDER_LAST_MS: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
    static TLS_RENDER_CTX: RefCell<Option<RenderContext<'static>>> = const { RefCell::new(None) };
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

// ── Remembered window position / size ─────────────────────────────────────────
#[derive(Clone, Copy, PartialEq)]
struct WinState {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    /// pixels_per_point (display scaling) when it was saved
    ppp: f32,
    maximized: bool,
}

impl WinState {
    const DEFAULT: WinState = WinState { x: 0.0, y: 0.0, w: 960.0, h: 540.0, ppp: 1.0, maximized: false };
}

fn settings_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("rust-mediaplayer").join("settings.txt"))
}

/// What's remembered between runs (besides window position)
#[derive(Clone, Copy, PartialEq)]
struct Settings {
    volume: i32,
    muted: bool,
    repeat: bool,
    on_top: bool,
    /// Hardware decoding (H). Off by default, like mpv itself.
    hwdec: bool,
    /// Play the next video in the folder when one ends (menu). Off by default.
    autoplay: bool,
}

fn save_settings(st: &Settings) {
    let Some(path) = settings_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(
        path,
        format!(
            "{} {} {} {} {} {}",
            st.volume, st.muted as u8, st.repeat as u8, st.on_top as u8, st.hwdec as u8, st.autoplay as u8
        ),
    );
}

/// Settings from last time (older files with fewer fields still load);
/// 80 % volume and everything off on first run
fn load_settings() -> Settings {
    let text = settings_path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    let mut it = text.split_whitespace();
    let volume = it.next().and_then(|t| t.parse::<f64>().ok());
    let mut flag = || it.next().and_then(|t| t.parse::<u8>().ok()) == Some(1);
    let (muted, repeat, on_top, hwdec, autoplay) = (flag(), flag(), flag(), flag(), flag());
    Settings {
        volume: volume.map_or(80.0, |v| v.clamp(0.0, VOL_MAX)).round() as i32,
        muted,
        repeat,
        on_top,
        hwdec,
        autoplay,
    }
}

fn state_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("rust-mediaplayer").join("window.txt"))
}

fn save_window_state(s: &WinState) {
    let Some(path) = state_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(
        path,
        format!("{} {} {} {} {} {}", s.x, s.y, s.w, s.h, s.ppp, s.maximized as u8),
    );
}

/// The saved state, if it exists and the window would still be on a connected monitor
fn load_window_state() -> Option<WinState> {
    let text = std::fs::read_to_string(state_path()?).ok()?;
    let v: Vec<f32> = text.split_whitespace().filter_map(|t| t.parse().ok()).collect();
    if v.len() < 6 { return None; }
    let s = WinState { x: v[0], y: v[1], w: v[2], h: v[3], ppp: v[4], maximized: v[5] != 0.0 };
    if !(s.w >= 200.0 && s.h >= 150.0 && s.w <= 20000.0 && s.h <= 20000.0 && s.ppp > 0.0) {
        return None;
    }

    // Is (most of) the title area inside the virtual desktop? (monitor unplugged → no)
    let (vx, vy, vw, vh) = unsafe {
        (GetSystemMetrics(76), GetSystemMetrics(77), GetSystemMetrics(78), GetSystemMetrics(79))
    };
    if vw > 0 && vh > 0 {
        let (px, py, pw) = (s.x * s.ppp, s.y * s.ppp, s.w * s.ppp);
        let (vx, vy, vw, vh) = (vx as f32, vy as f32, vw as f32, vh as f32);
        let visible = px + pw > vx + 100.0 && px < vx + vw - 100.0 && py > vy - 20.0 && py < vy + vh - 80.0;
        if !visible { return None; }
    }
    Some(s)
}

// ── Palette ───────────────────────────────────────────────────────────────────
/// Used only if the Windows accent colour can't be read
const DEFAULT_ACCENT: Color32 = Color32::from_rgb(0, 120, 212);
const KNOB_RING:  Color32 = Color32::from_rgb(186, 186, 188); // muted light ring around the knob
const TRACK:      Color32 = Color32::from_rgb(190, 184, 180); // unplayed seek track
const VOL_TRACK:  Color32 = Color32::from_rgb(160, 158, 155); // unplayed volume track

// ── Layout (points) ───────────────────────────────────────────────────────────
const TOP_H: f32 = 64.0;              // title strip + gradient height
const BOTTOM_H: f32 = 104.0;          // control area + gradient height
const SIDE: f32 = 20.0;               // left/right margin of the seek bar (and the button row)
const SEEK_FROM_BOTTOM: f32 = 53.0;   // seek bar centre line
const ROW_FROM_BOTTOM: f32 = 27.0;    // button row centre line
/// Control-row buttons: hover highlight size and centre-to-centre spacing
const BTN_BOX: f32 = 28.0;
const BTN_STEP: f32 = 32.0;
/// Volume slider track length
const VOL_W: f32 = 90.0;
const WIN_BTN_W: f32 = 46.0;
const WIN_BTN_H: f32 = 34.0;
const MORE_POPUP: &str = "more_popup";
/// Menu button (top left): centre. Its highlight sits 8 px from the left, 7 px from the top.
const MENU_X: f32 = 8.0 + BTN_BOX / 2.0;
const MENU_Y: f32 = 7.0 + BTN_BOX / 2.0;

/// The pop-up in the top right after seeking with the keys (accumulated amount)
/// or the mouse (seek bar dragged from this time)
#[derive(Clone, Copy)]
enum SeekOsd {
    Step(f64),
    Drag(f64),
}
/// Highest volume (percent). mpv allows up to 130 by default.
const VOL_MAX: f64 = 130.0;

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
    mpv_info: Vec<String>,
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
    /// Accumulated mouse-wheel scroll (points) not yet converted into volume steps
    scroll_accum: f32,
    /// Windows accent colour (re-read every couple of seconds so changes follow live)
    accent: Color32,
    accent_checked: std::time::Instant,
    /// When the volume was last changed by wheel/keys — drives the volume pop-up
    volume_osd: Option<std::time::Instant>,
    /// Technical info overlay (toggle with I)
    show_info: bool,
    info_lines: Vec<String>,
    /// Hardware decoding on/off (toggle with H)
    hw_decode: bool,
    /// File given on the command line (drop on the .exe / "Open with"), played once ready
    startup_file: Option<String>,
    /// The window was maximized when last closed: re-apply it at startup
    start_max: bool,
    start_max_done: bool,
    start_time: std::time::Instant,
    /// Fullscreen transition under way: overlays are held steady (no fade) until it settles
    fs_settle: Option<std::time::Instant>,
    /// In fullscreen the overlays only appear once the mouse has left the screen edges
    fs_armed: bool,
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
    rs_stats: (u32, f32, f32, f32),
    /// Native window handle and the taskbar integration built on it
    hwnd: isize,
    taskbar: Option<taskbar::Taskbar>,
    taskbar_tried: bool,
    /// Volume / mute: last seen, last written, and when they last changed
    settings_last: Settings,
    settings_saved: Settings,
    settings_changed: std::time::Instant,
    /// Current and last-written window geometry
    win_state: WinState,
    win_saved: WinState,
    win_changed: std::time::Instant,
    /// Seek pop-up (top right), and when it was last refreshed
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
    menu_anchor: Rect,
    /// Previous frame, for moving the time display on smoothly
    last_tick: std::time::Instant,
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
                && self.ctx.input(|i| i.pointer.latest_pos()).map_or(false, |p| self.rect.contains(p)))
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
        let h_mod = GetModuleHandleA(b"opengl32.dll\0".as_ptr() as *const _);
        GetProcAddress(h_mod, c_name.as_ptr())
    }
}

impl FluentMediaPlayer {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let caption_font = install_fonts(&cc.egui_ctx);
        let saved = load_window_state().unwrap_or(WinState::DEFAULT);
        let st = load_settings();
        let (volume, muted) = (st.volume as f64, st.muted);
        // A file passed on the command line: drag a video onto the .exe, double-click an
        // associated file, or "Open with". (Explorer passes the path as the first argument.)
        let startup_file = std::env::args_os()
            .skip(1)
            .map(PathBuf::from)
            .find(|p| p.is_file())
            .map(|p| p.to_string_lossy().into_owned());

        // video-timing-offset = 0 stops mpv from blocking the UI thread inside
        // render() while it waits for each frame's display time (the default 50 ms
        // wait made window moves/resizes follow the video's frame rate).
        let mpv = Mpv::with_initializer(|init| {
            // Draw video ONLY through our render context, never in a window of mpv's
            // own. Older libmpv builds happened to pick this automatically; newer
            // ones default to their own video output (a separate "mpv" window).
            let _ = init.set_property("vo", "libmpv");
            // mpv's own detailed log (overwritten every launch), for diagnosing
            // differences between libmpv versions
            if let Some(log) = settings_path().map(|p| p.with_file_name("mpv.log")) {
                if let Some(dir) = log.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = init.set_property("log-file", log.to_string_lossy().as_ref());
            }
            let _ = init.set_property("video-timing-offset", 0.0_f64);
            // Decoding: software by default (mpv's own default, and the newer mpv's
            // hardware decoder garbled frames on this setup). H switches to hardware
            // decoding with copy-back ("auto-copy"); the choice is remembered.
            let _ = init.set_property("hwdec", if st.hwdec { "auto-copy" } else { "no" });
            // Don't decode straight into GPU-mapped buffers (another known source of
            // garbled frames on some drivers).
            let _ = init.set_property("vd-lavc-dr", "no");
            // One decoding thread while hardware decoding. mpv 0.36 did this itself;
            // newer mpv runs the hardware decoder multithreaded, which produced
            // garbled blocks with copy-back decoding on AMD. (Software decoding gets
            // automatic threads again, see toggle_hw_decode.)
            let _ = init.set_property("vd-lavc-threads", if st.hwdec { "1" } else { "0" });
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
        let _ = mpv.set_property("volume", volume);
        let _ = mpv.set_property("mute", muted);
        if st.repeat {
            let _ = mpv.set_property("loop-file", "inf");
        }
        // Frame pacing: mpv's default (sync video to audio). "display-resample" needs
        // per-frame vsync reports that this embedding doesn't provide.
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

        Self {
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
            scroll_accum: 0.0,
            accent: system_accent(),
            accent_checked: std::time::Instant::now(),
            volume_osd: None,
            show_info: false,
            info_lines: Vec::new(),
            hw_decode: st.hwdec,
            startup_file,
            start_max: saved.maximized,
            start_max_done: false,
            start_time: std::time::Instant::now(),
            fs_settle: None,
            fs_armed: true,
            caption_font,
            last_size: egui::Vec2::ZERO,
            resize_until: None,
            resize_mode: false,
            last_frame: std::time::Instant::now(),
            rs_stats: (0, 0.0, 0.0, 0.0),
            hwnd: 0,
            taskbar: None,
            taskbar_tried: false,
            settings_last: st,
            settings_saved: st,
            settings_changed: std::time::Instant::now(),
            win_state: saved,
            win_saved: saved,
            win_changed: std::time::Instant::now(),
            seek_osd: None,
            chapters: Arc::new(Vec::new()),
            thumbs: thumbs::Thumbs::new(),
            autoplay: st.autoplay,
            snap_eof: false,
            main_menu: false,
            menu_anchor: Rect::NOTHING,
            last_tick: std::time::Instant::now(),
        }
    }

    /// Open a file the user picked (drop, Open dialog, command line). For a playlist,
    /// previous / next go through its entries; otherwise through the files of the
    /// same kind (videos / music / pictures) in the file's folder.
    fn open_file(&mut self, path: &str) {
        if is_playlist_file(path) {
            let entries = read_playlist(Path::new(path));
            let Some(first) = entries.first().map(|p| p.to_string_lossy().into_owned()) else { return };
            self.playlist = Some(PathBuf::from(path));
            self.folder_files = entries;
            self.folder_index = Some(0);
            self.load_entry(&first);
        } else {
            self.playlist = None;
            self.folder_files = scan_folder(Path::new(path));
            self.folder_index = self.folder_files.iter().position(|p| p == Path::new(path));
            self.load_entry(path);
        }
    }

    /// Start playing `path` (keeps the current previous / next list)
    fn load_entry(&mut self, path: &str) {
        self.current_file = path.to_owned();
        self.has_file = true;
        // Back to "subtitles off" for every new file, even if they were switched on
        // for the previous one
        self.link.set("sid", "no");
        self.link.set("loop-file", if self.wants_loop() { "inf" } else { "no" });
        self.link.command("loadfile", &[path]);
        self.link.set("pause", "no");
        self.is_playing = true;
        self.time_pos = 0.0;
        self.eof = false;
        self.chapters = Arc::new(Vec::new());
        self.thumbs.load(path);
        self.touch();
    }

    /// Open a file handed to us from outside (open dialog, a second launch)
    fn open_external(&mut self, path: String) {
        if !Path::new(&path).is_file() {
            return;
        }
        if self.render_ctx_initialized {
            self.open_file(&path);
        } else {
            self.startup_file = Some(path);
        }
    }

    /// Windows' "Open" dialog, starting in the current video's folder
    fn show_open_dialog(&mut self) {
        let dir = Path::new(&self.current_file)
            .parent()
            .map(|d| d.to_string_lossy().into_owned())
            .filter(|d| !d.is_empty());
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

    /// Switch hardware decoding on/off (handy if a video shows glitches)
    fn toggle_hw_decode(&mut self) {
        self.hw_decode = !self.hw_decode;
        // Threads must be set before the decoder is re-opened by the hwdec change
        self.link.set("vd-lavc-threads", if self.hw_decode { "1" } else { "0" });
        self.link.set("hwdec", if self.hw_decode { "auto-copy" } else { "no" });
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
        }
    }

    /// Remember the window geometry, then end the process at once
    fn quit(&mut self) -> ! {
        if self.win_state != self.win_saved {
            save_window_state(&self.win_state);
        }
        let cur = self.current_settings();
        if cur != self.settings_saved {
            save_settings(&cur);
        }
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

    /// Gather decoder / video details for the info overlay
    fn refresh_info(&mut self) {
        let mut lines = self.mpv_info.clone();
        lines.push(format!(
            "Hardware decoding: {}  (H to switch)",
            if self.hw_decode { "on" } else { "off" }
        ));
        let render_ms = RENDER_MS.with(|r| r.get());
        lines.push(format!(
            "Render: {render_ms:.1} ms per frame{}",
            if self.resize_mode { " (resize settings active)" } else { "" }
        ));
        let (n, sum, worst, worst_render) = self.rs_stats;
        if n > 0 {
            lines.push(format!(
                "Last resize: {n} frames, avg {:.0} ms, worst {worst:.0} ms (mpv render worst {worst_render:.0} ms)",
                sum / n as f32
            ));
        }
        if let Some((ws, private)) = process_memory_mb() {
            lines.push(format!("Memory: {ws:.0} MB working set, {private:.0} MB private"));
        }
        self.info_lines = lines;
    }

    fn set_mute(&mut self, mute: bool) {
        self.muted = mute;
        self.link.set("mute", yn(mute));
        self.touch();
    }

    fn toggle_repeat(&mut self) {
        self.repeat = !self.repeat;
        self.link.set("loop-file", if self.wants_loop() { "inf" } else { "no" });
    }

    /// Repeat is on, or the file is a GIF (those always loop, like in a browser)
    fn wants_loop(&self) -> bool {
        self.repeat || has_ext(&self.current_file, &["gif"])
    }

    fn cycle_captions(&mut self) {
        self.link.command("cycle", &["sub"]);
    }

    fn set_speed(&mut self, speed: f64) {
        self.speed = speed;
        self.link.set("speed", speed);
    }

    fn has_prev(&self) -> bool {
        self.folder_index.map_or(false, |i| i > 0)
    }

    fn has_next(&self) -> bool {
        self.folder_index.map_or(false, |i| i + 1 < self.folder_files.len())
    }

    /// Open the previous (-1) or next (+1) video in the current file's folder
    fn play_adjacent(&mut self, delta: isize) {
        let Some(i) = self.folder_index else { return };
        let target = i as isize + delta;
        if target < 0 || target as usize >= self.folder_files.len() { return; }
        let path = self.folder_files[target as usize].to_string_lossy().into_owned();
        self.folder_index = Some(target as usize);
        self.load_entry(&path);
    }

    fn skip(&mut self, secs: f64) {
        let new_pos = (self.time_pos + secs).clamp(0.0, self.duration);
        self.seek_to(new_pos);
    }

    /// Skip with the keys / taskbar buttons, and show "+10 s" (adding up quick repeats
    /// in the same direction)
    fn step(&mut self, secs: f64) {
        if !self.has_file {
            return;
        }
        self.skip(secs);
        let total = match self.seek_osd {
            Some((t0, SeekOsd::Step(prev))) if t0.elapsed().as_secs_f32() < 1.2 && prev.signum() == secs.signum() => prev + secs,
            _ => secs,
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
        // Hold the overlays hidden, with no fade, while the window changes size
        self.fs_settle = Some(std::time::Instant::now());
        self.fs_armed = false;
        self.show_controls = false;
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
        std::path::Path::new(&self.current_file)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_owned()
    }
}

impl eframe::App for FluentMediaPlayer {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // ── Player state from the mpv thread (never waits for mpv) ────────────
        let snap = self.link.snapshot();
        let now = std::time::Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64().min(0.25);
        self.last_tick = now;
        if snap.valid {
            self.duration = snap.duration;
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
                if self.duration > 0.0 {
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
            if snap.eof && !self.snap_eof && self.has_file && play_on && !self.repeat && self.has_next() {
                self.play_adjacent(1);
            }
            self.snap_eof = snap.eof;
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
        // A file from the open dialog, or from a second launch of the player
        if let Some(path) = taskbar::take_picked_file() {
            self.open_external(path);
        }
        if let Some(path) = taskbar::take_forwarded() {
            self.open_external(path);
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
        if self.resize_until.is_some() {
            let render_ms = RENDER_LAST_MS.with(|r| r.get());
            let st = &mut self.rs_stats;
            *st = (st.0 + 1, st.1 + frame_ms, st.2.max(frame_ms), st.3.max(render_ms));
        }
        let size = sc.size();
        if size != self.last_size {
            if self.resize_until.is_none() {
                self.rs_stats = (0, 0.0, 0.0, 0.0); // a new resize begins
            }
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
        let inp = ctx.input(|i| InputSnapshot {
            dropped: i.raw.dropped_files.iter()
                .filter_map(|f| f.path.as_ref().map(|p| p.to_string_lossy().into_owned()))
                .collect(),
            space: i.key_pressed(egui::Key::Space),
            right: i.key_pressed(egui::Key::ArrowRight),
            left:  i.key_pressed(egui::Key::ArrowLeft),
            up:    i.key_pressed(egui::Key::ArrowUp),
            down:  i.key_pressed(egui::Key::ArrowDown),
            f:     i.key_pressed(egui::Key::F),
            m:     i.key_pressed(egui::Key::M),
            n:     i.key_pressed(egui::Key::N),
            p:     i.key_pressed(egui::Key::P),
            info:  i.key_pressed(egui::Key::I),
            hw:    i.key_pressed(egui::Key::H),
            esc:   i.key_pressed(egui::Key::Escape),
            open:  i.modifiers.ctrl && i.key_pressed(egui::Key::O),
            scroll_y: i.raw_scroll_delta.y,
            pressed: i.pointer.primary_pressed(),
            moved: i.pointer.delta().length_sq() > 0.5,
            active: i.pointer.any_down()
                || i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. })),
            pointer: i.pointer.hover_pos(),
        });

        // ── Controls visibility logic ─────────────────────────────────────────
        if inp.moved || inp.active || inp.scroll_y != 0.0 {
            self.last_mouse_move = std::time::Instant::now();
        }
        let near_edge = inp.pointer
            .map_or(false, |p| p.y < TOP_H || p.y > screen.bottom() - BOTTOM_H);
        let menu_open = self.main_menu || ctx.memory(|m| m.is_popup_open(egui::Id::new(MORE_POPUP)));
        let idle = self.last_mouse_move.elapsed().as_secs_f32();
        let settling = self.fs_settle.map_or(false, |t0| t0.elapsed().as_secs_f32() < 0.45);
        if !settling {
            self.fs_settle = None;
        }
        if self.is_fullscreen && self.has_file {
            // Fullscreen: everything stays hidden until the mouse has left the screen
            // edges and then comes back to the top/bottom edge. (Right after switching,
            // the pointer position is stale, so nothing may pop up during the transition.)
            if !near_edge {
                self.fs_armed = true;
            }
            self.show_controls = !settling && self.fs_armed && (near_edge || menu_open);
        } else if settling {
            self.show_controls = true;
        } else {
            self.show_controls = !self.has_file || !self.is_playing || menu_open || near_edge || idle < 2.5;
        }
        // Hide the mouse cursor too once it has been still for a moment
        if !self.show_controls && self.has_file && idle > 2.0 {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }

        // ── Actions ───────────────────────────────────────────────────────────
        for s in &inp.dropped {
            if is_media_file(s) || is_playlist_file(s) { self.open_file(s); }
        }
        if inp.space { self.toggle_play_pause(); }
        if inp.right { self.step(10.0); }
        if inp.left  { self.step(-10.0); }
        if inp.up    { self.nudge_volume(5.0); }
        if inp.down  { self.nudge_volume(-5.0); }
        if inp.m     { let m = !self.muted; self.set_mute(m); self.volume_osd = Some(std::time::Instant::now()); self.seek_osd = None; }
        if inp.n     { self.play_adjacent(1); }
        if inp.p     { self.play_adjacent(-1); }
        if inp.info  { self.show_info = !self.show_info; }
        if inp.hw    { self.toggle_hw_decode(); }
        if inp.open  { self.main_menu = false; self.show_open_dialog(); }


        // Mouse wheel anywhere in the window = volume, 5 % per notch
        // (egui reports one wheel notch as 50 points)
        self.scroll_accum += inp.scroll_y;
        let steps = (self.scroll_accum / 50.0).trunc();
        if steps != 0.0 {
            self.scroll_accum -= steps * 50.0;
            self.nudge_volume(steps as f64 * 5.0);
        }

        if inp.f     { self.toggle_fullscreen(ctx); }
        if inp.esc {
            if self.main_menu {
                self.main_menu = false;
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

                // Double-click → toggle fullscreen
                if resp.double_clicked() {
                    self.toggle_fullscreen(ctx);
                }
                // Drag anywhere on the video → move the window
                if resp.drag_started() && !self.is_fullscreen && edge.is_none() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }

                if !self.has_file {
                    splash::draw(ui.painter(), rect, self.accent);
                } else if self.render_ctx_initialized {
                    ui.painter().add(egui::PaintCallback {
                        rect,
                        callback: Arc::new(egui_glow::CallbackFn::new(move |info, _painter| {
                            // Render at the real pixel size so HiDPI scaling stays correct
                            let vp = info.viewport_in_pixels();
                            let started = std::time::Instant::now();
                            TLS_RENDER_CTX.with(|c| {
                                if let Some(rc) = c.borrow_mut().as_mut() {
                                    // Acknowledge mpv's update first (its docs recommend calling
                                    // this after every update callback)
                                    let _ = rc.update();
                                    let _ = rc.render::<*mut std::os::raw::c_void>(
                                        0, vp.width_px, vp.height_px, true,
                                    );
                                }
                            });
                            let ms = started.elapsed().as_secs_f32() * 1000.0;
                            RENDER_MS.with(|r| r.set(r.get() * 0.8 + ms * 0.2));
                            RENDER_LAST_MS.with(|r| r.set(ms));
                        })),
                    });
                }
            });

        // ── Overlay: title bar + controls (fade in/out) ───────────────────────
        // (0 s fade while switching fullscreen on/off: the bars must not visibly fade out)
        let fade = if self.fs_settle.is_some() { 0.0 } else { 0.2 };
        let t = ctx.animate_bool_with_time(egui::Id::new("overlay_vis"), self.show_controls, fade);
        if self.fs_settle.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        if t > 0.0 {
            draw_top_bar(self, ctx, t);
            draw_bottom_bar(self, ctx, t);
            draw_main_menu(self, ctx);
        } else {
            self.main_menu = false;
        }

        // Resize cursor wins over any other cursor set this frame
        if let Some((_, icon)) = edge {
            ctx.set_cursor_icon(icon);
        }

        // Volume / seek pop-up (top right) for a moment after a change
        let osd_alpha = |t0: std::time::Instant| {
            let age = t0.elapsed().as_secs_f32();
            (age < 1.6).then(|| if age > 1.2 { 1.0 - (age - 1.2) / 0.4 } else { 1.0 })
        };
        if let Some(t0) = self.volume_osd {
            if let Some(alpha) = osd_alpha(t0) {
                draw_volume_osd(ctx, self.volume, self.muted, self.accent, alpha, self.is_fullscreen);
                ctx.request_repaint_after(std::time::Duration::from_millis(30));
            } else {
                self.volume_osd = None;
            }
        }
        if let Some((t0, kind)) = self.seek_osd {
            if let Some(alpha) = osd_alpha(t0) {
                let target = self.drag_frac.map_or(self.time_pos, |f| f as f64 * self.duration);
                draw_seek_osd(ctx, kind, target, self.duration, self.accent, alpha, self.is_fullscreen);
                ctx.request_repaint_after(std::time::Duration::from_millis(30));
            } else {
                self.seek_osd = None;
            }
        }

        // Technical info overlay (I)
        if self.show_info && self.has_file {
            draw_info_overlay(ctx, &self.info_lines);
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
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

struct InputSnapshot {
    dropped: Vec<String>,
    space: bool, right: bool, left: bool, up: bool, down: bool,
    f: bool, m: bool, n: bool, p: bool, info: bool, hw: bool, esc: bool, open: bool,
    scroll_y: f32,
    pressed: bool,
    moved: bool, active: bool,
    pointer: Option<Pos2>,
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

// ── Small drawing helpers ─────────────────────────────────────────────────────
fn black(alpha: u8, t: f32) -> Color32 {
    Color32::from_black_alpha((alpha as f32 * t) as u8)
}

/// Vertical two-colour gradient rectangle
fn gradient(p: &egui::Painter, rect: Rect, top: Color32, bottom: Color32) {
    let mut m = egui::Mesh::default();
    m.colored_vertex(rect.left_top(), top);
    m.colored_vertex(rect.right_top(), top);
    m.colored_vertex(rect.right_bottom(), bottom);
    m.colored_vertex(rect.left_bottom(), bottom);
    m.add_triangle(0, 1, 2);
    m.add_triangle(0, 2, 3);
    p.add(egui::Shape::mesh(m));
}

/// Polyline given as offsets from a centre point
fn poly(p: &egui::Painter, c: Pos2, pts: &[(f32, f32)], col: Color32, w: f32) {
    let v: Vec<Pos2> = pts.iter().map(|&(x, y)| pos2(c.x + x, c.y + y)).collect();
    p.add(egui::Shape::line(v, Stroke::new(w, col)));
}

/// Click target (one full step wide, so neighbours never overlap) with a faint
/// rounded hover highlight (8 px radius). Draw the icon afterwards.
fn hit(ui: &mut egui::Ui, centre: Pos2, id: &str, tip: &str, t: f32) -> egui::Response {
    hit_at(ui, centre, id, tip, t, true)
}

/// `hit` with the tooltip above (`above`) or below the button
fn hit_at(ui: &mut egui::Ui, centre: Pos2, id: &str, tip: &str, t: f32, above: bool) -> egui::Response {
    let r = ui.interact(
        Rect::from_center_size(centre, vec2(BTN_STEP, 34.0)),
        ui.id().with(id),
        // drags starting on a button must not move the window (see the window buttons)
        Sense::click_and_drag(),
    );
    if r.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        ui.painter().rect_filled(
            Rect::from_center_size(centre, vec2(BTN_BOX, BTN_BOX)),
            8.0,
            Color32::from_white_alpha((8.0 * t) as u8),
        );
    }
    hover_tooltip(ui.ctx(), &r, Rect::from_center_size(centre, vec2(BTN_BOX, BTN_BOX)), tip, above);
    r
}

// ── Tooltips ──────────────────────────────────────────────────────────────────
/// Show `text` near `anchor` once the pointer has rested on `resp` for a moment.
/// Placed above (bottom controls) or below (window buttons) from the start, instead
/// of egui's default of trying below the pointer and flipping when it doesn't fit.
fn hover_tooltip(ctx: &egui::Context, resp: &egui::Response, anchor: Rect, text: &str, above: bool) {
    const DELAY: f64 = 0.5;
    let key = resp.id.with("tip_since");
    if !resp.hovered() {
        ctx.data_mut(|d| d.remove::<f64>(key));
        return;
    }
    if resp.is_pointer_button_down_on() {
        // Clicked: no tooltip until the pointer leaves and comes back (like Windows)
        ctx.data_mut(|d| d.insert_temp(key, f64::INFINITY));
        return;
    }
    let now = ctx.input(|i| i.time);
    let since = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(key, || now));
    if !since.is_finite() {
        return;
    }
    let age = now - since;
    if age < DELAY {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(DELAY - age));
        return;
    }
    let alpha = ((age - DELAY) / 0.12).min(1.0) as f32; // quick fade-in
    if alpha < 1.0 {
        ctx.request_repaint();
    }
    draw_tooltip(ctx, anchor, text, above, alpha);
}

/// Windows 11-style dark tooltip: near-opaque dark grey, hairline border,
/// small corner radius and a faint shadow.
fn draw_tooltip(ctx: &egui::Context, anchor: Rect, text: &str, above: bool, alpha: f32) {
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("rmp_tooltip")));
    let galley = p.layout_no_wrap(
        text.to_owned(),
        FontId::proportional(12.5),
        Color32::from_rgb(235, 235, 235).gamma_multiply(alpha),
    );
    let pad = vec2(9.0, 5.0);
    let size = galley.size() + pad * 2.0;
    let screen = ctx.screen_rect();
    let x = (anchor.center().x - size.x / 2.0)
        .min(screen.right() - 6.0 - size.x)
        .max(screen.left() + 6.0)
        .round();
    let y = if above { anchor.top() - 6.0 - size.y } else { anchor.bottom() + 6.0 }.round();
    let rect = Rect::from_min_size(pos2(x, y), size);
    let r = 5.0;

    // faint shadow
    for (e, a) in [(1.0, 22.0), (2.5, 12.0), (4.0, 6.0)] {
        p.rect_filled(rect.expand(e).translate(vec2(0.0, 1.5)), r + e, Color32::from_black_alpha((a * alpha) as u8));
    }
    p.rect_filled(rect, r, Color32::from_rgba_unmultiplied(43, 43, 43, (245.0 * alpha) as u8));
    p.rect_stroke(rect.shrink(0.5), r, Stroke::new(1.0_f32, Color32::from_white_alpha((20.0 * alpha) as u8)));
    p.galley(rect.min + pad, galley, Color32::WHITE);
}

// ── Icons (drawn with shapes so they never depend on font coverage) ───────────
const SW: f32 = 1.35;
/// Overall scale of the control-row icons
const K: f32 = 0.92;

/// Like `poly`, with the offsets scaled by the icon scale `K`
fn kpoly(p: &egui::Painter, c: Pos2, pts: &[(f32, f32)], col: Color32, w: f32) {
    let v: Vec<Pos2> = pts.iter().map(|&(x, y)| pos2(c.x + x * K, c.y + y * K)).collect();
    p.add(egui::Shape::line(v, Stroke::new(w, col)));
}

fn kclosed(p: &egui::Painter, c: Pos2, pts: &[(f32, f32)], col: Color32, w: f32) {
    let v: Vec<Pos2> = pts.iter().map(|&(x, y)| pos2(c.x + x * K, c.y + y * K)).collect();
    p.add(egui::Shape::closed_line(v, Stroke::new(w, col)));
}

fn icon_pause(p: &egui::Painter, c: Pos2, col: Color32) {
    p.rect_filled(Rect::from_center_size(pos2(c.x - 3.5 * K, c.y), vec2(2.6 * K, 13.0 * K)), 0.5, col);
    p.rect_filled(Rect::from_center_size(pos2(c.x + 3.5 * K, c.y), vec2(2.6 * K, 13.0 * K)), 0.5, col);
}
fn icon_play(p: &egui::Painter, c: Pos2, col: Color32) {
    p.add(egui::Shape::convex_polygon(
        vec![
            pos2(c.x - 4.5 * K, c.y - 7.5 * K),
            pos2(c.x - 4.5 * K, c.y + 7.5 * K),
            pos2(c.x + 7.5 * K, c.y),
        ],
        col, Stroke::NONE,
    ));
}
fn icon_prev(p: &egui::Painter, c: Pos2, col: Color32) {
    p.line_segment([pos2(c.x - 6.5 * K, c.y - 6.0 * K), pos2(c.x - 6.5 * K, c.y + 6.0 * K)], Stroke::new(SW, col));
    kclosed(p, c, &[(6.5, -6.0), (6.5, 6.0), (-3.0, 0.0)], col, SW);
}
fn icon_next(p: &egui::Painter, c: Pos2, col: Color32) {
    p.line_segment([pos2(c.x + 6.5 * K, c.y - 6.0 * K), pos2(c.x + 6.5 * K, c.y + 6.0 * K)], Stroke::new(SW, col));
    kclosed(p, c, &[(-6.5, -6.0), (-6.5, 6.0), (3.0, 0.0)], col, SW);
}
fn icon_volume(p: &egui::Painter, c: Pos2, col: Color32, level: f64, muted: bool) {
    let c = pos2(c.x - 3.0 * K, c.y);
    kclosed(p, c, &[(-6.0, -3.0), (-3.0, -3.0), (1.5, -7.0), (1.5, 7.0), (-3.0, 3.0), (-6.0, 3.0)], col, SW);
    if muted || level <= 0.0 {
        kpoly(p, c, &[(5.0, -3.5), (10.5, 3.5)], col, SW);
        kpoly(p, c, &[(5.0, 3.5), (10.5, -3.5)], col, SW);
    } else {
        let arc = |r: f32| -> Vec<(f32, f32)> {
            (-4..=4).map(|k| {
                let a = (k as f32) * 11.0_f32.to_radians();
                (1.5 + r * a.cos(), r * a.sin())
            }).collect()
        };
        kpoly(p, c, &arc(5.0), col, SW);
        if level >= 40.0 { kpoly(p, c, &arc(9.0), col, SW); }
    }
}
fn icon_repeat(p: &egui::Painter, c: Pos2, col: Color32) {
    kpoly(p, c, &[(-8.0, 2.0), (-8.0, -2.0), (-5.0, -5.0), (6.0, -5.0)], col, SW);
    kpoly(p, c, &[(3.5, -7.5), (6.0, -5.0), (3.5, -2.5)], col, SW);
    kpoly(p, c, &[(8.0, -2.0), (8.0, 2.0), (5.0, 5.0), (-6.0, 5.0)], col, SW);
    kpoly(p, c, &[(-3.5, 2.5), (-6.0, 5.0), (-3.5, 7.5)], col, SW);
}
fn icon_captions(p: &egui::Painter, c: Pos2, col: Color32) {
    p.rect_stroke(Rect::from_center_size(c, vec2(18.0 * K, 13.0 * K)), 2.0, Stroke::new(SW, col));
    kpoly(p, c, &[(-5.0, -1.0), (-1.5, -1.0)], col, SW);
    kpoly(p, c, &[(1.5, -1.0), (5.0, -1.0)], col, SW);
    kpoly(p, c, &[(-5.0, 2.5), (2.0, 2.5)], col, SW);
}
fn icon_compact(p: &egui::Painter, c: Pos2, col: Color32) {
    p.rect_stroke(Rect::from_center_size(c, vec2(17.0 * K, 13.0 * K)), 1.5, Stroke::new(SW, col));
    p.rect_filled(Rect::from_min_size(pos2(c.x + 0.5 * K, c.y - 0.5 * K), vec2(6.0 * K, 4.5 * K)), 0.5, col);
}
fn icon_fullscreen(p: &egui::Painter, c: Pos2, col: Color32) {
    kpoly(p, c, &[(-6.5, 6.5), (6.5, -6.5)], col, SW);
    kpoly(p, c, &[(6.5, -1.5), (6.5, -6.5), (1.5, -6.5)], col, SW);
    kpoly(p, c, &[(-6.5, 1.5), (-6.5, 6.5), (-1.5, 6.5)], col, SW);
}
/// Playback speed: a speedometer, an open dial with a needle pointing up-right
fn icon_speed(p: &egui::Painter, c: Pos2, col: Color32) {
    let c = pos2(c.x, c.y + 1.5 * K); // the dial is open at the bottom: sit it a little lower
    let r = 8.0;
    // 270° arc from bottom-left over the top to bottom-right (screen y points down)
    let dial: Vec<(f32, f32)> = (0..=24)
        .map(|i| {
            let a = (135.0 + 270.0 * i as f32 / 24.0_f32).to_radians();
            (r * a.cos(), r * a.sin())
        })
        .collect();
    kpoly(p, c, &dial, col, SW);
    let n = (-50.0_f32).to_radians();
    kpoly(p, c, &[(0.0, 0.0), (5.5 * n.cos(), 5.5 * n.sin())], col, SW + 0.15);
    p.circle_filled(c, 1.7 * K, col);
}

/// Slider thumb: light ring with an accent dot, both circles share one centre that is
/// snapped to the physical pixel grid so the dot can never look off-centre.
fn draw_knob(p: &egui::Painter, ppp: f32, centre: Pos2, hot: bool, accent: Color32, t: f32) {
    let snap = |v: f32| (v * ppp).round() / ppp;
    let c = pos2(snap(centre.x), snap(centre.y));
    let (ring, dot) = if hot { (7.5, 4.6) } else { (6.5, 3.9) };
    p.circle_filled(pos2(c.x, c.y + 1.0), ring + 0.8, Color32::from_black_alpha((80.0 * t) as u8));
    p.circle_filled(c, ring, KNOB_RING.gamma_multiply(t));
    p.circle_filled(c, dot, accent);
}

// ── Title bar (overlaid on the video) ─────────────────────────────────────────
fn draw_top_bar(app: &mut FluentMediaPlayer, ctx: &egui::Context, t: f32) {
    let screen = ctx.screen_rect();
    let bar = Rect::from_min_size(screen.min, vec2(screen.width(), TOP_H));
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    let fullscreen = app.is_fullscreen;
    let title = if app.has_file { app.file_name() } else { "Rust Media Player".to_owned() };
    let white = Color32::WHITE.gamma_multiply(t);

    egui::Area::new(egui::Id::new("top_bar"))
        .order(egui::Order::Foreground)
        .fixed_pos(bar.min)
        .show(ctx, |ui| {
            ui.allocate_rect(bar, Sense::hover());
            let p = ui.painter().clone();
            gradient(&p, bar, black(150, t), Color32::TRANSPARENT);

            // Fullscreen: the bar covers the top edge of the screen, so it must
            // handle the double-click itself (the video underneath can't see it)
            if fullscreen {
                let bg = ui.interact(bar, ui.id().with("fs_bg"), Sense::click());
                if bg.double_clicked() {
                    app.toggle_fullscreen(ctx);
                }
            }

            // Drag region: move the window, double-click to maximise
            // (The whole strip, not just its top part: the strip covers the video,
            // so any part of it that doesn't drag is a dead spot.)
            if !fullscreen {
                let drag = ui.interact(bar, ui.id().with("drag"), Sense::click_and_drag());
                let pressed_on_edge = ctx
                    .input(|i| i.pointer.press_origin())
                    .map_or(false, |o| hit_edge(o, screen).is_some());
                if drag.drag_started() && !pressed_on_edge {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                if drag.double_clicked() {
                    app.toggle_maximize(ctx);
                }
            }

            // Left: menu button, then the file name level with it
            let mc = pos2(MENU_X, MENU_Y);
            let menu = hit_at(ui, mc, "menu", "Menu", t, false);
            app.menu_anchor = menu.rect;
            if app.main_menu && !menu.hovered() {
                // keep it highlighted while its menu is open
                p.rect_filled(Rect::from_center_size(mc, vec2(BTN_BOX, BTN_BOX)), 8.0, Color32::from_white_alpha((8.0 * t) as u8));
            }
            for dy in [-4.5, 0.0, 4.5] {
                p.line_segment([pos2(mc.x - 7.0, mc.y + dy), pos2(mc.x + 7.0, mc.y + dy)], Stroke::new(SW, white));
            }
            if menu.activated() {
                app.main_menu = !app.main_menu;
            }

            let y = MENU_Y;
            let right_limit = if fullscreen { screen.right() - 20.0 } else { screen.right() - WIN_BTN_W * 3.0 - 10.0 };
            let tx = MENU_X + BTN_BOX / 2.0 + 8.0;
            let clip = p.with_clip_rect(Rect::from_min_max(pos2(tx, 0.0), pos2(right_limit, TOP_H)));
            let font = FontId::new(14.5, FontFamily::Name("semibold".into()));
            clip.text(pos2(tx + 1.0, y + 1.0), Align2::LEFT_CENTER, &title, font.clone(), black(170, t));
            clip.text(pos2(tx, y), Align2::LEFT_CENTER, &title, font, white);

            // Right: window buttons (not shown in fullscreen)
            if !fullscreen {
                for k in 0..3 {
                    let r = Rect::from_min_size(
                        pos2(screen.right() - WIN_BTN_W * (3 - k) as f32, screen.top()),
                        vec2(WIN_BTN_W, WIN_BTN_H),
                    );
                    // click_and_drag: a button must also "own" drags that start on it,
                    // otherwise egui hands the drag to the draggable title strip behind
                    // it and the window moves
                    let resp = ui.interact(r, ui.id().with(("winbtn", k)), Sense::click_and_drag());
                    let label = match k {
                        0 => "Minimize",
                        1 if maximized => "Restore Down",
                        1 => "Maximize",
                        _ => "Close",
                    };
                    hover_tooltip(ctx, &resp, r, label, false);
                    // Like Windows: lighter while held down (darker for close), and no
                    // highlight while held with the pointer moved off the button
                    let held = resp.is_pointer_button_down_on();
                    let inside = ctx.input(|i| i.pointer.latest_pos()).map_or(false, |pp| r.contains(pp));
                    let pressed = held && inside;
                    if pressed || (resp.hovered() && !held) {
                        // Windows' own colours (the close red differs on 10 and 11)
                        let fill = if k == 2 {
                            let red = if app.caption_font == CaptionFont::Mdl2 {
                                Color32::from_rgb(232, 17, 35)
                            } else {
                                Color32::from_rgb(196, 43, 28)
                            };
                            red.gamma_multiply(if pressed { 0.72 } else { 1.0 } * t)
                        } else {
                            Color32::from_white_alpha((if pressed { 38.0 } else { 22.0 } * t) as u8)
                        };
                        p.rect_filled(r, 0.0, fill);
                    }
                    let c = r.center();
                    let s = Stroke::new(1.1_f32, white);
                    if app.caption_font != CaptionFont::None {
                        // ChromeMinimize / ChromeMaximize / ChromeRestore / ChromeClose
                        let glyph = match k {
                            0 => "\u{E921}",
                            1 if maximized => "\u{E923}",
                            1 => "\u{E922}",
                            _ => "\u{E8BB}",
                        };
                        p.text(c, Align2::CENTER_CENTER, glyph, FontId::new(10.0, FontFamily::Name("caption".into())), white);
                    } else {
                    match k {
                        0 => { p.line_segment([pos2(c.x - 5.0, c.y), pos2(c.x + 5.0, c.y)], s); }
                        1 => {
                            if maximized {
                                p.rect_stroke(Rect::from_min_size(pos2(c.x - 5.0, c.y - 2.0), vec2(7.0, 7.0)), 0.0, s);
                                poly(&p, c, &[(-2.0, -2.0), (-2.0, -5.0), (5.0, -5.0), (5.0, 2.0), (2.0, 2.0)], white, 1.1);
                            } else {
                                p.rect_stroke(Rect::from_center_size(c, vec2(10.0, 10.0)), 0.0, s);
                            }
                        }
                        _ => {
                            p.line_segment([pos2(c.x - 5.0, c.y - 5.0), pos2(c.x + 5.0, c.y + 5.0)], s);
                            p.line_segment([pos2(c.x - 5.0, c.y + 5.0), pos2(c.x + 5.0, c.y - 5.0)], s);
                        }
                    }
                    }
                    if resp.activated() {
                        match k {
                            0 => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                            1 => app.toggle_maximize(ctx),
                            // End the process right here (see `quit`). Hiding the window
                            // and asking egui to close it later doesn't work: a hidden
                            // window gets no more redraws, so the close was never handled.
                            _ => app.quit(),
                        }
                    }
                }
            }
        });
}

// ── Seek bar + control row (overlaid on the video) ────────────────────────────
fn draw_bottom_bar(app: &mut FluentMediaPlayer, ctx: &egui::Context, t: f32) {
    let screen = ctx.screen_rect();
    let bar = Rect::from_min_max(pos2(screen.left(), screen.bottom() - BOTTOM_H), screen.right_bottom());
    let white = Color32::WHITE.gamma_multiply(t);
    let accent = app.accent.gamma_multiply(t);

    egui::Area::new(egui::Id::new("bottom_bar"))
        .order(egui::Order::Foreground)
        .fixed_pos(bar.min)
        .show(ctx, |ui| {
            ui.allocate_rect(bar, Sense::hover());
            let p = ui.painter().clone();
            gradient(&p, bar, Color32::TRANSPARENT, black(175, t));

            // Empty parts of the bar behave like the video underneath: double-click
            // toggles fullscreen (so the bottom edge of the screen works too), drag
            // moves the window. Registered first, so the seek bar and buttons added
            // after it take precedence.
            let bg = ui.interact(bar, ui.id().with("bar_bg"), Sense::click_and_drag());
            if bg.double_clicked() {
                app.toggle_fullscreen(ctx);
            }
            let pressed_on_edge = ctx
                .input(|i| i.pointer.press_origin())
                .map_or(false, |o| hit_edge(o, screen).is_some());
            if bg.drag_started() && !app.is_fullscreen && !pressed_on_edge {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }

            // ── Seek bar ──────────────────────────────────────────────────────
            let seek_y = screen.bottom() - SEEK_FROM_BOTTOM;
            let sr = Rect::from_min_max(
                pos2(screen.left() + SIDE, seek_y - 10.0),
                pos2(screen.right() - SIDE, seek_y + 10.0),
            );
            let seek = ui.interact(sr, ui.id().with("seek"), Sense::click_and_drag());
            // The knob travels inset by its radius, so at 0 % / 100 % it sits inside
            // the bar's ends (flush with the play / "more" buttons) instead of poking out.
            const KNOB_INSET: f32 = 7.5;
            let (k0, kw) = (sr.left() + KNOB_INSET, sr.width() - 2.0 * KNOB_INSET);
            let frac_at = |x: f32| ((x - k0) / kw).clamp(0.0, 1.0);
            let dragging = seek.is_pointer_button_down_on() || seek.dragged();
            if dragging {
                if let Some(pos) = seek.interact_pointer_pos() {
                    let f = frac_at(pos.x);
                    // Seek pop-up: remembers where the drag started
                    let from = match (app.drag_frac, app.seek_osd) {
                        (Some(_), Some((_, SeekOsd::Drag(from)))) => from,
                        _ => app.time_pos,
                    };
                    app.seek_osd = Some((std::time::Instant::now(), SeekOsd::Drag(from)));
                    app.volume_osd = None;
                    app.drag_frac = Some(f);
                    if app.duration > 0.0 { app.seek_to(f as f64 * app.duration); }
                }
            } else {
                app.drag_frac = None;
            }
            let frac = app.drag_frac.unwrap_or(if app.duration > 0.0 {
                (app.time_pos / app.duration).clamp(0.0, 1.0) as f32
            } else { 0.0 });
            let hot = seek.hovered() || dragging;

            let track_h = if hot { 3.0 } else { 2.0 };
            p.rect_filled(
                Rect::from_min_max(pos2(sr.left(), seek_y - track_h / 2.0), pos2(sr.right(), seek_y + track_h / 2.0)),
                1.0, TRACK.gamma_multiply(t),
            );
            let fill_x = k0 + kw * frac;
            p.rect_filled(
                Rect::from_min_max(pos2(sr.left(), seek_y - 1.5), pos2(fill_x, seek_y + 1.5)),
                1.0, accent,
            );
            // Chapter marks: small gaps in the bar
            if app.duration > 0.0 {
                let ppp = ctx.pixels_per_point();
                for &(start, _) in app.chapters.iter() {
                    if start > 0.5 && start < app.duration - 0.5 {
                        let x = ((k0 + kw * (start / app.duration) as f32) * ppp).round() / ppp;
                        p.rect_filled(
                            Rect::from_center_size(pos2(x, seek_y), vec2(2.0, 3.0)),
                            0.0, Color32::from_black_alpha((200.0 * t) as u8),
                        );
                    }
                }
            }
            draw_knob(&p, ctx.pixels_per_point(), pos2(fill_x, seek_y), hot, accent, t);

            // Hover / drag time preview
            let preview = if dragging { app.drag_frac }
                else if seek.hovered() { seek.hover_pos().map(|pp| frac_at(pp.x)) }
                else { None };
            if let (Some(f), true) = (preview, app.duration > 0.0) {
                let secs = f as f64 * app.duration;
                let txt = fmt_time(secs);
                let font = FontId::proportional(13.0);
                let w = p.layout_no_wrap(txt.clone(), font.clone(), Color32::WHITE).size().x + 16.0;
                let cx = (k0 + kw * f).clamp(sr.left() + w / 2.0, sr.right() - w / 2.0);
                let r = Rect::from_center_size(pos2(cx, seek_y - 28.0), vec2(w, 23.0));
                p.rect_filled(r, 4.0, Color32::from_rgba_unmultiplied(32, 32, 32, (235.0 * t) as u8));
                p.text(r.center(), Align2::CENTER_CENTER, txt, font, white);

                // Thumbnail of that moment, above the time
                app.thumbs.request(secs);
                if let Some(aspect) = app.thumbs.aspect() {
                    let tw = (thumbs::THUMB_H * aspect).clamp(64.0, 200.0);
                    let th = (tw / aspect).clamp(40.0, 150.0);
                    let ppp = ctx.pixels_per_point();
                    let snap = |v: f32| (v * ppp).round() / ppp;
                    let tx = (k0 + kw * f - tw / 2.0).clamp(screen.left() + 8.0, screen.right() - 8.0 - tw);
                    let tr = Rect::from_min_size(pos2(snap(tx), snap(r.top() - 6.0 - th)), vec2(snap(tw), snap(th)));
                    let tp = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("thumb")));
                    for (e, a) in [(1.0, 60.0), (2.5, 30.0), (4.0, 14.0)] {
                        tp.rect_filled(tr.expand(e), 4.0 + e, Color32::from_black_alpha((a * t) as u8));
                    }
                    app.thumbs.paint(&tp, tr, 2.0);
                    tp.rect_stroke(tr.expand(0.5), 2.5, Stroke::new(1.0_f32, { let a = (70.0 * t) as u8; Color32::from_rgba_premultiplied(a, a, a, a) }));
                }
            }

            // ── Button row ────────────────────────────────────────────────────
            let row_y = screen.bottom() - ROW_FROM_BOTTOM;
            let left = screen.left();
            let right = screen.right();

            let time_text = format!("{} / {}", fmt_time(app.time_pos), fmt_time(app.duration));
            let time_font = FontId::new(14.5, FontFamily::Name("semibold".into()));
            let time_w = p.layout_no_wrap(time_text.clone(), time_font.clone(), white).size().x;
            let lay = row_layout(left, right, time_w);

            // Play / pause
            let c = pos2(lay.play, row_y);
            if hit(ui, c, "play", if app.is_playing { "Pause (Space)" } else { "Play (Space)" }, t).activated() {
                app.toggle_play_pause();
            }
            if app.is_playing { icon_pause(&p, c, white) } else { icon_play(&p, c, white) }

            // Previous / next file in the same folder (dimmed when there isn't one)
            let dim = white.gamma_multiply(0.4);
            let c = pos2(lay.prev, row_y);
            if app.has_prev() && hit(ui, c, "prev", "Previous file (P)", t).activated() { app.play_adjacent(-1); }
            icon_prev(&p, c, if app.has_prev() { white } else { dim });

            let c = pos2(lay.next, row_y);
            if app.has_next() && hit(ui, c, "next", "Next file (N)", t).activated() { app.play_adjacent(1); }
            icon_next(&p, c, if app.has_next() { white } else { dim });

            // Time
            p.text(pos2(lay.time_x, row_y), Align2::LEFT_CENTER, time_text, time_font, white);

            // Volume icon (click = mute) + slider (the slider is left out when the
            // window is too narrow for it; the wheel and arrow keys still work)
            let c = pos2(lay.mute, row_y);
            if hit(ui, c, "mute", if app.muted { "Unmute (M)" } else { "Mute (M)" }, t).activated() {
                let m = !app.muted;
                app.set_mute(m);
            }
            icon_volume(&p, c, white, app.volume, app.muted);

            if let Some(vl) = lay.vol_x {
            let vw = VOL_W;
            let vr = Rect::from_min_max(pos2(vl - 8.0, row_y - 12.0), pos2(vl + vw + 8.0, row_y + 12.0));
            let vresp = ui.interact(vr, ui.id().with("vol"), Sense::click_and_drag());
            if vresp.is_pointer_button_down_on() || vresp.dragged() {
                if let Some(pos) = vresp.interact_pointer_pos() {
                    let mut v = ((pos.x - vl) / vw).clamp(0.0, 1.0) as f64 * VOL_MAX;
                    // Gentle snap to 100 % so it's easy to land exactly on it
                    if (v - 100.0).abs() < 2.5 { v = 100.0; }
                    app.set_volume(v.round());
                    if app.muted { app.set_mute(false); }
                    app.volume_osd = Some(std::time::Instant::now());
                }
            }
            let vf = (app.volume / VOL_MAX).clamp(0.0, 1.0) as f32;
            let vhot = vresp.hovered() || vresp.dragged();
            p.rect_filled(
                Rect::from_min_max(pos2(vl, row_y - 1.0), pos2(vl + vw, row_y + 1.0)),
                1.0, VOL_TRACK.gamma_multiply(t),
            );
            // Small tick marking 100 % (everything right of it is boost)
            let x100 = vl + vw * (100.0 / VOL_MAX) as f32;
            p.line_segment(
                [pos2(x100, row_y - 4.0), pos2(x100, row_y + 4.0)],
                Stroke::new(1.0_f32, VOL_TRACK.gamma_multiply(t * 0.8)),
            );
            p.rect_filled(
                Rect::from_min_max(pos2(vl, row_y - 1.5), pos2(vl + vw * vf, row_y + 1.5)),
                1.0, accent,
            );
            draw_knob(&p, ctx.pixels_per_point(), pos2(vl + vw * vf, row_y), vhot, accent, t);
            }

            // Right-hand buttons
            let tint = |on: bool| if on { accent } else { white };

            let c = pos2(lay.repeat, row_y);
            if hit(ui, c, "repeat", "Repeat this video", t).activated() { app.toggle_repeat(); }
            icon_repeat(&p, c, tint(app.repeat));

            let c = pos2(lay.captions, row_y);
            if hit(ui, c, "captions", "Cycle subtitles", t).activated() { app.cycle_captions(); }
            icon_captions(&p, c, white);

            let c = pos2(lay.ontop, row_y);
            if hit(ui, c, "ontop", "Always on top", t).activated() { app.toggle_on_top(ctx); }
            icon_compact(&p, c, tint(app.on_top));

            let c = pos2(lay.fullscreen, row_y);
            if hit(ui, c, "fs", "Full screen (F)", t).activated() { app.toggle_fullscreen(ctx); }
            icon_fullscreen(&p, c, white);

            // "More" → playback speed menu
            let c = pos2(lay.speed, row_y);
            let more = hit(ui, c, "more", "Playback speed", t);
            icon_speed(&p, c, white);
            let popup_id = egui::Id::new(MORE_POPUP);
            if more.activated() {
                ui.memory_mut(|m| m.toggle_popup(popup_id));
            }
            egui::popup_above_or_below_widget(ui, popup_id, &more, egui::AboveOrBelow::Above, |ui| {
                ui.set_min_width(110.0);
                ui.label("Playback speed");
                for s in [0.5_f64, 0.75, 1.0, 1.25, 1.5, 2.0] {
                    if ui.selectable_label((app.speed - s).abs() < 1e-6, format!("{s}x")).clicked() {
                        app.set_speed(s);
                        ui.memory_mut(|m| m.close_popup());
                    }
                }
            });
        });
}

// ── Control-row layout ────────────────────────────────────────────────────────
/// Horizontal centres of the control-row items for a window spanning left..right.
/// The first button's highlight starts exactly where the seek bar starts and the last
/// one ends where the seek bar ends.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RowLayout {
    play: f32,
    prev: f32,
    next: f32,
    time_x: f32,
    mute: f32,
    /// Left end of the volume slider track, or None when there's no room for it
    vol_x: Option<f32>,
    repeat: f32,
    captions: f32,
    ontop: f32,
    speed: f32,
    /// The last button, flush with the seek bar's right end
    fullscreen: f32,
}

fn row_layout(left: f32, right: f32, time_w: f32) -> RowLayout {
    let half = BTN_BOX / 2.0;
    let play = left + SIDE + half;
    let prev = play + BTN_STEP;
    let next = prev + BTN_STEP;
    let time_x = next + half + 10.0;
    let time_end = time_x + time_w;

    let fullscreen = right - SIDE - half;
    let speed = fullscreen - BTN_STEP;
    let ontop = speed - BTN_STEP;
    let captions = ontop - BTN_STEP;
    let repeat = captions - BTN_STEP;

    // Slider sits between the volume icon and the repeat button
    let vol_x = repeat - half - 12.0 - VOL_W;
    let mute_with_slider = vol_x - 14.0 - half; // room for the knob at 0 %
    let (mute, vol_x) = if mute_with_slider - half >= time_end + 14.0 {
        (mute_with_slider, Some(vol_x))
    } else {
        (repeat - BTN_STEP, None)
    };
    RowLayout { play, prev, next, time_x, mute, vol_x, repeat, captions, ontop, speed, fullscreen }
}

// ── Folder navigation ─────────────────────────────────────────────────────────
/// Video files in the same folder as `path`, in Explorer-like (natural) order
/// (only files of the same kind: a video steps through videos, a picture through
/// pictures, a song through music)
fn scan_folder(path: &Path) -> Vec<PathBuf> {
    let Some(dir) = path.parent() else { return vec![path.to_owned()] };
    let kind = media_kind(&path.to_string_lossy());
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    let k = media_kind(&p.to_string_lossy());
                    p.is_file() && k.is_some() && (kind.is_none() || k == kind)
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort_by(|a, b| {
        let name = |p: &PathBuf| p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        natural_cmp(&name(a), &name(b))
    });
    files
}

/// Case-insensitive compare where digit runs compare by value ("2" < "10"),
/// like Windows Explorer
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                if x.is_ascii_digit() && y.is_ascii_digit() {
                    let mut na = String::new();
                    while let Some(&c) = ai.peek() {
                        if c.is_ascii_digit() { na.push(c); ai.next(); } else { break; }
                    }
                    let mut nb = String::new();
                    while let Some(&c) = bi.peek() {
                        if c.is_ascii_digit() { nb.push(c); bi.next(); } else { break; }
                    }
                    let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                    let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                    if o != Ordering::Equal { return o; }
                } else {
                    let o = x.to_lowercase().collect::<String>()
                        .cmp(&y.to_lowercase().collect::<String>());
                    if o != Ordering::Equal { return o; }
                    ai.next();
                    bi.next();
                }
            }
        }
    }
}

// ── Window edge resizing ──────────────────────────────────────────────────────
/// Which resize edge/corner (if any) the pointer is on, with its cursor icon
fn hit_edge(p: Pos2, screen: Rect) -> Option<(egui::viewport::ResizeDirection, egui::CursorIcon)> {
    use egui::viewport::ResizeDirection as D;
    use egui::CursorIcon as C;
    const EDGE: f32 = 6.0;
    const CORNER: f32 = 14.0;

    // Keep the close/maximize/minimize buttons clickable in the top-right corner
    if p.y < WIN_BTN_H && p.x > screen.right() - WIN_BTN_W * 3.0 {
        return None;
    }

    let (dl, dr) = (p.x - screen.left(), screen.right() - p.x);
    let (dt, db) = (p.y - screen.top(), screen.bottom() - p.y);

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

/// Reads a REG_BINARY value into `buf`, returning how many bytes were read
fn reg_binary(sub_key: &str, value: &str, buf: &mut [u8]) -> Option<usize> {
    let (key, val) = (wide(sub_key), wide(value));
    let mut len = buf.len() as u32;
    // RRF_RT_REG_BINARY = 0x08
    let rc = unsafe {
        RegGetValueW(
            0x8000_0001_u32 as i32 as isize,
            key.as_ptr(),
            val.as_ptr(),
            0x08,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut std::ffi::c_void,
            &mut len,
        )
    };
    (rc == 0).then_some(len as usize)
}

/// The accent colour from Windows Settings (static, or picked from the wallpaper)
fn system_accent() -> Color32 {
    const ACCENT_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent";

    // AccentPalette holds 8 RGBA colours: Light3, Light2, Light1, *Accent*, Dark1, Dark2, Dark3, —.
    // Entry 3 is the accent exactly as Settings shows it.
    let mut palette = [0u8; 32];
    let from_palette = reg_binary(ACCENT_KEY, "AccentPalette", &mut palette)
        .filter(|&n| n >= 16)
        .map(|_| (palette[12], palette[13], palette[14]));

    // Fallbacks. AccentColorMenu / DWM\AccentColor are 0xAABBGGRR, ColorizationColor is 0xAARRGGBB.
    let rgb = from_palette
        .or_else(|| {
            reg_dword(ACCENT_KEY, "AccentColorMenu")
                .or_else(|| reg_dword(r"Software\Microsoft\Windows\DWM", "AccentColor"))
                .map(|v| ((v & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, ((v >> 16) & 0xFF) as u8))
        })
        .or_else(|| {
            reg_dword(r"Software\Microsoft\Windows\DWM", "ColorizationColor")
                .map(|v| (((v >> 16) & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, (v & 0xFF) as u8))
        });

    match rgb {
        Some((r, g, b)) => vivid_accent(r, g, b),
        None => DEFAULT_ACCENT,
    }
}

/// Make the accent punchy on a dark video background: keep the hue, raise saturation
/// and brightness (never mix with white — that washes the colour out).
fn vivid_accent(r: u8, g: u8, b: u8) -> Color32 {
    let (h, s, v) = rgb_to_hsv(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    // Genuinely grey accents stay grey; anything with colour gets pushed
    let s = if s < 0.12 { s } else { (s * 1.25).clamp(0.65, 1.0) };
    let v = v.max(0.96);
    let (r, g, b) = hsv_to_rgb(h, s, v);
    Color32::from_rgb((r * 255.0).round() as u8, (g * 255.0).round() as u8, (b * 255.0).round() as u8)
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

// ── Pop-ups ───────────────────────────────────────────────────────────────────
/// Pill in the top right: icon, value, and a slim bar showing `frac`
fn draw_osd(
    ctx: &egui::Context,
    icon: &dyn Fn(&egui::Painter, Pos2, Color32),
    label: &str,
    frac: f32,
    accent: Color32,
    alpha: f32,
    fullscreen: bool,
) {
    let screen = ctx.screen_rect();
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("osd")));
    let font = FontId::new(17.0, FontFamily::Name("semibold".into()));
    let label_w = p.layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE).size().x;
    // Sit below the window buttons (they aren't shown in fullscreen)
    let top = if fullscreen { 16.0 } else { WIN_BTN_H + 12.0 };
    let size = vec2((label_w + 66.0).max(116.0).round(), 42.0);
    let rect = Rect::from_min_size(pos2(screen.right() - 16.0 - size.x, screen.top() + top), size);
    p.rect_filled(rect, 8.0, Color32::from_rgba_unmultiplied(24, 24, 24, (225.0 * alpha) as u8));

    let white = Color32::WHITE.gamma_multiply(alpha);
    icon(&p, pos2(rect.left() + 26.0, rect.center().y - 3.0), white);
    p.text(pos2(rect.right() - 16.0, rect.center().y - 3.0), Align2::RIGHT_CENTER, label, font, white);

    let bar = Rect::from_min_max(
        pos2(rect.left() + 14.0, rect.bottom() - 9.0),
        pos2(rect.right() - 14.0, rect.bottom() - 6.0),
    );
    p.rect_filled(bar, 1.5, VOL_TRACK.gamma_multiply(alpha * 0.6));
    p.rect_filled(
        Rect::from_min_max(bar.min, pos2(bar.left() + bar.width() * frac.clamp(0.0, 1.0), bar.max.y)),
        1.5,
        accent.gamma_multiply(alpha),
    );
}

/// "🔊 55 %" with the volume level
fn draw_volume_osd(ctx: &egui::Context, volume: f64, muted: bool, accent: Color32, alpha: f32, fullscreen: bool) {
    let label = if muted { "Muted".to_owned() } else { format!("{}%", volume.round() as i32) };
    let frac = if muted { 0.0 } else { (volume / VOL_MAX) as f32 };
    draw_osd(ctx, &|p, c, col| icon_volume(p, c, col, volume, muted), &label, frac, accent, alpha, fullscreen);
}

/// "⏩ +10 s" after the arrow keys, or "⏩ 12:34" (the new time) while dragging the
/// seek bar; the bar shows the position in the video
fn draw_seek_osd(ctx: &egui::Context, kind: SeekOsd, target: f64, duration: f64, accent: Color32, alpha: f32, fullscreen: bool) {
    let (forward, label) = match kind {
        SeekOsd::Step(secs) => {
            let sign = if secs < 0.0 { "\u{2212}" } else { "+" };
            let a = secs.abs();
            (secs >= 0.0, if a < 60.0 { format!("{sign}{} s", a.round()) } else { format!("{sign}{}", fmt_time(a)) })
        }
        SeekOsd::Drag(from) => (target >= from, fmt_time(target)),
    };
    let frac = if duration > 0.0 { (target / duration) as f32 } else { 0.0 };
    draw_osd(ctx, &|p, c, col| icon_seek(p, c, col, forward), &label, frac, accent, alpha, fullscreen);
}

/// Fast-forward (two triangles), mirrored for rewind
fn icon_seek(p: &egui::Painter, c: Pos2, col: Color32, forward: bool) {
    let d = if forward { 1.0 } else { -1.0 };
    for x0 in [-7.5_f32, 0.0] {
        p.add(egui::Shape::convex_polygon(
            vec![
                pos2(c.x + d * x0 * K, c.y - 6.0 * K),
                pos2(c.x + d * (x0 + 7.5) * K, c.y),
                pos2(c.x + d * x0 * K, c.y + 6.0 * K),
            ],
            col,
            Stroke::NONE,
        ));
    }
}

// ── Main menu (top left) ──────────────────────────────────────────────────────
fn draw_main_menu(app: &mut FluentMediaPlayer, ctx: &egui::Context) {
    if !app.main_menu {
        return;
    }
    const W: f32 = 228.0;
    const ITEM_H: f32 = 28.0;
    const PAD: f32 = 3.0;
    const SEP_H: f32 = 7.0;
    let box_left = MENU_X - BTN_BOX / 2.0;
    let rect = Rect::from_min_size(
        pos2(box_left, MENU_Y + BTN_BOX / 2.0 + 4.0),
        vec2(W, PAD * 2.0 + ITEM_H * 2.0 + SEP_H),
    );

    // Any press outside the menu (and its button, which toggles it) closes it
    let anchor = app.menu_anchor;
    let pressed_outside = ctx.input(|i| {
        i.pointer.any_pressed()
            && i.pointer.press_origin().map_or(false, |o| !rect.contains(o) && !anchor.contains(o))
    });
    if pressed_outside {
        app.main_menu = false;
        return;
    }

    let id = egui::Id::new("main_menu");
    egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .show(ctx, |ui| {
            // The menu's own background swallows clicks (so they don't reach the video)
            ui.allocate_rect(rect, Sense::click_and_drag());
            let p = ui.painter().clone();
            let r = 8.0;
            for (e, a) in [(1.0, 40.0), (3.0, 22.0), (6.0, 10.0)] {
                p.rect_filled(rect.expand(e).translate(vec2(0.0, 2.0)), r + e, Color32::from_black_alpha(a as u8));
            }
            p.rect_filled(rect, r, Color32::from_rgb(44, 44, 44));
            p.rect_stroke(rect.shrink(0.5), r, Stroke::new(1.0_f32, Color32::from_rgba_premultiplied(30, 30, 30, 30)));

            let text = Color32::from_rgb(240, 240, 240);
            let font = FontId::proportional(13.5);
            let item = |ui: &mut egui::Ui, top: f32, key: &str| {
                let ir = Rect::from_min_size(pos2(rect.left() + PAD, top), vec2(W - 2.0 * PAD, ITEM_H));
                let resp = ui.interact(ir, ui.id().with(key), Sense::click_and_drag());
                if resp.hovered() {
                    p.rect_filled(ir, 4.0, Color32::from_rgba_premultiplied(14, 14, 14, 14));
                }
                resp
            };
            let text_x = rect.left() + PAD + 28.0;

            // Open file…
            let top = rect.top() + PAD;
            let open = item(ui, top, "open");
            let cy = top + ITEM_H / 2.0;
            p.text(pos2(text_x, cy), Align2::LEFT_CENTER, "Open file\u{2026}", font.clone(), text);
            p.text(
                pos2(rect.right() - PAD - 10.0, cy),
                Align2::RIGHT_CENTER,
                "Ctrl+O",
                FontId::proportional(12.5),
                Color32::from_rgb(160, 160, 160),
            );
            if open.activated() {
                app.main_menu = false;
                app.show_open_dialog();
            }

            // separator
            let sep_y = top + ITEM_H + SEP_H / 2.0;
            p.line_segment(
                [pos2(rect.left() + 1.0, sep_y), pos2(rect.right() - 1.0, sep_y)],
                Stroke::new(1.0_f32, Color32::from_rgba_premultiplied(26, 26, 26, 26)),
            );

            // Auto-play next file (a check mark when on); the menu stays open
            let top = top + ITEM_H + SEP_H;
            let auto = item(ui, top, "autoplay");
            let cy = top + ITEM_H / 2.0;
            if app.autoplay {
                poly(&p, pos2(rect.left() + PAD + 14.0, cy), &[(-4.5, 0.0), (-1.5, 3.2), (4.5, -3.5)], text, 1.5);
            }
            p.text(pos2(text_x, cy), Align2::LEFT_CENTER, "Auto-play next file in folder", font, text);
            if auto.activated() {
                app.autoplay = !app.autoplay;
            }
        });
    ctx.move_to_top(egui::LayerId::new(egui::Order::Foreground, id));
}

/// Decoder / video details (toggle with I) — shows whether hardware decoding is active
fn draw_info_overlay(ctx: &egui::Context, lines: &[String]) {
    if lines.is_empty() { return; }
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("info_osd")));
    let font = FontId::proportional(13.0);
    let line_h = 20.0;
    let text_w = lines
        .iter()
        .map(|l| p.layout_no_wrap(l.clone(), font.clone(), Color32::WHITE).size().x)
        .fold(0.0_f32, f32::max);
    let screen = ctx.screen_rect();
    let rect = Rect::from_min_size(
        pos2(screen.left() + 20.0, screen.top() + 64.0),
        vec2(text_w + 28.0, lines.len() as f32 * line_h + 16.0),
    );
    p.rect_filled(rect, 8.0, Color32::from_rgba_unmultiplied(24, 24, 24, 215));
    for (i, line) in lines.iter().enumerate() {
        p.text(
            pos2(rect.left() + 14.0, rect.top() + 8.0 + line_h * (i as f32 + 0.5)),
            Align2::LEFT_CENTER,
            line,
            font.clone(),
            Color32::WHITE,
        );
    }
}

fn fmt_time(secs: f64) -> String {
    let s = if secs.is_finite() && secs > 0.0 { secs as u64 } else { 0 };
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

// ── File types ────────────────────────────────────────────────────────────────
/// Files mpv plays, by kind (drag-and-drop, the Open dialog, previous / next)
const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "ts", "m2ts", "mts", "mpg", "mpeg", "3gp", "ogv",
];
const MUSIC_EXTS: &[&str] = &[
    "mp3", "flac", "wav", "m4a", "aac", "ogg", "oga", "opus", "wma", "alac", "aiff", "aif", "ape", "wv", "mka",
];
const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "avif", "bmp", "tif", "tiff", "jxl",
];
const PLAYLIST_EXTS: &[&str] = &["m3u", "m3u8"];

#[derive(Clone, Copy, PartialEq, Debug)]
enum MediaKind {
    Video,
    Music,
    Image,
}

fn media_kind(path: &str) -> Option<MediaKind> {
    if has_ext(path, VIDEO_EXTS) {
        Some(MediaKind::Video)
    } else if has_ext(path, MUSIC_EXTS) {
        Some(MediaKind::Music)
    } else if has_ext(path, IMAGE_EXTS) {
        Some(MediaKind::Image)
    } else {
        None
    }
}

fn is_playlist_file(path: &str) -> bool {
    has_ext(path, PLAYLIST_EXTS)
}

/// The entries of an .m3u / .m3u8 playlist that exist (or are web addresses),
/// in order. Relative paths are relative to the playlist's folder.
fn read_playlist(path: &Path) -> Vec<PathBuf> {
    let Ok(bytes) = std::fs::read(path) else { return Vec::new() };
    let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
    playlist_text(&bytes)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|line| {
            if line.contains("://") && !line.starts_with("file://") {
                return Some(PathBuf::from(line)); // web address: mpv streams it
            }
            let line = match line.strip_prefix("file:///").or_else(|| line.strip_prefix("file://")) {
                Some(rest) => percent_decode(rest),
                None => line.to_owned(),
            };
            let p = PathBuf::from(line.replace('/', "\\"));
            let p = if p.is_absolute() { p } else { base.join(p) };
            (p.is_file() && !is_playlist_file(&p.to_string_lossy())).then_some(p)
        })
        .collect()
}

/// .m3u8 (and modern .m3u) files are UTF-8; older .m3u files use the Windows
/// language's code page
fn playlist_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_owned();
    }
    unsafe {
        let n = MultiByteToWideChar(0 /* CP_ACP */, 0, bytes.as_ptr(), bytes.len() as i32, std::ptr::null_mut(), 0);
        if n > 0 {
            let mut wide = vec![0u16; n as usize];
            MultiByteToWideChar(0, 0, bytes.as_ptr(), bytes.len() as i32, wide.as_mut_ptr(), n);
            return String::from_utf16_lossy(&wide);
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// "My%20Song.mp3" → "My Song.mp3" (for file:// entries)
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn has_ext(path: &str, exts: &[&str]) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map_or(false, |e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

fn is_media_file(path: &str) -> bool {
    media_kind(path).is_some()
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

    // Single window: if the player is already running, hand it the file (it plays
    // it in place of the current one) and leave
    let arg_file = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned());
    if taskbar::forward_to_running_instance(arg_file.as_deref()) {
        return Ok(());
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Rust Media Player")
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
