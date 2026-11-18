//! Windows taskbar integration — the same API PotPlayer and Groove use
//! (`ITaskbarList3`):
//!
//!  * a progress bar that fills up the taskbar icon while the video plays
//!  * transport buttons (previous file, back 10 s, play/pause, forward 10 s,
//!    next file) in the preview that pops up when you hover the taskbar icon
//!
//! It also hosts the window-procedure hook, which additionally
//!
//!  * receives files from a second launch of the player (single window)
//!  * catches the keyboard's media keys
//!
//!  * performs the fullscreen switch *between* frames (see `post_fullscreen`)
//!  * stops Windows erasing the window background on resize (a flash source)
//!
//! Everything here fails quietly: if Windows refuses a call, the player simply
//! runs without that part of the integration.
//!
//! The handful of Win32 / COM declarations are written out by hand (like the ones
//! in main.rs) so no extra dependency is needed.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// Thumbnail button ids (also the values returned by `take_click`)
pub const PREV: u32 = 1;
pub const BACK: u32 = 2;
pub const PLAY: u32 = 3;
pub const FWD: u32 = 4;
pub const NEXT: u32 = 5;

const TIMER_ID: usize = 0x7A01;
const SUBCLASS_ID: usize = 0x7A02;

// ── Win32 declarations ────────────────────────────────────────────────────────
#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

/// {56FDF344-FD6D-11d0-958A-006097C9A090}
const CLSID_TASKBAR_LIST: Guid = Guid {
    data1: 0x56FD_F344,
    data2: 0xFD6D,
    data3: 0x11D0,
    data4: [0x95, 0x8A, 0x00, 0x60, 0x97, 0xC9, 0xA0, 0x90],
};
/// {EA1AFB91-9E28-4B86-90E9-9E9F8A5EEFAF}
const IID_ITASKBAR_LIST3: Guid = Guid {
    data1: 0xEA1A_FB91,
    data2: 0x9E28,
    data3: 0x4B86,
    data4: [0x90, 0xE9, 0x9E, 0x9F, 0x8A, 0x5E, 0xEF, 0xAF],
};

#[repr(C)]
struct ThumbButton {
    mask: u32,
    id: u32,
    bitmap: u32,
    icon: isize,
    tip: [u16; 260],
    flags: u32,
}

#[repr(C)]
struct IconInfo {
    is_icon: i32,
    x_hotspot: u32,
    y_hotspot: u32,
    mask: isize,
    color: isize,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RectI {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct MonitorInfo {
    size: u32,
    monitor: RectI,
    work: RectI,
    flags: u32,
}

#[repr(C)]
struct CopyDataStruct {
    data: usize,
    len: u32,
    ptr: *const c_void,
}

#[repr(C)]
struct OpenFileNameW {
    struct_size: u32,
    owner: isize,
    instance: isize,
    filter: *const u16,
    custom_filter: *mut u16,
    max_custom_filter: u32,
    filter_index: u32,
    file: *mut u16,
    max_file: u32,
    file_title: *mut u16,
    max_file_title: u32,
    initial_dir: *const u16,
    title: *const u16,
    flags: u32,
    file_offset: u16,
    file_extension: u16,
    default_ext: *const u16,
    cust_data: isize,
    hook: *const c_void,
    template_name: *const u16,
    reserved_ptr: *mut c_void,
    reserved: u32,
    flags_ex: u32,
}

type SubclassProc = unsafe extern "system" fn(isize, u32, usize, isize, usize, usize) -> isize;

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, co_init: u32) -> i32;
    fn CoCreateInstance(
        clsid: *const Guid,
        outer: *mut c_void,
        context: u32,
        iid: *const Guid,
        out: *mut *mut c_void,
    ) -> i32;
}

