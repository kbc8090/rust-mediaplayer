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

// ── Updating yt-dlp (only when a link fails) ──────────────────────────────────
// YouTube changes how its pages work every few weeks, and an older yt-dlp then
// stops working. Nothing is checked in the background: only when a link fails to
// open (or save) does yt-dlp update itself (`yt-dlp -U`, from GitHub), and the
// link is tried once more with the new version.

/// What `yt-dlp -U` did
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum UpdateOutcome {
    Updated,
    /// Already the newest version: the problem is the link, not yt-dlp
    UpToDate,
    /// Couldn't update (offline, the folder can't be written to, ...)
    Failed,
}

/// Run `yt-dlp -U` and wait for it (call on a thread of its own)
pub(crate) fn update_ytdlp() -> UpdateOutcome {
    let Some(exe) = ytdlp_path() else { return UpdateOutcome::Failed };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("-U")
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashing up
    }
    let Ok(out) = cmd.output() else { return UpdateOutcome::Failed };
    let text = String::from_utf8_lossy(&out.stdout);
    if text.contains("Updated yt-dlp to") {
        UpdateOutcome::Updated
    } else if text.contains("is up to date") {
        UpdateOutcome::UpToDate
    } else {
        UpdateOutcome::Failed
    }
}

/// A link that failed being retried: yt-dlp updating, then the link again
pub(crate) struct LinkRetry {
    url: String,
    since: std::time::Instant,
    /// Why it failed the first time (yt-dlp's error), for the message
    reason: Option<String>,
    outcome: Arc<std::sync::Mutex<Option<UpdateOutcome>>>,
}

// ── What yt-dlp found (or why it failed) ──────────────────────────────────────

/// Where mpv keeps yt-dlp's answer for the last link it opened (its exit status,
/// its JSON about the video, and its error messages)
pub(crate) const YTDL_RESULT: &str = "user-data/mpv/ytdl/json-subprocess-result";

