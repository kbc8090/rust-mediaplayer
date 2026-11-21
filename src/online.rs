//! Online video: playing links (YouTube and the other sites yt-dlp supports).
//!
//! mpv has yt-dlp support built in: given a link, it runs yt-dlp to find the video
//! and audio streams and then plays those itself. yt-dlp.exe and deno.exe (the
//! JavaScript runtime yt-dlp needs to solve YouTube's puzzles; it finds it beside
//! itself) are shipped next to the player. This module holds the format choices
//! from the Online video panel, the Open URL box's state, following a link while it
//! loads, and keeping yt-dlp up to date.

use super::*;
use crate::mpv_link::Snapshot;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum VideoCodec {
    H264,
    Vp9,
    Av1,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum AudioCodec {
    Aac,
    Opus,
}

/// Highest quality choices, by picture height (0 = audio only)
pub(crate) const QUALITIES: [u32; 9] = [0, 360, 480, 720, 1080, 1440, 2160, 2880, 4320];

/// Preferred audio language: (code yt-dlp reports, name). "" = the video's own.
pub(crate) const LANGUAGES: [(&str, &str); 29] = [
    ("", "Default"),
    ("en", "English"),
    ("ru", "Russian"),
    ("ar", "Arabic"),
    ("cs", "Czech"),
    ("da", "Danish"),
    ("de", "German"),
    ("el", "Greek"),
    ("es", "Spanish"),
    ("fi", "Finnish"),
    ("fil", "Filipino"),
    ("fr", "French"),
    ("hi", "Hindi"),
    ("id", "Indonesian"),
    ("it", "Italian"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("ms", "Malay"),
    ("nl", "Dutch"),
    ("pl", "Polish"),
    ("pt", "Portuguese"),
    ("pt-BR", "Portuguese (Brazil)"),
    ("ro", "Romanian"),
    ("sv", "Swedish"),
    ("th", "Thai"),
    ("tr", "Turkish"),
    ("uk", "Ukrainian"),
    ("vi", "Vietnamese"),
    ("zh", "Chinese"),
];

/// Which formats to ask yt-dlp for (the Online video panel)
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Online {
    pub(crate) codec: VideoCodec,
    /// One of QUALITIES
    pub(crate) max_height: u32,
    /// 50 / 60 fps when offered
    pub(crate) hfr: bool,
    pub(crate) hdr: bool,
    pub(crate) audio: AudioCodec,
    /// A code from LANGUAGES ("" = the video's own)
    pub(crate) language: &'static str,
}

impl Default for Online {
    /// VP9 (decoded in hardware by most graphics cards, unlike AV1 on older ones),
    /// up to 1080p, 50/60 fps, no HDR (most monitors are SDR), Opus audio
    fn default() -> Self {
        Self { codec: VideoCodec::Vp9, max_height: 1080, hfr: true, hdr: false, audio: AudioCodec::Opus, language: "" }
    }
}

impl Online {
    /// yt-dlp's format selector (`-f`): the best video plus the best audio (in the
    /// chosen language when there's a choice), or audio alone; a single combined
    /// file if the site has no separate streams
    pub(crate) fn format(&self) -> String {
        let lang = self.language;
        let video = if self.max_height == 0 { "" } else { "bv*+" };
        let mut alts = Vec::new();
        if !lang.is_empty() {
            // Exactly the language, or a regional variant of it ("en-US"); a plain
            // prefix match would let Finnish ("fi") pick Filipino ("fil")
            alts.push(format!("{video}ba[language={lang}]"));
            alts.push(format!("{video}ba[language^={lang}-]"));
        }
        alts.push(format!("{video}ba"));
        alts.push("b".to_owned());
        alts.join("/")
    }

    /// yt-dlp's format sorting (`-S`): preferences in order, each falling back to
    /// the next best thing when a video doesn't have it. `res:1080` = the largest
    /// picture up to 1080p; `vcodec:vp9` = VP9, else the codecs ranked below it
    /// (H.264), and only then the ones above it (AV1).
    pub(crate) fn sort(&self) -> String {
        let mut s = Vec::new();
        if self.max_height > 0 {
            s.push(format!("res:{}", self.max_height));
        }
        s.push(if self.hfr { "fps" } else { "fps:30" }.to_owned());
        s.push(if self.hdr { "hdr:12" } else { "hdr:sdr" }.to_owned());
        s.push(
            match self.codec {
                VideoCodec::H264 => "vcodec:h264",
                VideoCodec::Vp9 => "vcodec:vp9",
                VideoCodec::Av1 => "vcodec:av01",
            }
            .to_owned(),
        );
        s.push(
            match self.audio {
                AudioCodec::Aac => "acodec:aac",
                AudioCodec::Opus => "acodec:opus",
            }
            .to_owned(),
        );
        s.join(",")
    }

    /// "1080p", "Audio only"
    pub(crate) fn quality_label(&self) -> String {
        if self.max_height == 0 {
            "Audio only".to_owned()
        } else {
            format!("{}p", self.max_height)
        }
    }

    /// One step up (+1) or down (-1) the QUALITIES list
    pub(crate) fn step_quality(&mut self, dir: i32) {
        let i = QUALITIES.iter().position(|q| *q == self.max_height).unwrap_or(4) as i32;
        self.max_height = QUALITIES[(i + dir).clamp(0, QUALITIES.len() as i32 - 1) as usize];
    }

    pub(crate) fn language_name(&self) -> &'static str {
        LANGUAGES.iter().find(|(c, _)| *c == self.language).map_or("Default", |(_, n)| n)
    }

    // ── settings.txt ──────────────────────────────────────────────────────────
    pub(crate) fn codec_key(&self) -> &'static str {
        match self.codec {
            VideoCodec::H264 => "h264",
            VideoCodec::Vp9 => "vp9",
            VideoCodec::Av1 => "av1",
        }
    }

    pub(crate) fn audio_key(&self) -> &'static str {
        match self.audio {
            AudioCodec::Aac => "aac",
            AudioCodec::Opus => "opus",
        }
    }

    /// Apply one `name=value` line of settings.txt; false if it isn't ours
    pub(crate) fn read_setting(&mut self, key: &str, value: &str) -> bool {
        match key {
            "online_codec" => {
                self.codec = match value {
                    "h264" => VideoCodec::H264,
                    "av1" => VideoCodec::Av1,
                    _ => VideoCodec::Vp9,
                }
            }
            "online_max_quality" => {
                let h = if value == "audio" { 0 } else { value.parse().unwrap_or(1080) };
                self.max_height = if QUALITIES.contains(&h) { h } else { 1080 };
            }
            "online_hfr" => self.hfr = value == "yes",
            "online_hdr" => self.hdr = value == "yes",
            "online_audio" => self.audio = if value == "aac" { AudioCodec::Aac } else { AudioCodec::Opus },
            "online_language" => {
                self.language = LANGUAGES.iter().find(|(c, _)| *c == value).map_or("", |(c, _)| c);
            }
            _ => return false,
        }
        true
    }

    /// Our lines for settings.txt
    pub(crate) fn settings_lines(&self) -> Vec<String> {
        let yn = |b: bool| if b { "yes" } else { "no" };
        vec![
            format!("online_codec={}", self.codec_key()),
            format!(
                "online_max_quality={}",
                if self.max_height == 0 { "audio".to_owned() } else { self.max_height.to_string() }
            ),
            format!("online_hfr={}", yn(self.hfr)),
            format!("online_hdr={}", yn(self.hdr)),
            format!("online_audio={}", self.audio_key()),
            format!("online_language={}", self.language),
        ]
    }
}

