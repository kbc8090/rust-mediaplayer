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
#[derive(Clone, Copy, Default)]
struct Point {
    x: i32,
    y: i32,
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
    fn SetPropW(hwnd: isize, name: *const u16, data: isize) -> i32;
    fn GetPropW(hwnd: isize, name: *const u16) -> isize;
    fn EnumWindows(callback: unsafe extern "system" fn(isize, isize) -> i32, lparam: isize) -> i32;
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
    fn GetCursorPos(point: *mut Point) -> i32;
    fn EnumDisplaySettingsW(device: *const u16, mode: u32, devmode: *mut u32) -> i32;
    fn SendMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn OpenClipboard(hwnd: isize) -> i32;
    fn CloseClipboard() -> i32;
    fn GetClipboardData(format: u32) -> isize;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalLock(mem: isize) -> *const u16;
    fn GlobalUnlock(mem: isize) -> i32;
}

/// The text on the clipboard, if there is any
pub fn clipboard_text() -> Option<String> {
    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        if OpenClipboard(0) == 0 {
            return None;
        }
        let mut text = None;
        let mem = GetClipboardData(CF_UNICODETEXT);
        if mem != 0 {
            let ptr = GlobalLock(mem);
            if !ptr.is_null() {
                let mut n = 0;
                while n < 1 << 20 && *ptr.add(n) != 0 {
                    n += 1;
                }
                text = Some(String::from_utf16_lossy(std::slice::from_raw_parts(ptr, n)));
                GlobalUnlock(mem);
            }
        }
        CloseClipboard();
        text
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateMutexW(attributes: *const c_void, initial_owner: i32, name: *const u16) -> isize;
    fn GetLastError() -> u32;
    fn WaitForSingleObject(handle: isize, ms: u32) -> u32;
    fn ReleaseMutex(mutex: isize) -> i32;
    fn CreateFileMappingW(file: isize, attributes: *const c_void, protect: u32, size_high: u32, size_low: u32, name: *const u16) -> isize;
    fn MapViewOfFile(mapping: isize, access: u32, offset_high: u32, offset_low: u32, bytes: usize) -> *mut u64;
    fn GetTickCount64() -> u64;
    fn GetCurrentProcessId() -> u32;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
}

/// Several launches within this long of each other count as one (Explorer's
/// "Open" on several selected files starts the player once per file)
const BURST_MS: u64 = 1500;

/// With multiple windows allowed: is another player starting right now (a burst
/// of launches)? Then returns its process id, to hand this launch's files to.
/// Otherwise notes this launch as the start of a possible burst and returns None.
/// (Shared between launches in a small named memory block: the last launch's
/// time, and the process that started the burst.)
unsafe fn burst_receiver() -> Option<u32> {
    let lock = CreateMutexW(std::ptr::null(), 0, wide("Local\\RustMediaPlayer.LaunchLock").as_ptr());
    if lock == 0 {
        return None;
    }
    WaitForSingleObject(lock, 2000);
    // (kept open for as long as this process runs, so later launches find it)
    let map = CreateFileMappingW(-1, std::ptr::null(), 0x04 /* PAGE_READWRITE */, 0, 16, wide("Local\\RustMediaPlayer.LastLaunch").as_ptr());
    let view = if map != 0 { MapViewOfFile(map, 0x0002 | 0x0004 /* write | read */, 0, 0, 16) } else { std::ptr::null_mut() };
    let mut receiver = None;
    if !view.is_null() {
        let now = GetTickCount64();
        let (last, pid) = (*view, *view.add(1) as u32);
        if pid != 0 && now.saturating_sub(last) < BURST_MS {
            receiver = Some(pid); // part of a burst: keep it going
        } else {
            *view.add(1) = GetCurrentProcessId() as u64; // a new one, started by this player
        }
        *view = now;
    }
    ReleaseMutex(lock);
    receiver
}