/// A link's details from yt-dlp
#[derive(Debug, Default)]
pub(crate) struct LinkInfo {
    /// The link they belong to
    pub(crate) url: String,
    /// Why yt-dlp couldn't open it (None: it could)
    pub(crate) error: Option<String>,
    /// Extra lines for the info panel (Tab)
    pub(crate) rows: Vec<(&'static str, String)>,
    /// A small video-only stream of it, for the seek-bar thumbnails
    pub(crate) preview: Option<PreviewStream>,
}

/// A low-resolution video stream the thumbnail player can open directly
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreviewStream {
    pub(crate) url: String,
    pub(crate) user_agent: Option<String>,
    /// Other HTTP headers the site wants ("Name: value")
    pub(crate) headers: Vec<String>,
}

/// Read mpv's copy of yt-dlp's answer (None if there isn't one)
pub(crate) fn parse_link_result(text: &str) -> Option<LinkInfo> {
    use serde_json::Value;
    let outer: Value = serde_json::from_str(text).ok()?;
    let stdout = outer["stdout"].as_str().unwrap_or("");
    if outer["status"].as_i64() != Some(0) || stdout.trim().is_empty() {
        let stderr = outer["stderr"].as_str().unwrap_or("");
        let error = error_from_lines(stderr.lines()).unwrap_or_else(|| match outer["error_string"].as_str() {
            Some("init") => "yt-dlp.exe couldn't be started".to_owned(),
            _ => "yt-dlp couldn't read this link".to_owned(),
        });
        return Some(LinkInfo { error: Some(error), ..Default::default() });
    }
    let j: Value = serde_json::from_str(stdout).ok()?;
    let text = |v: &Value| v.as_str().filter(|s| !s.trim().is_empty()).map(str::to_owned);
    let mut rows = Vec::new();

    if let Some(site) = text(&j["webpage_url_domain"]).or_else(|| text(&j["extractor_key"])) {
        rows.push(("Site", site));
    }
    if let Some(who) = text(&j["channel"]).or_else(|| text(&j["uploader"])) {
        rows.push(("Channel", who));
    }
    if let Some(d) = text(&j["upload_date"]).and_then(|d| nice_date(&d)) {
        rows.push(("Uploaded", d));
    }
    if j["is_live"].as_bool() == Some(true) {
        rows.push(("Live", "Yes".to_owned()));
    }
    if let Some(v) = j["view_count"].as_u64() {
        let mut views = thousands(v);
        if let Some(l) = j["like_count"].as_u64() {
            views = format!("{views} \u{b7} {} likes", thousands(l));
        }
        rows.push(("Views", views));
    }
    // The streams yt-dlp picked (two when the video and audio come separately)
    let picked: Vec<&Value> = match j["requested_formats"].as_array() {
        Some(list) => list.iter().collect(),
        None => vec![&j],
    };
    let mut size = 0.0;
    let mut size_known = true;
    for f in &picked {
        let has = |k: &str| text(&f[k]).is_some_and(|c| c != "none");
        let id = text(&f["format_id"]).unwrap_or_default();
        if has("vcodec") {
            let mut parts = vec![id.clone()];
            if let Some(h) = f["height"].as_u64() {
                let fps = f["fps"].as_f64().unwrap_or(0.0);
                parts.push(if fps > 31.0 { format!("{h}p{}", fps.round()) } else { format!("{h}p") });
            }
            parts.push(codec_label(&text(&f["vcodec"]).unwrap_or_default()));
            if let Some(dr) = text(&f["dynamic_range"]).filter(|d| d != "SDR") {
                parts.push(dr);
            }
            if let Some(br) = f["vbr"].as_f64().or_else(|| f["tbr"].as_f64()).filter(|b| *b > 0.0) {
                parts.push(format!("{:.1} Mb/s", br / 1000.0));
            }
            rows.push(("Video stream", parts.join(" \u{b7} ")));
        }
        if has("acodec") {
            let mut parts = vec![id, codec_label(&text(&f["acodec"]).unwrap_or_default())];
            if let Some(br) = f["abr"].as_f64().filter(|b| *b > 0.0) {
                parts.push(format!("{} kb/s", br.round()));
            }
            if let Some(lang) = text(&f["language"]) {
                parts.push(language_label(&lang));
            }
            rows.push(("Audio stream", parts.join(" \u{b7} ")));
        }
        match f["filesize"].as_f64().or_else(|| f["filesize_approx"].as_f64()) {
            Some(b) => size += b,
            None => size_known = false,
        }
    }
    if size_known && size > 0.0 {
        let mb = size / 1_000_000.0;
        let text = if mb >= 1000.0 { format!("{:.2} GB", mb / 1000.0) } else { format!("{mb:.0} MB") };
        rows.push(("Size to save", format!("about {text}")));
    }

    // Thumbnails: a small video-only stream that can be opened straight (not a
    // playlist of pieces), nearest 240p; H.264 first (cheapest to decode)
    let preview = if j["is_live"].as_bool() == Some(true) {
        None
    } else {
        j["formats"].as_array().and_then(|formats| {
            formats
                .iter()
                .filter(|f| {
                    let v = text(&f["vcodec"]).unwrap_or_default();
                    let a = text(&f["acodec"]).unwrap_or_else(|| "none".to_owned());
                    let h = f["height"].as_u64().unwrap_or(0);
                    !v.is_empty() && v != "none" && a == "none" && (120..=480).contains(&h)
                        && text(&f["protocol"]).is_some_and(|p| p == "https" || p == "http")
                        && text(&f["url"]).is_some()
                })
                .min_by_key(|f| {
                    let h = f["height"].as_u64().unwrap_or(0) as i64;
                    let v = text(&f["vcodec"]).unwrap_or_default();
                    let rank = if v.starts_with("avc") { 0 } else if v.starts_with("vp") { 1 } else { 2 };
                    ((h - 240).abs(), rank)
                })
                .map(|f| {
                    let mut user_agent = None;
                    let mut headers = Vec::new();
                    if let Some(map) = f["http_headers"].as_object() {
                        for (k, v) in map {
                            let Some(v) = v.as_str() else { continue };
                            if k.eq_ignore_ascii_case("user-agent") {
                                user_agent = Some(v.to_owned());
                            } else {
                                headers.push(format!("{k}: {v}"));
                            }
                        }
                    }
                    PreviewStream { url: text(&f["url"]).unwrap_or_default(), user_agent, headers }
                })
        })
    };
    Some(LinkInfo { url: String::new(), error: None, rows, preview })
}

/// "20240312" → "12 Mar 2024"
fn nice_date(d: &str) -> Option<String> {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    if d.len() != 8 || !d.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let month: usize = d[4..6].parse().ok()?;
    let day: u32 = d[6..8].parse().ok()?;
    Some(format!("{day} {} {}", MONTHS.get(month.checked_sub(1)?)?, &d[..4]))
}

/// 1234567 → "1,234,567"
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// yt-dlp's codec names in plain words ("avc1.640028" → "H.264")
fn codec_label(c: &str) -> String {
    let c = c.to_ascii_lowercase();
    let name = if c.starts_with("avc") || c.starts_with("h264") {
        "H.264"
    } else if c.starts_with("hev") || c.starts_with("hvc") || c.starts_with("h265") {
        "HEVC"
    } else if c.starts_with("vp09") || c.starts_with("vp9") {
        "VP9"
    } else if c.starts_with("av01") || c == "av1" {
        "AV1"
    } else if c.starts_with("mp4a") || c == "aac" {
        "AAC"
    } else if c.starts_with("opus") {
        "Opus"
    } else if c.starts_with("ec-3") || c.starts_with("eac3") {
        "E-AC-3"
    } else if c.starts_with("ac-3") || c == "ac3" {
        "AC-3"
    } else {
        return c;
    };
    name.to_owned()
}

/// "de" / "en-US" → "German" / "English (en-US)"
fn language_label(code: &str) -> String {
    if let Some((_, name)) = LANGUAGES.iter().find(|(c, _)| !c.is_empty() && c.eq_ignore_ascii_case(code)) {
        return (*name).to_owned();
    }
    let base = code.split('-').next().unwrap_or(code);
    match LANGUAGES.iter().find(|(c, _)| !c.is_empty() && c.eq_ignore_ascii_case(base)) {
        Some((_, name)) => format!("{name} ({code})"),
        None => code.to_owned(),
    }
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
    /// When mpv gave up on it (the message waits a moment for yt-dlp's reason)
    pub(crate) failed_at: Option<std::time::Instant>,
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
        self.link_retry = None;
        // (mpv's copy of yt-dlp's answer is for the previous link: clear it, so
        // whatever is there after this one has loaded or failed is about this one)
        self.link.command("del", &[YTDL_RESULT]);
        self.url_load = Some(UrlLoad { since: std::time::Instant::now(), seen: false, failed_at: None });
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
    pub(crate) fn track_url_load(&mut self, ctx: &egui::Context, snap: &Snapshot) {
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
                return;
            }
        }
        if !(load.seen && snap.idle) {
            return;
        }
        // Failed. Wait a moment for yt-dlp's answer, to say why.
        let info = snap.link_info.as_ref().filter(|i| i.url == self.current_file);
        let failed_at = *load.failed_at.get_or_insert_with(std::time::Instant::now);
        if info.is_none() && failed_at.elapsed() < std::time::Duration::from_millis(1500) {
            return;
        }
        let reason = info.and_then(|i| i.error.clone());
        self.url_load = None;
        self.has_file = false;
        let url = self.current_file.clone();
        if ytdlp_path().is_none() {
            self.show_link_error("Couldn't open this link", "yt-dlp.exe isn't in the player's folder".to_owned());
        } else if self.retried_url.as_deref() != Some(url.as_str()) {
            // First failure for this link: maybe yt-dlp is out of date. Update it
            // and try again once.
            self.retried_url = Some(url.clone());
            let outcome = Arc::new(std::sync::Mutex::new(None));
            let (slot, ctx) = (outcome.clone(), ctx.clone());
            std::thread::spawn(move || {
                let o = update_ytdlp();
                if let Ok(mut s) = slot.lock() {
                    *s = Some(o);
                }
                ctx.request_repaint();
            });
            self.link_retry = Some(LinkRetry { url, since: std::time::Instant::now(), reason, outcome });
        } else {
            let why = reason.unwrap_or_else(|| "yt-dlp couldn't play it".to_owned());
            self.show_link_error("Couldn't open this link", format!("{why} (even with the newest yt-dlp)"));
        }
    }

    /// yt-dlp has finished updating after a link failed: try the link again, or
    /// say why not
    pub(crate) fn poll_link_retry(&mut self) {
        let Some(outcome) = self.link_retry.as_ref().and_then(|r| r.outcome.lock().ok().and_then(|mut o| o.take())) else {
            return;
        };
        let Some(retry) = self.link_retry.take() else { return };
        let why = retry.reason.unwrap_or_else(|| "yt-dlp couldn't play it".to_owned());
        match outcome {
            // (unless something else has been opened in the meantime)
            UpdateOutcome::Updated if self.current_file == retry.url && !self.has_file => self.open_url(&retry.url),
            UpdateOutcome::Updated => {}
            UpdateOutcome::UpToDate => {
                self.show_link_error("Couldn't open this link", format!("{why} (yt-dlp is already the newest version)"));
            }
            UpdateOutcome::Failed => {
                self.show_link_error("Couldn't open this link", format!("{why} (updating yt-dlp didn't work: offline?)"));
            }
        }
    }

    fn show_link_error(&mut self, title: &'static str, detail: String) {
        self.link_error = Some((std::time::Instant::now(), title, detail));
    }
}