// ── Links ─────────────────────────────────────────────────────────────────────

/// A link for yt-dlp rather than a file on disk
pub(crate) fn is_url(s: &str) -> bool {
    let l = s.trim_start().to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://") || l.starts_with("ytdl://")
}

/// A pasted / typed link, with "https://" added when it was left off
/// ("www.youtube.com/watch?v=…", "youtu.be/…"); None if it isn't a link
pub(crate) fn normalize_url(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() || t.contains(char::is_whitespace) {
        return None;
    }
    if is_url(t) {
        return Some(t.to_owned());
    }
    let l = t.to_ascii_lowercase();
    let bare = ["www.", "youtube.com/", "m.youtube.com/", "youtu.be/", "music.youtube.com/"];
    bare.iter().any(|b| l.starts_with(b)).then(|| format!("https://{t}"))
}

/// A link without "https://" and "www.", shown until the video's title is known
pub(crate) fn short_url(s: &str) -> String {
    let t = s.trim();
    let t = t.split_once("://").map_or(t, |(_, rest)| rest);
    t.strip_prefix("www.").unwrap_or(t).to_owned()
}

/// yt-dlp.exe beside the player, if it's there
pub(crate) fn ytdlp_path() -> Option<PathBuf> {
    let p = std::env::current_exe().ok()?.parent()?.join("yt-dlp.exe");
    p.is_file().then_some(p)
}