/// The player window of process `pid` (0 if none yet)
unsafe fn find_window_of(pid: u32) -> isize {
    struct Search {
        tag: Vec<u16>,
        pid: u32,
        found: isize,
    }
    unsafe extern "system" fn check(hwnd: isize, lparam: isize) -> i32 {
        let s = &mut *(lparam as *mut Search);
        let mut owner = 0u32;
        GetWindowThreadProcessId(hwnd, &mut owner);
        if owner == s.pid && GetPropW(hwnd, s.tag.as_ptr()) != 0 {
            s.found = hwnd;
            return 0;
        }
        1
    }
    let mut s = Search { tag: wide(WINDOW_TAG), pid, found: 0 };
    EnumWindows(check, &mut s as *mut Search as isize);
    s.found
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
const WM_ENTERSIZEMOVE: u32 = 0x0231;
const WM_EXITSIZEMOVE: u32 = 0x0232;
const WM_GETMINMAXINFO: u32 = 0x0024;
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
        // A tag on the window, so a second launch can find this player (see
        // forward_to_running_instance)
        SetPropW(hwnd, wide(WINDOW_TAG).as_ptr(), 1);
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

#[link(name = "user32")]
unsafe extern "system" {
    fn GetDC(hwnd: isize) -> isize;
    fn ReleaseDC(hwnd: isize, dc: isize) -> i32;
    fn FillRect(dc: isize, rect: *const RectI, brush: isize) -> i32;
    fn GetClientRect(hwnd: isize, rect: *mut RectI) -> i32;
}
#[link(name = "gdi32")]
unsafe extern "system" {
    fn GetStockObject(which: i32) -> isize;
}

/// Fill the window black at once, before OpenGL has drawn anything in it
/// (Windows shows a new window white until then)
pub fn paint_black(hwnd: isize) {
    unsafe {
        let mut r: RectI = std::mem::zeroed();
        if GetClientRect(hwnd, &mut r) != 0 {
            let dc = GetDC(hwnd);
            if dc != 0 {
                FillRect(dc, &r, GetStockObject(4 /* BLACK_BRUSH */));
                ReleaseDC(hwnd, dc);
            }
        }
    }
}

/// Hide the window from the screen while it stays open and sized as it is
/// (DWM "cloaking"), or show it again: so it can be maximized before anything
/// is drawn in it, without a white flash
pub fn cloak(hwnd: isize, on: bool) {
    let v: i32 = on as i32;
    unsafe {
        DwmSetWindowAttribute(hwnd, 13 /* DWMWA_CLOAK */, &v as *const i32 as *const c_void, 4);
    }
}

// ── Windows 11 corners and border ─────────────────────────────────────────────
/// The window gets Windows 11's own rounded corners (and the thin border Windows
/// draws with them). Our window has no standard frame, so Windows has to be asked.
static ROUNDED: AtomicBool = AtomicBool::new(false);

/// Ask for rounded corners. Returns false on Windows 10, which has no such thing.
pub fn round_corners(hwnd: isize) -> bool {
    let ok = set_corners(hwnd, true);
    ROUNDED.store(ok, Ordering::Relaxed);
    ok
}

/// DWMWA_WINDOW_CORNER_PREFERENCE: DWMWCP_ROUND (2) / DWMWCP_DONOTROUND (1)
fn set_corners(hwnd: isize, round: bool) -> bool {
    let pref: u32 = if round { 2 } else { 1 };
    unsafe { DwmSetWindowAttribute(hwnd, 33, &pref as *const u32 as *const c_void, 4) == 0 }
}

/// Windows 11 border colour as 0x00BBGGRR, or BORDER_DEFAULT (Windows decides: grey,
/// or the accent colour when "Show accent colour on title bars and window borders"
/// is on) or BORDER_NONE
pub const BORDER_DEFAULT: u32 = 0xFFFF_FFFF;
pub const BORDER_NONE: u32 = 0xFFFF_FFFE;
/// The grey Windows 11 gives window borders in dark mode
pub const BORDER_GREY: u32 = 0x0040_4040;

pub fn set_border_color(hwnd: isize, color: u32) {
    // DWMWA_BORDER_COLOR
    unsafe { DwmSetWindowAttribute(hwnd, 34, &color as *const u32 as *const c_void, 4) };
}

unsafe fn enter_fullscreen_now(hwnd: isize) {
    if ROUNDED.load(Ordering::Relaxed) {
        set_corners(hwnd, false); // rounded corners on a full screen look wrong
    }
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

/// While the app is in fullscreen: does the window still cover its monitor?
/// Something outside the app can move or resize it, for example Win+Down,
/// Win+arrow snapping, Show Desktop, or Windows rearranging windows when a monitor
/// wakes up or changes. The app then has to drop its fullscreen state, or it keeps
/// refusing to be dragged or resized. Minimised counts as still fullscreen
/// (restoring brings it back as it was), and so does anything that can't be checked.
pub fn still_fullscreen(hwnd: isize) -> bool {
    unsafe {
        if IsIconic(hwnd) != 0 {
            return true;
        }
        let mut w = RectI::default();
        let mut info = MonitorInfo {
            size: std::mem::size_of::<MonitorInfo>() as u32,
            monitor: RectI::default(),
            work: RectI::default(),
            flags: 0,
        };
        let monitor = MonitorFromWindow(hwnd, 2); // MONITOR_DEFAULTTONEAREST
        if monitor == 0 || GetWindowRect(hwnd, &mut w) == 0 || GetMonitorInfoW(monitor, &mut info) == 0 {
            return true;
        }
        let m = info.monitor;
        w.left <= m.left && w.top <= m.top && w.right >= m.right && w.bottom >= m.bottom
    }
}

/// Fullscreen was ended from outside the app (see `still_fullscreen`): the window
/// is already wherever Windows put it, so just forget the size to go back to and
/// put the rounded corners back
pub fn forget_fullscreen(hwnd: isize) {
    if let Ok(mut r) = FS_RESTORE.lock() {
        *r = None;
    }
    FS_WAS_MAX.store(false, Ordering::Relaxed);
    if ROUNDED.load(Ordering::Relaxed) {
        set_corners(hwnd, true);
    }
}

unsafe fn exit_fullscreen_now(hwnd: isize) {
    if ROUNDED.load(Ordering::Relaxed) {
        set_corners(hwnd, true);
    }
    let saved = FS_RESTORE.lock().ok().and_then(|mut r| r.take());
    if let Some([l, t, r, b]) = saved {
        SetWindowPos(hwnd, 0, l, t, r - l, b - t, 0x0200);
        if FS_WAS_MAX.swap(false, Ordering::Relaxed) {
            ShowWindow(hwnd, 3); // SW_MAXIMIZE
        }
    }
}

// ── Single window ─────────────────────────────────────────────────────────────
/// The tag on the player's window. (It used to be found by its title, but the
/// title now names the file playing; and a folder called RustMediaPlayer open in
/// Explorer has a window with that title too.)
const WINDOW_TAG: &str = "RustMediaPlayer.Window";

/// The first top-level window carrying WINDOW_TAG (0 if none)
unsafe fn find_player_window() -> isize {
    struct Search {
        tag: Vec<u16>,
        found: isize,
    }
    unsafe extern "system" fn check(hwnd: isize, lparam: isize) -> i32 {
        let s = &mut *(lparam as *mut Search);
        if GetPropW(hwnd, s.tag.as_ptr()) != 0 {
            s.found = hwnd;
            return 0; // stop
        }
        1
    }
    let mut s = Search { tag: wide(WINDOW_TAG), found: 0 };
    EnumWindows(check, &mut s as *mut Search as isize);
    s.found
}

/// Files handed over by later launches of the player ("" = just come to front).
/// A list: Explorer's "Open" on several files starts the player once per file,
/// so several can arrive between two frames.
static FORWARDED: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn take_forwarded() -> Vec<String> {
    FORWARDED.lock().map(|mut f| std::mem::take(&mut *f)).unwrap_or_default()
}

/// Called at startup, before any window exists. If the player is already running,
/// hand it `files` (it opens them in place of what's playing), bring it to the front and
/// return true: this process should then just exit.
/// With `forward` false (multiple windows allowed) it only hands over when it's
/// part of a burst of launches (several files opened together from Explorer);
/// otherwise it still marks this player as running for later launches.
pub fn forward_to_running_instance(files: &[String], forward: bool) -> bool {
    unsafe {
        // The mutex lives as long as this process; a second process finds it taken
        let mutex = CreateMutexW(std::ptr::null(), 0, wide("Local\\RustMediaPlayer.SingleWindow").as_ptr());
        let running = mutex != 0 && GetLastError() == 183 /* ERROR_ALREADY_EXISTS */;
        // Multiple windows allowed: still one window for files opened together
        // from Explorer (a burst of launches), handed to the player that started it
        let burst = if forward { None } else { burst_receiver() };
        if !running || (!forward && burst.is_none()) || files.is_empty() && burst.is_some() {
            return false;
        }
        // The other player may still be starting up: give its window a moment
        let mut hwnd = 0;
        for _ in 0..30 {
            hwnd = match burst {
                Some(pid) => find_window_of(pid),
                None => find_player_window(),
            };
            if hwnd != 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if hwnd == 0 {
            return false; // couldn't find it: just run as a second player
        }
        // (each file in turn; none: just "come to the front")
        let none = [String::new()];
        for file in if files.is_empty() { &none[..] } else { files } {
            let payload: Vec<u16> = file.encode_utf16().collect();
            let cds = CopyDataStruct {
                data: COPYDATA_MAGIC,
                len: (payload.len() * 2) as u32,
                ptr: payload.as_ptr() as *const c_void,
            };
            let mut result = 0usize;
            SendMessageTimeoutW(hwnd, WM_COPYDATA, 0, &cds as *const _ as isize, 0x2 /* SMTO_ABORTIFHUNG */, 3000, &mut result);
        }
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, 9); // SW_RESTORE
        }
        SetForegroundWindow(hwnd);
        true
    }
}

// ── Monitor refresh rate ──────────────────────────────────────────────────────
/// The refresh rate (Hz) of the monitor the window is on, for smooth motion
pub fn monitor_refresh_hz(hwnd: isize) -> Option<u32> {
    #[repr(C)]
    struct MonitorInfoEx {
        info: MonitorInfo,
        device: [u16; 32],
    }
    unsafe {
        let mut mi: MonitorInfoEx = std::mem::zeroed();
        mi.info.size = std::mem::size_of::<MonitorInfoEx>() as u32;
        if GetMonitorInfoW(MonitorFromWindow(hwnd, 2), &mut mi as *mut MonitorInfoEx as *mut MonitorInfo) == 0 {
            return None;
        }
        // DEVMODEW: 220 bytes; dmSize at byte 68, dmDisplayFrequency at byte 184
        let mut dm = [0u32; 55];
        dm[17] = 220; // dmSize (u16 at 68) + dmDriverExtra 0 (u16 at 70)
        if EnumDisplaySettingsW(mi.device.as_ptr(), 0xFFFF_FFFF /* ENUM_CURRENT_SETTINGS */, dm.as_mut_ptr()) == 0 {
            return None;
        }
        let hz = dm[46];
        (hz > 1).then_some(hz)
    }
}

// ── Snap to screen edges ──────────────────────────────────────────────────────
static SNAP: AtomicBool = AtomicBool::new(true);

/// Moving / resizing the window pulls its edges onto the screen's edges when they
/// come within 6 px (menu setting)
pub fn set_snap(on: bool) {
    SNAP.store(on, Ordering::Relaxed);
}

/// Where a move / resize started. Windows moves the window by each small mouse step
/// from wherever it was put last, so after a snap, slow mouse steps were each
/// snapped back and only a fast flick could break free. Instead, the window's
/// intended place is worked out from the total mouse movement since the start.
struct DragStart {
    cursor: Point,
    rect: RectI,
    /// Smallest window size (resizing must not go below it)
    min: Point,
    /// Snapped to an edge: left/right axis, top/bottom axis (moving), or each of
    /// the four sides (resizing)
    snapped: [bool; 4],
}

thread_local! {
    static DRAG: std::cell::RefCell<Option<DragStart>> = const { std::cell::RefCell::new(None) };
}

unsafe fn drag_begin(hwnd: isize) {
    let mut cursor = Point::default();
    let mut rect = RectI::default();
    if GetCursorPos(&mut cursor) == 0 || GetWindowRect(hwnd, &mut rect) == 0 {
        return;
    }
    // Ask the window (winit) for its minimum size, as Windows itself does
    let mut mmi = [Point::default(); 5];
    SendMessageW(hwnd, WM_GETMINMAXINFO, 0, mmi.as_mut_ptr() as isize);
    let min = mmi[3];
    DRAG.with(|d| *d.borrow_mut() = Some(DragStart { cursor, rect, min, snapped: [false; 4] }));
}

/// Replace the proposed rectangle of a move (`sizing` = None) or a resize (`sizing`
/// = the edge being dragged, WMSZ_*) with one following the mouse exactly, with
/// its edges snapped to the screen's edges when they're close
unsafe fn snap_rect(hwnd: isize, r: &mut RectI, sizing: Option<usize>) {
    DRAG.with(|d| {
        let mut d = d.borrow_mut();
        let Some(st) = d.as_mut() else { return };
        let mut cur = Point::default();
        if GetCursorPos(&mut cur) == 0 {
            return;
        }
        // Dragging a maximized window un-maximizes it mid-drag (new size, put under
        // the mouse by Windows): measure from there instead
        if sizing.is_none() && (r.right - r.left, r.bottom - r.top) != (st.rect.right - st.rect.left, st.rect.bottom - st.rect.top) {
            st.rect = *r;
            st.cursor = cur;
            st.snapped = [false; 4];
        }
        let (dx, dy) = (cur.x - st.cursor.x, cur.y - st.cursor.y);

        // Where the mouse alone would put the window
        let mut want = st.rect;
        match sizing {
            None => {
                want.left += dx;
                want.right += dx;
                want.top += dy;
                want.bottom += dy;
            }
            Some(edge) => {
                // WMSZ_LEFT 1, RIGHT 2, TOP 3, TOPLEFT 4, TOPRIGHT 5, BOTTOM 6, BOTTOMLEFT 7, BOTTOMRIGHT 8
                if matches!(edge, 1 | 4 | 7) { want.left += dx; }
                if matches!(edge, 2 | 5 | 8) { want.right += dx; }
                if matches!(edge, 3..=5) { want.top += dy; }
                if matches!(edge, 6..=8) { want.bottom += dy; }
            }
        }

        if SNAP.load(Ordering::Relaxed) {
            let mut mi: MonitorInfo = std::mem::zeroed();
            mi.size = std::mem::size_of::<MonitorInfo>() as u32;
            if GetMonitorInfoW(MonitorFromRect(&want, 2 /* MONITOR_DEFAULTTONEAREST */), &mut mi) != 0 {
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
                // Snaps within 6 px, lets go after 10 px (at 100 % display scaling)
                let dpi = GetDpiForWindow(hwnd).max(96) as i32;
                let (enter, leave) = ((6 * dpi + 48) / 96, (10 * dpi + 48) / 96);
                let pull = |edge: i32, target: i32, snapped: &mut bool| {
                    let reach = if *snapped { leave } else { enter };
                    let d = target - edge;
                    *snapped = d.abs() <= reach;
                    if *snapped { d } else { 0 }
                };
                let (l, t, rt, b) = (want.left + il, want.top + it, want.right - ir, want.bottom - ib);
                match sizing {
                    None => {
                        // Per axis: whichever of the two screen edges is nearer
                        let x_to = if (l - w.left).abs() <= (rt - w.right).abs() { (l, w.left) } else { (rt, w.right) };
                        let y_to = if (t - w.top).abs() <= (b - w.bottom).abs() { (t, w.top) } else { (b, w.bottom) };
                        let ddx = pull(x_to.0, x_to.1, &mut st.snapped[0]);
                        let ddy = pull(y_to.0, y_to.1, &mut st.snapped[1]);
                        want.left += ddx;
                        want.right += ddx;
                        want.top += ddy;
                        want.bottom += ddy;
                    }
                    Some(edge) => {
                        if matches!(edge, 1 | 4 | 7) { want.left += pull(l, w.left, &mut st.snapped[0]); }
                        if matches!(edge, 2 | 5 | 8) { want.right += pull(rt, w.right, &mut st.snapped[1]); }
                        if matches!(edge, 3..=5) { want.top += pull(t, w.top, &mut st.snapped[2]); }
                        if matches!(edge, 6..=8) { want.bottom += pull(b, w.bottom, &mut st.snapped[3]); }
                    }
                }
            }
        }

        // Never resize below the minimum size (move the dragged edge back instead)
        if let Some(edge) = sizing {
            if want.right - want.left < st.min.x {
                if matches!(edge, 1 | 4 | 7) { want.left = want.right - st.min.x; } else { want.right = want.left + st.min.x; }
            }
            if want.bottom - want.top < st.min.y {
                if matches!(edge, 3..=5) { want.top = want.bottom - st.min.y; } else { want.bottom = want.top + st.min.y; }
            }
        }
        *r = want;
    });
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
static PICKED: Mutex<Vec<String>> = Mutex::new(Vec::new());
static DIALOG_OWNER: AtomicIsize = AtomicIsize::new(0);

/// The files picked in the open dialog (one or several), once it has closed
pub fn take_picked_files() -> Vec<String> {
    PICKED.lock().map(|mut p| std::mem::take(&mut *p)).unwrap_or_default()
}

/// Show Windows' "Open" dialog without blocking the player: it runs on its own
/// thread (the video keeps playing and drawing), and the choice is collected
/// with `take_picked_files`.
pub fn open_file_dialog(owner: isize, initial_dir: Option<String>) {
    if DIALOG_OPEN.swap(true, Ordering::Relaxed) {
        return; // already showing
    }
    DIALOG_OWNER.store(owner, Ordering::Relaxed);
    std::thread::spawn(move || {
        let picked = unsafe { run_open_dialog(owner, initial_dir) };
        if !picked.is_empty() {
            if let Ok(mut slot) = PICKED.lock() {
                *slot = picked;
            }
        }
        DIALOG_OPEN.store(false, Ordering::Relaxed);
        wake(DIALOG_OWNER.load(Ordering::Relaxed));
    });
}

unsafe fn run_open_dialog(owner: isize, initial_dir: Option<String>) -> Vec<String> {
    CoInitializeEx(std::ptr::null_mut(), 0x2 /* COINIT_APARTMENTTHREADED */);
    let filter: Vec<u16> = concat!(
        "Media files\0",
        "*.mp4;*.mkv;*.avi;*.mov;*.wmv;*.flv;*.webm;*.m4v;*.ts;*.m2ts;*.mts;*.mpg;*.mpeg;*.3gp;*.ogv;",
        "*.mp3;*.flac;*.wav;*.m4a;*.aac;*.ogg;*.oga;*.opus;*.wma;*.alac;*.aiff;*.aif;*.ape;*.wv;*.mka;",
        "*.jpg;*.jpeg;*.png;*.gif;*.webp;*.avif;*.bmp;*.tif;*.tiff;*.jxl;*.heic;*.heif;*.m3u;*.m3u8;*.zip;*.rar;*.7z\0",
        "Video\0*.mp4;*.mkv;*.avi;*.mov;*.wmv;*.flv;*.webm;*.m4v;*.ts;*.m2ts;*.mts;*.mpg;*.mpeg;*.3gp;*.ogv\0",
        "Music\0*.mp3;*.flac;*.wav;*.m4a;*.aac;*.ogg;*.oga;*.opus;*.wma;*.alac;*.aiff;*.aif;*.ape;*.wv;*.mka\0",
        "Pictures\0*.jpg;*.jpeg;*.png;*.gif;*.webp;*.avif;*.bmp;*.tif;*.tiff;*.jxl;*.heic;*.heif\0",
        "Playlists\0*.m3u;*.m3u8\0",
        "Archives\0*.zip;*.rar;*.7z\0",
        "All files\0*.*\0\0"
    )
        .encode_utf16()
        .collect();
    // (room for a few hundred picked files' names)
    let mut file = vec![0u16; 65_536];
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
    // OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_HIDEREADONLY |
    // OFN_ENABLESIZING | OFN_ALLOWMULTISELECT
    ofn.flags = 0x0008_0000 | 0x0000_1000 | 0x0000_0800 | 0x0000_0004 | 0x0080_0000 | 0x0000_0200;
    if GetOpenFileNameW(&mut ofn) == 0 {
        return Vec::new(); // cancelled
    }
    // One file: its full path. Several: the folder, then each file's name, each
    // ending in a 0 (and a second 0 after the last)
    let parts: Vec<String> = file.split(|&c| c == 0).take_while(|p| !p.is_empty()).map(String::from_utf16_lossy).collect();
    match parts.as_slice() {
        [] => Vec::new(),
        [one] => vec![one.clone()],
        [dir, names @ ..] => names.iter().map(|n| std::path::Path::new(dir).join(n).to_string_lossy().into_owned()).collect(),
    }
}

// ── Folder picker (Save As…) ──────────────────────────────────────────────────
/// {DC1C5A9C-E88A-4DDE-A5A1-60F82A20AEF7}
const CLSID_FILE_OPEN_DIALOG: Guid = Guid {
    data1: 0xDC1C_5A9C,
    data2: 0xE88A,
    data3: 0x4DDE,
    data4: [0xA5, 0xA1, 0x60, 0xF8, 0x2A, 0x20, 0xAE, 0xF7],
};
/// {D57C7288-D4AD-4768-BE02-9D969532D960}
const IID_IFILE_OPEN_DIALOG: Guid = Guid {
    data1: 0xD57C_7288,
    data2: 0xD4AD,
    data3: 0x4768,
    data4: [0xBE, 0x02, 0x9D, 0x96, 0x95, 0x32, 0xD9, 0x60],
};
/// {43826D1E-E718-42EE-BC55-A1E261C37BFE}
const IID_ISHELL_ITEM: Guid = Guid {
    data1: 0x4382_6D1E,
    data2: 0xE718,
    data3: 0x42EE,
    data4: [0xBC, 0x55, 0xA1, 0xE2, 0x61, 0xC3, 0x7B, 0xFE],
};
// IFileDialog vtable slots (IUnknown 0-2, IModalWindow::Show 3, IFileDialog 4-26)
const VT_RELEASE: usize = 2;
const VT_SHOW: usize = 3;
const VT_SET_OPTIONS: usize = 9;
const VT_GET_OPTIONS: usize = 10;
const VT_SET_DEFAULT_FOLDER: usize = 11;
const VT_SET_TITLE: usize = 17;
const VT_SET_OK_BUTTON_LABEL: usize = 18;
const VT_GET_RESULT: usize = 20;
// IShellItem (IUnknown 0-2, BindToHandler 3, GetParent 4, GetDisplayName 5)
const VT_GET_DISPLAY_NAME: usize = 5;
const FOS_PICKFOLDERS: u32 = 0x20;
const FOS_FORCEFILESYSTEM: u32 = 0x40;
const SIGDN_FILESYSPATH: u32 = 0x8005_8000;

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoTaskMemFree(mem: *mut c_void);
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn SHCreateItemFromParsingName(path: *const u16, bind: *mut c_void, iid: *const Guid, out: *mut *mut c_void) -> i32;
}

static PICKED_FOLDER: Mutex<Option<String>> = Mutex::new(None);

/// The folder picked in the folder dialog, once it has closed
pub fn take_picked_folder() -> Option<String> {
    PICKED_FOLDER.lock().ok().and_then(|mut p| p.take())
}

/// Show Windows' folder picker ("Save here") without blocking the player, like
/// the open dialog. Windows remembers the last folder used; the first time it
/// starts in Videos. The choice is collected with `take_picked_folder`.
pub fn folder_dialog(owner: isize) {
    if DIALOG_OPEN.swap(true, Ordering::Relaxed) {
        return; // a dialog is already showing
    }
    DIALOG_OWNER.store(owner, Ordering::Relaxed);
    std::thread::spawn(move || {
        let picked = unsafe { run_folder_dialog(owner) };
        if let Some(p) = picked {
            if let Ok(mut slot) = PICKED_FOLDER.lock() {
                *slot = Some(p);
            }
        }
        DIALOG_OPEN.store(false, Ordering::Relaxed);
        wake(DIALOG_OWNER.load(Ordering::Relaxed));
    });
}

unsafe fn run_folder_dialog(owner: isize) -> Option<String> {
    CoInitializeEx(std::ptr::null_mut(), 0x2 /* COINIT_APARTMENTTHREADED */);
    let mut dialog: *mut c_void = std::ptr::null_mut();
    let hr = CoCreateInstance(&CLSID_FILE_OPEN_DIALOG, std::ptr::null_mut(), 0x1 /* CLSCTX_INPROC_SERVER */, &IID_IFILE_OPEN_DIALOG, &mut dialog);
    if hr < 0 || dialog.is_null() {
        return None;
    }
    let release: extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(method(dialog, VT_RELEASE));

    let get_options: extern "system" fn(*mut c_void, *mut u32) -> i32 = std::mem::transmute(method(dialog, VT_GET_OPTIONS));
    let set_options: extern "system" fn(*mut c_void, u32) -> i32 = std::mem::transmute(method(dialog, VT_SET_OPTIONS));
    let mut options = 0u32;
    get_options(dialog, &mut options);
    set_options(dialog, options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM);

    let set_title: extern "system" fn(*mut c_void, *const u16) -> i32 = std::mem::transmute(method(dialog, VT_SET_TITLE));
    let set_ok: extern "system" fn(*mut c_void, *const u16) -> i32 = std::mem::transmute(method(dialog, VT_SET_OK_BUTTON_LABEL));
    let title = wide("Choose where to save the video");
    let ok = wide("Save here");
    set_title(dialog, title.as_ptr());
    set_ok(dialog, ok.as_ptr());

    // Videos, until Windows has a last-used folder of its own to start in
    if let Some(home) = std::env::var_os("USERPROFILE") {
        let videos = std::path::Path::new(&home).join("Videos");
        if videos.is_dir() {
            let path = wide(&videos.to_string_lossy());
            let mut item: *mut c_void = std::ptr::null_mut();
            if SHCreateItemFromParsingName(path.as_ptr(), std::ptr::null_mut(), &IID_ISHELL_ITEM, &mut item) >= 0 && !item.is_null() {
                let set_default: extern "system" fn(*mut c_void, *mut c_void) -> i32 =
                    std::mem::transmute(method(dialog, VT_SET_DEFAULT_FOLDER));
                set_default(dialog, item);
                let item_release: extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(method(item, VT_RELEASE));
                item_release(item);
            }
        }
    }

    let show: extern "system" fn(*mut c_void, isize) -> i32 = std::mem::transmute(method(dialog, VT_SHOW));
    let mut picked = None;
    if show(dialog, owner) >= 0 {
        let get_result: extern "system" fn(*mut c_void, *mut *mut c_void) -> i32 = std::mem::transmute(method(dialog, VT_GET_RESULT));
        let mut item: *mut c_void = std::ptr::null_mut();
        if get_result(dialog, &mut item) >= 0 && !item.is_null() {
            let display_name: extern "system" fn(*mut c_void, u32, *mut *mut u16) -> i32 =
                std::mem::transmute(method(item, VT_GET_DISPLAY_NAME));
            let mut name: *mut u16 = std::ptr::null_mut();
            if display_name(item, SIGDN_FILESYSPATH, &mut name) >= 0 && !name.is_null() {
                let mut n = 0;
                while n < 32768 && *name.add(n) != 0 {
                    n += 1;
                }
                picked = Some(String::from_utf16_lossy(std::slice::from_raw_parts(name, n)));
                CoTaskMemFree(name as *mut c_void);
            }
            let item_release: extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(method(item, VT_RELEASE));
            item_release(item);
        }
    }
    release(dialog);
    picked
}

// ── Process group (saving a video) ────────────────────────────────────────────
// yt-dlp.exe starts a second copy of itself and then ffmpeg, so ending yt-dlp
// alone would leave those running. A Windows "job" holds all of them: ending the
// job ends every process in it, and Windows ends it too when the player closes.
#[repr(C)]
struct JobLimits {
    // JOBOBJECT_BASIC_LIMIT_INFORMATION
    per_process_user_time: i64,
    per_job_user_time: i64,
    limit_flags: u32,
    min_working_set: usize,
    max_working_set: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
    // IO_COUNTERS
    io: [u64; 6],
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory: usize,
    peak_job_memory: usize,
}
const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> isize;
    fn SetInformationJobObject(job: isize, class: i32, info: *const c_void, len: u32) -> i32;
    fn AssignProcessToJobObject(job: isize, process: isize) -> i32;
    fn TerminateJobObject(job: isize, exit_code: u32) -> i32;
    fn CloseHandle(handle: isize) -> i32;
}

