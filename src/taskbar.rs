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
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU8, Ordering};
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
const WM_SETCURSOR: u32 = 0x0020;
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
const VT_SET_OVERLAY_ICON: usize = 18;
const VT_MARK_FULLSCREEN: usize = 8;

/// The taskbar's ITaskbarList3, once made (see Taskbar::new), for full screen (see mark_fullscreen)
static TASKBAR_LIST: AtomicIsize = AtomicIsize::new(0);

/// Tell the taskbar the window is (or no longer is) full screen, so it steps
/// behind it at once. (Left to notice by itself, it stayed in front for a
/// frame after the window had covered the screen.)
unsafe fn mark_fullscreen(hwnd: isize, on: bool) {
    let list = TASKBAR_LIST.load(Ordering::Relaxed) as *mut c_void;
    if !list.is_null() {
        let mark: extern "system" fn(*mut c_void, isize, i32) -> i32 = std::mem::transmute(method(list, VT_MARK_FULLSCREEN));
        mark(list, hwnd, on as i32);
    }
}

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
/// Its placement then (maximized, and the size it restores to), to go back to
static FS_PLACEMENT: Mutex<Option<WindowPlacement>> = Mutex::new(None);
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

// ── Starting maximized ────────────────────────────────────────────────────────
#[link(name = "user32")]
unsafe extern "system" {
    fn GetWindowPlacement(hwnd: isize, wp: *mut WindowPlacement) -> i32;
    fn SetWindowPlacement(hwnd: isize, wp: *const WindowPlacement) -> i32;
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WindowPlacement {
    length: u32,
    flags: u32,
    show_cmd: u32,
    min_pos: Point,
    max_pos: Point,
    normal: RectI,
}

/// Maximized last time. The window is still hidden (it's shown once its first
/// frame is drawn), and a hidden window can't be maximized without showing it
/// undrawn (a white flash). So: size it to the screen's work area now, as it
/// will be maximized, and its first frame is drawn at that size. Returns where
/// it was, for `finish_maximized`.
pub fn prepare_maximized(hwnd: isize) -> Option<WindowPlacement> {
    unsafe {
        let mut wp: WindowPlacement = std::mem::zeroed();
        wp.length = std::mem::size_of::<WindowPlacement>() as u32;
        if GetWindowPlacement(hwnd, &mut wp) == 0 {
            return None;
        }
        let mut mi: MonitorInfo = std::mem::zeroed();
        mi.size = std::mem::size_of::<MonitorInfo>() as u32;
        if GetMonitorInfoW(MonitorFromWindow(hwnd, 2 /* MONITOR_DEFAULTTONEAREST */), &mut mi) == 0 {
            return None;
        }
        let w = mi.work;
        // SWP_NOZORDER | SWP_NOACTIVATE
        SetWindowPos(hwnd, 0, w.left, w.top, w.right - w.left, w.bottom - w.top, 0x0004 | 0x0010);
        Some(wp)
    }
}

/// Once shown: maximize it (nothing moves: it's that size already), and put
/// back the size it had before, to restore to
pub fn finish_maximized(hwnd: isize, before: WindowPlacement) {
    unsafe {
        ShowWindow(hwnd, 3); // SW_MAXIMIZE
        let mut wp = before;
        wp.flags = 0;
        wp.show_cmd = 3; // SW_SHOWMAXIMIZED
        SetWindowPlacement(hwnd, &wp);
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
    let was_max = IsZoomed(hwnd) != 0;
    let mut window = RectI::default();
    let mut info = MonitorInfo {
        size: std::mem::size_of::<MonitorInfo>() as u32,
        monitor: RectI::default(),
        work: RectI::default(),
        flags: 0,
    };
    let monitor = MonitorFromWindow(hwnd, 2); // MONITOR_DEFAULTTONEAREST
    if monitor != 0 && GetMonitorInfoW(monitor, &mut info) != 0 {
        // A maximized window can't be sized past its maximized size: un-maximize
        // it first, but straight to covering the screen. (Restoring it showed it
        // at its normal size for a frame, the desktop around it, then full screen.)
        if was_max {
            let mut wp: WindowPlacement = std::mem::zeroed();
            wp.length = std::mem::size_of::<WindowPlacement>() as u32;
            if GetWindowPlacement(hwnd, &mut wp) != 0 {
                if let Ok(mut p) = FS_PLACEMENT.lock() {
                    *p = Some(wp);
                }
                let (m, w) = (info.monitor, info.work);
                let mut full = wp;
                full.flags = 0;
                full.show_cmd = 1; // SW_SHOWNORMAL
                // (its normal size is counted from the work area's corner)
                let (dx, dy) = (w.left - m.left, w.top - m.top);
                full.normal = RectI { left: m.left - dx, top: m.top - dy, right: m.right - dx, bottom: m.bottom + 1 - dy };
                SetWindowPlacement(hwnd, &full);
            } else {
                ShowWindow(hwnd, 9); // SW_RESTORE
            }
        }
    }
    if monitor != 0 && GetWindowRect(hwnd, &mut window) != 0 && info.size != 0 && info.monitor.right > info.monitor.left {
        if let Ok(mut r) = FS_RESTORE.lock() {
            *r = Some([window.left, window.top, window.right, window.bottom]);
        }
        FS_WAS_MAX.store(was_max, Ordering::Relaxed);
        let m = info.monitor;
        mark_fullscreen(hwnd, true);
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
    unsafe { mark_fullscreen(hwnd, false) };
    if let Ok(mut r) = FS_RESTORE.lock() {
        *r = None;
    }
    if let Ok(mut p) = FS_PLACEMENT.lock() {
        *p = None;
    }
    FS_WAS_MAX.store(false, Ordering::Relaxed);
    if ROUNDED.load(Ordering::Relaxed) {
        set_corners(hwnd, true);
    }
}

unsafe fn exit_fullscreen_now(hwnd: isize) {
    mark_fullscreen(hwnd, false);
    if ROUNDED.load(Ordering::Relaxed) {
        set_corners(hwnd, true);
    }
    let saved = FS_RESTORE.lock().ok().and_then(|mut r| r.take());
    let placement = FS_PLACEMENT.lock().ok().and_then(|mut p| p.take());
    if FS_WAS_MAX.swap(false, Ordering::Relaxed) {
        // Back to maximized straight from full screen, keeping the size it
        // restores to. (Putting it at that size first, then maximizing it,
        // showed it small for a frame, the desktop around it.)
        if let Some(mut wp) = placement {
            wp.flags = 0;
            wp.show_cmd = 3; // SW_SHOWMAXIMIZED
            SetWindowPlacement(hwnd, &wp);
        } else {
            ShowWindow(hwnd, 3); // SW_MAXIMIZE
        }
    } else if let Some([l, t, r, b]) = saved {
        SetWindowPos(hwnd, 0, l, t, r - l, b - t, 0x0200);
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
        // (the dialog's thread set up COM for itself, see run_*_dialog: undone as it ends)
        unsafe { CoUninitialize() };
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
        // (the dialog's thread set up COM for itself, see run_*_dialog: undone as it ends)
        unsafe { CoUninitialize() };
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
    /// The badge on the taskbar button: 0 none, 1 picture, 2 music, 3 playing,
    /// 4 paused (see set_badge), and its icons once made
    badge: Option<u8>,
    badge_icons: [isize; 4],
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
            TASKBAR_LIST.store(list as isize, Ordering::Relaxed);
            Some(Self {
                list,
                hwnd,
                icons,
                buttons_added: false,
                last_try: Instant::now() - Duration::from_secs(5),
                last_status: None,
                last_progress: (-1, 0),
                badge: None,
                badge_icons: [0; 4],
            })
        }
    }

    /// The badge in the taskbar button's corner, over the player's icon: 1 a
    /// picture, 2 music, 3 playing, 4 paused (a video), 0 none. (With the taskbar's buttons combined,
    /// Windows shows the program's own icon there whatever the window's is.)
    /// Only talks to Windows when it changes; kept trying until the button exists.
    pub fn set_badge(&mut self, which: u8) {
        if self.badge == Some(which) || !self.buttons_added {
            return;
        }
        let icon = match which {
            1..=4 => {
                let i = which as usize - 1;
                if self.badge_icons[i] == 0 {
                    self.badge_icons[i] = badge::icon(which).unwrap_or(0);
                }
                self.badge_icons[i]
            }
            _ => 0,
        };
        let text = wide(match which {
            1 => "Picture",
            2 => "Music",
            3 => "Playing",
            4 => "Paused",
            _ => "",
        });
        unsafe {
            let set: extern "system" fn(*mut c_void, isize, isize, *const u16) -> i32 =
                std::mem::transmute(method(self.list, VT_SET_OVERLAY_ICON));
            if set(self.list, self.hwnd, icon, text.as_ptr()) >= 0 {
                self.badge = Some(which);
            }
        }
    }

    /// Bring the taskbar in line with the player. Cheap: only talks to Windows when
    /// something actually changed.
    pub fn sync(&mut self, st: Status, pos_secs: f64, dur_secs: f64) {
        if BUTTONS_LOST.swap(false, Ordering::Relaxed) {
            self.buttons_added = false;
            self.last_status = None;
            // (a new taskbar button: the badge too, again)
            self.badge = None;
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
    if msg == WM_SETCURSOR && (lparam & 0xFFFF) == 1 /* HTCLIENT */ {
        // A hand over a picture (see set_hand), in place of the cursor egui asks for
        let which = HAND.load(Ordering::Relaxed);
        if which != 0 {
            if let Some(c) = hand_cursor(which) {
                SetCursor(c);
                return 1;
            }
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

// ── Hand cursors (a picture: open hand over it, closed while dragging it) ─────
// Windows has no grabbing hand of its own, so these are drawn here: an open
// and a closed hand in the classic small style, white with a dark outline,
// for 32, 48 and 64 px cursors (normal, larger, larger still), as PNGs written
// out as text
mod hand {
    pub(super) const OPEN_32: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAABmJLR0QA/wD/AP+gvaeTAAACJ0lEQVRYhe2UwYsSURzHv87q0g5J",
        "mFIJWoSh4EEIiT2ODISQFCgGQniTWBAvQYc8+C94imi9GV0XvOxhD+IpOwiFbgjCHmrCi+sIOVLr6nw7tNYedmt2HeuQn9vjvfl+",
        "P7x5/IAlS5acnUs+n68aiUQ+A7gH4DqAO3+jeBXAVQBPO52OTpKxWGyYzWYnpVJp6nK5Xi6y/GY0Gv1SKBRot9sbo9GIJJnJZPTx",
        "eEySTKVSGoAYgEcAbEaDrQbPPSyVShe9Xi+63e7t4xs2248uq9W6Wi6XKx6Px5JMJiVVVR+bKbAmiiIAQBRF4cQgq1VIp9MrABAK",
        "he7WajVDwSeGnQeLxfIrVBAM55omcF7+ucDv3sC1o1f9dpECp93ABVmWP7Rarc2NjY33AG6Q/PmTSQIAdF0/8WOSK/OK3SoWiyTJ",
        "8XjMcDj8dTAYkCTr9Tr39vY4HA65vb3Nfr/PyWTCnZ0d6rpOkkwkEgfzCiAYDO5qmkaSbDabuqqqNIokSftzCwAI5HK5ySx0Nv2M",
        "IMvyRzMEIAjCk2q1OjXcbLYAAMHv979TFIWzmW+E9fX1XbMEAMAB4Hk+n9eNlB8eHtLhcLw2UwAA4HQ6XymK8keJra2tKYAHpgsA",
        "uCxJ0n6v1zu1vN1u6z6f781ZJqzFwJnjXHG73ZvxePx+IBAQRFGckrRomiY0Go2DSqXyYjQaPQPwbVECM9YAeAHYj9YqgE8ApufM",
        "W7LkP+Y7saXu9cOPIjcAAAAASUVORK5CYII=",
    );
    pub(super) const OPEN_48: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAADAAAAAwCAYAAABXAvmHAAAABmJLR0QA/wD/AP+gvaeTAAADC0lEQVRoge2XT0gUURzHP+vEzqxL",
        "LYKTSgqGYrQKQgaJxMYSEupF3Q4puYx5qA6BIBR0CayTCCF4kC7SIcJIIkmoi9ZhDf8cvKgXb4uyC7KJllju7uuiw7ZkjL7dtmA+",
        "p/cb3vx+3+97b957AzY2NjY2Nv8xShZy1gONwBbwJQv5s8rD3t5eMT4+LhobG38APqAGeAjczNKAZQzF5/N9F/tsbW0JTdM+NzU1",
        "xUOhUHJkZES4XK7XuRaZTouu6wu6ri8AN7q6ukQqpaWlm5FIxIyDwaAATmdSQJ7Eu+daW1vfhsPhunA4XNfW1vYivYMQQikqKjLj",
        "kpISgCaPx/OpoKAgBFyTqA+SBq50d3crqqqiqio9PT2Wcvn9/mdra2u+9fX1hpaWlndAhYQGKQOay+USB0Fq+090dnY63W43mqZh",
        "GMYJ4LKEBikDx0JV1fS2JpPvrxvINLaBXGMbyDVWDZzUNO2V3+//WlhYOA+cz7Iuy1gyoCjK0+np6etTU1Pu5eXli1VVVR8Bd3q/",
        "RCKRHv+SX4jfHhVSq8DSyxUVFVfr6+sdALquMzo6qufl5d3yeDyOgz7FxcWO2tpa9vb2AIhGoxiGkb+xsWHmqa6uxuv1mvH+NeOC",
        "jAFLKIryfGVlJZl6Uevr6xOzs7NChsXFRQE8yboB4ExDQ8O3eDxuFt/d3RWhUEjKwMTEhABuywizuv7WZmZm7g0ODpoPVFWlpqZG",
        "pvbfJz8///3S0lLSwuD+czMAwM7Ozi3DMHbi8bhMTZP9XcnSLfYwjrqFrc/Pz9/u7++XqWmyvb0NsJ2RZEfkUXNz897w8LDUEhoY",
        "GBCAPxcGAFxOp/PN6urqsb+J9vb2JHAqVwYAvIFAIHEc8dFoVLjd7g+5FH/A47GxsSPPQkdHRwK4lGvxAKrT6Xw5NDQkUg+6w4jF",
        "YiIYDCaBu5ko7rDQx2qeO+Xl5fcDgUB5ZWUlZWVl5v/v5uYmkUiEubk5JicnZ2Ox2APgU6YKZxIH4AXOAsUp2/R3YANYAKIZrmlj",
        "Y2NjY2OTM34CslXSLM7s6eoAAAAASUVORK5CYII=",
    );
    pub(super) const OPEN_64: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAYAAACqaXHeAAAABmJLR0QA/wD/AP+gvaeTAAAEKElEQVR4nO2ZT0gjVxzHv3Fmk1r/",
        "prUxrFEo7iEGC/WStdBDSqEgxYMJqUhrSUW81KUV8dBD2VZqc/FgQRe1tRexNxvRBqwKsj1UpGBVXAMJuLhCuybpRm3BOAn59TLK",
        "jlnLJnkzI/R9YA7ze8l739933rz35j2Aw+FwOBwOh8Ph/B8RNGzLAOAtAO8AKALwh4Zt684NQRB+7u7uprGxMXK73QTg3lPllQBe",
        "A2DUUaOqfOr3++lpurq6CMC7AEacTmems7OT6urqTgB49BarBj/u7e0pDFhaWiIAS319fZTJZIiI6OTkhJqamiQAt/QWXAhOQRBW",
        "bTZbXBCEVQC3ASzHYjGFAWtrawTg0eHhoSI+NTVFAO5oJbaIcX237Hb7/XA47Do4OHg5Eom4HA7HfQAvXvH7YovFogjI9xb5+gTA",
        "XQBvMNapGl8FAgHFE52fnycAD6/oAcogES0sLBCAMZvN9mRiYoICgQC1tLRkZDOuPT/s7u4qEgqHwwTgcY4G7K6vr1/EUqkUNTY2",
        "SgAqWAtm/QoYDAbD5QDkNcBzU1FRcdPpdF7ci6IIl8t1Q54qmcLaACYYjcasBZrJZAIAE+u2rqUBWsIN0FuA3nAD9BagN9wAvQXo",
        "jZjn/8wA3pNXZqsAfmOsSzPyMaCxpqbml4GBAbPFYsHMzAyCweCXAL5QQZ/q5GyAKIpjy8vL5oaGBgBAe3s7PB7P3bm5ud//Y8lr",
        "SKVSioB8f9UrqNmrmXNDDofjzfPkAaCoqAiTk5OwWq3fAXjdaFTuaplMJni93lf29/cV8Xg8Dq/X+1IsFlPERVFEa2tr1udzVVUV",
        "ALyaq17mWCyWw2QyefkjjmZnZwnAk3g8nlXGAnmj5APW+eTcA6LR6DcjIyNZcbfbjY6ODnM8HmelTUEoFAKAP1WpPEfE4uLije3t",
        "7aynlEgkKBKJqNID+vv7CcDbrJPJZ7BJn56efuTz+dKXB7bKykpYrVZm4rQg39F2a2NjY9Dv92cVlJaWFixKSwqZbvxDQ0Mbm5ub",
        "DOVoTyEGpCVJ+tDn86UkSWIo6dkQEQAQ63oLXXA82Nra+nxwcJCRnKs5Pj4GgL9VbygPBFEUV0ZHR+ns7IwkSVJlFvB4PHSdT4yK",
        "AXxdUlLysLm5OXN+1MUSeVuc+aaoGnwfDAaZJn90dESCIKzpndjzUu90OtMse8H4+DgB+EzvxHLh3vT0NJPkk8kk2e32JIAqvZPK",
        "hUqz2fxoZ2enYAN6e3s1PS1mSUN1dXV0ZWUl7yff09NDAL7N9WjtOnHTYDD81NbWRouLi/Ssz+jLhEIhGh4eptra2iMAH6stUCtn",
        "bwN432g0uurr6xvKy8vFsrKyi8JkMolEIpGORqN/xWKxXwEsA5gG8I/awvTqWi/Ia4dzUloky+FwOBwOh8PhcDgcmX8B1MnN4tMw",
        "8OQAAAAASUVORK5CYII=",
    );
    pub(super) const CLOSED_32: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAABmJLR0QA/wD/AP+gvaeTAAAB1ElEQVRYhe2Tv6uSURiAH4/iIh8J",
        "htonKXxIBA45hSDm6NLSICXSn6BUtNnk4KBDfPoHXLAhkLsktQoN4iK0OghOSUqgBjnc78dpMbjL9Z5Lxl2+Zzrwvrzv8573HPDw",
        "8PDwuGX8N8i9CzwD9tFo9H2lUjnfbDZP9vv9vUKh8GG9Xocty/oKBAD31KKBfD6/mkwmTrVaveh0Oq6UUg4GA6fVarlSSmmapmMY",
        "xrd6ve7G4/GPyoUV81Llcjmay+VwXZfZbAZAJBLBcRwJ+AzDoNlsPqpWq75YLPai0Wi8Bb6fQqAAPBZCAOD3X701TdMk4NM0DSCk",
        "Mtl1Ai97vd5ZNpv12bYNgK7r4m9Q13URDocBSKVSIhgMApBMJgEiKgJH0XX9kzxgWZZUpd/vS+ChSg9xLLhcLs9M03QBAgHV5wLb",
        "7ZZT/oTntVrNtm1b+Qa63a4EHpxKAOBpt9tVFmi32xK4r1L46Aou8WU4HP5UtZ3P5zbwQzVfCSHE6/F47Fw3/Xa7lYlE4vNJmx/w",
        "p9PpyXQ6vVJitVq5pVJpByRVi/puKBEQQrwpFouvMplMPBQK+QAsy2KxWPwajUaD3W73Dlj9L4HLCODO4fwbuPiHWh4eHrfHH2IW",
        "UkVvB8ieAAAAAElFTkSuQmCC",
    );
    pub(super) const CLOSED_48: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAADAAAAAwCAYAAABXAvmHAAAABmJLR0QA/wD/AP+gvaeTAAACjUlEQVRoge3WzUsbQRgG8Gd2s9lU",
        "/Cje3JBtMFA0xYMHEcXm0IuU0KJQyDVXjy0ERP8BK/RWaCRFCLkIBb32QwJaDHgQUigpIqHQHJQealN2003SzNtLDTYeGjMxoTi/",
        "27uzM/s+uzvsApIkSZIkSZIkXVfqFaypALgD4AaA4hWs33Y+AA8A3ALgGRwczMRiMYpGo+RyuZ4BuGua5tHMzIytqmocgKvbDZ93",
        "f3Z2trK2tkbhcLgK4MXGxganPxYWFmh8fNyybZuIiJLJJAfwBMCq1+v9oCjKcwD9XeveMIyPjuMQEVGlUiHTNL8fHh7WAyQSCVpc",
        "XDwrybIsApDb2triREQ7Ozvc7XYnRXpQRCb7/X5T13UAgKZpCAQCvYyx+jhjDJqm1WtN02AYhndubo4BQCgUYmNjY2GRHoQCtELX",
        "dVdD7RZZr5UN9TgYDMaq1eovzrmnYYwRETt/gIga52sN4x3d1I+WlpaIiKhWq9Hm5iadl8vl6PT0tF4XCgXK5/P12rZtymazf82Z",
        "n5+vdTLAy5OTE2qnqampikhDl90Dn7PZ7IV3QgRjzGnnev/SOzQ09CmTyfAmbm5Tpqenf3QyAAD09/T0vEmlUv9tAABQFUVZXV5e",
        "Fg4wOTnZvf+lkZGRr6IBRkdHv3QtAICn+/v7Le+HUqlEHo/ntUgDol/i9Xg83vLk7e1tchznrWAPYlRVXd/d3b30UyiXyzQxMWED",
        "uNnVAAD6fD5ffm9vr+kQlmVRJBLhAB52u/kzfbquv4pGozydTvPj42PinF9o+uDggK+srNDw8HABwL12XJg1cc5l+AFEANzWdd0Y",
        "GBjoA4BisfizXC5/A3AE4D2AdwB4m68tSZIkSZJ07fwGGfUkT1rZeVYAAAAASUVORK5CYII=",
    );
    pub(super) const CLOSED_64: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAYAAACqaXHeAAAABmJLR0QA/wD/AP+gvaeTAAADQklEQVR4nO3ZT0hjVxTH8e97xhg0",
        "qaLVVQazKEnagn+of2hAKNKFWJSWiussirSgIiJCXQhZzK7LFhHGjTAtSrspaEtBkZQwRVoRLNSWtOGtokYzHTeBxvdON44dE9sZ",
        "yU1S8H52OdycnN+Fd5P3ApqmaZqmaZqmaZqmaZqmadrdUVPhz3sD+Bh4H3ABv1zWPcCbwCvACZCv8FzKeYFx4CPg1cvaO/39/fbO",
        "zo7s7e1JNBoV4D4Qbmxs/H1yclJmZ2elra0tDfRUef6SBJqbm61YLCbLy8sSiURs4EOfz/drJpORpxzHkb6+Pruuru6n/f39q3oq",
        "lZKmpqYUYAIfAF8DXwCD1Q72or5KJBJXgfL5vHR3d//V09PjSIG5uTkZGBgoqo+NjQnwYGpqSizLkoODAxkcHHSAd6sd7rn8fv9Z",
        "YaBYLCaRSKSwLAsLCzI6OlpUn5iYkK6uLttx/tmb09NTcbvdP6qe11TdsKGhob6w5vV6b92ns7PTNAzj6nVLSwvt7e2vlT7hdco3",
        "QJWamuIvKNM0jRsXl8CloEc9MAzcA35W0K+iSt2Aez6f77uZmZlwKBRie3ub3d1dKVzk8XjweDxFbzZNE7fbXTyUy3Vj3TTN2hLn",
        "Ve7Lra2tawdYPB4vOtRU6e3tdaod+JpwOJwrW9obBIPBC9UZSjkEzVwup3yg/2IYxv9qAxzLsh6urq4qHOe5is6Xaquvra19tLS0",
        "VJFLIBQK5aod+CYe4PPp6Wm5uLi4kxsAYACLQ0NDztHR0Z3cgKeGOjo67HJtQCAQeKJ6YNU/hb9Np9Nn+Xx5nmdks9ms6p7K7wUy",
        "mczDjY0N1W05OTnh/Pz8D9V9y3Ez9GBlZUV503g8DvC98sbl4HK5fkgmk0qv/5GREQG6q53tRb01PDwszz7QKEUikRDDML6pdqjb",
        "+mR+fr7k8JZlid/vzwKBage6LRP4dHx8XI6Pj28d3LZtWV9fl9bW1jTQX+0wpXjP6/X+Fo1GZW1tTQ4PDyWbzV67PGzblrOzM0km",
        "k7K5uSmLi4sSDAZzwGdASzmHU/6I6V+Yl398vA28DrwMvFSw5gnwGEgBj4Bt4M8KzadpmqZpmqZpmqZp2h3yNywrxzFO4f2nAAAA",
        "AElFTkSuQmCC",
    );
}

#[link(name = "user32")]
unsafe extern "system" {
    fn SetCursor(cursor: isize) -> isize;
    fn GetSystemMetrics(index: i32) -> i32;
    fn LoadCursorW(instance: isize, name: *const u16) -> isize;
}

/// Which hand to show over the window's client area: 0 none (whatever egui
/// asks for), 1 open, 2 closed
static HAND: AtomicU8 = AtomicU8::new(0);
static HAND_CURSORS: std::sync::OnceLock<[isize; 2]> = std::sync::OnceLock::new();

/// Show the open hand (1), the closed hand (2), or the usual cursor again (0)
pub fn set_hand(which: u8) {
    if HAND.swap(which, Ordering::Relaxed) == which {
        return;
    }
    unsafe {
        match hand_cursor(which) {
            Some(c) => {
                SetCursor(c);
            }
            // (back to the arrow straight away, not at the next mouse move;
            // anything else egui wants follows at that move)
            None => {
                SetCursor(LoadCursorW(0, 32512 as *const u16 /* IDC_ARROW */));
            }
        }
    }
}

/// The cursor for hand 1 or 2 (made the first time it's needed)
fn hand_cursor(which: u8) -> Option<isize> {
    if which == 0 {
        return None;
    }
    let cursors = HAND_CURSORS.get_or_init(|| {
        // (the size Windows' cursors are: 32 px normally, more with scaling)
        let size = unsafe { GetSystemMetrics(13 /* SM_CXCURSOR */) };
        let (open, closed) = if size > 56 {
            (hand::OPEN_64, hand::CLOSED_64)
        } else if size > 40 {
            (hand::OPEN_48, hand::CLOSED_48)
        } else {
            (hand::OPEN_32, hand::CLOSED_32)
        };
        [make_cursor(open).unwrap_or(0), make_cursor(closed).unwrap_or(0)]
    });
    let c = cursors[(which as usize - 1).min(1)];
    (c != 0).then_some(c)
}

/// A cursor from a PNG (as base64 text), its point in the middle of the palm
fn make_cursor(png_base64: &str) -> Option<isize> {
    let png = base64_decode(png_base64)?;
    let img = eframe::icon_data::from_png_bytes(&png).ok()?;
    let (w, h) = (img.width as usize, img.height as usize);
    let mut bgra = vec![0_u8; w * h * 4];
    for (i, px) in img.rgba.chunks_exact(4).enumerate() {
        bgra[i * 4..i * 4 + 4].copy_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    unsafe {
        let color = CreateBitmap(w as i32, h as i32, 1, 32, bgra.as_ptr() as *const c_void);
        let mask_bits = vec![0_u8; w.div_ceil(16) * 2 * h];
        let mask = CreateBitmap(w as i32, h as i32, 1, 1, mask_bits.as_ptr() as *const c_void);
        // (the point: the middle of the palm, which is in the middle of the image)
        let info = IconInfo { is_icon: 0, x_hotspot: (w / 2) as u32, y_hotspot: (h / 2 + h / 32) as u32, mask, color };
        let cursor = CreateIconIndirect(&info);
        DeleteObject(color);
        DeleteObject(mask);
        (cursor != 0).then_some(cursor)
    }
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let bytes: Vec<u8> = text.bytes().filter(|&c| c != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut n = 0_u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= value(c)? << (18 - 6 * i);
        }
        out.extend_from_slice(&n.to_be_bytes()[1..chunk.len()]);
    }
    Some(out)
}

// ── File types: each kind's icon in Explorer, and in the window ───────────────
// The .exe carries three icons (see build.rs): 1 videos (also the program's own),
// 2 pictures, 3 music.
#[link(name = "user32")]
unsafe extern "system" {
    fn LoadImageW(instance: isize, name: *const u16, kind: u32, cx: i32, cy: i32, flags: u32) -> isize;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> isize;
}
#[link(name = "shell32")]
unsafe extern "system" {
    fn SHChangeNotify(event: i32, flags: u32, item1: *const c_void, item2: *const c_void);
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegGetValueW(key: isize, sub_key: *const u16, value: *const u16, flags: u32, kind: *mut u32, data: *mut c_void, len: *mut u32) -> i32;
}

/// The window's icon (title, Alt+Tab, the taskbar button unless the player is
/// pinned there): 1 video, 2 picture, 3 music, from the .exe's own icons
pub fn set_window_icon(hwnd: isize, which: u16) {
    const IMAGE_ICON: u32 = 1;
    const LR_SHARED: u32 = 0x8000;
    const WM_SETICON: u32 = 0x0080;
    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        for (size_metric, kind) in [(11 /* SM_CXICON */, 1 /* ICON_BIG */), (49 /* SM_CXSMICON */, 0 /* ICON_SMALL */)] {
            let s = GetSystemMetrics(size_metric);
            // (a resource number in place of a name, as Windows allows)
            let icon = LoadImageW(module, which as usize as *const u16, IMAGE_ICON, s, s, LR_SHARED);
            if icon != 0 {
                SendMessageW(hwnd, WM_SETICON, kind, icon);
            }
        }
    }
}

