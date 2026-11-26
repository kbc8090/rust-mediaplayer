//! Files: which ones the player opens, folder listing and sorting for previous / next, playlists.

use super::*;

// ── Folder navigation ─────────────────────────────────────────────────────────
/// Video files in the same folder as `path`, in Explorer-like (natural) order
/// (only files of the same kind: a video steps through videos, a picture through
/// pictures, a song through music)
pub(crate) fn scan_folder(path: &Path, by: SortBy, desc: bool) -> Vec<PathBuf> {
    let Some(dir) = path.parent() else { return vec![path.to_owned()] };
    let kind = media_kind(&path.to_string_lossy());
    // Windows hands over each file's type, size and dates with the folder listing
    // itself, so no file is opened one by one here (that took a while for a big
    // folder on an old hard drive). Other files are skipped by name first.
    let files: Vec<(PathBuf, std::fs::Metadata)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let p = e.path();
                    let k = media_kind(&p.to_string_lossy());
                    if k.is_none() || (kind.is_some() && k != kind) {
                        return None;
                    }
                    let md = match e.metadata() {
                        Ok(m) if !m.file_type().is_symlink() => Some(m),
                        // (a link: what it points to)
                        _ => std::fs::metadata(&p).ok(),
                    };
                    md.filter(|m| m.is_file()).map(|m| (p, m))
                })
                .collect()
        })
        .unwrap_or_default();
    sort_files(files, by, desc)
}

/// Sort like Explorer: by name (natural order), date or size; ties go by name
fn sort_files(files: Vec<(PathBuf, std::fs::Metadata)>, by: SortBy, desc: bool) -> Vec<PathBuf> {
    let name = |p: &Path| p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let since = |t: std::io::Result<std::time::SystemTime>| {
        t.ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos())
    };
    let key = |md: &std::fs::Metadata| -> u128 {
        match by {
            SortBy::Name => 0,
            SortBy::Modified => since(md.modified()),
            SortBy::Created => since(md.created()),
            SortBy::Size => md.len() as u128,
        }
    };
    // (each name worked out once, not again on every comparison)
    let mut keyed: Vec<(u128, String, PathBuf)> = files.into_iter().map(|(p, md)| (key(&md), name(&p), p)).collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| natural_cmp(&a.1, &b.1)));
    let mut out: Vec<PathBuf> = keyed.into_iter().map(|(_, _, p)| p).collect();
    if desc {
        out.reverse();
    }
    out
}

/// Case-insensitive compare where digit runs compare by value ("2" < "10"),
/// like Windows Explorer
pub(crate) fn natural_cmp(a: &str, b: &str) -> Ordering {
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

// ── File types ────────────────────────────────────────────────────────────────
/// Files mpv plays, by kind (drag-and-drop, the Open dialog, previous / next)
pub(crate) const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "ts", "m2ts", "mts", "mpg", "mpeg", "3gp", "ogv",
];

pub(crate) const MUSIC_EXTS: &[&str] = &[
    "mp3", "flac", "wav", "m4a", "aac", "ogg", "oga", "opus", "wma", "alac", "aiff", "aif", "ape", "wv", "mka",
];

pub(crate) const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "avif", "bmp", "tif", "tiff", "jxl",
];

pub(crate) const PLAYLIST_EXTS: &[&str] = &["m3u", "m3u8"];

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum MediaKind {
    Video,
    Music,
    Image,
}

pub(crate) fn media_kind(path: &str) -> Option<MediaKind> {
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

pub(crate) fn is_playlist_file(path: &str) -> bool {
    has_ext(path, PLAYLIST_EXTS)
}

/// The entries of an .m3u / .m3u8 playlist that exist (or are web addresses),
/// in order. Relative paths are relative to the playlist's folder.
pub(crate) fn read_playlist(path: &Path) -> Vec<PlaylistEntry> {
    let Ok(bytes) = std::fs::read(path) else { return Vec::new() };
    let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut out = Vec::new();
    // "#EXTINF:243,Artist - Title" describes the entry on the next line
    let mut info: (Option<f64>, Option<String>) = (None, None);
    for line in playlist_text(&bytes).lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let (secs, title) = rest.split_once(',').unwrap_or((rest, ""));
            let secs = secs.split_whitespace().next().and_then(|s| s.parse::<f64>().ok()).filter(|s| *s > 0.0);
            let title = title.trim();
            info = (secs, (!title.is_empty()).then(|| title.to_owned()));
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let (secs, title) = std::mem::take(&mut info);
        let p = if line.contains("://") && !line.starts_with("file://") {
            PathBuf::from(line) // web address: mpv streams it
        } else {
            let line = match line.strip_prefix("file:///").or_else(|| line.strip_prefix("file://")) {
                Some(rest) => percent_decode(rest),
                None => line.to_owned(),
            };
            let p = PathBuf::from(line.replace('/', "\\"));
            let p = if p.is_absolute() { p } else { base.join(p) };
            if !p.is_file() || is_playlist_file(&p.to_string_lossy()) {
                continue;
            }
            p
        };
        out.push(PlaylistEntry { path: p, title, secs });
    }
    out
}