/// Processes that end together (see above)
pub struct ProcessGroup(isize);

const JOB_OBJECT_LIMIT_BREAKAWAY_OK: u32 = 0x0800;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> isize;
}

/// Put the player itself in a job that Windows ends along with the player:
/// everything it starts (yt-dlp, the Deno yt-dlp runs, ffmpeg, mpv's own yt-dlp
/// runs) is in it too, so nothing is left running once the player has closed,
/// even if it crashes. (Explorer, opened by "Show in folder", is started outside
/// it.) The job's handle stays open for as long as the player runs.
pub fn end_children_with_player() {
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job == 0 {
            return;
        }
        let mut limits: JobLimits = std::mem::zeroed();
        limits.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        let set = SetInformationJobObject(
            job,
            JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
            &limits as *const JobLimits as *const c_void,
            std::mem::size_of::<JobLimits>() as u32,
        );
        if set == 0 || AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
            CloseHandle(job);
        }
        // (otherwise the handle is kept: closing it would end the player)
    }
}

impl ProcessGroup {
    /// A group holding `process` (its handle) and everything it starts from now on
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn new(process: isize) -> Option<Self> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job == 0 {
                return None;
            }
            let mut limits: JobLimits = std::mem::zeroed();
            limits.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                &limits as *const JobLimits as *const c_void,
                std::mem::size_of::<JobLimits>() as u32,
            );
            if AssignProcessToJobObject(job, process) == 0 {
                CloseHandle(job);
                return None;
            }
            Some(Self(job))
        }
    }

    /// End every process in the group now
    pub fn end(&self) {
        unsafe {
            TerminateJobObject(self.0, 1);
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
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
    if msg == WM_ENTERSIZEMOVE {
        drag_begin(hwnd);
    } else if msg == WM_EXITSIZEMOVE {
        DRAG.with(|d| *d.borrow_mut() = None);
    } else if (msg == WM_MOVING || msg == WM_SIZING) && lparam != 0 {
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
            if let Ok(mut list) = FORWARDED.lock() {
                list.push(path);
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

// ── Explorer's order for files opened / dropped together ──────────────────────
// Explorer hands over several files in no useful order (dropped: its own
// internal order; "Open": one launch per file, a moment apart). The order the
// user sees is the open Explorer window's (sorted by date, grouped, …), so ask
// the window whose selection holds those files: its view lists the selected
// items in the order it shows them.

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoUninitialize();
}

/// A VARIANT holding a 32-bit number (VT_I4); 24 bytes on 64-bit Windows
#[repr(C)]
struct VariantI4 {
    vt: u16,
    reserved: [u16; 3],
    value: i64,
    rest: i64,
}

/// {9BA05972-F6A8-11CF-A442-00A0C90A8F39}
const CLSID_SHELL_WINDOWS: Guid = Guid { data1: 0x9BA0_5972, data2: 0xF6A8, data3: 0x11CF, data4: [0xA4, 0x42, 0x00, 0xA0, 0xC9, 0x0A, 0x8F, 0x39] };
/// {85CB6900-4D95-11CF-960C-0080C7F4EE85}
const IID_ISHELL_WINDOWS: Guid = Guid { data1: 0x85CB_6900, data2: 0x4D95, data3: 0x11CF, data4: [0x96, 0x0C, 0x00, 0x80, 0xC7, 0xF4, 0xEE, 0x85] };
/// {6D5140C1-7436-11CE-8034-00AA006009FA}
const IID_ISERVICE_PROVIDER: Guid = Guid { data1: 0x6D51_40C1, data2: 0x7436, data3: 0x11CE, data4: [0x80, 0x34, 0x00, 0xAA, 0x00, 0x60, 0x09, 0xFA] };
/// SID_STopLevelBrowser {4C96BE40-915C-11CF-99D3-00AA004AE837}
const SID_TOP_LEVEL_BROWSER: Guid = Guid { data1: 0x4C96_BE40, data2: 0x915C, data3: 0x11CF, data4: [0x99, 0xD3, 0x00, 0xAA, 0x00, 0x4A, 0xE8, 0x37] };
/// {000214E2-0000-0000-C000-000000000046}
const IID_ISHELL_BROWSER: Guid = Guid { data1: 0x0002_14E2, data2: 0x0000, data3: 0x0000, data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46] };
/// {CDE725B0-CCC9-4519-917E-325D72FAB4CE}
const IID_IFOLDER_VIEW: Guid = Guid { data1: 0xCDE7_25B0, data2: 0xCCC9, data3: 0x4519, data4: [0x91, 0x7E, 0x32, 0x5D, 0x72, 0xFA, 0xB4, 0xCE] };
/// {B63EA76D-1F85-456F-A19C-48159EFA858B}
const IID_ISHELL_ITEM_ARRAY: Guid = Guid { data1: 0xB63E_A76D, data2: 0x1F85, data3: 0x456F, data4: [0xA1, 0x9C, 0x48, 0x15, 0x9E, 0xFA, 0x85, 0x8B] };

// vtable slots: IShellWindows (IDispatch 3-6) get_Count 7, Item 8;
// IServiceProvider QueryService 3; IShellBrowser (IOleWindow 3-4)
// QueryActiveShellView 15; IFolderView Items 8; IShellItemArray GetCount 7,
// GetItemAt 8
const SVGIO_SELECTION: u32 = 0x1;
const SVGIO_ALLVIEW: u32 = 0x2;
const SVGIO_FLAG_VIEWORDER: u32 = 0x8000_0000;
/// An item's full path as Explorer parses it: for a file inside a zip,
/// "C:\…\Videos.zip\Season 1\Ep 2.mkv" (it has no file-system path)
const SIGDN_DESKTOPABSOLUTEPARSING: u32 = 0x8002_8000;

/// A COM object that's released when dropped
struct Com(*mut c_void);

impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            let release: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(method(self.0, 2));
            release(self.0);
        }
    }
}

