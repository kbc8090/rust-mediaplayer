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
pub(crate) fn natural_cmp(sa: &str, sb: &str) -> Ordering {
    let (a, b) = (sa.as_bytes(), sb.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            // Numbers by value, read in place: leading zeros skipped, then the
            // longer number is bigger, then digit by digit
            let (si, sj) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() { i += 1; }
            while j < b.len() && b[j].is_ascii_digit() { j += 1; }
            let da = &a[si..i];
            let db = &b[sj..j];
            let da = &da[da.iter().take_while(|&&c| c == b'0').count()..];
            let db = &db[db.iter().take_while(|&&c| c == b'0').count()..];
            let o = da.len().cmp(&db.len()).then_with(|| da.cmp(db));
            if o != Ordering::Equal {
                return o;
            }
        } else if a[i].is_ascii() && b[j].is_ascii() {
            let o = a[i].to_ascii_lowercase().cmp(&b[j].to_ascii_lowercase());
            if o != Ordering::Equal {
                return o;
            }
            i += 1;
            j += 1;
        } else {
            // (a character outside ASCII on either side: compared as characters,
            // lower-cased, as before)
            let x = sa.get(i..).and_then(|s| s.chars().next());
            let y = sb.get(j..).and_then(|s| s.chars().next());
            let (Some(x), Some(y)) = (x, y) else { return a[i..].cmp(&b[j..]) };
            let o = x.to_lowercase().cmp(y.to_lowercase());
            if o != Ordering::Equal {
                return o;
            }
            i += x.len_utf8();
            j += y.len_utf8();
        }
    }
    (a.len() - i).cmp(&(b.len() - j)).then(Ordering::Equal)
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
    "jpg", "jpeg", "png", "gif", "webp", "avif", "bmp", "tif", "tiff", "jxl", "heic", "heif",
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

/// Whether a picture reaches mpv's drawing with a transparency channel, as far
/// as its first bytes tell (a PNG or WebP's header). Not whether it looks
/// see-through: an all-solid RGBA PNG has the channel all the same, and mpv
/// turns palette PNGs and lossless WebPs into a format with one. Any other type,
/// a file inside an archive or online, or one that can't be read or
/// understood: it may.
pub(crate) fn may_have_alpha(path: &str) -> bool {
    use std::io::Read;
    let png = has_ext(path, &["png"]);
    if !(png || has_ext(path, &["webp"])) || is_url(path) || is_archive_entry(path) {
        return true;
    }
    let Ok(file) = std::fs::File::open(path) else { return true };
    let mut head = Vec::with_capacity(64 * 1024);
    if file.take(64 * 1024).read_to_end(&mut head).is_err() {
        return true;
    }
    let says = if png { png_alpha(&head) } else { webp_alpha(&head) };
    says.unwrap_or(true)
}