/// An entry of a playlist, with what the playlist says about it
pub(crate) struct PlaylistEntry {
    pub(crate) path: PathBuf,
    /// From "#EXTINF:" (often "Artist - Title")
    pub(crate) title: Option<String>,
    pub(crate) secs: Option<f64>,
}

/// .m3u8 (and modern .m3u) files are UTF-8; older .m3u files use the Windows
/// language's code page
pub(crate) fn playlist_text(bytes: &[u8]) -> String {
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
pub(crate) fn percent_decode(s: &str) -> String {
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

pub(crate) fn has_ext(path: &str, exts: &[&str]) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

pub(crate) fn is_media_file(path: &str) -> bool {
    media_kind(path).is_some()
}

// ── Archives (.zip, .rar, .7z) ────────────────────────────────────────────────
// mpv plays the files inside an archive without unpacking it to disk, from
// "archive://<the archive's path, %-escaped>|/<path inside>". Seeking is instant
// in files stored as they are (usual for big videos in a RAR); in compressed
// ones mpv has to unpack from the start up to the point, so long jumps there
// take a moment.

pub(crate) const ARCHIVE_EXTS: &[&str] = &["zip", "rar", "7z"];

pub(crate) fn is_archive_file(path: &str) -> bool {
    has_ext(path, ARCHIVE_EXTS) && !is_archive_entry(path)
}

/// A file inside an archive ("archive://…|/…")
pub(crate) fn is_archive_entry(path: &str) -> bool {
    path.starts_with("archive://")
}

/// The archive a file inside one is in ("C:\…\Album.zip")
pub(crate) fn archive_of(entry: &str) -> Option<String> {
    let escaped = entry.strip_prefix("archive://")?.split('|').next()?;
    // (%XX → that byte; the bytes are UTF-8)
    let b = escaped.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (b[i], b.get(i + 1).copied().and_then(hex), b.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, ..) => {
                out.push(c);
                i += 1;
            }
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// Where a file is inside its archive ("Season 1/Episode 2.mkv")
fn inside_of(entry: &str) -> &str {
    entry.split_once("|/").map_or("", |(_, inside)| inside)
}

/// What's in an archive, as far as playing goes
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Inside {
    /// Still being read (while dragged over the window)
    Looking,
    /// The files to play, in the same order as a folder's: all of one kind
    /// (the kind most of its files are: an album's songs, not also its cover)
    Media(Vec<String>),
    /// Nothing the player plays
    Nothing,
    /// Not readable as an archive (damaged, password-protected, not one)
    Unreadable,
}

/// The files in an archive that mpv plays, as mpv names them. None if mpv
/// can't open it as an archive.
fn archive_entries(archive: &str) -> Option<Vec<String>> {
    // mpv itself lists it, so the names are exactly the ones it will look for
    // again when one is played (names that aren't plain ASCII go through a
    // character conversion in mpv that's easier asked than copied). A separate
    // mpv that plays nothing: it starts in a few milliseconds and is gone again.
    let mpv = libmpv2::Mpv::with_initializer(|init| {
        for (k, v) in [
            ("config", "no"),
            ("terminal", "no"),
            ("pause", "yes"),
            ("vo", "null"),
            ("ao", "null"),
            ("vid", "no"),
            ("aid", "no"),
            ("sid", "no"),
            ("ytdl", "no"),
            ("load-scripts", "no"),
            ("osc", "no"),
            ("load-stats-overlay", "no"),
            ("load-osd-console", "no"),
            ("load-auto-profiles", "no"),
            ("load-select", "no"),
            ("load-positioning", "no"),
            ("load-commands", "no"),
            ("load-context-menu", "no"),
            // (no nested archives or playlists inside it)
            ("directory-filter-types", "video,audio,image"),
        ] {
            let _ = init.set_property(k, v);
        }
        Ok(())
    })
    .ok()?;
    mpv.command("loadfile", &[archive]).ok()?;
    // Opening an archive turns it into a list of what's in it, and that
    // "file" ends. (A big RAR on an old hard drive can take a few seconds.)
    // (An error instead: it didn't open as an archive.)
    let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let left = until.saturating_duration_since(std::time::Instant::now()).as_secs_f64();
        if left <= 0.0 {
            return None;
        }
        match mpv.wait_event(left.min(0.5)) {
            Some(Ok(libmpv2::events::Event::EndFile(_))) => break,
            Some(Err(_)) | Some(Ok(libmpv2::events::Event::Shutdown)) => return None,
            _ => {}
        }
    }
    let count = mpv.get_property::<i64>("playlist-count").unwrap_or(0);
    Some(
        (0..count.min(100_000))
            .filter_map(|i| mpv.get_property::<String>(&format!("playlist/{i}/filename")).ok())
            .filter(|f| is_archive_entry(f))
            .collect(),
    )
}

/// What there is to play in an archive (reads it: run it on another thread)
pub(crate) fn archive_media(archive: &str) -> Inside {
    let Some(entries) = archive_entries(archive) else { return Inside::Unreadable };
    let of = |k: MediaKind| entries.iter().filter(|e| media_kind(e) == Some(k)).count();
    let (v, m, i) = (of(MediaKind::Video), of(MediaKind::Music), of(MediaKind::Image));
    let kind = if v == 0 && m == 0 && i == 0 {
        return Inside::Nothing;
    } else if v >= m && v >= i {
        MediaKind::Video
    } else if m >= i {
        MediaKind::Music
    } else {
        MediaKind::Image
    };
    let mut files: Vec<String> = entries.into_iter().filter(|e| media_kind(e) == Some(kind)).collect();
    // (in the same order as a folder's files)
    files.sort_by(|a, b| natural_cmp(inside_of(a), inside_of(b)));
    Inside::Media(files)
}

/// A file Explorer copied out of a zip it shows (double-clicked, or dragged
/// out) to the temporary folder, "…\Temp\Temp1_Videos.zip\Season 1\Ep 2.mkv":
/// the zip's name ("Videos.zip") and the path inside it ("Season 1\Ep 2.mkv")
pub(crate) fn explorer_zip_copy(path: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = path.split('\\').collect();
    let temp = std::env::temp_dir().to_string_lossy().trim_end_matches('\\').to_lowercase();
    for i in 1..parts.len().saturating_sub(1) {
        // "Temp1_Videos.zip" (Temp, a number, _, the zip's name), in the temporary
        // folder (…\Temp, or wherever TEMP points)
        let folder = parts[i];
        let Some(after) = folder.get(..4).filter(|t| t.eq_ignore_ascii_case("temp")).map(|_| &folder[4..]) else { continue };
        let digits = after.chars().take_while(|c| c.is_ascii_digit()).count();
        let Some(zip) = after[digits..].strip_prefix('_').filter(|z| digits > 0 && has_ext(z, &["zip"])) else { continue };
        let in_temp = parts[i - 1].eq_ignore_ascii_case("temp") || parts[..i].join("\\").to_lowercase() == temp;
        if in_temp {
            return Some((zip.to_owned(), parts[i + 1..].join("\\")));
        }
    }
    None
}

/// The same file name, give or take letter case, from Explorer and from mpv
/// (whose names show "?" for letters it couldn't convert)
fn same_name(explorer: &str, mpv: &str) -> bool {
    let a: Vec<char> = explorer.chars().flat_map(char::to_lowercase).collect();
    let b: Vec<char> = mpv.chars().flat_map(char::to_lowercase).collect();
    a == b || (a.len() == b.len() && a.iter().zip(&b).all(|(x, y)| x == y || *y == '?'))
}

/// A file Explorer copied out of a zip (see `explorer_zip_copy`) and the
/// rest of that folder of the zip, as a playlist: the files of the same kind,
/// in the order the Explorer window shows them, the copy itself (which plays
/// at once, and seeks fastest) in its place among them. None if the zip or
/// its Explorer window can't be found. (Asks Explorer and reads the zip: run
/// it on another thread.)
pub(crate) fn explorer_zip_playlist(copy: &str) -> Option<(String, Vec<String>, usize)> {
    let (zip, inside) = explorer_zip_copy(copy)?;
    let shown = format!("{zip}\\{inside}");
    let items = taskbar::explorer_zip_folder(shown.clone())?;
    // "C:\Downloads\Videos.zip\Season 1\Ep 2.mkv" → the zip, "C:\Downloads\Videos.zip"
    let want = format!("\\{shown}");
    let ends = |s: &str| {
        let n = want.chars().count();
        let tail: String = s.chars().skip(s.chars().count().saturating_sub(n)).collect();
        s.chars().count() > n && same_name(&want, &tail)
    };
    let clicked = items.iter().find(|s| ends(s))?;
    let keep = clicked.chars().count() - inside.chars().count() - 1;
    let archive: String = clicked.chars().take(keep).collect();
    if !Path::new(&archive).is_file() {
        return None;
    }
    let kind = media_kind(copy)?;
    let listed = archive_entries(&archive)?;
    let mut files = Vec::new();
    let mut at = None;
    for item in &items {
        // (only what's in that zip's folder: its path inside the zip, / between folders)
        if item.chars().count() <= keep + 1 || media_kind(item) != Some(kind) {
            continue;
        }
        let rel: String = item.chars().skip(keep + 1).collect::<String>().replace('\\', "/");
        if item == clicked {
            at = Some(files.len());
            files.push(copy.to_owned());
        } else if let Some(e) = listed.iter().find(|e| same_name(&rel, inside_of(e))) {
            files.push(e.clone());
        }
    }
    let at = at?;
    (files.len() > 1).then_some((zip, files, at))
}