#[link(name = "comctl32")]
unsafe extern "system" {
    fn SetWindowSubclass(hwnd: isize, proc_: Option<SubclassProc>, id: usize, data: usize) -> i32;
    fn DefSubclassProc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterWindowMessageW(name: *const u16) -> u32;
    fn SetTimer(hwnd: isize, id: usize, elapse_ms: u32, func: *const c_void) -> usize;
    fn IsIconic(hwnd: isize) -> i32;
    fn RedrawWindow(hwnd: isize, update_rect: *const c_void, update_region: isize, flags: u32) -> i32;
    fn CreateIconIndirect(info: *const IconInfo) -> isize;
    fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
    fn IsZoomed(hwnd: isize) -> i32;
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    fn GetWindowRect(hwnd: isize, rect: *mut RectI) -> i32;
    fn MonitorFromWindow(hwnd: isize, flags: u32) -> isize;
    fn GetMonitorInfoW(monitor: isize, info: *mut MonitorInfo) -> i32;
    fn SetWindowPos(hwnd: isize, insert_after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
    fn FindWindowW(class: *const u16, title: *const u16) -> isize;
    fn SendMessageTimeoutW(
        hwnd: isize,
        msg: u32,
        wparam: usize,
        lparam: isize,
        flags: u32,
        timeout_ms: u32,
        result: *mut usize,
    ) -> isize;
    fn SetForegroundWindow(hwnd: isize) -> i32;
    fn MonitorFromRect(rect: *const RectI, flags: u32) -> isize;
    fn GetDpiForWindow(hwnd: isize) -> u32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateMutexW(attributes: *const c_void, initial_owner: i32, name: *const u16) -> isize;
    fn GetLastError() -> u32;
}

#[link(name = "comdlg32")]
unsafe extern "system" {
    fn GetOpenFileNameW(ofn: *mut OpenFileNameW) -> i32;
}

#[link(name = "dwmapi")]
unsafe extern "system" {
    fn DwmSetWindowAttribute(hwnd: isize, attribute: u32, value: *const c_void, size: u32) -> i32;
    fn DwmGetWindowAttribute(hwnd: isize, attribute: u32, value: *mut c_void, size: u32) -> i32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateBitmap(width: i32, height: i32, planes: u32, bits_per_pixel: u32, bits: *const c_void) -> isize;
    fn DeleteObject(object: isize) -> i32;
}

const WM_ERASEBKGND: u32 = 0x0014;
/// Private message: switch fullscreen on (wparam 1) or off (wparam 2)
const WM_APP_FULLSCREEN: u32 = 0x8001;
const WM_COMMAND: u32 = 0x0111;
const WM_COPYDATA: u32 = 0x004A;
const WM_KEYDOWN: u32 = 0x0100;
const WM_APPCOMMAND: u32 = 0x0319;
const WM_MOVING: u32 = 0x0216;
const WM_SIZING: u32 = 0x0214;
/// Marks our WM_COPYDATA messages ("RMP1")
const COPYDATA_MAGIC: usize = 0x524D_5031;
const WM_TIMER: u32 = 0x0113;
const THBN_CLICKED: u32 = 0x1800;
const RDW_INTERNALPAINT: u32 = 0x0002;

const THB_ICON: u32 = 0x2;
const THB_TOOLTIP: u32 = 0x4;
const THB_FLAGS: u32 = 0x8;
const THBF_ENABLED: u32 = 0x0;
const THBF_DISABLED: u32 = 0x1;

const TBPF_NOPROGRESS: i32 = 0;
const TBPF_NORMAL: i32 = 2;
const TBPF_PAUSED: i32 = 8;

// ITaskbarList3 vtable slots (IUnknown 0-2, ITaskbarList 3-7, ITaskbarList2 8)
const VT_HR_INIT: usize = 3;
const VT_SET_PROGRESS_VALUE: usize = 9;
const VT_SET_PROGRESS_STATE: usize = 10;
const VT_THUMB_ADD_BUTTONS: usize = 15;
const VT_THUMB_UPDATE_BUTTONS: usize = 16;

/// Raw pointer to function number `slot` of a COM object's vtable
unsafe fn method(this: *mut c_void, slot: usize) -> *const c_void {
    let vtable = *(this as *const *const *const c_void);
    *vtable.add(slot)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ── State shared with the window procedure ────────────────────────────────────
/// Last thumbnail button clicked (0 = none)
static CLICKED: AtomicU32 = AtomicU32::new(0);
/// Id of the "TaskbarButtonCreated" window message (sent again if Explorer restarts)
static BUTTON_MSG: AtomicU32 = AtomicU32::new(0);
static BUTTONS_LOST: AtomicBool = AtomicBool::new(false);

/// Window rectangle to go back to when leaving fullscreen: left, top, right, bottom
static FS_RESTORE: Mutex<Option<[i32; 4]>> = Mutex::new(None);
/// The window was maximized when fullscreen was entered
static FS_WAS_MAX: AtomicBool = AtomicBool::new(false);
/// Entering fullscreen could not be done natively (caller falls back to egui's way)
static FS_FAILED: AtomicBool = AtomicBool::new(false);

/// True once if the last fullscreen request could not be carried out
pub fn take_fullscreen_failed() -> bool {
    FS_FAILED.swap(false, Ordering::Relaxed)
}

/// Ask the window procedure to switch fullscreen on/off. The switch is *posted*, so it
/// runs between two frames instead of in the middle of one: the old frame is never
/// stretched into the resized window, which is what looked like flicker.
pub fn post_fullscreen(hwnd: isize, enter: bool) {
    unsafe {
        PostMessageW(hwnd, WM_APP_FULLSCREEN, if enter { 1 } else { 2 }, 0);
    }
}

/// Install the window-procedure hook (taskbar clicks, fullscreen, erase-background)
/// and switch off Windows' own window animations.
pub fn hook_window(hwnd: isize) -> bool {
    unsafe {
        let ok = SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, 0) != 0;
        if ok {
            // While the window is minimised the UI loop sleeps; this timer wakes it
            // twice a second so the taskbar progress keeps moving.
            SetTimer(hwnd, TIMER_ID, 500, std::ptr::null());
        }
        // DWMWA_TRANSITIONS_FORCEDISABLED: no open/close/min/max animations, which
        // make fullscreen switches look jumpy.
        let on: i32 = 1;
        DwmSetWindowAttribute(hwnd, 3, &on as *const i32 as *const c_void, 4);
        ok
    }
}

unsafe fn enter_fullscreen_now(hwnd: isize) {
    // A maximized window can't be sized past its maximized size: un-maximize first.
    let was_max = IsZoomed(hwnd) != 0;
    if was_max {
        ShowWindow(hwnd, 9); // SW_RESTORE
    }
    let mut window = RectI::default();
    let mut info = MonitorInfo {
        size: std::mem::size_of::<MonitorInfo>() as u32,
        monitor: RectI::default(),
        work: RectI::default(),
        flags: 0,
    };
    let monitor = MonitorFromWindow(hwnd, 2); // MONITOR_DEFAULTTONEAREST
    if monitor != 0 && GetWindowRect(hwnd, &mut window) != 0 && GetMonitorInfoW(monitor, &mut info) != 0 {
        if let Ok(mut r) = FS_RESTORE.lock() {
            *r = Some([window.left, window.top, window.right, window.bottom]);
        }
        FS_WAS_MAX.store(was_max, Ordering::Relaxed);
        let m = info.monitor;
        // One pixel taller than the monitor: a borderless OpenGL window that exactly
        // covers the screen gets switched by Windows / the GPU driver into an
        // "exclusive fullscreen" present mode, and that switch is a visible blink.
        // The extra row is off-screen; the taskbar still hides as usual.
        SetWindowPos(hwnd, 0, m.left, m.top, m.right - m.left, m.bottom - m.top + 1, 0x0200); // SWP_NOOWNERZORDER
    } else {
        if was_max {
            ShowWindow(hwnd, 3); // SW_MAXIMIZE — couldn't go fullscreen, put it back
        }
        FS_FAILED.store(true, Ordering::Relaxed);
    }
}

unsafe fn exit_fullscreen_now(hwnd: isize) {
    let saved = FS_RESTORE.lock().ok().and_then(|mut r| r.take());
    if let Some([l, t, r, b]) = saved {
        SetWindowPos(hwnd, 0, l, t, r - l, b - t, 0x0200);
        if FS_WAS_MAX.swap(false, Ordering::Relaxed) {
            ShowWindow(hwnd, 3); // SW_MAXIMIZE
        }
    }
}

// ── Single window ─────────────────────────────────────────────────────────────
/// A file handed over by a second launch of the player ("" = just come to front)
static FORWARDED: Mutex<Option<String>> = Mutex::new(None);

pub fn take_forwarded() -> Option<String> {
    FORWARDED.lock().ok().and_then(|mut f| f.take())
}

/// Called at startup, before any window exists. If the player is already running,
/// hand it `file` (it opens it in place of what's playing), bring it to the front and
/// return true: this process should then just exit.
pub fn forward_to_running_instance(file: Option<&str>) -> bool {
    unsafe {
        // The mutex lives as long as this process; a second process finds it taken
        let mutex = CreateMutexW(std::ptr::null(), 0, wide("Local\\RustMediaPlayer.SingleWindow").as_ptr());
        if mutex == 0 || GetLastError() != 183 /* ERROR_ALREADY_EXISTS */ {
            return false;
        }
        // The other player may still be starting up: give its window a moment
        let title = wide("Rust Media Player");
        let mut hwnd = 0;
        for _ in 0..30 {
            hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
            if hwnd != 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if hwnd == 0 {
            return false; // couldn't find it: just run as a second player
        }
        let payload: Vec<u16> = file.unwrap_or("").encode_utf16().collect();
        let cds = CopyDataStruct {
            data: COPYDATA_MAGIC,
            len: (payload.len() * 2) as u32,
            ptr: payload.as_ptr() as *const c_void,
        };
        let mut result = 0usize;
        SendMessageTimeoutW(hwnd, WM_COPYDATA, 0, &cds as *const _ as isize, 0x2 /* SMTO_ABORTIFHUNG */, 3000, &mut result);
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, 9); // SW_RESTORE
        }
        SetForegroundWindow(hwnd);
        true
    }
}