/// YouTube changes often and older yt-dlp versions stop working, so once a day,
/// after a link has opened, yt-dlp updates itself (`yt-dlp -U`, quietly, in the
/// background). It can only do that if the player's folder can be written to.
pub(crate) fn update_ytdlp_if_due() {
    let Some(exe) = ytdlp_path() else { return };
    let Some(stamp) = settings_path().and_then(|p| p.parent().map(|d| d.join("yt-dlp-checked.txt"))) else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let last = std::fs::read_to_string(&stamp).ok().and_then(|t| t.trim().parse::<u64>().ok()).unwrap_or(0);
    if now.saturating_sub(last) < 24 * 60 * 60 {
        return;
    }
    if let Some(dir) = stamp.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&stamp, now.to_string());
    std::thread::spawn(move || {
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("-U")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashing up
        }
        let _ = cmd.status();
    });
}

// ── The Open URL box and a link being opened ─────────────────────────────────

/// The Open URL box while it's open
pub(crate) struct UrlBox {
    pub(crate) text: String,
    /// Give the text field the keyboard (when the box has just opened)
    pub(crate) focus: bool,
}

/// The Open URL box's text field (it keeps the keyboard; nothing else does)
pub(crate) const URL_FIELD: &str = "url_field";

/// A link being opened: since when, and whether mpv has started on it
pub(crate) struct UrlLoad {
    pub(crate) since: std::time::Instant,
    pub(crate) seen: bool,
}

impl FluentMediaPlayer {
    /// Open the Open URL box, filled in with the clipboard if it holds a link
    pub(crate) fn show_url_box(&mut self) {
        let text = taskbar::clipboard_text().and_then(|t| normalize_url(&t)).unwrap_or_default();
        self.url_box = Some(UrlBox { text, focus: true });
        self.main_menu = false;
    }

    /// Play a link: no previous / next (there's no folder) and no seek-bar
    /// thumbnails (they'd need yt-dlp run a second time)
    pub(crate) fn open_url(&mut self, url: &str) {
        self.url_box = None;
        self.playlist = None;
        self.folder_files = Vec::new();
        self.folder_index = None;
        self.scan_gen += 1; // a folder scan still running is no longer wanted
        self.media_title.clear();
        self.link_error = None;
        self.apply_online();
        self.url_load = Some(UrlLoad { since: std::time::Instant::now(), seen: false });
        self.load_entry(url);
    }

    /// Hand the Online video choices to mpv's yt-dlp support (used for the next
    /// link opened)
    pub(crate) fn apply_online(&self) {
        self.link.set("ytdl-format", self.online.format());
        // (change-list "append" takes the value as it is: the sort string has commas,
        // which a plain set would split into separate options)
        self.link.command("change-list", &["ytdl-raw-options", "clr", ""]);
        self.link.command("change-list", &["ytdl-raw-options", "append", &format!("format-sort={}", self.online.sort())]);
    }

    /// Follow a link being opened: done once mpv is playing it, failed if mpv gave
    /// up on it (yt-dlp couldn't find a video there, no internet, ...)
    pub(crate) fn track_url_load(&mut self, snap: &Snapshot) {
        let Some(load) = self.url_load.as_mut() else { return };
        // (the first moment can still show the previous file: the same link
        // opened again would look loaded already)
        if load.since.elapsed() < std::time::Duration::from_millis(150) {
            return;
        }
        if snap.path == self.current_file {
            load.seen = true;
            if snap.duration > 0.0 || snap.dwidth > 0 || snap.time_pos > 0.0 {
                self.url_load = None;
                update_ytdlp_if_due();
                return;
            }
        }
        if load.seen && snap.idle {
            self.url_load = None;
            self.has_file = false;
            let why = if ytdlp_path().is_none() {
                "yt-dlp.exe isn't in the player's folder"
            } else {
                "Couldn't open this link"
            };
            self.link_error = Some((std::time::Instant::now(), why));
        }
    }
}