/// While a link opens: a spinner and "Opening link…" in the middle. While
/// yt-dlp updates after a failure: the spinner, why, and what happens next. If it
/// failed for good: a note saying why, for a few seconds.
pub(crate) fn draw_link_status(app: &FluentMediaPlayer, ctx: &egui::Context) {
    let screen = ctx.screen_rect();
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("link_status")));
    let c = screen.center();
    let text_col = Color32::from_rgb(222, 222, 222);
    let dim = Color32::from_rgb(170, 170, 170);
    let spinner = |since: std::time::Instant| {
        // A ring with a turning accent arc
        let t = since.elapsed().as_secs_f32();
        let r = 15.0;
        let at = c - vec2(0.0, 14.0);
        p.circle_stroke(at, r, Stroke::new(3.0_f32, Color32::from_rgba_premultiplied(40, 40, 40, 40)));
        let pts: Vec<Pos2> = (0..=24)
            .map(|i| {
                let a = t * 5.0 + i as f32 / 24.0 * 1.6;
                at + r * vec2(a.cos(), a.sin())
            })
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(3.0_f32, app.accent)));
        ctx.request_repaint();
    };
    // Lines of text under the spinner / in the note, centred, wrapped if long
    let lines = |at: Pos2, items: &[(&str, FontId, Color32)]| {
        let width = (screen.width() - 80.0).clamp(200.0, 520.0);
        let mut y = at.y;
        for (t, f, col) in items {
            let g = p.layout((*t).to_owned(), f.clone(), *col, width);
            let size = g.size();
            p.galley(pos2(at.x - size.x / 2.0, y), g, Color32::WHITE);
            y += size.y + 4.0;
        }
    };
    if let Some(load) = &app.url_load {
        spinner(load.since);
        lines(c + vec2(0.0, 14.0), &[("Opening link\u{2026}", FontId::proportional(14.0), text_col)]);
    } else if let Some(retry) = &app.link_retry {
        spinner(retry.since);
        let reason = retry.reason.as_deref().map(|r| format!("It didn't open: {r}"));
        let mut items = vec![("Updating yt-dlp\u{2026}", FontId::proportional(14.5), text_col)];
        if let Some(r) = &reason {
            items.push((r.as_str(), FontId::proportional(12.5), dim));
        }
        items.push((
            "The link didn't open, and yt-dlp may be out of date. It will try the link again with the new version.",
            FontId::proportional(12.5),
            dim,
        ));
        lines(c + vec2(0.0, 14.0), &items);
    } else if let Some((t0, title, detail)) = &app.link_error {
        let age = t0.elapsed().as_secs_f32();
        const SHOW: f32 = 8.0;
        if age < SHOW {
            // A small dark note, below the middle (clear of the home screen's ribbon)
            let a = ((SHOW - age) / 0.4).min(1.0);
            let top = pos2(c.x, screen.top() + screen.height() * 0.70);
            // (laid out once to measure, then drawn over its backing)
            let width = (screen.width() - 80.0).clamp(200.0, 520.0);
            let g1 = p.layout((*title).to_owned(), FontId::proportional(14.0), text_col.gamma_multiply(a), width);
            let g2 = p.layout(detail.clone(), FontId::proportional(12.5), dim.gamma_multiply(a), width);
            let w = g1.size().x.max(g2.size().x);
            let h = g1.size().y + 4.0 + g2.size().y;
            let r = Rect::from_min_size(pos2(top.x - w / 2.0, top.y), vec2(w, h)).expand2(vec2(16.0, 10.0));
            p.rect_filled(r, 10.0, Color32::from_rgb(36, 36, 36).gamma_multiply(0.92 * a));
            let (s1, s2) = (g1.size(), g2.size());
            p.galley(pos2(top.x - s1.x / 2.0, top.y), g1, Color32::WHITE);
            p.galley(pos2(top.x - s2.x / 2.0, top.y + s1.y + 4.0), g2, Color32::WHITE);
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
    /// It failed: yt-dlp is updating, then it tries again
    Updating,
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
    /// Shared with the thread running yt-dlp, to cancel it
    run: Arc<SaveRun>,
}

/// The running yt-dlp: its process group (yt-dlp and what it starts, ended
/// together), its process id (if the group couldn't be made), and "cancelled"
#[derive(Default)]
struct SaveRun {
    group: std::sync::Mutex<Option<taskbar::ProcessGroup>>,
    pid: std::sync::atomic::AtomicU32,
    cancelled: std::sync::atomic::AtomicBool,
}

impl SaveRun {
    fn cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// End yt-dlp and everything it started
    fn end(&self) {
        if let Some(g) = self.group.lock().ok().and_then(|mut g| g.take()) {
            g.end();
            return;
        }
        let pid = self.pid.load(std::sync::atomic::Ordering::Relaxed);
        if pid == 0 {
            return;
        }
        // (no group: end yt-dlp and what it started by process id)
        let mut cmd = std::process::Command::new("taskkill");
        cmd.args(["/PID", &pid.to_string(), "/T", "/F"])
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

/// How one run of yt-dlp went
struct Attempt {
    saved: Option<String>,
    /// The file's name, for tidying up after a failure
    file: Option<String>,
    error: Option<String>,
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
            run: Arc::default(),
        }
    }

    /// Start yt-dlp saving `url` into `folder`, with the formats from `o`. If it
    /// fails, yt-dlp updates itself and it's tried once more.
    fn start(ctx: &egui::Context, url: &str, title: String, folder: &str, o: Online) -> Self {
        let Some(exe) = ytdlp_path() else {
            return Self::failed(title, "yt-dlp.exe isn't in the player's folder");
        };
        let tools = exe.parent().map(Path::to_path_buf).unwrap_or_default();
        if !tools.join("ffmpeg.exe").is_file() {
            return Self::failed(title, "ffmpeg.exe isn't in the player's folder");
        }
        let audio_only = o.max_height == 0;
        let mut args: Vec<std::ffi::OsString> = Vec::new();
        let mut add = |a: &str| args.push(a.into());
        for a in ["--no-playlist", "--newline", "--no-mtime", "--encoding", "utf-8"] {
            add(a);
        }
        add("-f");
        add(&o.format());
        add("-S");
        add(&o.sort());
        add("--merge-output-format");
        add("mp4/webm/mkv");
        add("-P");
        add(folder);
        add("-o");
        // (audio gets its own name: as "Title.webm" it would be taken for an earlier
        // save of the video, and that file used and deleted)
        add(if audio_only { "%(title).150B (audio).%(ext)s" } else { "%(title).150B.%(ext)s" });
        // Progress, the file's name before it downloads (to tidy up after a cancel),
        // and where it ended up; one line each, easy to read back
        for a in ["--progress", "--progress-template", PROGRESS_TEMPLATE] {
            add(a);
        }
        for a in ["--print", "before_dl:RMP-FILE %(filename)s", "--print", "after_move:RMP-SAVED %(filepath)s", "--no-simulate"] {
            add(a);
        }
        if audio_only {
            add("-x"); // the sound on its own: .opus or .m4a rather than .webm
        }
        add("--ffmpeg-location");
        args.push(tools.into_os_string());
        args.push("--".into());
        args.push(url.into());

        let stage = Arc::new(std::sync::Mutex::new(SaveStage::Starting));
        let run = Arc::new(SaveRun::default());
        let (st, shared, ctx) = (stage.clone(), run.clone(), ctx.clone());
        std::thread::spawn(move || {
            let set = |s: SaveStage| {
                if let Ok(mut slot) = st.lock() {
                    *slot = s;
                }
                ctx.request_repaint();
            };
            let mut attempt = run_ytdlp(&exe, &args, &shared, &set);
            let mut note = "";
            // Failed (not cancelled): maybe yt-dlp is out of date. Update, try again.
            if attempt.saved.is_none() && !shared.cancelled() {
                set(SaveStage::Updating);
                match update_ytdlp() {
                    UpdateOutcome::Updated if !shared.cancelled() => {
                        set(SaveStage::Starting);
                        attempt = run_ytdlp(&exe, &args, &shared, &set);
                        note = " (even with the newest yt-dlp)";
                    }
                    UpdateOutcome::Updated => {}
                    UpdateOutcome::UpToDate => note = " (yt-dlp is already the newest version)",
                    UpdateOutcome::Failed => note = " (updating yt-dlp didn't work: offline?)",
                }
            }
            match attempt.saved {
                Some(path) => set(SaveStage::Done(path)),
                None => {
                    if let Some(f) = &attempt.file {
                        remove_leftovers(f);
                    }
                    let why = attempt.error.unwrap_or_else(|| "yt-dlp stopped before the video was saved".to_owned());
                    set(SaveStage::Failed(format!("{why}{note}")));
                }
            }
        });
        SaveJob { title, stage, ended: None, run }
    }
}

/// Run yt-dlp once with `args`, passing its progress on through `set`
fn run_ytdlp(exe: &Path, args: &[std::ffi::OsString], run: &SaveRun, set: &dyn Fn(SaveStage)) -> Attempt {
    use std::io::BufRead;
    use std::sync::atomic::Ordering;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args)
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let failed = |why: &str| Attempt { saved: None, file: None, error: Some(why.to_owned()) };
    if run.cancelled() {
        return failed("Cancelled");
    }
    let Ok(mut child) = cmd.spawn() else { return failed("Couldn't start yt-dlp.exe") };
    run.pid.store(child.id(), Ordering::Relaxed);
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        if let Ok(mut g) = run.group.lock() {
            *g = taskbar::ProcessGroup::new(child.as_raw_handle() as isize);
        }
    }
    // (cancelled while it was starting)
    if run.cancelled() {
        run.end();
    }
    let errors = child.stderr.take().map(|e| std::thread::spawn(move || last_error(e)));
    let (mut file, mut saved) = (None, None);
    let (mut had_video, mut had_audio) = (false, false);
    if let Some(out) = child.stdout.take() {
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
    if let Ok(mut g) = run.group.lock() {
        *g = None;
    }
    run.pid.store(0, Ordering::Relaxed);
    Attempt { saved: saved.filter(|_| ok), file, error }
}

impl Drop for SaveJob {
    /// Closing the card (or the player) while it's saving cancels it
    fn drop(&mut self) {
        if self.running() {
            self.run.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
            self.run.end();
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
    let lines: Vec<String> = std::io::BufReader::new(stderr)
        .split(b'\n')
        .map_while(Result::ok)
        .map(|l| String::from_utf8_lossy(&l).into_owned())
        .collect();
    error_from_lines(lines.iter().map(String::as_str))
}

/// The last "ERROR: ..." line among yt-dlp's messages, shortened (see last_error)
fn error_from_lines<'a>(lines: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut last = None;
    for line in lines {
        let line = line.trim();
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