/// Tell Windows the player opens these videos, music and pictures, each kind
/// with its own icon: they show in Explorer once the player is the default
/// for them (Settings > Apps > Default apps > Choose default apps by file
/// type, or Open with > Always). All in the user's own part of the registry
/// (no admin rights), redone only when the player has moved or this list
/// changed. Windows doesn't let a program make itself the default; that
/// stays the user's choice.
pub fn register_file_types(video: &[&str], music: &[&str], pictures: &[&str]) {
    const HKCU: isize = 0x8000_0001_u32 as i32 as isize;
    const REG_NONE: u32 = 0;
    const REG_SZ: u32 = 1;
    const RRF_RT_REG_SZ: u32 = 0x2;
    let Ok(exe) = std::env::current_exe() else { return };
    let exe = exe.to_string_lossy().into_owned();
    // (what was registered last: this .exe, and this version of the list)
    let stamp = format!("{exe}|1|{}|{}|{}", video.len(), music.len(), pictures.len());
    let set = |key: &str, name: Option<&str>, value: &str| unsafe {
        let v = wide(value);
        let n = name.map(wide);
        RegSetKeyValueW(
            HKCU,
            wide(key).as_ptr(),
            n.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
            REG_SZ,
            v.as_ptr() as *const c_void,
            (v.len() * 2) as u32,
        );
    };
    unsafe {
        let mut buf = vec![0u16; 2048];
        let mut len = (buf.len() * 2) as u32;
        let got = RegGetValueW(
            HKCU,
            wide("Software\\RustMediaPlayer").as_ptr(),
            wide("Registered").as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut c_void,
            &mut len,
        );
        if got == 0 {
            let s = String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]);
            if s == stamp {
                return;
            }
        }
    }
    let caps = "Software\\RustMediaPlayer\\Capabilities";
    set(caps, Some("ApplicationName"), "Rust Media Player");
    set(caps, Some("ApplicationDescription"), "Plays videos, music and pictures");
    for (exts, icon, word) in [(video, 1, "Video"), (music, 3, "Audio"), (pictures, 2, "Picture")] {
        for ext in exts {
            // One type per extension, so Explorer's Type column still says
            // which ("MP4 Video", "FLAC Audio", "JPG Picture")
            let progid = format!("RustMediaPlayer.{ext}");
            let key = format!("Software\\Classes\\{progid}");
            set(&key, None, &format!("{} {word}", ext.to_uppercase()));
            set(&format!("{key}\\DefaultIcon"), None, &format!("{exe},-{icon}"));
            set(&format!("{key}\\shell\\open\\command"), None, &format!("\"{exe}\" \"%1\""));
            // (listed under Open with, and in Default apps)
            unsafe {
                RegSetKeyValueW(
                    HKCU,
                    wide(&format!("Software\\Classes\\.{ext}\\OpenWithProgids")).as_ptr(),
                    wide(&progid).as_ptr(),
                    REG_NONE,
                    std::ptr::null(),
                    0,
                );
            }
            set(&format!("{caps}\\FileAssociations"), Some(&format!(".{ext}")), &progid);
        }
    }
    set("Software\\RegisteredApplications", Some("Rust Media Player"), caps);
    set("Software\\RustMediaPlayer", Some("Registered"), &stamp);
    // Explorer: file types changed, redraw their icons
    unsafe { SHChangeNotify(0x0800_0000 /* SHCNE_ASSOCCHANGED */, 0, std::ptr::null(), std::ptr::null()) };
}