/// A PNG's chunks up to its picture data: IHDR's colour type 3, 4 or 6
/// (palette, grey or colour with alpha), or a tRNS chunk (a transparent
/// colour). Only grey (0) and colour (2) without one have no channel. None:
/// not a PNG, or its picture data starts further in than `b` reaches.
fn png_alpha(b: &[u8]) -> Option<bool> {
    if b.len() < 33 || &b[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut i = 8;
    while i + 8 <= b.len() {
        let len = u32::from_be_bytes(b[i..i + 4].try_into().ok()?) as usize;
        match &b[i + 4..i + 8] {
            b"IHDR" => {
                if matches!(*b.get(i + 8 + 9)?, 3 | 4 | 6) {
                    return Some(true);
                }
            }
            b"tRNS" => return Some(true),
            b"IDAT" => return Some(false),
            _ => {}
        }
        i = i.checked_add(12 + len)?;
    }
    None
}

/// A WebP's chunks: only a plain lossy picture ("VP8 ") without an alpha chunk
/// arrives without the channel; lossless ("VP8L") always arrives with one, and
/// so does an animation, or anything flagged as having alpha. None: not
/// understood.
fn webp_alpha(b: &[u8]) -> Option<bool> {
    if b.len() < 20 || &b[0..4] != b"RIFF" || &b[8..12] != b"WEBP" {
        return None;
    }
    let mut i = 12;
    while i + 8 <= b.len() {
        let len = u32::from_le_bytes(b[i + 4..i + 8].try_into().ok()?) as usize;
        match &b[i..i + 4] {
            b"VP8X" if *b.get(i + 8)? & 0x12 != 0 => return Some(true), // alpha, or animation
            b"VP8 " => return Some(false),
            b"VP8L" | b"ALPH" | b"ANIM" | b"ANMF" => return Some(true),
            _ => {}
        }
        // (chunks are padded to an even length)
        i = i.checked_add(8 + len + (len & 1))?;
    }
    None
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

/// Each kind of file in an archive, with its files
pub(crate) type KindFiles = Vec<(MediaKind, Vec<String>)>;

/// What's in an archive, as far as playing goes
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Inside {
    /// Still being read (while dragged over the window)
    Looking,
    /// The files to play, in the same order as a folder's: all of one kind
    /// (an album's songs, not also its cover picture)
    Media(Vec<String>),
    /// Several kinds worth playing (305 pictures, 160 videos): each kind's
    /// files, the kind there's most of first. Which plays is asked.
    Mixed(KindFiles),
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
    let mut kinds: KindFiles = [MediaKind::Video, MediaKind::Music, MediaKind::Image]
        .into_iter()
        .map(|k| {
            let mut files: Vec<String> = entries.iter().filter(|e| media_kind(e) == Some(k)).cloned().collect();
            // (in the same order as a folder's files)
            files.sort_by(|a, b| natural_cmp(inside_of(a), inside_of(b)));
            (k, files)
        })
        .filter(|(_, files)| !files.is_empty())
        .collect();
    // A few pictures next to songs or videos are covers and scans (cover.jpg,
    // back.jpg…), not something to choose to play
    if kinds.iter().any(|(k, _)| *k != MediaKind::Image) {
        kinds.retain(|(k, files)| *k != MediaKind::Image || files.len() > 5);
    }
    kinds.sort_by_key(|(_, files)| std::cmp::Reverse(files.len()));
    match kinds.len() {
        0 => Inside::Nothing,
        1 => Inside::Media(kinds.remove(0).1),
        _ => Inside::Mixed(kinds),
    }
}

/// A file in the temporary folder (where Explorer copies a file out of a zip
/// it shows, when it's double-clicked or dragged out): its name there
pub(crate) fn temp_copy_name(path: &str) -> Option<String> {
    let temp = std::env::temp_dir().to_string_lossy().trim_end_matches('\\').to_lowercase();
    let lower = path.to_lowercase();
    // (TEMP itself, or any …\Temp\ folder: the path may be the short 8.3 form)
    let in_temp = lower.strip_prefix(&temp).is_some_and(|r| r.starts_with('\\')) || lower.contains("\\temp\\");
    let name = path.rsplit('\\').next().filter(|n| !n.is_empty())?;
    in_temp.then(|| name.to_owned())
}

/// The path with every letter shown as x and every digit as 9 (for error
/// messages: its shape, without the names)
pub(crate) fn path_shape(path: &str) -> String {
    let temp = std::env::temp_dir().to_string_lossy().trim_end_matches('\\').to_owned();
    let (head, rest) = match path.get(..temp.len()) {
        Some(h) if h.eq_ignore_ascii_case(&temp) => ("%TEMP%", &path[temp.len()..]),
        _ => ("", path),
    };
    let rest: String = rest
        .chars()
        .map(|c| if c.is_alphabetic() { 'x' } else if c.is_numeric() { '9' } else { c })
        .collect();
    format!("{head}{rest}")
}

/// The same file name, give or take letter case, from Explorer and from mpv
/// (whose names show "?" for letters it couldn't convert)
/// (Both lower-cased already: see lower_chars.)
fn same_name_lowered(a: &[char], b: &[char]) -> bool {
    a == b || (a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x == y || *y == '?'))
}

/// A name's characters, lower-cased (see same_name_lowered)
fn lower_chars(name: &str) -> Vec<char> {
    name.chars().flat_map(char::to_lowercase).collect()
}

/// A file Explorer copied out of a zip (see `temp_copy_name`) and the rest
/// of that folder of the zip, as a playlist: the files of the same kind, by
/// name, the copy itself (which plays at once, and seeks fastest) in its
/// place among them. Err: why the zip or its Explorer window couldn't be
/// found. (Asks Explorer and reads the zip: run it on another thread.)
pub(crate) fn explorer_zip_playlist(copy: &str) -> Result<(String, Vec<String>, usize), String> {
    // (only the file's name is used from the copy: how Windows names the
    // folder it copies to varies, and it may leave out the folders of the zip)
    let name = temp_copy_name(copy).ok_or_else(|| format!("not in the temporary folder: {}", path_shape(copy)))?;
    let (clicked, items) = taskbar::explorer_zip_folder(name)?;
    // "C:\Downloads\Videos.zip\Season 1\Ep 2.mkv" → the zip, "C:\Downloads\Videos.zip"
    // (the first "….zip" along it that's a file: a folder could have such a name)
    let chars: Vec<char> = clicked.chars().collect();
    let keep = (4..chars.len())
        .filter(|&k| chars[k] == '\\' && chars[k - 4..k].iter().collect::<String>().eq_ignore_ascii_case(".zip"))
        .find(|&k| Path::new(&chars[..k].iter().collect::<String>()).is_file())
        .ok_or_else(|| format!("couldn't find the zip along {}", path_shape(&clicked)))?;
    let archive: String = chars[..keep].iter().collect();
    let zip = Path::new(&archive).file_name().map_or_else(|| archive.clone(), |n| n.to_string_lossy().into_owned());
    let kind = media_kind(copy).ok_or("not a media file")?;
    let listed = archive_entries(&archive).ok_or_else(|| format!("mpv couldn't read {}", path_shape(&archive)))?;
    // (each with its name, to sort by: Explorer hands a zip's files over in the
    // zip's own order, not the one it shows)
    let mut found: Vec<(String, String)> = Vec::new();
    // (mpv's names lower-cased once, and looked up by name: comparing every
    // file in the window with every file in the zip, lower-casing both each
    // time, took a big zip a second or more)
    let lowered: Vec<Vec<char>> = listed.iter().map(|e| lower_chars(inside_of(e))).collect();
    let by_name: std::collections::HashMap<&[char], usize> = lowered.iter().enumerate().rev().map(|(i, n)| (n.as_slice(), i)).collect();
    for item in &items {
        // (only what's in that zip's folder: its path inside the zip, / between folders)
        if item.chars().count() <= keep + 1 || media_kind(item) != Some(kind) {
            continue;
        }
        let rel: String = item.chars().skip(keep + 1).collect::<String>().replace('\\', "/");
        if *item == clicked {
            found.push((rel, copy.to_owned()));
        } else {
            let want = lower_chars(&rel);
            let hit = by_name.get(want.as_slice()).copied().or_else(|| lowered.iter().position(|n| same_name_lowered(&want, n)));
            if let Some(i) = hit {
                found.push((rel, listed[i].clone()));
            }
        }
    }
    // By name, in natural order (Ep 2 before Ep 10), as a zip's files usually are
    found.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    let at = found.iter().position(|(_, f)| f == copy).ok_or("lost the clicked file")?;
    let files: Vec<String> = found.into_iter().map(|(_, f)| f).collect();
    if files.len() > 1 {
        Ok((zip, files, at))
    } else {
        Err(format!(
            "only the clicked file matched: {} in the window, mpv listed {} (first: {})",
            items.len(),
            listed.len(),
            listed.first().map_or_else(|| "none".to_owned(), |e| path_shape(inside_of(e)))
        ))
    }
}

// ── Photo details (EXIF) ──────────────────────────────────────────────────────
/// What a photo's EXIF says about how it was taken (the Tab overlay)
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PhotoDetails {
    pub(crate) camera: Option<String>,
    pub(crate) lens: Option<String>,
    /// "1 May 2024, 14:03"
    pub(crate) taken: Option<String>,
    /// "1/120 s · f/1.8 · ISO 64"
    pub(crate) exposure: Option<String>,
    /// "6.9 mm (26 mm equiv.)"
    pub(crate) focal: Option<String>,
    /// Where it was taken is saved in it (GPS)
    pub(crate) gps: bool,
}

impl PhotoDetails {
    pub(crate) fn is_empty(&self) -> bool {
        *self == PhotoDetails::default()
    }
}

/// A photo's EXIF details: JPEG, HEIC / AVIF, WebP, PNG, TIFF (whichever keep
/// it). Reads only as far as it's found (a JPEG's are at the start).
pub(crate) fn photo_details(path: &str) -> Option<PhotoDetails> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut data = Vec::new();
    let mut chunk = vec![0_u8; 1 << 20];
    // (in 1 MB steps, up to 32 MB: a phone's HEIC can keep it after the picture)
    while data.len() < 32 << 20 {
        let n = f.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        // (only what's new is searched, from a little before it, for a marker
        // split between two reads: searching it all again each time read the
        // first megabytes over and over, in a file without EXIF up to 32 times)
        let from = data.len().saturating_sub(16);
        data.extend_from_slice(&chunk[..n]);
        if let Some(at) = find_tiff(&data, from) {
            // (a little more read, if the details run past what's read so far)
            let mut more = vec![0_u8; 256 << 10];
            if let Ok(m) = f.read(&mut more) {
                data.extend_from_slice(&more[..m]);
            }
            return Some(parse_exif(&data[at..]));
        }
    }
    None
}

