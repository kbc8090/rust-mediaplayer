//! Settings (settings.txt), the remembered window position, and where the app keeps its files.

use super::*;

// ── Remembered window position / size ─────────────────────────────────────────
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct WinState {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) w: f32,
    pub(crate) h: f32,
    /// pixels_per_point (display scaling) when it was saved
    pub(crate) ppp: f32,
    pub(crate) maximized: bool,
}

impl WinState {
    pub(crate) const DEFAULT: WinState = WinState { x: 0.0, y: 0.0, w: 960.0, h: 540.0, ppp: 1.0, maximized: false };
}

/// Picture & sound settings (one "name=value" per line)
pub(crate) fn adjust_path() -> PathBuf {
    settings_path().map_or_else(|| PathBuf::from("adjust.txt"), |p| p.with_file_name("adjust.txt"))
}

/// Where the picture shader files go (see `apply_shader`)
pub(crate) fn shader_dir() -> Option<PathBuf> {
    settings_path().and_then(|p| p.parent().map(|d| d.join("shaders")))
}

pub(crate) fn settings_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("rust-mediaplayer").join("settings.txt"))
}

/// What's remembered between runs (besides window position)
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Settings {
    pub(crate) volume: i32,
    pub(crate) muted: bool,
    pub(crate) repeat: bool,
    pub(crate) on_top: bool,
    /// Decoding method, an index into HW_MODES (H). 0 = software, the default like
    /// mpv itself.
    pub(crate) hwdec: usize,
    /// Play the next video in the folder when one ends (menu). Off by default.
    pub(crate) autoplay: bool,
    /// Order of previous / next in a folder (menu)
    pub(crate) sort_by: SortBy,
    pub(crate) sort_desc: bool,
    /// Window edges snap to the screen edges (menu). On by default.
    pub(crate) snap: bool,
    /// Opening a file starts another player window instead of reusing this one
    /// (menu). Off by default.
    pub(crate) multi: bool,
    /// Even out loud and quiet parts (menu). Off by default.
    pub(crate) normalize: bool,
    /// Accent-colour window border (menu). Off by default.
    pub(crate) border: bool,
    /// Sharper upscaling, mpv's "high-quality" scaler (menu). Off by default.
    pub(crate) hq: bool,
    /// Frosted-glass menus, panels and tooltips (menu). On by default.
    pub(crate) glass: bool,
    /// A right-click on the video opens the main menu there (menu). On by default.
    pub(crate) right_click_menu: bool,
    /// A playlist starts again when it ends, and shows its "Up next" card
    /// (playlist panel). Both off by default.
    pub(crate) playlist_replay: bool,
    pub(crate) playlist_summary: bool,
    /// Online video panel (formats asked of yt-dlp)
    pub(crate) online: Online,
}

/// Order of the files that previous / next step through, like Explorer's "Sort by"
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum SortBy {
    Name,
    Modified,
    Created,
    Size,
}

impl SortBy {
    /// The name used in settings.txt
    pub(crate) fn key(self) -> &'static str {
        match self {
            SortBy::Name => "name",
            SortBy::Modified => "modified",
            SortBy::Created => "created",
            SortBy::Size => "size",
        }
    }

    pub(crate) fn from_key(k: &str) -> Self {
        match k {
            "modified" => SortBy::Modified,
            "created" => SortBy::Created,
            "size" => SortBy::Size,
            _ => SortBy::Name,
        }
    }

    /// The number older settings files used
    pub(crate) fn from_code(c: u8) -> Self {
        match c {
            1 => SortBy::Modified,
            2 => SortBy::Created,
            3 => SortBy::Size,
            _ => SortBy::Name,
        }
    }
}

pub(crate) fn save_settings(st: &Settings) {
    let Some(path) = settings_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, settings_text(st));
}

/// settings.txt: one `name=value` per line, so settings can be added or removed
/// later without shifting the others
pub(crate) fn settings_text(st: &Settings) -> String {
    let yn = |b: bool| if b { "yes" } else { "no" };
    [
        format!("volume={}", st.volume),
        format!("muted={}", yn(st.muted)),
        format!("repeat={}", yn(st.repeat)),
        format!("always_on_top={}", yn(st.on_top)),
        format!("decoding={}", HW_MODES[st.hwdec].0),
        format!("autoplay={}", yn(st.autoplay)),
        format!("sort_by={}", st.sort_by.key()),
        format!("sort_order={}", if st.sort_desc { "descending" } else { "ascending" }),
        format!("snap={}", yn(st.snap)),
        format!("multiple_windows={}", yn(st.multi)),
        format!("normalize={}", yn(st.normalize)),
        format!("accent_border={}", yn(st.border)),
        format!("hq_upscaling={}", yn(st.hq)),
        format!("frosted_glass={}", yn(st.glass)),
        format!("right_click_menu={}", yn(st.right_click_menu)),
        format!("playlist_replay={}", yn(st.playlist_replay)),
        format!("playlist_up_next={}", yn(st.playlist_summary)),
    ]
    .into_iter()
    .chain(st.online.settings_lines())
    .collect::<Vec<_>>()
    .join("\n")
        + "\n"
}