impl Com {
    fn new(p: *mut c_void) -> Option<Com> {
        (!p.is_null()).then_some(Com(p))
    }

    /// QueryInterface
    unsafe fn query(&self, iid: &Guid) -> Option<Com> {
        let qi: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 = std::mem::transmute(method(self.0, 0));
        let mut out = std::ptr::null_mut();
        if qi(self.0, iid, &mut out) < 0 {
            return None;
        }
        Com::new(out)
    }
}

/// Sort `files` (several dropped / opened together) the way the Explorer window
/// they were selected in shows them. Files that window doesn't list keep their
/// order, after the rest. Leaves them alone if no open Explorer window has them
/// all selected (e.g. dragged from the desktop).
pub fn sort_like_explorer(files: &mut [std::path::PathBuf]) {
    let wanted: Vec<String> = files.iter().map(|f| f.to_string_lossy().into_owned()).collect();
    let Some(order) = explorer_selection_with(wanted) else { return };
    let order: Vec<String> = order.iter().map(|o| o.to_lowercase()).collect();
    files.sort_by_key(|f| {
        let f = f.to_string_lossy().to_lowercase();
        order.iter().position(|o| *o == f).unwrap_or(usize::MAX)
    });
}

/// Everything selected, in the order it shows them, in the open Explorer window
/// whose selection includes all of `files` (None if there's no such window).
/// Asked on another thread, with a time limit: a busy Explorer mustn't hold up
/// the player.
pub fn explorer_selection_with(files: Vec<String>) -> Option<Vec<String>> {
    let wanted: Vec<String> = files.iter().map(|f| f.to_lowercase()).collect();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let order = unsafe {
            CoInitializeEx(std::ptr::null_mut(), 0x0 /* COINIT_MULTITHREADED */);
            let order = explorer_selection(&wanted);
            CoUninitialize();
            order
        };
        let _ = tx.send(order);
    });
    rx.recv_timeout(Duration::from_millis(700)).ok().flatten()
}