// ── Snap to screen edges ──────────────────────────────────────────────────────
static SNAP: AtomicBool = AtomicBool::new(true);

/// Moving / resizing the window pulls its edges onto the screen's edges when they
/// come within 8 px (menu setting)
pub fn set_snap(on: bool) {
    SNAP.store(on, Ordering::Relaxed);
}

/// Adjust the proposed window rectangle of a move (`sizing` = None) or a resize
/// (`sizing` = which edge is being dragged, WMSZ_*)
unsafe fn snap_rect(hwnd: isize, r: &mut RectI, sizing: Option<usize>) {
    let mut mi: MonitorInfo = std::mem::zeroed();
    mi.size = std::mem::size_of::<MonitorInfo>() as u32;
    if GetMonitorInfoW(MonitorFromRect(r, 2 /* MONITOR_DEFAULTTONEAREST */), &mut mi) == 0 {
        return;
    }
    // The screen area without the taskbar
    let w = mi.work;
    // Any invisible border around the window shouldn't count: snap what you see
    let (mut il, mut it, mut ir, mut ib) = (0, 0, 0, 0);
    let (mut win, mut vis) = (RectI::default(), RectI::default());
    if GetWindowRect(hwnd, &mut win) != 0
        && DwmGetWindowAttribute(hwnd, 9 /* DWMWA_EXTENDED_FRAME_BOUNDS */, &mut vis as *mut RectI as *mut c_void, 16) == 0
    {
        (il, it, ir, ib) = (vis.left - win.left, vis.top - win.top, win.right - vis.right, win.bottom - vis.bottom);
        if il.abs() > 32 || it.abs() > 32 || ir.abs() > 32 || ib.abs() > 32 {
            (il, it, ir, ib) = (0, 0, 0, 0);
        }
    }
    let (l, t, rt, b) = (r.left + il, r.top + it, r.right - ir, r.bottom - ib);
    // 8 px at 100 % display scaling, more on high-DPI screens
    let reach = (8 * GetDpiForWindow(hwnd).max(96) as i32 + 48) / 96;
    let pull = |edge: i32, target: i32| ((edge - target).abs() <= reach).then_some(target - edge);
    match sizing {
        None => {
            let dx = pull(l, w.left).or_else(|| pull(rt, w.right)).unwrap_or(0);
            let dy = pull(t, w.top).or_else(|| pull(b, w.bottom)).unwrap_or(0);
            r.left += dx;
            r.right += dx;
            r.top += dy;
            r.bottom += dy;
        }
        // WMSZ_LEFT 1, RIGHT 2, TOP 3, TOPLEFT 4, TOPRIGHT 5, BOTTOM 6, BOTTOMLEFT 7, BOTTOMRIGHT 8
        Some(edge) => {
            if matches!(edge, 1 | 4 | 7) {
                r.left += pull(l, w.left).unwrap_or(0);
            }
            if matches!(edge, 2 | 5 | 8) {
                r.right += pull(rt, w.right).unwrap_or(0);
            }
            if matches!(edge, 3 | 4 | 5) {
                r.top += pull(t, w.top).unwrap_or(0);
            }
            if matches!(edge, 6 | 7 | 8) {
                r.bottom += pull(b, w.bottom).unwrap_or(0);
            }
        }
    }
}