/// Settings from last time; 80 % volume, snapping and frosted glass on, everything
/// else off on first run (and for anything missing from the file)
pub(crate) fn load_settings() -> Settings {
    let text = settings_path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    parse_settings(&text)
}

pub(crate) fn parse_settings(text: &str) -> Settings {
    if !text.trim().is_empty() && !text.contains('=') {
        return parse_old_settings(text);
    }
    let mut st = Settings {
        volume: 80,
        muted: false,
        repeat: false,
        on_top: false,
        hwdec: 0,
        autoplay: false,
        sort_by: SortBy::Name,
        sort_desc: false,
        snap: true,
        multi: false,
        normalize: false,
        border: false,
        hq: false,
        glass: true,
        right_click_menu: true,
        playlist_replay: false,
        playlist_summary: false,
        online: Online::default(),
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let (key, value) = (key.trim(), value.trim());
        let on = value == "yes";
        match key {
            "volume" => {
                if let Ok(v) = value.parse::<f64>() {
                    st.volume = v.clamp(0.0, VOL_MAX).round() as i32;
                }
            }
            "muted" => st.muted = on,
            "repeat" => st.repeat = on,
            "always_on_top" => st.on_top = on,
            "decoding" => st.hwdec = HW_MODES.iter().position(|(v, _)| *v == value).unwrap_or(0),
            "autoplay" => st.autoplay = on,
            "sort_by" => st.sort_by = SortBy::from_key(value),
            "sort_order" => st.sort_desc = value == "descending",
            "snap" => st.snap = on,
            "multiple_windows" => st.multi = on,
            "normalize" => st.normalize = on,
            "accent_border" => st.border = on,
            "hq_upscaling" => st.hq = on,
            "frosted_glass" => st.glass = on,
            "right_click_menu" => st.right_click_menu = on,
            "playlist_replay" => st.playlist_replay = on,
            "playlist_up_next" => st.playlist_summary = on,
            _ => {
                st.online.read_setting(key, value);
            }
        }
    }
    st
}

/// The settings.txt of earlier versions: numbers in a fixed order. Read once; the
/// next save writes the new format.
pub(crate) fn parse_old_settings(text: &str) -> Settings {
    let mut it = text.split_whitespace();
    let volume = it.next().and_then(|t| t.parse::<f64>().ok());
    let mut num = || it.next().and_then(|t| t.parse::<usize>().ok());
    let (muted, repeat, on_top) = (num() == Some(1), num() == Some(1), num() == Some(1));
    // The old decoding list: 0 software, 1 D3D11 copy, 2 DXVA2 copy, 3 NVIDIA
    // direct, 4 NVIDIA copy (copy methods are now all "Hardware")
    let hwdec = match num().unwrap_or(0) {
        0 => 0,
        3 => 2,
        _ => 1,
    };
    let autoplay = num() == Some(1);
    let sort_by = SortBy::from_code(it.next().and_then(|t| t.parse::<u8>().ok()).unwrap_or(0));
    let mut flag = || it.next().and_then(|t| t.parse::<u8>().ok());
    let sort_desc = flag() == Some(1);
    let snap = flag() != Some(0);
    let multi = flag() == Some(1);
    let normalize = flag() == Some(1);
    let border = flag() == Some(1);
    let hq = flag() == Some(1);
    let glass = flag() != Some(0);
    Settings {
        volume: volume.map_or(80.0, |v| v.clamp(0.0, VOL_MAX)).round() as i32,
        muted,
        repeat,
        on_top,
        hwdec,
        autoplay,
        sort_by,
        sort_desc,
        snap,
        multi,
        normalize,
        border,
        hq,
        glass,
        right_click_menu: true,
        playlist_replay: false,
        playlist_summary: false,
        online: Online::default(),
    }
}

pub(crate) fn state_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("rust-mediaplayer").join("window.txt"))
}

pub(crate) fn save_window_state(s: &WinState) {
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
pub(crate) fn load_window_state() -> Option<WinState> {
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