/// The selection, in view order, of the first Explorer window whose selection
/// includes every one of `wanted` (given in lower case)
unsafe fn explorer_selection(wanted: &[String]) -> Option<Vec<String>> {
    let mut out = std::ptr::null_mut();
    if CoCreateInstance(&CLSID_SHELL_WINDOWS, std::ptr::null_mut(), 0x17 /* CLSCTX_ALL */, &IID_ISHELL_WINDOWS, &mut out) < 0 {
        return None;
    }
    let windows = Com::new(out)?;
    let get_count: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32 = std::mem::transmute(method(windows.0, 7));
    let item: unsafe extern "system" fn(*mut c_void, VariantI4, *mut *mut c_void) -> i32 = std::mem::transmute(method(windows.0, 8));
    let mut count = 0;
    if get_count(windows.0, &mut count) < 0 {
        return None;
    }
    for i in 0..count.min(64) {
        let mut out = std::ptr::null_mut();
        let index = VariantI4 { vt: 3 /* VT_I4 */, reserved: [0; 3], value: i as i64, rest: 0 };
        if item(windows.0, index, &mut out) < 0 {
            continue;
        }
        let Some(window) = Com::new(out) else { continue };
        if let Some(selection) = window_items(&window, SVGIO_SELECTION, SIGDN_FILESYSPATH) {
            let lower: Vec<String> = selection.iter().map(|s| s.to_lowercase()).collect();
            if wanted.iter().all(|w| lower.contains(w)) {
                return Some(selection);
            }
        }
    }
    None
}