// ── Media keys ────────────────────────────────────────────────────────────────
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MediaKey {
    PlayPause,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
}

static MEDIA_KEY: AtomicU32 = AtomicU32::new(0);

pub fn take_media_key() -> Option<MediaKey> {
    Some(match MEDIA_KEY.swap(0, Ordering::Relaxed) {
        1 => MediaKey::PlayPause,
        2 => MediaKey::Play,
        3 => MediaKey::Pause,
        4 => MediaKey::Stop,
        5 => MediaKey::Next,
        6 => MediaKey::Previous,
        _ => return None,
    })
}

// ── Open file dialog ──────────────────────────────────────────────────────────
static DIALOG_OPEN: AtomicBool = AtomicBool::new(false);
static PICKED: Mutex<Option<String>> = Mutex::new(None);
static DIALOG_OWNER: AtomicIsize = AtomicIsize::new(0);

/// The file picked in the open dialog, once it has closed
pub fn take_picked_file() -> Option<String> {
    PICKED.lock().ok().and_then(|mut p| p.take())
}

/// Show Windows' "Open" dialog without blocking the player: it runs on its own
/// thread (the video keeps playing and drawing), and the choice is collected
/// with `take_picked_file`.
pub fn open_file_dialog(owner: isize, initial_dir: Option<String>) {
    if DIALOG_OPEN.swap(true, Ordering::Relaxed) {
        return; // already showing
    }
    DIALOG_OWNER.store(owner, Ordering::Relaxed);
    std::thread::spawn(move || {
        let picked = unsafe { run_open_dialog(owner, initial_dir) };
        if let Some(p) = picked {
            if let Ok(mut slot) = PICKED.lock() {
                *slot = Some(p);
            }
        }
        DIALOG_OPEN.store(false, Ordering::Relaxed);
        wake(DIALOG_OWNER.load(Ordering::Relaxed));
    });
}