/// While a link opens: a spinner and "Opening link…" in the middle; if it failed,
/// a short message there for a few seconds
pub(crate) fn draw_link_status(app: &FluentMediaPlayer, ctx: &egui::Context) {
    let screen = ctx.screen_rect();
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("link_status")));
    let c = screen.center();
    let text_col = Color32::from_rgb(222, 222, 222);
    if let Some(load) = &app.url_load {
        let t = load.since.elapsed().as_secs_f32();
        // A ring with a turning accent arc
        let r = 15.0;
        p.circle_stroke(c - vec2(0.0, 14.0), r, Stroke::new(3.0_f32, Color32::from_rgba_premultiplied(40, 40, 40, 40)));
        let start = t * 5.0;
        let pts: Vec<Pos2> = (0..=24)
            .map(|i| {
                let a = start + i as f32 / 24.0 * 1.6;
                c - vec2(0.0, 14.0) + r * vec2(a.cos(), a.sin())
            })
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(3.0_f32, app.accent)));
        p.text(c + vec2(0.0, 22.0), Align2::CENTER_CENTER, "Opening link\u{2026}", FontId::proportional(14.0), text_col);
        ctx.request_repaint();
    } else if let Some((t0, why)) = app.link_error {
        let age = t0.elapsed().as_secs_f32();
        const SHOW: f32 = 5.0;
        if age < SHOW {
            // A small dark note, below the middle (clear of the home screen's ribbon)
            let a = ((SHOW - age) / 0.4).min(1.0);
            let g = p.layout_no_wrap(why.to_owned(), FontId::proportional(14.0), text_col.gamma_multiply(a));
            let pos = pos2(c.x, screen.top() + screen.height() * 0.74);
            let r = Rect::from_center_size(pos, g.size() + vec2(28.0, 16.0));
            p.rect_filled(r, r.height() / 2.0, Color32::from_rgb(36, 36, 36).gamma_multiply(0.92 * a));
            p.galley(r.center() - g.size() / 2.0, g, text_col.gamma_multiply(a));
            ctx.request_repaint();
        }
    }
}

// ── Saving a link to a file (Save As…) ────────────────────────────────────────
// yt-dlp downloads the same video and audio that are playing (the Online video
// settings), and ffmpeg (shipped beside it) joins them into one file in the
// folder picked: .mp4 for H.264 / AV1 with AAC, .webm for VP9 / AV1 with Opus,
// .mkv for any other mix. "Audio only" saves just the sound (.opus / .m4a).

/// What's being downloaded
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SavePart {
    Video,
    Audio,
    /// One file with both
    Both,
}

/// Where a save has got to
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SaveStage {
    /// yt-dlp is looking the video up
    Starting,
    /// Downloading, and how far that part has got (0–1, when known)
    Downloading(SavePart, Option<f32>),
    /// Downloaded: ffmpeg is joining the video and audio (true), or tidying up
    Finishing(bool),
    /// Saved, at this path
    Done(String),
    /// Didn't work: why
    Failed(String),
}

/// A save in progress, or just finished (the card in the top right shows it).
/// Dropping one that's still running cancels it.
pub(crate) struct SaveJob {
    pub(crate) title: String,
    stage: Arc<std::sync::Mutex<SaveStage>>,
    /// When it finished (the card stays a few seconds after that)
    pub(crate) ended: Option<std::time::Instant>,
    /// yt-dlp and what it starts, ended together on cancel
    group: Option<taskbar::ProcessGroup>,
    pid: u32,
}

impl SaveJob {
    pub(crate) fn stage(&self) -> SaveStage {
        self.stage.lock().map(|s| s.clone()).unwrap_or(SaveStage::Starting)
    }

    pub(crate) fn running(&self) -> bool {
        !matches!(self.stage(), SaveStage::Done(_) | SaveStage::Failed(_))
    }

    fn failed(title: String, why: &str) -> Self {
        SaveJob {
            title,
            stage: Arc::new(std::sync::Mutex::new(SaveStage::Failed(why.to_owned()))),
            ended: None,
            group: None,
            pid: 0,
        }
    }

