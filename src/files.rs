//! Files: which ones the player opens, folder listing and sorting for previous / next, playlists.

use super::*;

// ── Folder navigation ─────────────────────────────────────────────────────────
/// Video files in the same folder as `path`, in Explorer-like (natural) order
/// (only files of the same kind: a video steps through videos, a picture through
/// pictures, a song through music)
pub(crate) fn scan_folder(path: &Path, by: SortBy, desc: bool) -> Vec<PathBuf> {
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
    sort_files(&mut files, by, desc);
    files
}

/// Sort like Explorer: by name (natural order), date or size; ties go by name
pub(crate) fn sort_files(files: &mut Vec<PathBuf>, by: SortBy, desc: bool) {
    let name = |p: &PathBuf| p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if by == SortBy::Name {
        files.sort_by(|a, b| natural_cmp(&name(a), &name(b)));
    } else {
        // Read each file's date / size once
        let key = |p: &PathBuf| -> u128 {
            let Ok(md) = std::fs::metadata(p) else { return 0 };
            let since = |t: std::io::Result<std::time::SystemTime>| {
                t.ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos())
            };
            match by {
                SortBy::Modified => since(md.modified()),
                SortBy::Created => since(md.created()),
                _ => md.len() as u128,
            }
        };
        let mut keyed: Vec<(u128, String, PathBuf)> = files.drain(..).map(|p| (key(&p), name(&p), p)).collect();
        keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| natural_cmp(&a.1, &b.1)));
        files.extend(keyed.into_iter().map(|(_, _, p)| p));
    }
    if desc {
        files.reverse();
    }
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
pub(crate) fn read_playlist(path: &Path) -> Vec<PathBuf> {
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