/// Where the EXIF (a little TIFF file) starts: at the start of a TIFF; after
/// "Exif\0\0" (JPEG, HEIC, AVIF); in a PNG's eXIf or a WebP's EXIF chunk
fn find_tiff(d: &[u8], start: usize) -> Option<usize> {
    let tiff = |at: usize| d.get(at..at + 4).is_some_and(|h| h == b"II*\0" || h == b"MM\0*");
    if start == 0 && tiff(0) {
        return Some(0);
    }
    // (WebP's "EXIF" chunk: its 4-byte size, then the TIFF, sometimes after "Exif\0\0")
    for (marker, skip) in [(&b"Exif\0\0"[..], 0), (&b"eXIf"[..], 0), (&b"EXIF"[..], 4)] {
        let mut from = start;
        while let Some(i) = d[from..].windows(marker.len()).position(|w| w == marker) {
            let at = from + i + marker.len() + skip;
            if tiff(at) {
                return Some(at);
            }
            from = from + i + 1;
        }
    }
    None
}

/// The details out of EXIF (a TIFF structure: directories of tagged values)
fn parse_exif(t: &[u8]) -> PhotoDetails {
    let le = t.starts_with(b"II");
    let u16_at = |o: usize| t.get(o..o + 2).map(|b| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) });
    let u32_at = |o: usize| {
        t.get(o..o + 4).map(|b| {
            let b = [b[0], b[1], b[2], b[3]];
            if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) }
        })
    };
    // A directory's entries: tag → (type, count, where its value is)
    let dir = |at: usize| -> Vec<(u16, u16, u32, usize)> {
        let Some(n) = u16_at(at) else { return Vec::new() };
        (0..n.min(512) as usize)
            .filter_map(|i| {
                let e = at + 2 + i * 12;
                let (tag, kind, count) = (u16_at(e)?, u16_at(e + 2)?, u32_at(e + 4)?);
                let size = match kind {
                    3 | 8 => 2,
                    4 | 9 | 11 => 4,
                    5 | 10 | 12 => 8,
                    _ => 1,
                } * count as usize;
                // (4 bytes or less: right there; more: where the entry points)
                let value = if size <= 4 { e + 8 } else { u32_at(e + 8)? as usize };
                Some((tag, kind, count, value))
            })
            .collect()
    };
    let find = |d: &[(u16, u16, u32, usize)], tag: u16| d.iter().find(|e| e.0 == tag).copied();
    let text = |e: Option<(u16, u16, u32, usize)>| {
        let (_, _, count, at) = e?;
        let raw = t.get(at..at + count as usize)?;
        let s = String::from_utf8_lossy(raw).trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_owned();
        (!s.is_empty()).then_some(s)
    };
    let number = |e: Option<(u16, u16, u32, usize)>| -> Option<f64> {
        let (_, kind, _, at) = e?;
        match kind {
            3 => u16_at(at).map(f64::from),
            4 => u32_at(at).map(f64::from),
            5 | 10 => {
                let (n, d) = (u32_at(at)?, u32_at(at + 4)?);
                let (n, d) = if kind == 10 { (n as i32 as f64, d as i32 as f64) } else { (n as f64, d as f64) };
                (d != 0.0).then(|| n / d)
            }
            _ => None,
        }
    };
    let Some(first) = u32_at(4) else { return PhotoDetails::default() };
    let ifd0 = dir(first as usize);
    let exif = find(&ifd0, 0x8769).and_then(|e| u32_at(e.3)).map_or_else(Vec::new, |o| dir(o as usize));
    let gps = find(&ifd0, 0x8825).and_then(|e| u32_at(e.3)).is_some_and(|o| !dir(o as usize).is_empty());

    // Camera: make and model ("Apple iPhone 15 Pro"; the make once, if the model has it)
    // ("NIKON CORPORATION" + "NIKON D850" → "NIKON D850")
    let camera = match (text(find(&ifd0, 0x010F)), text(find(&ifd0, 0x0110))) {
        (Some(make), Some(model)) => {
            let brand = make.split_whitespace().next().unwrap_or("").to_lowercase();
            Some(if !brand.is_empty() && model.to_lowercase().starts_with(&brand) { model } else { format!("{make} {model}") })
        }
        (make, model) => make.or(model),
    };
    // Taken: "2024:05:01 14:03:22" → "1 May 2024, 14:03"
    let taken = text(find(&exif, 0x9003)).or_else(|| text(find(&ifd0, 0x0132))).map(|s| {
        const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        let n: Vec<u32> = s.split([':', ' ']).filter_map(|p| p.parse().ok()).collect();
        match n.as_slice() {
            [y, mo, d, h, mi, ..] if (1..=12).contains(mo) => format!("{d} {} {y}, {h:02}:{mi:02}", MONTHS[*mo as usize - 1]),
            _ => s,
        }
    });
    // Exposure: "1/120 s · f/1.8 · ISO 64"
    let trim = |x: f64, places: usize| {
        let s = format!("{x:.places$}");
        if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_owned() } else { s }
    };
    let mut parts = Vec::new();
    if let Some(t) = number(find(&exif, 0x829A)).filter(|t| *t > 0.0) {
        parts.push(if t < 0.5 { format!("1/{:.0} s", 1.0 / t) } else { format!("{} s", trim(t, 1)) });
    }
    if let Some(f) = number(find(&exif, 0x829D)).filter(|f| *f > 0.0) {
        parts.push(format!("f/{}", trim(f, 1)));
    }
    if let Some(iso) = number(find(&exif, 0x8827)).filter(|i| *i > 0.0) {
        parts.push(format!("ISO {iso:.0}"));
    }
    let exposure = (!parts.is_empty()).then(|| parts.join("  \u{b7}  "));
    // Focal length: "6.9 mm (26 mm equiv.)"
    let focal = number(find(&exif, 0x920A)).filter(|f| *f > 0.0).map(|f| {
        match number(find(&exif, 0xA405)).filter(|e| *e > 0.0 && (*e - f).abs() > 0.5) {
            Some(e) => format!("{} mm ({e:.0} mm equiv.)", trim(f, 1)),
            None => format!("{} mm", trim(f, 1)),
        }
    });
    PhotoDetails { camera, lens: text(find(&exif, 0xA434)), taken, exposure, focal, gps }
}