    /// Start yt-dlp saving `url` into `folder`, with the formats from `o`
    fn start(ctx: &egui::Context, url: &str, title: String, folder: &str, o: Online) -> Self {
        let Some(exe) = ytdlp_path() else {
            return Self::failed(title, "yt-dlp.exe isn't in the player's folder");
        };
        let tools = exe.parent().map(Path::to_path_buf).unwrap_or_default();
        if !tools.join("ffmpeg.exe").is_file() {
            return Self::failed(title, "ffmpeg.exe isn't in the player's folder");
        }
        let audio_only = o.max_height == 0;
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--no-playlist", "--newline", "--no-mtime", "--encoding", "utf-8"])
            .args(["-f", &o.format(), "-S", &o.sort()])
            .args(["--merge-output-format", "mp4/webm/mkv"])
            .arg("-P")
            .arg(folder)
            // (audio gets its own name: as "Title.webm" it would be taken for an
            // earlier save of the video, and that file used and deleted)
            .args(["-o", if audio_only { "%(title).150B (audio).%(ext)s" } else { "%(title).150B.%(ext)s" }])
            .arg("--ffmpeg-location")
            .arg(&tools)
            // Progress, the file's name before it downloads (to tidy up after a
            // cancel), and where it ended up; one line each, easy to read back
            .args(["--progress", "--progress-template", PROGRESS_TEMPLATE])
            .args(["--print", "before_dl:RMP-FILE %(filename)s"])
            .args(["--print", "after_move:RMP-SAVED %(filepath)s", "--no-simulate"]);
        if audio_only {
            cmd.arg("-x"); // the sound on its own: .opus or .m4a rather than .webm
        }
        cmd.arg("--")
            .arg(url)
            .env("PYTHONIOENCODING", "utf-8")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let Ok(mut child) = cmd.spawn() else {
            return Self::failed(title, "Couldn't start yt-dlp.exe");
        };
        #[cfg(windows)]
        let group = {
            use std::os::windows::io::AsRawHandle;
            taskbar::ProcessGroup::new(child.as_raw_handle() as isize)
        };
        #[cfg(not(windows))]
        let group = None;

        let stage = Arc::new(std::sync::Mutex::new(SaveStage::Starting));
        let (st, ctx) = (stage.clone(), ctx.clone());
        let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
        let pid = child.id();
        std::thread::spawn(move || {
            use std::io::BufRead;
            let errors = stderr.map(|e| std::thread::spawn(move || last_error(e)));
            let set = |s: SaveStage| {
                if let Ok(mut slot) = st.lock() {
                    *slot = s;
                }
                ctx.request_repaint();
            };
            let (mut file, mut saved) = (None, None);
            let (mut had_video, mut had_audio) = (false, false);
            if let Some(out) = stdout {
                for line in std::io::BufReader::new(out).split(b'\n') {
                    let Ok(line) = line else { break };
                    let line = String::from_utf8_lossy(&line);
                    let line = line.trim_end_matches('\r');
                    if let Some(rest) = line.strip_prefix("RMP-PROGRESS ") {
                        if let Some(s) = read_progress(rest, &mut had_video, &mut had_audio) {
                            set(s);
                        }
                    } else if let Some(p) = line.strip_prefix("RMP-FILE ") {
                        file = Some(p.to_owned());
                    } else if let Some(p) = line.strip_prefix("RMP-SAVED ") {
                        saved = Some(p.to_owned());
                    }
                }
            }
            let ok = child.wait().is_ok_and(|s| s.success());
            let error = errors.and_then(|t| t.join().ok()).flatten();
            match saved {
                Some(path) if ok => set(SaveStage::Done(path)),
                _ => {
                    if let Some(f) = &file {
                        remove_leftovers(f);
                    }
                    set(SaveStage::Failed(error.unwrap_or_else(|| "yt-dlp stopped before the video was saved".to_owned())));
                }
            }
        });
        SaveJob { title, stage, ended: None, group, pid }
    }
}

impl Drop for SaveJob {
    /// Closing the card (or the player) while it's saving cancels it
    fn drop(&mut self) {
        if !self.running() {
            return;
        }
        match &self.group {
            Some(g) => g.end(),
            None => {
                // (no group: end yt-dlp and what it started by process id)
                let mut cmd = std::process::Command::new("taskkill");
                cmd.args(["/PID", &self.pid.to_string(), "/T", "/F"])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    cmd.creation_flags(0x0800_0000);
                }
                let _ = cmd.status();
            }
        }
    }
}

/// yt-dlp's progress lines: video codec | audio codec | status | bytes so far |
/// total bytes | estimated total (the codecs say whether it's the video or audio)
const PROGRESS_TEMPLATE: &str = "download:RMP-PROGRESS %(info.vcodec)s|%(info.acodec)s|%(progress.status)s|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s";