unsafe fn run_open_dialog(owner: isize, initial_dir: Option<String>) -> Option<String> {
    CoInitializeEx(std::ptr::null_mut(), 0x2 /* COINIT_APARTMENTTHREADED */);
    let filter: Vec<u16> = concat!(
        "Media files\0",
        "*.mp4;*.mkv;*.avi;*.mov;*.wmv;*.flv;*.webm;*.m4v;*.ts;*.m2ts;*.mts;*.mpg;*.mpeg;*.3gp;*.ogv;",
        "*.mp3;*.flac;*.wav;*.m4a;*.aac;*.ogg;*.oga;*.opus;*.wma;*.alac;*.aiff;*.aif;*.ape;*.wv;*.mka;",
        "*.jpg;*.jpeg;*.png;*.gif;*.webp;*.avif;*.bmp;*.tif;*.tiff;*.jxl;*.m3u;*.m3u8\0",
        "Video\0*.mp4;*.mkv;*.avi;*.mov;*.wmv;*.flv;*.webm;*.m4v;*.ts;*.m2ts;*.mts;*.mpg;*.mpeg;*.3gp;*.ogv\0",
        "Music\0*.mp3;*.flac;*.wav;*.m4a;*.aac;*.ogg;*.oga;*.opus;*.wma;*.alac;*.aiff;*.aif;*.ape;*.wv;*.mka\0",
        "Pictures\0*.jpg;*.jpeg;*.png;*.gif;*.webp;*.avif;*.bmp;*.tif;*.tiff;*.jxl\0",
        "Playlists\0*.m3u;*.m3u8\0",
        "All files\0*.*\0\0"
    )
        .encode_utf16()
        .collect();
    let mut file = vec![0u16; 4096];
    let dir = initial_dir.map(|d| wide(&d));
    let title = wide("Open");
    let mut ofn: OpenFileNameW = std::mem::zeroed();
    ofn.struct_size = std::mem::size_of::<OpenFileNameW>() as u32;
    ofn.owner = owner;
    ofn.filter = filter.as_ptr();
    ofn.filter_index = 1;
    ofn.file = file.as_mut_ptr();
    ofn.max_file = file.len() as u32;
    ofn.initial_dir = dir.as_ref().map_or(std::ptr::null(), |d| d.as_ptr());
    ofn.title = title.as_ptr();
    // OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_HIDEREADONLY | OFN_ENABLESIZING
    ofn.flags = 0x0008_0000 | 0x0000_1000 | 0x0000_0800 | 0x0000_0004 | 0x0080_0000;
    if GetOpenFileNameW(&mut ofn) == 0 {
        return None; // cancelled
    }
    let len = file.iter().position(|&c| c == 0).unwrap_or(file.len());
    Some(String::from_utf16_lossy(&file[..len]))
}

/// The thumbnail button that was clicked since the last call, if any
pub fn take_click() -> Option<u32> {
    match CLICKED.swap(0, Ordering::Relaxed) {
        0 => None,
        id => Some(id),
    }
}

/// What the thumbnail buttons need to know about the player
#[derive(Clone, Copy, PartialEq)]
pub struct Status {
    pub has_file: bool,
    pub playing: bool,
    pub has_prev: bool,
    pub has_next: bool,
}

pub struct Taskbar {
    list: *mut c_void,
    hwnd: isize,
    /// prev, back, play, pause, forward, next
    icons: [isize; 6],
    buttons_added: bool,
    last_try: Instant,
    last_status: Option<Status>,
    last_progress: (i32, u64),
}