/// A file Explorer copied out of a zip it shows: the file as that window has
/// it selected, and everything in that folder of the zip, in the order the
/// window shows them, as full paths such as "C:\…\Videos.zip\Season 1\Ep 1.mkv".
/// `name`: the file's name ("Ep 2.mkv"; the window's selected file of that
/// name inside a zip). Err: what was found instead.
pub fn explorer_zip_folder(name: String) -> Result<(String, Vec<String>), String> {
    let (inside, tail) = (".zip\\".to_owned(), format!("\\{}", name.to_lowercase()));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let items = unsafe {
            CoInitializeEx(std::ptr::null_mut(), 0x0 /* COINIT_MULTITHREADED */);
            let items = explorer_zip(&inside, &tail);
            CoUninitialize();
            items
        };
        let _ = tx.send(items);
    });
    rx.recv_timeout(Duration::from_millis(1500)).unwrap_or_else(|_| Err("Explorer didn't answer in time".to_owned()))
}

unsafe fn explorer_zip(inside: &str, tail: &str) -> Result<(String, Vec<String>), String> {
    let mut out = std::ptr::null_mut();
    if CoCreateInstance(&CLSID_SHELL_WINDOWS, std::ptr::null_mut(), 0x17 /* CLSCTX_ALL */, &IID_ISHELL_WINDOWS, &mut out) < 0 {
        return Err("couldn't ask Explorer for its windows".to_owned());
    }
    let windows = Com::new(out).ok_or("no Explorer window list")?;
    let get_count: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32 = std::mem::transmute(method(windows.0, 7));
    let item: unsafe extern "system" fn(*mut c_void, VariantI4, *mut *mut c_void) -> i32 = std::mem::transmute(method(windows.0, 8));
    let mut count = 0;
    if get_count(windows.0, &mut count) < 0 {
        return Err("couldn't count Explorer's windows".to_owned());
    }
    // (what was seen, should it not be found)
    let mut seen = Vec::new();
    for i in 0..count.min(64) {
        let mut out = std::ptr::null_mut();
        let index = VariantI4 { vt: 3 /* VT_I4 */, reserved: [0; 3], value: i as i64, rest: 0 };
        if item(windows.0, index, &mut out) < 0 {
            continue;
        }
        let Some(window) = Com::new(out) else { continue };
        let Some(selection) = window_items(&window, SVGIO_SELECTION, SIGDN_DESKTOPABSOLUTEPARSING) else {
            seen.push("(a window with nothing selected)".to_owned());
            continue;
        };
        let picked = selection.iter().find(|s| {
            let s = s.to_lowercase();
            s.contains(inside) && s.ends_with(tail)
        });
        if let Some(picked) = picked {
            let all = window_items(&window, SVGIO_ALLVIEW, SIGDN_DESKTOPABSOLUTEPARSING).ok_or("couldn't list that window's files")?;
            return Ok((picked.clone(), all));
        }
        seen.push(selection.first().map_or_else(|| "(a selection without paths)".to_owned(), |s| crate::files::path_shape(s)));
    }
    Err(format!("{count} Explorer windows, none with it selected; they had: {}", seen.join(" | ")))
}