fn read_progress(line: &str, had_video: &mut bool, had_audio: &mut bool) -> Option<SaveStage> {
    let f: Vec<&str> = line.split('|').collect();
    if f.len() < 6 {
        return None;
    }
    let has = |c: &str| c != "none" && c != "NA" && !c.is_empty();
    let part = match (has(f[0]), has(f[1])) {
        (true, false) => SavePart::Video,
        (false, true) => SavePart::Audio,
        _ => SavePart::Both,
    };
    match part {
        SavePart::Video => *had_video = true,
        SavePart::Audio => *had_audio = true,
        SavePart::Both => {}
    }
    if f[2] == "finished" {
        // (the video comes first: when it's done, the audio is next, not the end)
        if part == SavePart::Video && !*had_audio {
            return Some(SaveStage::Downloading(part, Some(1.0)));
        }
        return Some(SaveStage::Finishing(*had_video && *had_audio));
    }
    let num = |s: &str| s.parse::<f64>().ok().filter(|n| *n > 0.0);
    let done = num(f[3]).unwrap_or(0.0);
    let frac = num(f[4]).or_else(|| num(f[5])).map(|total| (done / total).clamp(0.0, 1.0) as f32);
    Some(SaveStage::Downloading(part, frac))
}

/// The last error yt-dlp wrote, shortened for the card: "ERROR: [youtube]
/// abc123: Video unavailable" becomes "Video unavailable"
fn last_error(stderr: std::process::ChildStderr) -> Option<String> {
    use std::io::BufRead;
    let mut last = None;
    for line in std::io::BufReader::new(stderr).split(b'\n').map_while(Result::ok) {
        let line = String::from_utf8_lossy(&line).trim().to_owned();
        let Some(mut msg) = line.strip_prefix("ERROR:").map(str::trim) else { continue };
        if msg.starts_with('[') {
            if let Some(i) = msg.find("] ") {
                msg = &msg[i + 2..];
            }
        }
        // a video id followed by a colon
        if let Some((first, rest)) = msg.split_once(": ") {
            if !first.contains(' ') && first.len() <= 20 {
                msg = rest;
            }
        }
        last = Some(msg.chars().take(160).collect());
    }
    last
}

/// After a cancel or failure: delete the half-downloaded pieces of `file`
/// ("Title.f303.webm", "Title.f303.webm.part", "Title.temp.webm", …), and only those
fn remove_leftovers(file: &str) {
    let path = Path::new(file);
    let (Some(dir), Some(stem)) = (path.parent(), path.file_stem().map(|s| s.to_string_lossy().into_owned())) else {
        return;
    };
    let leftover = |name: &str| -> bool {
        let Some(rest) = name.strip_prefix(stem.as_str()) else { return false };
        if !rest.starts_with('.') {
            return false;
        }
        if rest.ends_with(".part") || rest.ends_with(".ytdl") || rest.contains(".part-Frag") || rest.starts_with(".temp.") {
            return true;
        }
        // ".f<format id>.<ext>": one downloaded part, never joined
        rest.strip_prefix(".f")
            .and_then(|r| r.split_once('.'))
            .is_some_and(|(id, ext)| !id.is_empty() && id.bytes().any(|b| b.is_ascii_digit()) && !id.contains(' ') && !ext.contains('.'))
    };
    // (the files can stay locked for a moment after yt-dlp has been ended)
    for _ in 0..5 {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut left = false;
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if leftover(&name) && std::fs::remove_file(e.path()).is_err() {
                left = true;
            }
        }
        if !left {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
}

/// Open the file's folder in Explorer with the file selected
pub(crate) fn show_in_folder(path: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // (explorer needs the path quoted after the comma, exactly like this)
        let _ = std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{path}\"")).spawn();
    }
    #[cfg(not(windows))]
    let _ = path;
}

impl FluentMediaPlayer {
    /// Save As…: ask for a folder (the save starts once one is picked)
    pub(crate) fn ask_save_folder(&mut self) {
        if !self.has_file || !is_url(&self.current_file) {
            return;
        }
        self.save_pending = Some((self.current_file.clone(), self.file_name()));
        taskbar::folder_dialog(self.hwnd);
    }

    /// A folder was picked for Save As…
    pub(crate) fn start_save(&mut self, ctx: &egui::Context, folder: &str) {
        let Some((url, title)) = self.save_pending.take() else { return };
        // (one at a time: a save still running is replaced, which cancels it)
        self.save = Some(SaveJob::start(ctx, &url, title, folder, self.online));
    }
}