impl Taskbar {
    pub fn new(hwnd: isize) -> Option<Self> {
        unsafe {
            // Normally already initialised by the window library; harmless if so
            CoInitializeEx(std::ptr::null_mut(), 0x2 /* COINIT_APARTMENTTHREADED */);

            let mut list: *mut c_void = std::ptr::null_mut();
            let hr = CoCreateInstance(
                &CLSID_TASKBAR_LIST,
                std::ptr::null_mut(),
                0x1, /* CLSCTX_INPROC_SERVER */
                &IID_ITASKBAR_LIST3,
                &mut list,
            );
            if hr < 0 || list.is_null() {
                return None;
            }
            let hr_init: extern "system" fn(*mut c_void) -> i32 = std::mem::transmute(method(list, VT_HR_INIT));
            if hr_init(list) < 0 {
                return None;
            }

            BUTTON_MSG.store(RegisterWindowMessageW(wide("TaskbarButtonCreated").as_ptr()), Ordering::Relaxed);

            let icons = [
                make_icon(Glyph::Prev)?,
                make_icon(Glyph::Back)?,
                make_icon(Glyph::Play)?,
                make_icon(Glyph::Pause)?,
                make_icon(Glyph::Forward)?,
                make_icon(Glyph::Next)?,
            ];
            Some(Self {
                list,
                hwnd,
                icons,
                buttons_added: false,
                last_try: Instant::now() - Duration::from_secs(5),
                last_status: None,
                last_progress: (-1, 0),
            })
        }
    }

    /// Bring the taskbar in line with the player. Cheap: only talks to Windows when
    /// something actually changed.
    pub fn sync(&mut self, st: Status, pos_secs: f64, dur_secs: f64) {
        if BUTTONS_LOST.swap(false, Ordering::Relaxed) {
            self.buttons_added = false;
            self.last_status = None;
        }

        // Thumbnail buttons. They can only be added once the taskbar button exists,
        // so keep retrying (at most every 0.7 s) until Windows accepts them.
        if !self.buttons_added {
            if self.last_try.elapsed() > Duration::from_millis(700) {
                self.last_try = Instant::now();
                if self.thumb_call(VT_THUMB_ADD_BUTTONS, st) >= 0 {
                    self.buttons_added = true;
                    self.last_status = Some(st);
                }
            }
        } else if self.last_status != Some(st) {
            self.thumb_call(VT_THUMB_UPDATE_BUTTONS, st);
            self.last_status = Some(st);
        }

        // Progress bar on the icon
        let have_progress = st.has_file && dur_secs.is_finite() && dur_secs > 0.0;
        let state = if !have_progress {
            TBPF_NOPROGRESS
        } else if st.playing {
            TBPF_NORMAL
        } else {
            TBPF_PAUSED
        };
        let pos = pos_secs.clamp(0.0, if have_progress { dur_secs } else { 0.0 });
        let key = (state, pos as u64);
        if key != self.last_progress {
            self.last_progress = key;
            unsafe {
                let set_state: extern "system" fn(*mut c_void, isize, i32) -> i32 =
                    std::mem::transmute(method(self.list, VT_SET_PROGRESS_STATE));
                set_state(self.list, self.hwnd, state);
                if have_progress {
                    let set_value: extern "system" fn(*mut c_void, isize, u64, u64) -> i32 =
                        std::mem::transmute(method(self.list, VT_SET_PROGRESS_VALUE));
                    set_value(self.list, self.hwnd, (pos * 10.0) as u64, (dur_secs * 10.0) as u64);
                }
            }
        }
    }

    /// ThumbBarAddButtons / ThumbBarUpdateButtons for the current status
    fn thumb_call(&self, slot: usize, st: Status) -> i32 {
        let (play_icon, play_tip) = if st.playing {
            (self.icons[3], "Pause")
        } else {
            (self.icons[2], "Play")
        };
        let buttons = [
            thumb(PREV, self.icons[0], "Previous file", st.has_prev),
            thumb(BACK, self.icons[1], "Back 10 seconds", st.has_file),
            thumb(PLAY, play_icon, play_tip, st.has_file),
            thumb(FWD, self.icons[4], "Forward 10 seconds", st.has_file),
            thumb(NEXT, self.icons[5], "Next file", st.has_next),
        ];
        unsafe {
            let f: extern "system" fn(*mut c_void, isize, u32, *const ThumbButton) -> i32 =
                std::mem::transmute(method(self.list, slot));
            f(self.list, self.hwnd, buttons.len() as u32, buttons.as_ptr())
        }
    }
}