/// One Explorer window's items (`which`: the selected ones, or all), in the
/// order it shows them, as `name_kind` (a file-system path, or Explorer's
/// own full path)
unsafe fn window_items(window: &Com, which: u32, name_kind: u32) -> Option<Vec<String>> {
    let services = window.query(&IID_ISERVICE_PROVIDER)?;
    let query_service: unsafe extern "system" fn(*mut c_void, *const Guid, *const Guid, *mut *mut c_void) -> i32 =
        std::mem::transmute(method(services.0, 3));
    let mut out = std::ptr::null_mut();
    if query_service(services.0, &SID_TOP_LEVEL_BROWSER, &IID_ISHELL_BROWSER, &mut out) < 0 {
        return None;
    }
    let browser = Com::new(out)?;
    let active_view: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32 = std::mem::transmute(method(browser.0, 15));
    let mut out = std::ptr::null_mut();
    if active_view(browser.0, &mut out) < 0 {
        return None;
    }
    let view = Com::new(out)?;
    let folder_view = view.query(&IID_IFOLDER_VIEW)?;
    let items: unsafe extern "system" fn(*mut c_void, u32, *const Guid, *mut *mut c_void) -> i32 = std::mem::transmute(method(folder_view.0, 8));
    let mut out = std::ptr::null_mut();
    if items(folder_view.0, which | SVGIO_FLAG_VIEWORDER, &IID_ISHELL_ITEM_ARRAY, &mut out) < 0 {
        return None; // (nothing selected)
    }
    let array = Com::new(out)?;
    let get_count: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32 = std::mem::transmute(method(array.0, 7));
    let get_item: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32 = std::mem::transmute(method(array.0, 8));
    let mut count = 0;
    if get_count(array.0, &mut count) < 0 {
        return None;
    }
    let mut paths = Vec::new();
    for i in 0..count.min(5000) {
        let mut out = std::ptr::null_mut();
        if get_item(array.0, i, &mut out) < 0 {
            continue;
        }
        let Some(item) = Com::new(out) else { continue };
        let display_name: unsafe extern "system" fn(*mut c_void, u32, *mut *mut u16) -> i32 = std::mem::transmute(method(item.0, VT_GET_DISPLAY_NAME));
        let mut name: *mut u16 = std::ptr::null_mut();
        if display_name(item.0, name_kind, &mut name) >= 0 && !name.is_null() {
            let mut n = 0;
            while n < 32768 && *name.add(n) != 0 {
                n += 1;
            }
            paths.push(String::from_utf16_lossy(std::slice::from_raw_parts(name, n)));
            CoTaskMemFree(name as *mut c_void);
        }
    }
    Some(paths)
}