// ── Taskbar badges (see Taskbar::set_badge) ───────────────────────────────────
// A white picture, music note, play or pause sign on a dark grey rounded square, at the sizes
// Windows asks for (its small icon size: 16 px, more on larger display scaling),
// as PNGs written out as text
mod badge {
    const PICTURE_16: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAABmJLR0QA/wD/AP+gvaeTAAACA0lEQVQ4jYWSzW8SURTFz33APGRw",
        "wQgLkUSgiV2py0YoNgZSJFX/BJPWNJialKQmJNLEj42uG5P2D1E+VJZOSNOKUXd12w6LCoNJyzAQ5rloxVDscHfvJL9zz733EQA0",
        "Go3A0ZHxZmBZSS65LsGmzF6/6WCsKsvuZ8Fg8BedwtvFUuVKrVaTWi3djoei+BCPxXqZTHpflt0zjmx25W258nG2WCxJhtG1hQHA",
        "MLrY2/vpIMbk8NVwgFmWSKqqKtlBRIS5uQRu3rg+1FS1JllikHRyzhVdb9t2vZ2IY72QhxACi0tZaFoDut6C5HL7mRCCJsVut39D",
        "CAHDMGAYxlAXQpBz4tAAvn3/gaVHj9HpdHA2LTsPmpqKjrwPDrQx+FyDVPIOtjY3kJ5PDbVoNAKi8WnHDCKRMHK5JwCA1dUVTE9f",
        "QyIRx9bmBu4tZMYMRnbg9cp48bwAzjkAwOVy4dXLdciyF0SE5eVFfKl/haY1/iUgIoHTWz9dyyEYvDzSQVEUcH7yTdxuN/L5NTB2",
        "EpyIhNPsmU1F8V0ECDu7dezs1v+3ljOmPliWhX7fPHQ6GKvGY7GH794XpVKpMhH+Ww/uL5gAq5Kmaf7j4+52ufwh9FmtSbresgV9",
        "PgWJ2Vtm+u78vucCnyEA0DTN3+l0Xw8GgxTnPGBnYJrmIZHjk8cjFUKhUPMPMNrBMhz2oPAAAAAASUVORK5CYII=",
    );
    const PICTURE_20: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAABQAAAAUCAYAAACNiR0NAAAABmJLR0QA/wD/AP+gvaeTAAACYElEQVQ4jaWUy2sTURTGv3vnTpp0",
        "kpoE+w5Y24Dd2EmVYifpRioqQqBVd4KCtNGmQrIQbBFfILUWxbpQ60L/C5eKi8w0dJVJheIfkEiyLBPSSXInLmoKfTKp3+6c79zf",
        "uYf7IPinfL44Vq1WEvV6XSEEbthQvQ6DEKIxJi739nZqAEAAIJf7M2ua5oKqrkp6dl3Y2tqyw4PT6URIHuKRiGKIomM+EOj+RPL5",
        "4phplr99+Pi5TdeztkB7FZJlxOOxzdZW51VarVYSqZQmHRcGABldh6qtSqZZTVLLspRs9pdgd3Eg0Is7t2/h3HBoV17X14VazVKY",
        "IFDJKJVswQgheL34Eu3tJ8E5x9R0HLlcHgBgGCUwRj20mdEopfB4ti+AIAhoa/Psq2HNADnnWFp6h8nJKDKZLDY2fv8fEABSqoaU",
        "qh3qHzmy3+9HMDjQVMNDgS0tLVhYeIG3bxbR13dqlydJUvPAZGIW/af74HI58fzZY7jd24cRn4nh65cV+Hxe+8Bo9BrGxy/uxD09",
        "3Zife4ibNyYxMRGFz+dFIvHAHnBw8Azu35vaVzgych6x2N2dOKxcwJXLl44Ger0n8PTJHERRPLD7Xs3MTKOzo2NXjnFuldyS5G3s",
        "7vuPn7ZgDQ0E+1EoFuF2S6jVLINRSjVZPns9o+tCOr2GdHqtKWBDsjzEGaMqZUx8H4kopZAsHwsEAMPDMiLhUcPhYMuNDzZeqVRe",
        "qdqqpOvrQrlctgVyuVyQ5SEeCY+WRFF8FAj0rJCGWSgUFNOsJms1KywIZP+rP0Cc1zcZo5rDwZa7urrSAPAXJxnHkLVpuroAAAAA",
        "SUVORK5CYII=",
    );
    const PICTURE_24: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAABgAAAAYCAYAAADgdz34AAAABmJLR0QA/wD/AP+gvaeTAAADC0lEQVRIia2QX0hTYRjGn/fz6+xs",
        "6mpGguA2ZxebXeSftlkaNvsDXVQWxjTSLCn7g0Fa6EURVNRFN3VbBGUFxSC6DCkq8dZW0s3WlQ3DUYZr/tnx7HS+bnRojfJov6uP",
        "932/53l4CHOEQqGcQCBwUNdFk6pqlUSwwgBCIClJ/L0Q9HRw8O2zYDD4EwAIABKJhC2VUvpmZ9X6kZGYHI1GuaqmjehDklbB43Fr",
        "TqdDkSTTK7NZOmaz2RI8FArlTE8rfZOTk7v6Hj6Sh4bChoQXQkTc692Ud6T18G7Ael8I0UjxeLxJUdR7d+7ey1uJ+EJ8Pi86TrRP",
        "mUxyO9N1ah4Zia0o+e8MDb3D51hMBkQz0zStIhqNcqMijDEcOLAPF86fQ1VVxaKdEAKRyCeeTmuVnDHKn5lJGU7Z0LAHp0+dAABs",
        "374NHSc7MTr6JbOfmZ4BY2RlhpXnWF9amnlzzuF0OrLeLdvg9Zu30DQNAPD16zcMD3/Meme4+3nC4Q/oONmJEqcTH4aHMTU1/X8N",
        "AGB09Mui3rOxpIp27qhHYeG6ZYX4p0F9fR16erpx4/oVWCyW/2vgcpWg69xZAIDDYUd311kQEQDAZDLh9q2bOHOmY3kGZrMZly72",
        "QpblzKyubiuago0gIvT2dmPDhjLsb9iLar/PmAERobenG3Z78R+7o0dbce3qZWytrcnMuro6YbXmL92gKdiImprN2T8wBr/fu2hW",
        "UFCQqfKfBuXlG9HW1pL1+G/U1m5BIFD3x5zrupi0WMxrMVdNZcVG9Pe/NGwAAKWlLgwMDEIIAUuuBbouklyS+HuPx11MRFwIgQd9",
        "j5clvhAiQpnHrXHOwkwIeup0OhSvd9OKhefx+70odtgVgD3Jqa72RVyu9V6Px10yPv6dj42NrSh5td+HlpZDSl5u7ouiosJrBAAT",
        "ExNrUin1vqrO7vwci8mRyCeeTCYhhL5EYQZrvhVlZW6t2GFXzCZTvyxLx202W4Lmj4QQLB7/1giI5nRaqyTCaiPphcAPzlkYYE+K",
        "igqfE5EOAL8Ax0oKLvDeyxYAAAAASUVORK5CYII=",
    );
    const PICTURE_32: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAABmJLR0QA/wD/AP+gvaeTAAAEN0lEQVRYhb2XX0xTVxzHv79zT3tp",
        "bwLY2PLPAm6EMlNYMqBloouRscQHI9tgEBqHqXFTB0HUZMpQ3GJ8NE63PW1ZooHscXNZyBh70bggsCXqi8BeBqyArZM/llLae88e",
        "sAh0KtB236d7fud3z/eTc87N/f0Iq+Tz+bJCIdVFRBVCIJsIKatz1iMhME2EESLqYQwdZrPZs3yeniYKGh9/0CKEaA6FFlJDoXCS",
        "pmlcCI3FAkDENMZYWKfj83q9/hFj0qW0tM2fE5FYAhBCsImJB1+oqvqe3+9P7esfkHp7+zA29jfm5uZi8YfRaITVmoUyZxlKS19T",
        "FUWZkiT+XXq6uYmIBAGAxzN5XNPUVq/XZ/r6m29paGg4JtNnyWbLx0H3AWE2b/6Hc/5ZerrlMvl8vqxgMPzb48ezWZevfCUlyjyi",
        "ggIbGj86rCYnJ49yzrazUEh1hUILqX39Awk3B4D79wfR3/+HFAwGTZoGFyOiilAonNTb25dw84h6b9/G4iUXFVwI5GiaxkdHx2Je",
        "mHMOV30tystfx+zsLK5e7cSdu/ei8sbGxqBpGgeQw4iQLITGAoFAzACu+lq4XHXIzc1BYaEd58+fQ2ZGRlTe3FwAQmiMCCkxfeOr",
        "tWPH9hVjWdbD4SyJyhNCRB4prgDT0zNRsZn/iC1XXAGuXetEMLiwNB4cHMKNm7ee+w6PJ8Cdu/fw4eFGOEpLMDMzgxs3byEcDv9/",
        "AADg8Yzj+x9+XHN+XI9gI1o3QNW+vajatzduAOs6gl273sDRox8AAAKBAH7u7okZYM07kJOTjZZjTUvjxsYjyMt7OSrP6SzFoUNu",
        "MLa2pdeUZTAk4UzbKRgMSUsxWdbjXPsnSEl5WjDZ7dtwpu0UaqrfRl1tdXwAiAgnTxxDdrY1as5iMaP9bCs458jMyED72Vbo9XoA",
        "wP799cjPz4sdoPrdKuzcWf7Mebt9G5oaj+DChU9X7IYkSThxvBk6nW7jAK8WFcLtbngRI/bseQuZmdE/na1bc9HwvmtjAKZNm3D6",
        "9ElIkvRCgOeppuYdFBXZ1wfAOUdb28cwmUwxmWPZHTIYDCtiTyT4Yt3O0oxGI4tUwJWVuyHLMoaH/4wZIKLKyt24fv0n4EmlTMQ0",
        "IcQUB/AXY+wlqzVLPzi4WBN2dXWjq6s7buarZbVuAWMsDNAIY4x+1en4vNPhTJjhapWVOaDT8XnGqIcxhg69Xv/I4ShWbbb8hJu/",
        "UlCAkuJiVZblh0Sig5nNZg9j0iVFUaYOug+IggJbQs3d7gahKMYpgC5aLJaJSGtGExPeK6oarvP7/an9A78vtWZ+vz8mU0VRYLVu",
        "QVmZAyXFxaqiGKc4551paebmpdYsAjE56W0SQrQEg0FTIppTWZYfAnQxPd385YrmdLm8Xm+GpsGlaeJNANlESI0FQAhMARhhjH4h",
        "Eh0Wi2Vi+fy/pn2IcLWZvjsAAAAASUVORK5CYII=",
    );
    const MUSIC_16: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAABmJLR0QA/wD/AP+gvaeTAAACFElEQVQ4jY2Rv08TYRjHPy8td/2B",
        "CT1gaS8R5cpqmIiIU4ktMfonuKj8cMEQHWAEgw46GCfjQNwgYdNSUEebpv4JXqsLPRDKHZUCd9e056LBBlv8bs/75PN9v8/zCIDt",
        "7e2+avXkab3RSMhSZw9t5Li1fV9Hx6dwODAXjUbL4jecT69vxHK5nGSaVjseRYlwbWTEHR9PboXDgWHf5OSDV5mND6Pp9Lp0cmK3",
        "hQFs26FQLPpAhPsv9vf5Gw0vkc1mpVZAT49CPK4Rj2sMxjWGhq6w+OQZ2WxOSqXGEn5ZlhXLOjgDLi+/JhwK0d3dfaYXDAaxLBOp",
        "M9Dr9zxP/OvnWDQKgGma6HqRr3oBXS8wPXWfUskAwPM84W8V3XVdZh4+olj83vQ+OXG3qe5oZVCvNzg4qLRqn2/wv2o5wt8aHNSY",
        "nZ1BjUWx7eZTNxlo2gDTU/dQFAWf7zTc9NQEly/1A1Cp/KRSOR3NL4Tw/hTzc49R1RgA5XIZ160BkM9/QdMGKJVKPH/xkt3dPQCE",
        "EJ7fcZ19RYlcME0LXS+iqjF2dn6wsLjE4eEhACura6ysrjVFV5QItZqzJ3T925tMZvPOu/dpCaCrq4tqtXruXm7fuukkk8m3wjCM",
        "3qMjO5/JbKqfsznJssy2YCSicH30qpNM3dgKBeVhAWAYRu/xsb1Ur9fHZFnua2fgOM6eEL6PoZA0r6rq/i9xodLKeIj6ogAAAABJ",
        "RU5ErkJggg==",
    );
    const MUSIC_20: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAABQAAAAUCAYAAACNiR0NAAAABmJLR0QA/wD/AP+gvaeTAAACo0lEQVQ4jcWUy08TURTGvzszxQ7T",
        "8iqJKKUk2hIpkSkPE9uia0wqJpro0oQFQjWBnbIwIVHR6IaQWB//hyZA2NEpG7BTHrG0uMC0UBblkU7ambYzLtAmbZo6rvyW53zn",
        "l3PPufcS/FYyeTiUzyuTmqa5CYEJOqRpyBBCBIYxzLW3nxcAgABAIrH/WJbl2WAwxImRDTqXy+nhwWg0wsX3Fr1ed8ZgqJu2Wi98",
        "IMnk4ZAsZ7+8D3xqEMWILlClXDwPv3/stL7eOEzl88rkyorA/SuMEIJOWwdstg6ERRFBIcTJcn6KUVXVHYls0n8DWCwtcDjscDjs",
        "6HLY4XRegdlshhBaxczMK4jiBu1xe9wMTVNcRpKqQny+W7gz4oO5wYyW5uaqHhN3tr9MRgLDUGamVleDA/3o7LSVxbLZHOLxOL5H",
        "Y7h5w4voTqwsXxOoqiryhQIWFpYQjcYQje5gb+8nVFUFAAz0u1B5I2oCNze34eiyY34+UMtWJkq3838Bax65mnp6ujFy2weWNYJl",
        "WX1Am60D9suX0NjUWBZ3Orvx7u0sGOasTNO0s7dbCzg6+hAP7t8DIQSqquL4+KSUuzbYX4IBQCp1iMWl5bL6shm2tlpKMACgKAqx",
        "WLyUX1xaxu7uDyiKgvX1MJ5NP8fBQaq8w2JRlUwc1wQAkiQhnT6CxdICAFhb+4YXL9+UzPv7B5jwT1abEkwmDoWCmmEoihJ4/urd",
        "sCjS2WwOj8afwOXikU4fYWtrG5qmVQVUiud7iwxDBUkikfIqSu5rIPC5ISyKuoor1dfHY2J87IRlzw3/+WD9iqK8DgohThQ36Gw2",
        "qwvEsix4vrfo9VyXDAbDU6v14sfS1lOplFuW81OFguqhaWLWAywWtVOGoYS6Omaura1tFQB+ASH1AWn+jnRIAAAAAElFTkSuQmCC",
    );
    const MUSIC_24: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAABgAAAAYCAYAAADgdz34AAAABmJLR0QA/wD/AP+gvaeTAAADTklEQVRIibWT309TZxzGn/ecl9NS",
        "aOFIBJm0lYyuxYspoaUXm4Zl0+zWgFgzXaYy/wCXjLiZ3Rgv3BL1dtOEsJphmhkvFxKNbu7OWrxR+wNjOWzSZEi7HmhPD6fn64Wz",
        "AYuzxfhcve/3fd/P87y/GP5TJBIRBwcHh02TDui60ccYHKhDRMhLEp8mYldu3751dWRkpAwADAByuZxcLGoTpZL+UTqtWBOJBNf1",
        "lXr4kKQG+Hxew+12aZJkud7YKB2RZTnHI5GIuLysTaiqumfi57A1Go3VBV4txhj3+/ubPz/82aeAY5yIhlgmkzmgafqlH3+61Pwm",
        "8NUKBPw4/uXRJYvFelQwTRZKp5UNJ2eMwe1yYu/ejzE8tA8AEI3exayiWAEKccMwdiYSCV4rsK1tEzyeHng8PXjP04Pt232w2+0A",
        "gEKhgF+vXgMRIR5P8u5t3X1cEJi9UCj+L/Tbb77G5vbN2LKlA5tk+ZXzRFGstAvLBQgCc9SUfPfuD8EYW3eMiKDM/YWWFgdKmlY1",
        "XvPRvNDi4iJSqUdIpmaQSs3g/v2HUFUVFy78AKmhYWMGuq4jmZzBmTNnsZjN1hVIqNVgYeFp3fCaDd5Eb92g7kteLaezC4FAP8yy",
        "CS6uj9qwwf7hfTh27AsIwvNDMAwDijJXm0FrawtCof3YueN9qKoK8aV0HR3tGB09suZvcM5RKumvN7DZbDh/7nts3fpOpUZEEERh",
        "zZyXP96sMocTX41VGVRdcjDoXwMHANM0EY3erfQfP04jHP4F+byKpaVl3LhxE2Njp1Aul6t3YJqk2myNbS8KT/6eBxFVEhIRLl4c",
        "x9TU9TULw5cnEb48WQWs7LLJBtOkPJckPu3zebsYY5yIkEimcPLkd9i16wOU9BL++P1PPHgYfyVoPTHG0OvzGpwLMU7Errjdrj1+",
        "f3/znTtRAEBs+h5i0/fqgq7WwIAfXS6nBgiTYjAYiHd3v+v3+bzbFhae8vn5+Q2DGWMIDgRw6NBBrbmp6bfOzvbTDACy2WxrsaiP",
        "63rpk1lFscbjSZ7P50Fk1ggW4LA70NvrNbpcTq3RYpmyWqVRWZZzlbdGREIm888QQKGVFaOPMbTUk54I/3IuxABhsrOz/RpjzASA",
        "ZwaaTAONMeX0AAAAAElFTkSuQmCC",
    );
    const MUSIC_32: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAABmJLR0QA/wD/AP+gvaeTAAAEYklEQVRYhe2XXUxbZRjH/8/b03Ow",
        "hzA+dkoJdCOhUNjixDE+nboEs7nN6J0xwcyF3Rh14ky88UbFzMtlDhUTp8tMMMhExezKbcm82ML4cDFhCnReDNqu0A6KUuDs9Lyv",
        "F9KuHZStfMQb/1fveZ7nze9/PvKe5yHcp1AoVGgYZhMRNQqBLUTYdH9NOhICM0QYI6KLjKFD0zR/Yp7uFQq6fXvymBCixTDuZhtG",
        "NINzLgnB2VoMEDHOGItardKCLMvTjFlO5udv/piIRNyAEIIFApOfmKb5YiQSye7rH7D09vbB6/Vhbm5uLXzYbDY4nYWoq61DdfVO",
        "U1XVsMUidToc2lEiEgQAfv/E25yb7waDodzTX56h0VHPmqCp5HaX4UjzYaFpm6ckSWp1OOynKBQKFep69Ors7N+Fp9o+s2wUPKby",
        "cjfeeP1VMysra1ySWAMzDLPJMO5m9/UPbBhclmVkZqoAgOHhEfT3/2rRdT2XczRJRNRoGNGM3t6+dYExxuB0FqGs1IXSUhe2b69A",
        "cXExvjpzFt3dPwIAeq9dQ21tdYaiiEZJCGzlnEvj495VAfPyclG6CCtbBGZmZi6pc5WUxNderxeccwnAVokIWUJwNj8//1DA/fv3",
        "oazUBbtdg8tVgpyc7LRNz83NQwjOiLBJSndzy5uvgbH0j4YFXY+vhRCxJaVt4EHinGPc64PHcxMez008/dRubNtWAY/n5rL1azYw",
        "NTUFj+dPjC4Ch4Z+x+zsbDy/8/HKFfenbcAwDCiKgvbPT+Py5V8wPR1enfNFpf0yI5EIAGBo6Maa4asysN7638B/bmBdzwGr1YoX",
        "nn8Oe/c2oqioEFNT09ATDqANNSDLMo4ffx+P7Xg0HrPbtQfuW9EAEWHPnifR0FAPu6bB5/fDYll+y4ED+5LgD6uUBhRFxoet76Gy",
        "ckc8VlHhTjzHk7QSvKurG1euXF02l/IjPHTo5SR4TES0bP2NG38sG79+/Tec/boDMzN/LZtP+QR2P1GfKoWen85jcnIyKfb9Dz2Q",
        "rBIOHnwWuTk58Hp9+PnCJfT0nEc0Gk11E0L6t29n+TabjSV2wLcDARQUOJbABwevo739C3DOk+Kcc3R2nkNn57mUxmOy2WwgYlwI",
        "EWYAbjHGok5nYVJRW1s7hodH4te6ruPbru/wQetHS+DpyuksAmMsCtCYxBhdslql+tqaWnlk5F5T6vP50fLWO8jLy4NqewRenx+m",
        "aa4JHFNdXQ2sVmmBMbrIGEOHLMvTNTVVpttdllQohEAoFMKtsfF1g1eUl2NXVZWpKModItHBNE3zM2Y5qapq+EjzYVFe7l4XUCp4",
        "c/MrQlVtYYBO2O32QGw0o0Ag2Gaa0ZcikUh2/8BgfDSL/f9XK1VV4XQWoa6uBruqqkxVtYUlSfomP19riY9mMRMTE8GjQohjuq7n",
        "bsRwqijKHYBOOBzap0nDaaKCwWAB52jiXDwDYAsR0u+7EyQEwgDGGKMLRKLDbrcHEvP/ADbLyWEhiL2jAAAAAElFTkSuQmCC",
    );

    const PLAY_16: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAABmJLR0QA/wD/AP+gvaeTAAAByUlEQVQ4jZWSwWoTURSG/3snnZnM",
        "ENMZk00MZB7ArouTBEoj1r6EYEELblx0pQ/QLNRVoRiL3Sj4ACVN1ezqOGRdV3VToUwgNZnEtJnchJnrpkK040XP8p7zf/eDcwgA",
        "tNvt7Pl5UA2jqKLIc9chKDaZdiVKm7quPsnlct/JZbhV3z+44bqu3Ov5ojxM00DRtierqyunuq4uSuvrj7YaBx9K9fq+HARjYRgA",
        "gmCM4+OvEqFUtwpWlkYRrziOI88OlctFzM+nhSDHceWIh5WEoiim7/d/az58sAZVVfHm7Tvs7dXBOb8C8P0e5Dk1QznnJO6HdPoa",
        "1u7fQ+3lFiyrEGvBOSdUpKlpSVhWAS+eV7Gx8RjJpHplRgj4ValUCpXlJey+rmFh4eb/Ay51Y98T/xIeDof47LawvV3Dn6sWAkaj",
        "AJ1OB5vVZzg5+RZvQAiJdRsMfgjXCACEEJ5gE9Y1TSM1e8KvdnZxdPQF/f7gr3amaWA6ZWdUorRZtO3JbPPw0BGGAaBUtBlAm8Tz",
        "vMzFxbjVaLzPf3Jc2fd7wqBhmCiXbrGVu3dOtaSySADA87zMaDTeDMPwtqIoWRGAMXZGiPRR0+Sn+Xy++xMoILvyIVNVDQAAAABJ",
        "RU5ErkJggg==",
    );
    const PLAY_20: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAABQAAAAUCAYAAACNiR0NAAAABmJLR0QA/wD/AP+gvaeTAAACIElEQVQ4jaXUPW/TQBwG8OfO5ySu",
        "XcRCCTSCwgqJ0615YUGl0C/CkEpNkCohPgAvRa2UqIIGFTrBR0AtL2IhcRix04EuUJAalFCWypFjO/YxlDAElJjmWe/5/+6W/xH8",
        "TqPRyrquk+ecpwiBggDhHCYhRGNMLE5OntYAgADA/v73Bdu271WrNVk36kKn0wniIRKJIKkmvEwmZYpi6E4sdmadNBqtrG1bLx89",
        "fnJC141AUH+Sqopc7ubh2FjkBnVdJ1+paPJxMQD4qOuoajXZtt0C9X0/ZRg7Qn8pkbiM+fnrIIQEQnW9LnS7fooKApXNdvuvgqom",
        "UMgvoFxew8ULU0NB02yDMTpOB5UIIZg6fw6l0goWF3OQJGkoPBDsoeFwGNdmr+LF801cyaZHA3sJh8NQFAVLS7fwcPkuJiZOjQb2",
        "IkkRxOOX8OzpOubmZkcHh4X974BldfBpdxerqyW0Wj+OD9q2DcdxUSyu4X1FO/4LOedwHAdv3r7DxsYmLGvwng8EOefY+/oNyw9W",
        "8PnL3rC7j0DP89uKLJ/sP9B1AwcHP7G9/Rqc86GQosjodn2TUko1VY17/QXD2MHW1qtAGI5W1WOMViljYimTSbWTqhpo8F+ZnlaR",
        "Sc+YoRAr9j7YnOM496taTdb1umBZViBIkiSoasLLpGfaoijejsXOlv/8Tc1mM2XbbqHb9dOCQMaDgJ7HDxmjWijEitFo9AMA/AI3",
        "utaIaUfU7wAAAABJRU5ErkJggg==",
    );
    const PLAY_24: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAABgAAAAYCAYAAADgdz34AAAABmJLR0QA/wD/AP+gvaeTAAAC3ElEQVRIia2TzU8TQRyG35lOt0uh",
        "hcZYU/kSPLQ9GCQUOCgEE02MCScIoAIJBNR/wBg/Tn7GqIFIJGIwyEeANBguRGPiwYDeEEI80HJA6IVaMJQidNlddjwICRqQXeC5",
        "zrvv85udGYIN/H6/qaSkpFzTeKUsq7mEwA4DcI6YILBxzkn/yMintxUVFesAQAAgGo064nGpc21NPjMzExKDwSCTZcVIPwTBDI/H",
        "rWZmZkiCYPmYkCDUORyOKPP7/aaVFalzeXn5XGdXtzg6OmaoeCuEEObz5SXV1lw+D9g7OOdlJBwOV0qS3N72qj1pP+Vbyc/34Upj",
        "/S+LRaynmkaqZmZC+5r8X0ZHv2I2FBIBXkVVVT0ZDAbZdsGUlGTcuX0D2dlZhgSccwQCU0xR1FxKKbGtrsa3DTqdh1FcfBqtL5px",
        "7WoDRFHULVldWQWlxE71hCmlKC29gJ7u1ygqOqV/KwB0CQDAbDbDZrPh1s3rePTwLo44nQcrwJ9rCJPJhJycE+joaENtzSUwtu3x",
        "7U2wCWMMjDFUVpaj7WULvF73wQo2MZvNSE09iuamJ2hsqDt4gR7+/wN3QVEUhMM/8PRZMyYngwcnUFUVANDV3YuBgUGsr6/vmDUk",
        "4JxD0zRMTHxDU3MLIpH5Xb/RLZBlBfF4HM9bWjEy8kX3ULoEmqZhaOgd3nT2QJIk3eUAwDSNL1utCYe2W4xE5jE8/Bm9fX5MT383",
        "VGxNtELTeIwJAhv3eNxphBDGOf8rFI0u4f6Dx4aKsfHivR63yhgdo5yT/szMDMnnyzNctBMFBT6kZaRLAO0zFRbmB7Kyjvs8Hvex",
        "hYWfbG5ubs/FhBAUFuSjuvqilJSY+N7lct4jALC4uJgSj8sdsrx2djYUEgOBKRaLxcC5prOYwm6zw+t1q2kZ6VKCxfJBFIUGh8MR",
        "JZshzjkNh+fLAF6lKGouIUg2Mj3nWGKMjgG0z+VyDhJCNAD4DSZWFmSdzVf2AAAAAElFTkSuQmCC",
    );
    const PLAY_32: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAABmJLR0QA/wD/AP+gvaeTAAAD1klEQVRYhb2XXWibZRTH/+fkTTLe",
        "d7SlWz60TQdeLPEDGaYfQWQg3YXsTizboBcb6Y0wR90uJqggSEGFMfq12trOKVtXL1rq3RA3kSHSNd0YorLOK9vYpUu6pbqkTd+8",
        "z/GipGaD1fXNkv/VC+c55/974H0O5xAeUSqVqjNNq52IWkXQQITqR89sRSJYJsIcEV1mxqjH41kojtN/B4Xu3Ll7XEQ6TXOtxjTz",
        "25RSmojiUgCIWDFz3unUVl0u131mR7fPt7OHiGQDQEQ4kbjbb1nWgUwmUzMdm3FMTU0jHv8L2Wy2FH/ouo5AoA6Rlgiaml6xDMNI",
        "OxzaN36/5xgRCQHAwsLiCaWs95PJVO3I2XN0+/YfJZk+TsHgbnREj4jHs/Oepmkf+/3eXkqlUnW5XP7nBw/+qevtG3CUy7ygUCiI",
        "d46+bVVVVc1rGr/Kpmm1m+ZazXRspuzmAHDr1ixisRuOXC5XqxTamYhaTTO/bWpquuzmBU1du4b1n1xaWQS7lFLa/Hx806S2t97E",
        "2MWvsXfvayUDxONxKKU0ALuYCFUiildWVjZNqq9/Fjt21OLDD95DV9dH8Pt9tgGy2RWIKCZCta033tQYxsjwAA4dbIOmaVvOF5HC",
        "J9kCICI4nU5Eo4cxMjyAPXtetlMGAGC7yxGtN1G/34fPPu3CyZMnUF1dVTmAjQLMICLsa30d5778Avv3v7EBVxGAYhmGjnc7j+LU",
        "qU/Q0BCoPEDh5i+9+AKGBvvQ0XEYLpercgDFIA6HAwcPtGFwsBfbtxuVBXhIsnl464/4STxFoJTC+MQkzp8fw9raWmUAlFJgZvz6",
        "2+/o6TmDubn5/815qgDZ7ApGzn6FS5e+K+525QVQSoGIcOWHHzE0NILl5b+3lG8bQERAREgkFtHd04+bN3+xVccWgIjANE1cuDCG",
        "8Ylvkc/nbZnbBojNXEd//yASiUVbpkWtWrT1uZ19uq7zZhNwPL6ApaV7+HxwGFev/mTLuCBd10HESkTSGoA/mfm5QKDONTv7+Jlw",
        "fGIS4xOTJRkXFAjUg5nzAM0xM11xOrXVluaWp1L8SRSJNMPp1FaZ6TIzY9Tlct1vbg5bweDusps/HwqhMRy23G73EpGMssfjWWB2",
        "dBuGke6IHpFQKFhW82j0sBiGngbotNfrTRRWM0okkn2WlT+UyWRqYjPXN1azTCZTkqlhGAgE6hGJNKMxHLYMQ09rmnbR5/N0bqxm",
        "BYjFxeQxETmey+Vqy7Gcut3uJYBO+/2eMw8tp8VKJpPPKIV2pWQfgAYi1JQCIII0gDlm+p5IRr1eb6I4/i++uJaCvKMJpQAAAABJ",
        "RU5ErkJggg==",
    );
    const PAUSE_16: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAABmJLR0QA/wD/AP+gvaeTAAABfElEQVQ4je2Sv07bUBTGv3Md7BCL",
        "wTfOEiKRp4DgpFMQgaUPUBY2JN6APgA8AC/QDoUdKSRpM9Yy4RHSFTkD9nUHwLmJ7NsFDAHJiL3f+J3zO/90CAAmk0nl7i4+SdK0",
        "behLZeRIzuahxtjQNItH1Wo1oEd41L3sr3qepwsR5fHg3ELTcWa7u50b0yxuaAcHh6e9/s9Wt3upx/E0FwaAOJ5iPP6jEWNmfa1e",
        "YWmq2q7r6u+Sr+S6np6qpM0Mw+BR9DcLNBrrGPQvcPbjW+adn33HoH+BRmM986JIQF8q2gWlFC3uyEFEsO3nW5bLPIu9lFKK2EdH",
        "f63/BQBGROqlIYSAUgpBEGZeEIRQSkEIsQATkSrImQw5t1aeXvjq6hqdnc8LiV/29t905tzCfC5vmcbYsOk4s4+O3mo6EmBD8n3f",
        "vr+fjnq9Qe236+lRJHJBy+L41NqUnZ3tm9KysUEA4Pu+/fAwPU6SZMswjEpeASnlLZH2q1TSv9ZqtfAft4GbaDactM0AAAAASUVO",
        "RK5CYII=",
    );
    const PAUSE_20: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAABQAAAAUCAYAAACNiR0NAAAABmJLR0QA/wD/AP+gvaeTAAABs0lEQVQ4je2UsU7bUBSG/3t9beXi",
        "0omocZKmT9Bcs+GYB+BJ2iZtBFvFC7RrOjQtr8AEQh1gxk5HbsKOSiFVCBOy5diObYaSSFSRGiVj+01H/9H97tEZDsED/f7NZhxH",
        "21mWWYTgCeYgy+ARQlzG1Fap9MwFAAIA19e/3oZh+MFxOrrs9pTRaDSPD7lcDqaoJrZteaqq7ZbLxhfS799shmHw7XN776mU3blE",
        "f2IKgUbj1d3KSm6LxnG0fXrq6ovKAOBMSjhuRw/DeIemaWp1u+fKwrYHpOwp43FqUUWhuuf7j5pFw8DhwT5Ojo/QfFef5s1mHSfH",
        "Rzg82EfRMB698TwfjNFVOuu3fH4NnHMAQOXF82leqfyuOefI59dmTjpTuAz/hf+scDi8RRAEAIDLHz+n+aQOggDD4e1MIbm4uLxq",
        "t/dKZ1IuNZlpCtTfvL6ilFJXiJfJUjYAQlQTxqhDGVM/2bblm0IsLFtfF7BrG56msdbkwDaiKProuB1dyp4y2d/f4JxDiGpi1zZ8",
        "VVXfl8vFr2TSHAwGVhjGO+NxWlMUsjqPMEmyO8aoq2msVSgUvgPAPZ0xpvZA4zVjAAAAAElFTkSuQmCC",
    );
    const PAUSE_24: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAABgAAAAYCAYAAADgdz34AAAABmJLR0QA/wD/AP+gvaeTAAACTUlEQVRIie2Sz0sbQQCF34zTySZq",
        "0qUgBPKjUkw2twY37tVCCz20J4taagu1ttC/o3+EtEQk7UEJSA8RitJDwZM01Ws2XuJaMFCLyaYm62S700sJoo006NHv+mbe9xiG",
        "4C/5fL5vfHz8kefJKSHcNCEIogekhM0525aSLG9sfFmZnJz8DQAEAGq1mtpqObnjY3GnUrEU0zSZEO1e+sH5NWha0o3HYw7nvs9+",
        "P3+uqmqN5fP5vqMjJ9doNO7l3n9QisWtnopPQghhuj468Ozpk/tAcFFKOUGq1eqU44js23fZgYuUnyST0fHq5ewvn0+ZpZ5HpisV",
        "60LLT1MsfsOuZSmAnKau6942TZNdWjsAKSVKpTJrt900pZQMNputM4cYY1jIzmN9rYDVwgoMI9PJDCOD1cIK1tcKWMjOg7Gz+5pH",
        "TVBKgrTbilAoiGg0AgDgnCORGOlkicQIOOcAgGg0glCo+4/uKrgsrgRXgivBOYJ63cbe3ncAgBACZXOnk5XLOxBCAAAsaw/1ut1V",
        "wDxPNgIB/43Tgeu6eDH3+p+XNje/4sHDiXOXB/oD8DxpU87ZtqYlXULIuRd6gRCClJZ0GaNbVEqyHI/HHF0fvTTB2JiOSCzqAHSp",
        "zzAypeHhW7qmJW8eHPxk+/v7F1pujGUwM/PYGejv/xQOD70hAHB4eHi91RKLQhzf3bUspVQqM9u2IaX3n8UUwcEgUqmkG4lFHb/P",
        "t6YofE5V1Vrn4aWUtFr9MQHI6XbbTROCUC/rpUSdMboF0KVweOgjIcQDgD9FzOenJKktDAAAAABJRU5ErkJggg==",
    );
    const PAUSE_32: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAABmJLR0QA/wD/AP+gvaeTAAADCUlEQVRYhe2XT29TRxTFz73v2Y78",
        "hLGiPMeVMexis60TxzHZhc+AIrlSUdggtQjRBVVZsOgfKd1EFNquWnVREuAzAJuGpE6csiXQVRM3cWIDJuU5Mc8ztwtkYyBGBmNW",
        "PqsZzZl7fov3ZuYSXlGpVIq4rsoQ0YQIDhPh4Kuet5EInhBhjYhuMWPWtu2N5nV6YRTa3Nw+JyJnXfdZ0HVrfVprU0RzJwBErJm5",
        "5vGYe16v9zGzcWlwcOAHIpIGgIhwobD9o1LqhOM4weXcipHNLiOf/xeVSqWTfPj9fkSjEaRGUxgZ+VhZllU2DPN6OGyfISIhANjY",
        "2PpCa3WhWCz1//Lrb/Tgwd8dhbZSLDaEU1MnxbYHHpmm+XU4HLpMpVIpUq3WFp8+/S9y+crPRrfC64rHY/j8s9MqEAismyan2XVV",
        "xnWfBZdzK10PB4DV1fvI5e4a1Wq1X2tkmIgmXLfWl80udz28ruzSEp5/5DLBIjiitTbX1/MfDCCfz0NrbQI4YhIhIKJ5d3e35YZM",
        "ZhKfZCZhGAYAYH5+Ad98O/2S5+LFrzB+LA0AUErh96vXMDd3Y996lcouRDQT4WBb//hYKtkIB4Dx8fRrnmPpscbYMAykx0Zb1hOR",
        "+pDaAiCiN87b9eynjk6596EeQA+gB9AD6AG0BdB0eew7b9fzzgCLfy5BKdUo/Mf8wmueO3cWG6FKKSwsZlvWa7qohDY3t+/t7OwM",
        "nf/yAnf6Am5XlmXh++nvdCBwYJUB/MPMtWg08kHCASAaPQRmrgG0xsx02+Mx90aTrR8Q71upVBIej7nHTLeYGbNer/dxMplQsdhQ",
        "18OPxuMYTiSUz+d7SCSzbNv2BrNxybKs8qmpkxKPx7oaPjX1qViWvwzQTCgUKtRbMyoUileUqk06jhPMrfzVaM0cx+ko1LIsRKOH",
        "kEolMZxIKMvyl03TnBsctM82WrM6xNZW8YyInKtWq/3daE59Pt9DgGbCYfunl5rTZhWLxY+0RkZrOQ7gMBGCnQCIoAxgjZluEsls",
        "KBQqNK//D1i8VUl28TLOAAAAAElFTkSuQmCC",
    );

    /// The badge for 1 a picture, 2 music, 3 playing, 4 paused, at Windows' small icon size
    pub(super) fn icon(which: u8) -> Option<isize> {
        let size = unsafe { super::GetSystemMetrics(49 /* SM_CXSMICON */) };
        let pick = |set: [&'static str; 4]| match size {
            ..=17 => set[0],
            18..=21 => set[1],
            22..=27 => set[2],
            _ => set[3],
        };
        let png = match which {
            1 => pick([PICTURE_16, PICTURE_20, PICTURE_24, PICTURE_32]),
            2 => pick([MUSIC_16, MUSIC_20, MUSIC_24, MUSIC_32]),
            3 => pick([PLAY_16, PLAY_20, PLAY_24, PLAY_32]),
            _ => pick([PAUSE_16, PAUSE_20, PAUSE_24, PAUSE_32]),
        };
        super::make_icon_png(png)
    }
}

/// An icon from a PNG written out as text
fn make_icon_png(png_base64: &str) -> Option<isize> {
    let png = base64_decode(png_base64)?;
    let img = eframe::icon_data::from_png_bytes(&png).ok()?;
    let (w, h) = (img.width as usize, img.height as usize);
    let mut bgra = vec![0_u8; w * h * 4];
    for (i, px) in img.rgba.chunks_exact(4).enumerate() {
        bgra[i * 4..i * 4 + 4].copy_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    unsafe {
        let color = CreateBitmap(w as i32, h as i32, 1, 32, bgra.as_ptr() as *const c_void);
        let mask_bits = vec![0_u8; w.div_ceil(16) * 2 * h];
        let mask = CreateBitmap(w as i32, h as i32, 1, 1, mask_bits.as_ptr() as *const c_void);
        let info = IconInfo { is_icon: 1, x_hotspot: 0, y_hotspot: 0, mask, color };
        let icon = CreateIconIndirect(&info);
        DeleteObject(color);
        DeleteObject(mask);
        (icon != 0).then_some(icon)
    }
}