fn thumb(id: u32, icon: isize, tip: &str, enabled: bool) -> ThumbButton {
    let mut tip_buf = [0_u16; 260];
    for (dst, src) in tip_buf.iter_mut().zip(tip.encode_utf16().take(259)) {
        *dst = src;
    }
    ThumbButton {
        mask: THB_ICON | THB_TOOLTIP | THB_FLAGS,
        id,
        bitmap: 0,
        icon,
        tip: tip_buf,
        flags: if enabled { THBF_ENABLED } else { THBF_DISABLED },
    }
}

/// Window procedure hook: receives the thumbnail button clicks (WM_COMMAND with
/// THBN_CLICKED) and the minimised-window wake-up timer.
unsafe extern "system" fn subclass_proc(
    hwnd: isize,
    msg: u32,
    wparam: usize,
    lparam: isize,
    _subclass_id: usize,
    _ref_data: usize,
) -> isize {
    if (msg == WM_MOVING || msg == WM_SIZING) && lparam != 0 && SNAP.load(Ordering::Relaxed) {
        // (then carries on to the normal handling with the adjusted rectangle)
        snap_rect(hwnd, &mut *(lparam as *mut RectI), (msg == WM_SIZING).then_some(wparam));
    }
    if msg == WM_COPYDATA && lparam != 0 {
        let cds = &*(lparam as *const CopyDataStruct);
        if cds.data == COPYDATA_MAGIC {
            let n = cds.len as usize / 2;
            let path = if n > 0 && !cds.ptr.is_null() {
                String::from_utf16_lossy(std::slice::from_raw_parts(cds.ptr as *const u16, n))
            } else {
                String::new()
            };
            if let Ok(mut slot) = FORWARDED.lock() {
                *slot = Some(path);
            }
            wake(hwnd);
            return 1;
        }
    } else if msg == WM_KEYDOWN && (0xB0..=0xB3).contains(&wparam) {
        // VK_MEDIA_NEXT_TRACK / PREV_TRACK / STOP / PLAY_PAUSE
        let key = match wparam {
            0xB0 => 5,
            0xB1 => 6,
            0xB2 => 4,
            _ => 1,
        };
        MEDIA_KEY.store(key, Ordering::Relaxed);
        wake(hwnd);
        return 0;
    } else if msg == WM_APPCOMMAND {
        let cmd = ((lparam as usize >> 16) & 0x0FFF) as u32;
        let key = match cmd {
            14 => 1, // APPCOMMAND_MEDIA_PLAY_PAUSE
            46 => 2, // APPCOMMAND_MEDIA_PLAY
            47 => 3, // APPCOMMAND_MEDIA_PAUSE
            13 => 4, // APPCOMMAND_MEDIA_STOP
            11 => 5, // APPCOMMAND_MEDIA_NEXTTRACK
            12 => 6, // APPCOMMAND_MEDIA_PREVIOUSTRACK
            _ => 0,
        };
        if key != 0 {
            MEDIA_KEY.store(key, Ordering::Relaxed);
            wake(hwnd);
            return 1;
        }
    }
    if msg == WM_ERASEBKGND {
        // The whole client area is painted by OpenGL; erasing it first only flashes.
        return 1;
    } else if msg == WM_APP_FULLSCREEN {
        if wparam == 1 {
            enter_fullscreen_now(hwnd);
        } else {
            exit_fullscreen_now(hwnd);
        }
        return 0;
    } else if msg == WM_COMMAND {
        let code = ((wparam >> 16) & 0xFFFF) as u32;
        let id = (wparam & 0xFFFF) as u32;
        if code == THBN_CLICKED && (PREV..=NEXT).contains(&id) {
            CLICKED.store(id, Ordering::Relaxed);
            wake(hwnd);
            return 0;
        }
    } else if msg == WM_TIMER && wparam == TIMER_ID {
        if IsIconic(hwnd) != 0 {
            wake(hwnd);
        }
        return 0;
    } else if msg != 0 && msg == BUTTON_MSG.load(Ordering::Relaxed) {
        BUTTONS_LOST.store(true, Ordering::Relaxed);
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

/// Make the UI loop run one update even if the window is minimised
/// (eframe doesn't schedule redraws for minimised windows on its own).
fn wake(hwnd: isize) {
    unsafe {
        RedrawWindow(hwnd, std::ptr::null(), 0, RDW_INTERNALPAINT);
    }
}

// ── Button icons, drawn in code ───────────────────────────────────────────────
#[derive(Clone, Copy)]
enum Glyph {
    Prev,
    Back,
    Play,
    Pause,
    Forward,
    Next,
}

type P = (f32, f32);

fn in_tri(p: P, a: P, b: P, c: P) -> bool {
    let s = |p1: P, p2: P, p3: P| (p1.0 - p3.0) * (p2.1 - p3.1) - (p2.0 - p3.0) * (p1.1 - p3.1);
    let (d1, d2, d3) = (s(p, a, b), s(p, b, c), s(p, c, a));
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

fn in_rect(p: P, x0: f32, x1: f32, y0: f32, y1: f32) -> bool {
    p.0 >= x0 && p.0 <= x1 && p.1 >= y0 && p.1 <= y1
}

/// Triangle with its flat edge at x = `base` and its tip at x = `tip`
/// (points right if tip > base, left otherwise)
fn tri(p: P, base: f32, tip: f32) -> bool {
    in_tri(p, (base, 7.0), (base, 25.0), (tip, 16.0))
}

fn glyph_hit(g: Glyph, p: P) -> bool {
    match g {
        Glyph::Play => tri(p, 10.5, 25.5),
        Glyph::Pause => in_rect(p, 8.5, 14.0, 7.0, 25.0) || in_rect(p, 18.0, 23.5, 7.0, 25.0),
        Glyph::Prev => in_rect(p, 6.5, 10.0, 7.0, 25.0) || tri(p, 25.5, 12.0),
        Glyph::Next => in_rect(p, 22.0, 25.5, 7.0, 25.0) || tri(p, 6.5, 20.0),
        Glyph::Back => tri(p, 16.0, 3.5) || tri(p, 28.5, 16.0),
        Glyph::Forward => tri(p, 3.5, 16.0) || tri(p, 16.0, 28.5),
    }
}

/// 32×32 icon: white glyph with a soft dark halo, so it reads on light and dark
/// thumbnail backgrounds alike. Returns an HICON.
fn make_icon(g: Glyph) -> Option<isize> {
    const N: usize = 32;
    const SS: usize = 4; // supersampling for smooth edges

    let mut cov = [0.0_f32; N * N];
    for y in 0..N {
        for x in 0..N {
            let mut hits = 0;
            for sy in 0..SS {
                for sx in 0..SS {
                    let p = (
                        x as f32 + (sx as f32 + 0.5) / SS as f32,
                        y as f32 + (sy as f32 + 0.5) / SS as f32,
                    );
                    if glyph_hit(g, p) {
                        hits += 1;
                    }
                }
            }
            cov[y * N + x] = hits as f32 / (SS * SS) as f32;
        }
    }

    // Halo = the glyph blurred (3×3 box, twice) and boosted
    let blur = |src: &[f32; N * N]| {
        let mut out = [0.0_f32; N * N];
        for y in 0..N {
            for x in 0..N {
                let mut sum = 0.0;
                for dy in -1_i32..=1 {
                    for dx in -1_i32..=1 {
                        let (xx, yy) = (x as i32 + dx, y as i32 + dy);
                        if (0..N as i32).contains(&xx) && (0..N as i32).contains(&yy) {
                            sum += src[yy as usize * N + xx as usize];
                        }
                    }
                }
                out[y * N + x] = sum / 9.0;
            }
        }
        out
    };
    let halo = blur(&blur(&cov));

    let mut bgra = [0_u8; N * N * 4];
    for i in 0..N * N {
        let m = cov[i];
        let h = (halo[i] * 2.2).min(1.0) * 0.6;
        let a = m + (1.0 - m) * h; // result alpha
        let c = if a > 0.0 { 255.0 * m / a } else { 0.0 }; // white weighted against the black halo
        let px = &mut bgra[i * 4..i * 4 + 4];
        px[0] = c as u8;
        px[1] = c as u8;
        px[2] = c as u8;
        px[3] = (a * 255.0).round() as u8;
    }

    unsafe {
        let color = CreateBitmap(N as i32, N as i32, 1, 32, bgra.as_ptr() as *const c_void);
        let mask_bits = [0_u8; N * N / 8];
        let mask = CreateBitmap(N as i32, N as i32, 1, 1, mask_bits.as_ptr() as *const c_void);
        let info = IconInfo { is_icon: 1, x_hotspot: 0, y_hotspot: 0, mask, color };
        let icon = CreateIconIndirect(&info);
        DeleteObject(color);
        DeleteObject(mask);
        (icon != 0).then_some(icon)
    }
}