// ── Desktop wallpaper ─────────────────────────────────────────────────────────
#[link(name = "user32")]
unsafe extern "system" {
    fn SystemParametersInfoW(action: u32, param: u32, pv: *mut c_void, flags: u32) -> i32;
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegSetKeyValueW(key: isize, sub_key: *const u16, name: *const u16, kind: u32, data: *const c_void, len: u32) -> i32;
}

/// Make the picture at `path` (a file Windows can read: JPG, PNG, BMP, TIFF,
/// GIF) the desktop wallpaper, filling the screen. Windows normally saves a JPG
/// it's given again at about 85 % quality; asked for 100 here first.
pub fn set_wallpaper(path: &str) -> bool {
    const HKCU: isize = 0x8000_0001_u32 as i32 as isize;
    const REG_SZ: u32 = 1;
    const REG_DWORD: u32 = 4;
    unsafe {
        let key = wide("Control Panel\\Desktop");
        let quality: u32 = 100;
        RegSetKeyValueW(HKCU, key.as_ptr(), wide("JPEGImportQuality").as_ptr(), REG_DWORD, &quality as *const u32 as *const c_void, 4);
        // Fill: the screen covered, the picture's shape kept (cropped to fit)
        for (name, value) in [("WallpaperStyle", "10"), ("TileWallpaper", "0")] {
            let v = wide(value);
            RegSetKeyValueW(HKCU, key.as_ptr(), wide(name).as_ptr(), REG_SZ, v.as_ptr() as *const c_void, (v.len() * 2) as u32);
        }
        let mut file = wide(path);
        // SPI_SETDESKWALLPAPER; SPIF_UPDATEINIFILE | SPIF_SENDCHANGE (kept after a restart)
        SystemParametersInfoW(0x0014, 0, file.as_mut_ptr() as *mut c_void, 0x01 | 0x02) != 0
    }
}
