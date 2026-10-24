// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! Audio file metadata read from the container headers: the title, artist, album, genre, year,
//! track and disc numbers, comment, cover picture, and the stream facts a library view shows
//! (duration, bitrate, sample rate, channels). This is the scanner behind a music player's
//! library, so it reads headers only and never the audio itself: an ID3 tag, a `moov` atom, the
//! FLAC metadata blocks, the first two Ogg packets plus the last page, a RIFF or AIFF chunk list.
//! A thousand files scan in the time one decodes.
//!
//! Containers: MP3 (ID3v2.2/2.3/2.4 with ID3v1 as the fallback, Xing/Info/VBRI or a CBR
//! estimate for the duration), MP4/M4A/M4B (`ilst` atoms, `mvhd` for the duration), FLAC
//! (STREAMINFO, VORBIS_COMMENT, PICTURE), Ogg Vorbis and Opus (Vorbis comments, a
//! `METADATA_BLOCK_PICTURE`, the last page's granule position), WAV (`fmt `, `LIST INFO`, an
//! `id3 ` chunk) and AIFF/AIFC (`COMM`, `ID3 `, `NAME`/`AUTH`/`ANNO`).
//!
//! The reader only reads: it writes no tags, and it skips lyrics, ReplayGain, chapters, ratings
//! and every other frame a player does not list by. Input is treated as hostile: a truncated
//! frame, an absurd size or a zero length ends that field, never the process. Allocations are
//! capped (a picture over 32 MiB and a text field over 1 MiB are skipped). Bytes that are no
//! audio container at all read as [`Format::Unknown`] with every field `None`; an `io::Error`
//! means the file could not be opened or read.

use std::fs::File;
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

/// Everything the reader found. A field the file does not carry stays `None`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub year: Option<i32>,
    pub track: Option<u32>,
    pub track_total: Option<u32>,
    pub disc: Option<u32>,
    pub disc_total: Option<u32>,
    pub comment: Option<String>,
    pub duration_secs: Option<f64>,
    pub bitrate_kbps: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    /// The front cover where the file marks one, else the first picture it carries.
    pub picture: Option<Picture>,
    pub format: Format,
}

/// An embedded picture, as stored (JPEG or PNG bytes, usually).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    /// The MIME type the file declares, or one guessed from the bytes (`image/jpeg`, `image/png`).
    pub mime: String,
    pub data: Vec<u8>,
}

/// The container the reader recognized.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    #[default]
    Unknown,
    Mp3,
    Mp4,
    Flac,
    OggVorbis,
    Opus,
    Wav,
    Aiff,
}

/// File extensions (lowercase, no dot) of the containers this module reads.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "m4a", "m4b", "aac", "mp4", "flac", "ogg", "oga", "opus", "wav", "aif", "aiff",
];

/// Whether `ext` (with or without a leading dot, any case) names a container this module reads.
pub fn is_audio_extension(ext: &str) -> bool {
    let ext = ext.trim_start_matches('.');
    AUDIO_EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

/// Opens `path` and reads its tags. Errors only when the file cannot be opened or read.
pub fn read_file(path: &Path) -> io::Result<Tags> {
    let mut reader = BufReader::new(File::open(path)?);
    read_from(&mut reader)
}

/// Reads the tags of whatever `reader` holds, detecting the container by its magic numbers (an
/// ID3v2 header counts as MP3 unless FLAC follows it). Fills what the file carries; a truncated
/// file yields what was readable before the cut.
pub fn read_from<R: Read + Seek>(reader: &mut R) -> io::Result<Tags> {
    let mut tags = Tags::default();
    match fill(reader, &mut tags) {
        Ok(()) => Ok(tags),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(tags),
        Err(e) => Err(e),
    }
}

/// [`read_from`] over an in-memory file.
pub fn read(bytes: &[u8]) -> Tags {
    read_from(&mut Cursor::new(bytes)).unwrap_or_default()
}

/// A picture larger than this is skipped.
const PICTURE_CAP: usize = 32 * 1024 * 1024;
/// A text field larger than this is skipped.
const TEXT_CAP: usize = 1024 * 1024;
/// The most of one metadata block (a whole ID3 tag, an `ilst` child, a Vorbis comment block)
/// that is read into memory: a capped picture plus room for text.
const BLOCK_CAP: usize = PICTURE_CAP + 2 * TEXT_CAP;
/// How far past the ID3 tag an MP3 is scanned for its first frame header.
const FRAME_SCAN: usize = 64 * 1024;
/// How much of an Ogg file's tail is scanned for its last page.
const OGG_TAIL: u64 = 64 * 1024;

fn fill<R: Read + Seek>(r: &mut R, tags: &mut Tags) -> io::Result<()> {
    let len = r.seek(SeekFrom::End(0))?;
    r.seek(SeekFrom::Start(0))?;
    let head = read_up_to(r, 12)?;
    if head.len() < 4 {
        return Ok(());
    }
    if &head[..3] == b"ID3" || mpeg_header(&head).is_some() {
        return parse_mp3(r, len, tags);
    }
    match &head[..4] {
        b"fLaC" => parse_flac(r, 0, len, tags),
        b"OggS" => parse_ogg(r, len, tags),
        b"RIFF" if head.get(8..12) == Some(b"WAVE") => parse_wav(r, len, tags),
        b"FORM" if matches!(head.get(8..12), Some(b"AIFF" | b"AIFC")) => parse_aiff(r, len, tags),
        _ if matches!(head.get(4..8), Some(b"ftyp" | b"moov")) => parse_mp4(r, len, tags),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------------------------
// Byte and reader helpers
// ---------------------------------------------------------------------------------------------

fn be16(b: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(..2)?.try_into().ok()?))
}
fn be24(b: &[u8]) -> Option<u32> {
    let b = b.get(..3)?;
    Some(u32::from_be_bytes([0, b[0], b[1], b[2]]))
}
fn be32(b: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(..4)?.try_into().ok()?))
}
fn be64(b: &[u8]) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(..8)?.try_into().ok()?))
}
fn le16(b: &[u8]) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(..2)?.try_into().ok()?))
}
fn le32(b: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(..4)?.try_into().ok()?))
}

/// Reads up to `n` bytes, fewer at the end of the stream. The caller caps `n`; nothing is
/// allocated ahead of what the stream yields.
fn read_up_to<R: Read>(r: &mut R, n: usize) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    r.take(n as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Reads up to `n` bytes at `pos`.
fn read_at<R: Read + Seek>(r: &mut R, pos: u64, n: usize) -> io::Result<Vec<u8>> {
    r.seek(SeekFrom::Start(pos))?;
    read_up_to(r, n)
}

fn set<T>(slot: &mut Option<T>, value: Option<T>) {
    if slot.is_none() {
        *slot = value;
    }
}

fn clean(text: &str) -> &str {
    text.trim_matches(|c: char| c == '\0' || c.is_whitespace())
}

fn set_text(slot: &mut Option<String>, text: &str) {
    let text = clean(text);
    if slot.is_none() && !text.is_empty() {
        *slot = Some(text.to_string());
    }
}

fn leading_number(s: &str) -> Option<u32> {
    let digits: String = s
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .take(9)
        .collect();
    digits.parse().ok()
}

/// `"3/12"` → `(Some(3), Some(12))`; `"3"` → `(Some(3), None)`.
fn number_pair(s: &str) -> (Option<u32>, Option<u32>) {
    let mut parts = s.splitn(2, '/');
    let a = parts.next().and_then(leading_number);
    let b = parts.next().and_then(leading_number);
    (a, b)
}

/// The leading year of `"2004"`, `"2004-05-01"` or `"2004/05"`.
fn leading_year(s: &str) -> Option<i32> {
    leading_number(s)
        .filter(|&y| y > 0 && y < 10_000)
        .map(|y| y as i32)
}

fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

/// UTF-8 when it is valid, else Latin-1: the RIFF/AIFF text chunks declare no encoding.
fn text_guess(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => latin1(b),
    }
}

fn utf16(b: &[u8], big_endian: bool) -> String {
    let units: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| {
            if big_endian {
                u16::from_be_bytes(*p)
            } else {
                u16::from_le_bytes(*p)
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

fn mime_from_magic(data: &[u8]) -> Option<&'static str> {
    match data {
        [0xFF, 0xD8, ..] => Some("image/jpeg"),
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [b'G', b'I', b'F', ..] => Some("image/gif"),
        [b'B', b'M', ..] => Some("image/bmp"),
        _ => None,
    }
}

/// Builds a [`Picture`] when `data` is within the cap, settling the MIME type from the bytes when
/// the declared one is missing or not a `type/subtype`.
fn picture(mime: &str, data: &[u8]) -> Option<Picture> {
    if data.is_empty() || data.len() > PICTURE_CAP {
        return None;
    }
    let declared = clean(mime);
    let mime = if declared.contains('/') {
        declared.to_string()
    } else {
        mime_from_magic(data)
            .unwrap_or("application/octet-stream")
            .to_string()
    };
    Some(Picture {
        mime,
        data: data.to_vec(),
    })
}

/// Keeps a front cover (`kind` 3) over anything else, else the first picture offered.
fn offer_picture(tags: &mut Tags, best: &mut Option<u32>, kind: u32, pic: Option<Picture>) {
    let Some(pic) = pic else { return };
    let take = match *best {
        None => true,
        Some(3) => false,
        Some(_) => kind == 3,
    };
    if take {
        tags.picture = Some(pic);
        *best = Some(kind);
    }
}

// ---------------------------------------------------------------------------------------------
// ID3v1 genre table
// ---------------------------------------------------------------------------------------------

/// The ID3v1 genre names, index 0–147 (the ID3v1 set plus the Winamp extensions every tagger
/// writes). Entry 133 is left blank: its original name is a slur, and a file carrying it reads as
/// having no genre.
const GENRES: &[&str] = &[
    "Blues",
    "Classic Rock",
    "Country",
    "Dance",
    "Disco",
    "Funk",
    "Grunge",
    "Hip-Hop",
    "Jazz",
    "Metal",
    "New Age",
    "Oldies",
    "Other",
    "Pop",
    "R&B",
    "Rap",
    "Reggae",
    "Rock",
    "Techno",
    "Industrial",
    "Alternative",
    "Ska",
    "Death Metal",
    "Pranks",
    "Soundtrack",
    "Euro-Techno",
    "Ambient",
    "Trip-Hop",
    "Vocal",
    "Jazz+Funk",
    "Fusion",
    "Trance",
    "Classical",
    "Instrumental",
    "Acid",
    "House",
    "Game",
    "Sound Clip",
    "Gospel",
    "Noise",
    "AlternRock",
    "Bass",
    "Soul",
    "Punk",
    "Space",
    "Meditative",
    "Instrumental Pop",
    "Instrumental Rock",
    "Ethnic",
    "Gothic",
    "Darkwave",
    "Techno-Industrial",
    "Electronic",
    "Pop-Folk",
    "Eurodance",
    "Dream",
    "Southern Rock",
    "Comedy",
    "Cult",
    "Gangsta",
    "Top 40",
    "Christian Rap",
    "Pop/Funk",
    "Jungle",
    "Native American",
    "Cabaret",
    "New Wave",
    "Psychedelic",
    "Rave",
    "Showtunes",
    "Trailer",
    "Lo-Fi",
    "Tribal",
    "Acid Punk",
    "Acid Jazz",
    "Polka",
    "Retro",
    "Musical",
    "Rock & Roll",
    "Hard Rock",
    "Folk",
    "Folk-Rock",
    "National Folk",
    "Swing",
    "Fast Fusion",
    "Bebob",
    "Latin",
    "Revival",
    "Celtic",
    "Bluegrass",
    "Avantgarde",
    "Gothic Rock",
    "Progressive Rock",
    "Psychedelic Rock",
    "Symphonic Rock",
    "Slow Rock",
    "Big Band",
    "Chorus",
    "Easy Listening",
    "Acoustic",
    "Humour",
    "Speech",
    "Chanson",
    "Opera",
    "Chamber Music",
    "Sonata",
    "Symphony",
    "Booty Bass",
    "Primus",
    "Porn Groove",
    "Satire",
    "Slow Jam",
    "Club",
    "Tango",
    "Samba",
    "Folklore",
    "Ballad",
    "Power Ballad",
    "Rhythmic Soul",
    "Freestyle",
    "Duet",
    "Punk Rock",
    "Drum Solo",
    "A capella",
    "Euro-House",
    "Dance Hall",
    "Goa",
    "Drum & Bass",
    "Club-House",
    "Hardcore",
    "Terror",
    "Indie",
    "BritPop",
    "",
    "Polsk Punk",
    "Beat",
    "Christian Gangsta Rap",
    "Heavy Metal",
    "Black Metal",
    "Crossover",
    "Contemporary Christian",
    "Christian Rock",
    "Merengue",
    "Salsa",
    "Thrash Metal",
    "Anime",
    "JPop",
    "Synthpop",
];

fn genre_name(index: usize) -> Option<String> {
    GENRES
        .get(index)
        .filter(|g| !g.is_empty())
        .map(|g| g.to_string())
}

/// An ID3v2 `TCON` value: `"(17)"`, `"17"`, `"(17)Rock"` and `"(RX)"` all resolve to a name.
fn id3_genre(text: &str) -> Option<String> {
    let text = clean(text);
    if text.is_empty() {
        return None;
    }
    if let Some(rest) = text.strip_prefix('(')
        && let Some(close) = rest.find(')')
    {
        let code = &rest[..close];
        let tail = clean(&rest[close + 1..]);
        if !tail.is_empty() {
            return Some(tail.to_string());
        }
        return match code {
            "RX" => Some("Remix".to_string()),
            "CR" => Some("Cover".to_string()),
            n => n.parse::<usize>().ok().and_then(genre_name),
        };
    }
    if let Ok(n) = text.parse::<usize>() {
        return genre_name(n).or_else(|| Some(text.to_string()));
    }
    Some(text.to_string())
}

// ---------------------------------------------------------------------------------------------
// ID3v2 and ID3v1
// ---------------------------------------------------------------------------------------------

fn syncsafe(b: &[u8]) -> Option<u32> {
    let b = b.get(..4)?;
    Some(
        ((b[0] as u32 & 0x7F) << 21)
            | ((b[1] as u32 & 0x7F) << 14)
            | ((b[2] as u32 & 0x7F) << 7)
            | (b[3] as u32 & 0x7F),
    )
}

/// The whole length of the ID3v2 tag whose header `head` starts with (header, body, and the 2.4
/// footer when flagged), or `None` when `head` is no ID3v2 header.
fn id3v2_total_len(head: &[u8]) -> Option<usize> {
    if head.len() < 10 || &head[..3] != b"ID3" || !(2..=4).contains(&head[3]) {
        return None;
    }
    let size = syncsafe(&head[6..10])? as usize;
    let footer = if head[3] == 4 && head[5] & 0x10 != 0 {
        10
    } else {
        0
    };
    Some(10 + size + footer)
}

/// Undoes ID3v2 unsynchronisation: every `FF 00` becomes `FF`.
fn de_unsync(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        out.push(b[i]);
        if b[i] == 0xFF && b.get(i + 1) == Some(&0) {
            i += 1;
        }
        i += 1;
    }
    out
}

/// Decodes a text in one of ID3v2's four encodings (0 Latin-1, 1 UTF-16 with BOM, 2 UTF-16BE,
/// 3 UTF-8).
fn id3_text(encoding: u8, b: &[u8]) -> String {
    match encoding {
        0 => latin1(b),
        1 => match b {
            [0xFF, 0xFE, rest @ ..] => utf16(rest, false),
            [0xFE, 0xFF, rest @ ..] => utf16(rest, true),
            _ => utf16(b, false),
        },
        2 => utf16(b, true),
        _ => String::from_utf8_lossy(b).into_owned(),
    }
}

/// Splits `b` at the encoding's string terminator (`00`, or `00 00` on a UTF-16 boundary).
fn split_terminated(encoding: u8, b: &[u8]) -> (&[u8], &[u8]) {
    if encoding == 1 || encoding == 2 {
        let mut i = 0;
        while i + 1 < b.len() {
            if b[i] == 0 && b[i + 1] == 0 {
                return (&b[..i], &b[i + 2..]);
            }
            i += 2;
        }
        (b, &[])
    } else {
        match b.iter().position(|&c| c == 0) {
            Some(i) => (&b[..i], &b[i + 1..]),
            None => (b, &[]),
        }
    }
}

/// A text frame's value: the encoding byte, then one or more null-separated strings (2.4 allows
/// several), joined with `, `.
fn id3_text_frame(frame: &[u8]) -> Option<String> {
    let (&encoding, body) = frame.split_first()?;
    if body.len() > TEXT_CAP {
        return None;
    }
    let text = id3_text(encoding, body);
    let joined = text
        .split('\0')
        .map(clean)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    Some(joined)
}

#[derive(Default)]
struct Id3State {
    /// A `COMM` with an empty description was taken; a described one no longer replaces it.
    comment_plain: bool,
    picture_kind: Option<u32>,
}

/// Parses an ID3v2 tag that `data` starts with (as much of it as `data` holds).
fn parse_id3v2(data: &[u8], tags: &mut Tags) {
    let Some(total) = id3v2_total_len(data) else {
        return;
    };
    let major = data[3];
    let flags = data[5];
    if major == 2 && flags & 0x40 != 0 {
        return; // 2.2 "compression": a scheme never defined, so nothing to read.
    }
    let body_end = total
        .saturating_sub(if major == 4 && flags & 0x10 != 0 {
            10
        } else {
            0
        })
        .min(data.len());
    let body = &data[10.min(body_end)..body_end];
    let unsynced_tag = flags & 0x80 != 0;
    let owned;
    let mut body = if unsynced_tag && major < 4 {
        owned = de_unsync(body);
        &owned[..]
    } else {
        body
    };
    if flags & 0x40 != 0 && major >= 3 {
        let skip = if major == 3 {
            be32(body).map(|n| n as usize + 4)
        } else {
            syncsafe(body).map(|n| n as usize)
        };
        match skip.and_then(|n| body.get(n..)) {
            Some(rest) => body = rest,
            None => return,
        }
    }
    let mut state = Id3State::default();
    let header_len = if major == 2 { 6 } else { 10 };
    let mut pos = 0usize;
    while pos + header_len <= body.len() {
        let header = &body[pos..pos + header_len];
        if header[0] == 0 {
            break; // padding
        }
        let (id, size) = if major == 2 {
            (&header[..3], be24(&header[3..]).unwrap_or(0) as usize)
        } else if major == 3 {
            (&header[..4], be32(&header[4..]).unwrap_or(0) as usize)
        } else {
            (&header[..4], syncsafe(&header[4..]).unwrap_or(0) as usize)
        };
        let frame_flags = if major >= 3 { header[9] } else { 0 };
        pos += header_len;
        let Some(end) = pos.checked_add(size).filter(|&e| e <= body.len()) else {
            break;
        };
        let mut frame = &body[pos..end];
        pos = end;
        let owned_frame;
        match major {
            3 => {
                if frame_flags & 0xC0 != 0 {
                    continue; // compressed or encrypted
                }
                if frame_flags & 0x20 != 0 {
                    frame = frame.get(1..).unwrap_or(&[]);
                }
            }
            4 => {
                if frame_flags & 0x0C != 0 {
                    continue;
                }
                if frame_flags & 0x40 != 0 {
                    frame = frame.get(1..).unwrap_or(&[]);
                }
                if frame_flags & 0x01 != 0 {
                    frame = frame.get(4..).unwrap_or(&[]);
                }
                if frame_flags & 0x02 != 0 || unsynced_tag {
                    owned_frame = de_unsync(frame);
                    frame = &owned_frame;
                }
            }
            _ => {}
        }
        id3_frame(id, frame, tags, &mut state);
    }
}

/// Applies one frame. 2.2 three-letter ids are mapped onto their 2.3 names first.
fn id3_frame(raw_id: &[u8], frame: &[u8], tags: &mut Tags, state: &mut Id3State) {
    let id: &[u8] = match raw_id {
        b"TT2" => b"TIT2",
        b"TP1" => b"TPE1",
        b"TAL" => b"TALB",
        b"TP2" => b"TPE2",
        b"TCO" => b"TCON",
        b"TYE" => b"TYER",
        b"TRK" => b"TRCK",
        b"TPA" => b"TPOS",
        b"COM" => b"COMM",
        b"PIC" => b"APIC",
        b"TLE" => b"TLEN",
        other => other,
    };
    match id {
        b"TIT2" | b"TPE1" | b"TALB" | b"TPE2" | b"TCON" | b"TYER" | b"TDRC" | b"TRCK" | b"TPOS"
        | b"TLEN" => {
            let Some(text) = id3_text_frame(frame) else {
                return;
            };
            match id {
                b"TIT2" => set_text(&mut tags.title, &text),
                b"TPE1" => set_text(&mut tags.artist, &text),
                b"TALB" => set_text(&mut tags.album, &text),
                b"TPE2" => set_text(&mut tags.album_artist, &text),
                b"TCON" => set(&mut tags.genre, id3_genre(&text)),
                b"TYER" | b"TDRC" => set(&mut tags.year, leading_year(&text)),
                b"TRCK" => {
                    let (n, total) = number_pair(&text);
                    set(&mut tags.track, n);
                    set(&mut tags.track_total, total);
                }
                b"TPOS" => {
                    let (n, total) = number_pair(&text);
                    set(&mut tags.disc, n);
                    set(&mut tags.disc_total, total);
                }
                _ => {
                    let ms = leading_number(&text).filter(|&ms| ms > 0);
                    set(&mut tags.duration_secs, ms.map(|ms| ms as f64 / 1000.0));
                }
            }
        }
        b"COMM" => {
            let Some((&encoding, rest)) = frame.split_first() else {
                return;
            };
            let Some(rest) = rest.get(3..) else { return }; // language
            let (description, text) = split_terminated(encoding, rest);
            if text.len() > TEXT_CAP {
                return;
            }
            let plain = description.is_empty();
            if tags.comment.is_none() || (plain && !state.comment_plain) {
                let text = id3_text(encoding, text);
                let text = clean(&text);
                if !text.is_empty() {
                    tags.comment = Some(text.to_string());
                    state.comment_plain = plain;
                }
            }
        }
        b"APIC" => {
            let Some((&encoding, rest)) = frame.split_first() else {
                return;
            };
            let (mime, rest) = if raw_id == b"PIC" {
                // 2.2: a three-letter image format where 2.3 has a MIME type.
                let Some(format) = rest.get(..3) else { return };
                let mime = match latin1(format).to_ascii_uppercase().as_str() {
                    "JPG" => "image/jpeg".to_string(),
                    "PNG" => "image/png".to_string(),
                    other => format!("image/{}", other.to_ascii_lowercase()),
                };
                (mime, &rest[3..])
            } else {
                let (mime, rest) = split_terminated(0, rest);
                (latin1(mime), rest)
            };
            let Some((&kind, rest)) = rest.split_first() else {
                return;
            };
            let (_description, data) = split_terminated(encoding, rest);
            offer_picture(
                tags,
                &mut state.picture_kind,
                kind as u32,
                picture(&mime, data),
            );
        }
        _ => {}
    }
}

/// Fills fields an ID3v2 tag left empty from a 128-byte ID3v1 tag.
fn parse_id3v1(tag: &[u8], tags: &mut Tags) {
    if tag.len() != 128 || &tag[..3] != b"TAG" {
        return;
    }
    set_text(&mut tags.title, &latin1(&tag[3..33]));
    set_text(&mut tags.artist, &latin1(&tag[33..63]));
    set_text(&mut tags.album, &latin1(&tag[63..93]));
    set(&mut tags.year, leading_year(&latin1(&tag[93..97])));
    let comment = if tag[125] == 0 && tag[126] != 0 {
        set(&mut tags.track, Some(tag[126] as u32));
        &tag[97..125]
    } else {
        &tag[97..127]
    };
    set_text(&mut tags.comment, &latin1(comment));
    set(&mut tags.genre, genre_name(tag[127] as usize));
}

// ---------------------------------------------------------------------------------------------
// MP3
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MpegVersion {
    V1,
    V2,
    V25,
}

#[derive(Clone, Copy, Debug)]
struct MpegFrame {
    version: MpegVersion,
    layer: u8,
    bitrate_kbps: u32,
    sample_rate: u32,
    channels: u8,
    samples_per_frame: u32,
    frame_len: usize,
    /// Offset of a Xing/Info header from the frame start (after the side info).
    xing_offset: usize,
}

const BITRATES_V1: [[u32; 15]; 3] = [
    [
        0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
    ],
    [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
    ],
    [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ],
];
const BITRATES_V2: [[u32; 15]; 3] = [
    [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
    ],
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
];

/// Parses a 4-byte MPEG audio frame header; free-format and reserved values read as none.
fn mpeg_header(b: &[u8]) -> Option<MpegFrame> {
    let h = be32(b)?;
    if h & 0xFFE0_0000 != 0xFFE0_0000 {
        return None;
    }
    let version = match (h >> 19) & 3 {
        3 => MpegVersion::V1,
        2 => MpegVersion::V2,
        0 => MpegVersion::V25,
        _ => return None,
    };
    let layer = match (h >> 17) & 3 {
        0 => return None,
        bits => 4 - bits as u8,
    };
    let bitrate_index = ((h >> 12) & 15) as usize;
    if bitrate_index == 0 || bitrate_index == 15 {
        return None;
    }
    let rate_index = ((h >> 10) & 3) as usize;
    if rate_index == 3 {
        return None;
    }
    let padding = (h >> 9) & 1;
    let channels = if (h >> 6) & 3 == 3 { 1 } else { 2 };
    let table = if version == MpegVersion::V1 {
        &BITRATES_V1
    } else {
        &BITRATES_V2
    };
    let bitrate_kbps = table[layer as usize - 1][bitrate_index];
    let sample_rate = match version {
        MpegVersion::V1 => [44100, 48000, 32000][rate_index],
        MpegVersion::V2 => [22050, 24000, 16000][rate_index],
        MpegVersion::V25 => [11025, 12000, 8000][rate_index],
    };
    let samples_per_frame = match (layer, version) {
        (1, _) => 384,
        (2, _) | (_, MpegVersion::V1) => 1152,
        _ => 576,
    };
    let slot = if layer == 1 { 4 } else { 1 };
    let frame_len =
        (samples_per_frame / 8 * bitrate_kbps * 1000 / sample_rate + padding * slot) as usize;
    let xing_offset = 4 + match (version, channels) {
        (MpegVersion::V1, 1) => 17,
        (MpegVersion::V1, _) => 32,
        (_, 1) => 9,
        _ => 17,
    };
    Some(MpegFrame {
        version,
        layer,
        bitrate_kbps,
        sample_rate,
        channels,
        samples_per_frame,
        frame_len,
        xing_offset,
    })
}

/// Finds the first frame header in `window`, requiring the frame after it to agree (when the
/// window reaches that far) so a stray `FF Ex` in junk does not pass.
fn find_frame(window: &[u8]) -> Option<(usize, MpegFrame)> {
    for i in 0..window.len().saturating_sub(4) {
        let Some(frame) = mpeg_header(&window[i..]) else {
            continue;
        };
        let next = i + frame.frame_len;
        if next + 4 <= window.len() {
            match mpeg_header(&window[next..]) {
                Some(n)
                    if n.version == frame.version
                        && n.layer == frame.layer
                        && n.sample_rate == frame.sample_rate => {}
                _ => continue,
            }
        }
        return Some((i, frame));
    }
    None
}

fn parse_mp3<R: Read + Seek>(r: &mut R, len: u64, tags: &mut Tags) -> io::Result<()> {
    let head = read_at(r, 0, 10)?;
    let mut audio_start = 0u64;
    if let Some(total) = id3v2_total_len(&head) {
        tags.format = Format::Mp3;
        let tag = read_at(r, 0, total.min(BLOCK_CAP))?;
        parse_id3v2(&tag, tags);
        audio_start = total as u64;
        if audio_start < len && read_at(r, audio_start, 4)? == b"fLaC" {
            tags.format = Format::Flac;
            return parse_flac(r, audio_start, len, tags);
        }
    }
    let mut has_v1 = false;
    if len >= audio_start + 128 {
        let tail = read_at(r, len - 128, 128)?;
        if tail.len() == 128 && &tail[..3] == b"TAG" {
            has_v1 = true;
            tags.format = Format::Mp3;
            parse_id3v1(&tail, tags);
        }
    }
    if audio_start >= len {
        return Ok(());
    }
    let window = read_at(r, audio_start, FRAME_SCAN)?;
    let Some((offset, frame)) = find_frame(&window) else {
        return Ok(());
    };
    tags.format = Format::Mp3;
    tags.sample_rate = Some(frame.sample_rate);
    tags.channels = Some(frame.channels);
    let audio_bytes =
        len.saturating_sub(audio_start + offset as u64 + if has_v1 { 128 } else { 0 });
    let body = &window[offset..];
    // Xing/Info (VBR and often CBR) or VBRI (Fraunhofer): a frame count for the duration.
    let mut frames: Option<u32> = None;
    let mut bytes: Option<u32> = None;
    if let Some(tag) = body.get(frame.xing_offset..frame.xing_offset + 4)
        && (tag == b"Xing" || tag == b"Info")
    {
        let flags = be32(&body[frame.xing_offset + 4..]).unwrap_or(0);
        let mut at = frame.xing_offset + 8;
        if flags & 1 != 0 {
            frames = be32(&body[at..]);
            at += 4;
        }
        if flags & 2 != 0 {
            bytes = be32(&body[at..]);
        }
    } else if body.get(36..40) == Some(b"VBRI") {
        bytes = be32(&body[46..]);
        frames = be32(&body[50..]);
    }
    let vbr_secs = frames
        .filter(|&f| f > 0)
        .map(|f| f as f64 * frame.samples_per_frame as f64 / frame.sample_rate as f64);
    if let Some(secs) = vbr_secs {
        tags.duration_secs = Some(secs);
        let stream_bytes = bytes
            .filter(|&b| b > 0)
            .map(|b| b as u64)
            .unwrap_or(audio_bytes);
        tags.bitrate_kbps = Some((stream_bytes as f64 * 8.0 / secs / 1000.0).round() as u32);
    } else {
        tags.bitrate_kbps = Some(frame.bitrate_kbps);
        if tags.duration_secs.is_none() && frame.bitrate_kbps > 0 {
            tags.duration_secs =
                Some(audio_bytes as f64 * 8.0 / (frame.bitrate_kbps as f64 * 1000.0));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// MP4 / M4A
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Mp4State {
    mdat_bytes: u64,
    picture_kind: Option<u32>,
    /// The `esds` average bitrate, used when no `mdat` gives the measured one.
    declared_bitrate: Option<u32>,
}

fn parse_mp4<R: Read + Seek>(r: &mut R, len: u64, tags: &mut Tags) -> io::Result<()> {
    tags.format = Format::Mp4;
    let mut state = Mp4State::default();
    mp4_walk(r, 0, len, 0, tags, &mut state)?;
    if let Some(secs) = tags.duration_secs.filter(|&s| s > 0.0)
        && state.mdat_bytes > 0
    {
        tags.bitrate_kbps = Some((state.mdat_bytes as f64 * 8.0 / secs / 1000.0).round() as u32);
    }
    set(&mut tags.bitrate_kbps, state.declared_bitrate);
    Ok(())
}

/// Walks the atoms in `start..end`, descending into the containers on the way to `mvhd`, `stsd`
/// and `ilst`, and reading only those leaves.
fn mp4_walk<R: Read + Seek>(
    r: &mut R,
    start: u64,
    end: u64,
    depth: u8,
    tags: &mut Tags,
    state: &mut Mp4State,
) -> io::Result<()> {
    let mut pos = start;
    while pos + 8 <= end {
        let header = read_at(r, pos, 16)?;
        let Some(size32) = be32(&header) else { break };
        let Some(name) = header.get(4..8) else { break };
        let name: [u8; 4] = name.try_into().unwrap_or([0; 4]);
        let mut header_len = 8u64;
        let size = match size32 {
            0 => end - pos,
            1 => {
                header_len = 16;
                match be64(&header[8..]) {
                    Some(s) => s,
                    None => break,
                }
            }
            s => s as u64,
        };
        if size < header_len {
            break;
        }
        let body_start = pos + header_len;
        let body_end = pos.saturating_add(size).min(end);
        match &name {
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"udta" if depth < 8 => {
                mp4_walk(r, body_start, body_end, depth + 1, tags, state)?;
            }
            b"meta" if depth < 8 => {
                // A full atom (4 bytes of version/flags) in iTunes files; a plain one elsewhere.
                let peek = read_at(r, body_start, 4)?;
                let skip = if peek.len() == 4 && be32(&peek) == Some(0) {
                    4
                } else {
                    0
                };
                mp4_walk(r, body_start + skip, body_end, depth + 1, tags, state)?;
            }
            b"ilst" => mp4_ilst(r, body_start, body_end, tags, state)?,
            b"mvhd" => {
                let body = read_at(r, body_start, 32)?;
                let (timescale, duration) = if body.first() == Some(&1) {
                    (
                        be32(&body[20.min(body.len())..]),
                        be64(&body[24.min(body.len())..]),
                    )
                } else {
                    (
                        be32(&body[12.min(body.len())..]),
                        be32(&body[16.min(body.len())..]).map(|d| d as u64),
                    )
                };
                if let (Some(ts), Some(d)) = (timescale, duration)
                    && ts > 0
                    && d != u32::MAX as u64
                    && d != u64::MAX
                {
                    tags.duration_secs = Some(d as f64 / ts as f64);
                }
            }
            b"stsd" => {
                let body = read_at(
                    r,
                    body_start,
                    4096.min(body_end.saturating_sub(body_start) as usize),
                )?;
                mp4_stsd(&body, tags, state);
            }
            b"mdat" => state.mdat_bytes = state.mdat_bytes.saturating_add(size - header_len),
            _ => {}
        }
        pos = pos.saturating_add(size);
    }
    Ok(())
}

/// The `stsd` body: an `mp4a` (or `alac`, `samr`…) sample entry's channel count and sample rate,
/// corrected from the AAC `esds` configuration when the entry carries one (encoders write 2
/// channels into the entry whatever the stream holds).
fn mp4_stsd(body: &[u8], tags: &mut Tags, state: &mut Mp4State) {
    // version/flags (4), entry count (4), then the first entry: size (4), format (4),
    // reserved (6), data reference index (2), version (2), revision (2), vendor (4),
    // channels (2), sample size (2), compression id (2), packet size (2), rate (16.16),
    // then child boxes (`esds` for AAC).
    let Some(entry) = body.get(8..) else { return };
    let Some(format) = entry.get(4..8) else {
        return;
    };
    if !format.iter().all(|c| c.is_ascii_graphic()) {
        return;
    }
    let mut channels = be16(&entry[24.min(entry.len())..])
        .filter(|&c| (1..=64).contains(&c))
        .map(|c| c as u8);
    let mut rate = be32(&entry[32.min(entry.len())..])
        .map(|r| r >> 16)
        .filter(|&r| r > 0);
    let entry_len = be32(entry)
        .map(|n| n as usize)
        .unwrap_or(0)
        .min(entry.len());
    let mut pos = 36usize;
    while pos + 8 <= entry_len {
        let Some(size) = be32(&entry[pos..])
            .map(|n| n as usize)
            .filter(|&n| n >= 8 && pos + n <= entry_len)
        else {
            break;
        };
        if &entry[pos + 4..pos + 8] == b"esds" {
            let (c, r) = mp4_esds(&entry[pos + 12..pos + size], state);
            if c.is_some() {
                channels = c;
            }
            if r.is_some() {
                rate = r;
            }
            break;
        }
        pos += size;
    }
    set(&mut tags.channels, channels);
    set(&mut tags.sample_rate, rate);
}

/// Walks the MPEG-4 descriptors of an `esds` body (after version/flags) to the
/// AudioSpecificConfig: `(channels, sample rate)` where the config states them, and the
/// declared average bitrate into `state`.
fn mp4_esds(b: &[u8], state: &mut Mp4State) -> (Option<u8>, Option<u32>) {
    let mut pos = 0usize;
    while pos < b.len() {
        let tag = b[pos];
        pos += 1;
        // Expandable size: up to four bytes of seven bits, the top bit continuing.
        let mut size = 0usize;
        for _ in 0..4 {
            let Some(&c) = b.get(pos) else {
                return (None, None);
            };
            pos += 1;
            size = (size << 7) | (c & 0x7F) as usize;
            if c & 0x80 == 0 {
                break;
            }
        }
        match tag {
            0x03 => {
                // ES_Descriptor: ES_ID (2), flags (1), then the optional fields the flags name;
                // the DecoderConfigDescriptor follows inside.
                let flags = b.get(pos + 2).copied().unwrap_or(0);
                pos += 3;
                if flags & 0x80 != 0 {
                    pos += 2;
                }
                if flags & 0x40 != 0 {
                    pos += 1 + b.get(pos).copied().unwrap_or(0) as usize;
                }
                if flags & 0x20 != 0 {
                    pos += 2;
                }
            }
            0x04 => {
                // DecoderConfigDescriptor: object type (1), stream type (1), buffer size (3),
                // max bitrate (4), average bitrate (4), then the DecoderSpecificInfo.
                if let Some(avg) = be32(&b[(pos + 9).min(b.len())..]).filter(|&a| a > 0) {
                    state.declared_bitrate = Some((avg as f64 / 1000.0).round() as u32);
                }
                pos += 13;
            }
            0x05 => {
                // AudioSpecificConfig: object type (5 bits, 31 → 6 more), frequency index
                // (4 bits, 15 → 24 bits of rate), channel configuration (4 bits).
                let Some(cfg) = b.get(pos..) else {
                    return (None, None);
                };
                let mut bits = [0u8; 8];
                let n = cfg.len().min(8);
                bits[..n].copy_from_slice(&cfg[..n]);
                let mut word = u64::from_be_bytes(bits);
                let mut take = |count: u32| -> u64 {
                    let v = word >> (64 - count);
                    word <<= count;
                    v
                };
                if take(5) == 31 {
                    take(6);
                }
                let rate = match take(4) {
                    15 => Some(take(24) as u32).filter(|&r| r > 0),
                    index => [
                        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000,
                        11025, 8000, 7350,
                    ]
                    .get(index as usize)
                    .copied(),
                };
                let channels = match take(4) {
                    0 => None, // in-stream program config
                    7 => Some(8),
                    c => Some(c as u8),
                };
                return (channels, rate);
            }
            _ => pos += size,
        }
    }
    (None, None)
}

/// The children of `ilst`: each is an atom named after the field, holding `data` atoms.
fn mp4_ilst<R: Read + Seek>(
    r: &mut R,
    start: u64,
    end: u64,
    tags: &mut Tags,
    state: &mut Mp4State,
) -> io::Result<()> {
    let mut pos = start;
    while pos + 8 <= end {
        let header = read_at(r, pos, 8)?;
        let Some(size) = be32(&header).map(|s| s as u64) else {
            break;
        };
        if size < 8 || pos.saturating_add(size) > end {
            break;
        }
        let name: [u8; 4] = header[4..8].try_into().unwrap_or([0; 4]);
        let body_len = (size - 8) as usize;
        if body_len <= BLOCK_CAP && &name != b"----" {
            let body = read_at(r, pos + 8, body_len)?;
            mp4_ilst_item(&name, &body, tags, state);
        }
        pos += size;
    }
    Ok(())
}

fn mp4_ilst_item(name: &[u8; 4], body: &[u8], tags: &mut Tags, state: &mut Mp4State) {
    // The first `data` atom of the item.
    let mut pos = 0usize;
    while pos + 16 <= body.len() {
        let Some(size) = be32(&body[pos..]).map(|s| s as usize) else {
            return;
        };
        if size < 16 || pos + size > body.len() {
            return;
        }
        if &body[pos + 4..pos + 8] == b"data" {
            let kind = be32(&body[pos + 8..]).unwrap_or(0) & 0x00FF_FFFF;
            let payload = &body[pos + 16..pos + size];
            mp4_data(name, kind, payload, tags, state);
            return;
        }
        pos += size;
    }
}

fn mp4_data(name: &[u8; 4], kind: u32, payload: &[u8], tags: &mut Tags, state: &mut Mp4State) {
    let text = || -> Option<String> {
        if kind == 1 && payload.len() <= TEXT_CAP {
            Some(String::from_utf8_lossy(payload).into_owned())
        } else {
            None
        }
    };
    match name {
        b"\xA9nam" => set_text(&mut tags.title, &text().unwrap_or_default()),
        b"\xA9ART" => set_text(&mut tags.artist, &text().unwrap_or_default()),
        b"\xA9alb" => set_text(&mut tags.album, &text().unwrap_or_default()),
        b"aART" => set_text(&mut tags.album_artist, &text().unwrap_or_default()),
        b"\xA9gen" => set_text(&mut tags.genre, &text().unwrap_or_default()),
        b"\xA9cmt" => set_text(&mut tags.comment, &text().unwrap_or_default()),
        b"\xA9day" => set(&mut tags.year, text().as_deref().and_then(leading_year)),
        b"gnre" => {
            let index = be16(payload).map(|n| n as usize).filter(|&n| n > 0);
            set(&mut tags.genre, index.and_then(|n| genre_name(n - 1)));
        }
        b"trkn" | b"disk" => {
            let number = be16(&payload[2.min(payload.len())..])
                .filter(|&n| n > 0)
                .map(|n| n as u32);
            let total = be16(&payload[4.min(payload.len())..])
                .filter(|&n| n > 0)
                .map(|n| n as u32);
            if name == b"trkn" {
                set(&mut tags.track, number);
                set(&mut tags.track_total, total);
            } else {
                set(&mut tags.disc, number);
                set(&mut tags.disc_total, total);
            }
        }
        b"covr" => {
            let mime = match kind {
                13 => "image/jpeg",
                14 => "image/png",
                27 => "image/bmp",
                _ => "",
            };
            offer_picture(tags, &mut state.picture_kind, 3, picture(mime, payload));
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------------
// FLAC and Vorbis comments
// ---------------------------------------------------------------------------------------------

fn parse_flac<R: Read + Seek>(r: &mut R, start: u64, len: u64, tags: &mut Tags) -> io::Result<()> {
    tags.format = Format::Flac;
    let mut best_picture = None;
    let mut pos = start + 4;
    loop {
        let header = read_at(r, pos, 4)?;
        if header.len() < 4 {
            break;
        }
        let last = header[0] & 0x80 != 0;
        let kind = header[0] & 0x7F;
        let block_len = be24(&header[1..]).unwrap_or(0) as usize;
        let body_pos = pos + 4;
        match kind {
            0 => {
                let body = read_at(r, body_pos, block_len.min(34))?;
                flac_streaminfo(&body, tags);
            }
            4 if block_len <= BLOCK_CAP => {
                let body = read_at(r, body_pos, block_len)?;
                parse_vorbis_comments(&body, tags, &mut best_picture);
            }
            6 if block_len <= BLOCK_CAP => {
                let body = read_at(r, body_pos, block_len)?;
                if let Some((kind, pic)) = flac_picture(&body) {
                    offer_picture(tags, &mut best_picture, kind, Some(pic));
                }
            }
            _ => {}
        }
        pos = body_pos + block_len as u64;
        if last || pos >= len {
            break;
        }
    }
    if let Some(secs) = tags.duration_secs.filter(|&s| s > 0.0) {
        tags.bitrate_kbps =
            Some((len.saturating_sub(start) as f64 * 8.0 / secs / 1000.0).round() as u32);
    }
    Ok(())
}

fn flac_streaminfo(body: &[u8], tags: &mut Tags) {
    // min/max block size (2+2), min/max frame size (3+3), then 64 bits: sample rate (20),
    // channels-1 (3), bits per sample-1 (5), total samples (36).
    let Some(packed) = be64(&body[10.min(body.len())..]) else {
        return;
    };
    let sample_rate = (packed >> 44) as u32;
    let channels = ((packed >> 41) & 7) as u8 + 1;
    let total_samples = packed & 0xF_FFFF_FFFF;
    if sample_rate > 0 {
        tags.sample_rate = Some(sample_rate);
        tags.channels = Some(channels);
        if total_samples > 0 {
            tags.duration_secs = Some(total_samples as f64 / sample_rate as f64);
        }
    }
}

/// A FLAC `PICTURE` block body: `(picture type, picture)`.
fn flac_picture(body: &[u8]) -> Option<(u32, Picture)> {
    let kind = be32(body)?;
    let mime_len = be32(&body[4..])? as usize;
    let mime = body.get(8..8 + mime_len)?;
    let mut at = 8 + mime_len;
    let description_len = be32(body.get(at..)?)? as usize;
    at = at.checked_add(4 + description_len)?;
    at = at.checked_add(16)?; // width, height, depth, colors
    let data_len = be32(body.get(at..)?)? as usize;
    let data = body.get(at + 4..(at + 4).checked_add(data_len)?)?;
    Some((kind, picture(&latin1(mime), data)?))
}

/// A Vorbis comment block (FLAC `VORBIS_COMMENT`, the Vorbis comment packet after its 7-byte
/// marker, `OpusTags` after its 8-byte marker).
fn parse_vorbis_comments(body: &[u8], tags: &mut Tags, best_picture: &mut Option<u32>) {
    let Some(vendor_len) = le32(body).map(|n| n as usize) else {
        return;
    };
    let mut pos = 4usize.saturating_add(vendor_len);
    let Some(count) = le32(&body[pos.min(body.len())..]) else {
        return;
    };
    pos += 4;
    for _ in 0..count.min(4096) {
        let Some(entry_len) = le32(&body[pos.min(body.len())..]).map(|n| n as usize) else {
            return;
        };
        pos += 4;
        let Some(entry) = pos
            .checked_add(entry_len)
            .and_then(|end| body.get(pos..end))
        else {
            return;
        };
        pos += entry_len;
        let Some(eq) = entry.iter().position(|&c| c == b'=') else {
            continue;
        };
        let key = entry[..eq].to_ascii_uppercase();
        let value = &entry[eq + 1..];
        if key == b"METADATA_BLOCK_PICTURE" {
            if value.len() <= BLOCK_CAP / 3 * 4
                && let Some(block) = base64_decode(value)
                && let Some((kind, pic)) = flac_picture(&block)
            {
                offer_picture(tags, best_picture, kind, Some(pic));
            }
            continue;
        }
        if value.len() > TEXT_CAP {
            continue;
        }
        let value = String::from_utf8_lossy(value);
        vorbis_field(&key, &value, tags);
    }
}

fn vorbis_field(key: &[u8], value: &str, tags: &mut Tags) {
    match key {
        b"TITLE" => set_text(&mut tags.title, value),
        b"ARTIST" => set_text(&mut tags.artist, value),
        b"ALBUM" => set_text(&mut tags.album, value),
        b"ALBUMARTIST" | b"ALBUM ARTIST" | b"ALBUM_ARTIST" => {
            set_text(&mut tags.album_artist, value)
        }
        b"GENRE" => set_text(&mut tags.genre, value),
        b"DATE" | b"YEAR" => set(&mut tags.year, leading_year(value)),
        b"TRACKNUMBER" => {
            let (n, total) = number_pair(value);
            set(&mut tags.track, n);
            set(&mut tags.track_total, total);
        }
        b"TRACKTOTAL" | b"TOTALTRACKS" => set(&mut tags.track_total, leading_number(value)),
        b"DISCNUMBER" => {
            let (n, total) = number_pair(value);
            set(&mut tags.disc, n);
            set(&mut tags.disc_total, total);
        }
        b"DISCTOTAL" | b"TOTALDISCS" => set(&mut tags.disc_total, leading_number(value)),
        b"COMMENT" | b"DESCRIPTION" => set_text(&mut tags.comment, value),
        _ => {}
    }
}

/// Standard (and URL-safe) base64 with optional padding; whitespace is skipped, anything else
/// fails the decode.
fn base64_decode(s: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for &c in s {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b'\n' | b'\r' | b' ' | b'\t' => continue,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------------
// Ogg (Vorbis, Opus)
// ---------------------------------------------------------------------------------------------

fn parse_ogg<R: Read + Seek>(r: &mut R, len: u64, tags: &mut Tags) -> io::Result<()> {
    // The first two packets (identification, comments), assembled across pages by the lacing
    // values: a segment of 255 continues the packet into the next segment or page.
    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    let mut pos = 0u64;
    let mut pages = 0;
    while packets.len() < 2 && pos + 27 <= len && pages < 1024 {
        pages += 1;
        let header = read_at(r, pos, 27)?;
        if header.len() < 27 || &header[..4] != b"OggS" {
            break;
        }
        let segments = header[26] as usize;
        let lacing = read_at(r, pos + 27, segments)?;
        if lacing.len() < segments {
            break;
        }
        let body_len: usize = lacing.iter().map(|&l| l as usize).sum();
        let body = read_at(r, pos + 27 + segments as u64, body_len)?;
        let mut at = 0usize;
        for &lace in &lacing {
            let lace = lace as usize;
            let Some(segment) = body.get(at..at + lace) else {
                break;
            };
            at += lace;
            if current.len() + lace > BLOCK_CAP {
                return finish_ogg(r, len, &packets, tags);
            }
            current.extend_from_slice(segment);
            if lace < 255 {
                packets.push(std::mem::take(&mut current));
                if packets.len() == 2 {
                    break;
                }
            }
        }
        pos += 27 + segments as u64 + body_len as u64;
    }
    finish_ogg(r, len, &packets, tags)
}

fn finish_ogg<R: Read + Seek>(
    r: &mut R,
    len: u64,
    packets: &[Vec<u8>],
    tags: &mut Tags,
) -> io::Result<()> {
    let Some(first) = packets.first() else {
        return Ok(());
    };
    let mut pre_skip = 0u64;
    let mut nominal_bitrate = None;
    // Granule positions count samples at this rate (Opus always at 48 kHz).
    let granule_rate = if let Some(body) = first.strip_prefix(b"\x01vorbis") {
        tags.format = Format::OggVorbis;
        tags.channels = body.get(4).copied().filter(|&c| c > 0);
        let rate = le32(&body[5.min(body.len())..]).filter(|&r| r > 0);
        tags.sample_rate = rate;
        nominal_bitrate = le32(&body[13.min(body.len())..])
            .map(|b| b as i32)
            .filter(|&b| b > 0);
        rate.unwrap_or(0) as f64
    } else if let Some(body) = first.strip_prefix(b"OpusHead") {
        tags.format = Format::Opus;
        tags.channels = body.get(1).copied().filter(|&c| c > 0);
        pre_skip = le16(&body[2.min(body.len())..]).unwrap_or(0) as u64;
        let input_rate = le32(&body[4.min(body.len())..]).filter(|&r| r > 0);
        tags.sample_rate = Some(input_rate.unwrap_or(48000));
        48000.0
    } else {
        return Ok(());
    };
    if let Some(second) = packets.get(1) {
        let comments = match tags.format {
            Format::OggVorbis => second.strip_prefix(b"\x03vorbis"),
            _ => second.strip_prefix(b"OpusTags"),
        };
        if let Some(comments) = comments {
            let mut best_picture = None;
            parse_vorbis_comments(comments, tags, &mut best_picture);
        }
    }
    // The last page's granule position is the stream's length in samples.
    let tail_start = len.saturating_sub(OGG_TAIL);
    let tail = read_at(r, tail_start, (len - tail_start) as usize)?;
    let mut granule = None;
    let mut i = tail.len();
    while i >= 27 {
        i -= 1;
        if tail[i..].starts_with(b"OggS") && tail.get(i + 4) == Some(&0) {
            if let Some(g) = tail
                .get(i + 6..i + 14)
                .map(|g| i64::from_le_bytes(g.try_into().unwrap_or([0; 8])))
            {
                granule = Some(g);
            }
            break;
        }
    }
    if let Some(granule) = granule.filter(|&g| g > 0)
        && granule_rate > 0.0
    {
        let samples = (granule as u64).saturating_sub(pre_skip);
        let secs = samples as f64 / granule_rate;
        if secs > 0.0 {
            tags.duration_secs = Some(secs);
            let bitrate = match nominal_bitrate {
                Some(b) => (b as f64 / 1000.0).round() as u32,
                None => (len as f64 * 8.0 / secs / 1000.0).round() as u32,
            };
            tags.bitrate_kbps = Some(bitrate);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// WAV and AIFF
// ---------------------------------------------------------------------------------------------

fn parse_wav<R: Read + Seek>(r: &mut R, len: u64, tags: &mut Tags) -> io::Result<()> {
    tags.format = Format::Wav;
    let mut byte_rate = 0u64;
    let mut data_bytes = 0u64;
    let mut pos = 12u64;
    while pos + 8 <= len {
        let header = read_at(r, pos, 8)?;
        let Some(size) = le32(&header[4.min(header.len())..]).map(|s| s as u64) else {
            break;
        };
        let body = pos + 8;
        match &header[..4] {
            b"fmt " => {
                let fmt = read_at(r, body, size.min(40) as usize)?;
                // format tag (2), channels (2), sample rate (4), byte rate (4), block align, bits.
                let channels = le16(&fmt[2.min(fmt.len())..]).filter(|&c| c > 0);
                let sample_rate = le32(&fmt[4.min(fmt.len())..]).filter(|&r| r > 0);
                byte_rate = le32(&fmt[8.min(fmt.len())..]).unwrap_or(0) as u64;
                set(&mut tags.channels, channels.map(|c| c.min(255) as u8));
                set(&mut tags.sample_rate, sample_rate);
                if byte_rate > 0 {
                    tags.bitrate_kbps = Some((byte_rate as f64 * 8.0 / 1000.0).round() as u32);
                }
            }
            b"data" => {
                data_bytes = if size == 0 || size == u32::MAX as u64 {
                    len.saturating_sub(body)
                } else {
                    size
                };
            }
            b"LIST" => {
                let list = read_at(r, body, size.min(TEXT_CAP as u64) as usize)?;
                if list.get(..4) == Some(b"INFO") {
                    riff_info(&list[4..], tags);
                }
            }
            b"id3 " | b"ID3 " => {
                let tag = read_at(r, body, size.min(BLOCK_CAP as u64) as usize)?;
                parse_id3v2(&tag, tags);
            }
            _ => {}
        }
        pos = body.saturating_add(size).saturating_add(size & 1);
    }
    if byte_rate > 0 && data_bytes > 0 {
        tags.duration_secs = Some(data_bytes as f64 / byte_rate as f64);
    }
    Ok(())
}

/// The sub-chunks of a `LIST` of type `INFO`: id, little-endian size, null-terminated text.
fn riff_info(list: &[u8], tags: &mut Tags) {
    let mut pos = 0usize;
    while pos + 8 <= list.len() {
        let id = &list[pos..pos + 4];
        let Some(size) = le32(&list[pos + 4..]).map(|s| s as usize) else {
            break;
        };
        let Some(text) = (pos + 8)
            .checked_add(size)
            .and_then(|end| list.get(pos + 8..end))
        else {
            break;
        };
        let text = text_guess(text.split(|&c| c == 0).next().unwrap_or(&[]));
        match id {
            b"INAM" => set_text(&mut tags.title, &text),
            b"IART" => set_text(&mut tags.artist, &text),
            b"IPRD" => set_text(&mut tags.album, &text),
            b"IGNR" => set_text(&mut tags.genre, &text),
            b"ICRD" => set(&mut tags.year, leading_year(&text)),
            b"ITRK" | b"IPRT" => {
                let (n, total) = number_pair(&text);
                set(&mut tags.track, n);
                set(&mut tags.track_total, total);
            }
            b"ICMT" => set_text(&mut tags.comment, &text),
            _ => {}
        }
        pos += 8 + size + (size & 1);
    }
}

/// An 80-bit IEEE extended float (the AIFF sample rate).
fn extended80(b: &[u8]) -> Option<f64> {
    let se = be16(b)?;
    let mantissa = be64(&b[2..])?;
    let exponent = (se & 0x7FFF) as i32;
    if exponent == 0 && mantissa == 0 {
        return Some(0.0);
    }
    if exponent == 0x7FFF {
        return None;
    }
    let value = mantissa as f64 * 2f64.powi(exponent - 16383 - 63);
    Some(if se & 0x8000 != 0 { -value } else { value })
}

fn parse_aiff<R: Read + Seek>(r: &mut R, len: u64, tags: &mut Tags) -> io::Result<()> {
    tags.format = Format::Aiff;
    let mut pos = 12u64;
    while pos + 8 <= len {
        let header = read_at(r, pos, 8)?;
        let Some(size) = be32(&header[4.min(header.len())..]).map(|s| s as u64) else {
            break;
        };
        let body = pos + 8;
        match &header[..4] {
            b"COMM" => {
                let comm = read_at(r, body, size.min(18) as usize)?;
                let channels = be16(&comm).filter(|&c| c > 0);
                let frames = be32(&comm[2.min(comm.len())..]).unwrap_or(0);
                let bits = be16(&comm[6.min(comm.len())..]).unwrap_or(0);
                let rate =
                    extended80(&comm[8.min(comm.len())..]).filter(|r| r.is_finite() && *r > 0.0);
                set(&mut tags.channels, channels.map(|c| c.min(255) as u8));
                if let Some(rate) = rate {
                    tags.sample_rate = Some(rate.round() as u32);
                    if frames > 0 {
                        tags.duration_secs = Some(frames as f64 / rate);
                    }
                    if let Some(c) = channels
                        && bits > 0
                    {
                        tags.bitrate_kbps =
                            Some((rate * c as f64 * bits as f64 / 1000.0).round() as u32);
                    }
                }
            }
            b"ID3 " | b"id3 " => {
                let tag = read_at(r, body, size.min(BLOCK_CAP as u64) as usize)?;
                parse_id3v2(&tag, tags);
            }
            b"NAME" | b"AUTH" | b"ANNO" => {
                let text = read_at(r, body, size.min(TEXT_CAP as u64) as usize)?;
                let text = text_guess(&text);
                match &header[..4] {
                    b"NAME" => set_text(&mut tags.title, &text),
                    b"AUTH" => set_text(&mut tags.artist, &text),
                    _ => set_text(&mut tags.comment, &text),
                }
            }
            _ => {}
        }
        pos = body.saturating_add(size).saturating_add(size & 1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- fixture builders -------------------------------------------------------------------

    fn be(n: u32) -> [u8; 4] {
        n.to_be_bytes()
    }
    fn le(n: u32) -> [u8; 4] {
        n.to_le_bytes()
    }
    fn syncsafe_bytes(n: u32) -> [u8; 4] {
        [
            ((n >> 21) & 0x7F) as u8,
            ((n >> 14) & 0x7F) as u8,
            ((n >> 7) & 0x7F) as u8,
            (n & 0x7F) as u8,
        ]
    }

    /// An ID3v2 tag: header with `major`/`flags`, then `body` (frames, extended header, padding).
    fn id3v2(major: u8, flags: u8, body: &[u8]) -> Vec<u8> {
        let mut out = b"ID3".to_vec();
        out.extend([major, 0, flags]);
        out.extend(syncsafe_bytes(body.len() as u32));
        out.extend_from_slice(body);
        out
    }
    fn frame_v23(id: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend(be(payload.len() as u32));
        out.extend([0, 0]);
        out.extend_from_slice(payload);
        out
    }
    fn frame_v24(id: &[u8; 4], flags: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend(syncsafe_bytes(payload.len() as u32));
        out.extend([0, flags]);
        out.extend_from_slice(payload);
        out
    }
    fn frame_v22(id: &[u8; 3], payload: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend(&be(payload.len() as u32)[1..]);
        out.extend_from_slice(payload);
        out
    }
    /// A Latin-1 text payload (encoding byte 0).
    fn text_latin1(s: &str) -> Vec<u8> {
        let mut out = vec![0u8];
        out.extend(s.chars().map(|c| c as u8));
        out
    }
    fn text_utf8(s: &str) -> Vec<u8> {
        let mut out = vec![3u8];
        out.extend_from_slice(s.as_bytes());
        out
    }
    fn comm(description: &str, text: &str) -> Vec<u8> {
        let mut out = vec![0u8];
        out.extend_from_slice(b"eng");
        out.extend(description.bytes());
        out.push(0);
        out.extend(text.bytes());
        out
    }
    fn apic(mime: &str, kind: u8, data: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8];
        out.extend(mime.bytes());
        out.push(0);
        out.push(kind);
        out.push(0); // empty description
        out.extend_from_slice(data);
        out
    }
    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 4];
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 9];

    /// One MPEG-1 Layer III frame, 128 kbps, 44100 Hz, stereo: 417 bytes.
    fn mp3_frame() -> Vec<u8> {
        let mut f = vec![0u8; 417];
        f[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
        f
    }
    fn mp3_frames(n: usize) -> Vec<u8> {
        (0..n).flat_map(|_| mp3_frame()).collect()
    }
    fn id3v1(
        title: &str,
        artist: &str,
        album: &str,
        year: &str,
        comment: &str,
        track: u8,
        genre: u8,
    ) -> Vec<u8> {
        let mut out = b"TAG".to_vec();
        for (field, width) in [(title, 30), (artist, 30), (album, 30), (year, 4)] {
            let mut bytes = field.as_bytes().to_vec();
            bytes.resize(width, 0);
            out.extend(bytes);
        }
        let mut c = comment.as_bytes().to_vec();
        c.resize(28, 0);
        out.extend(c);
        out.extend([0, track, genre]);
        assert_eq!(out.len(), 128);
        out
    }

    fn atom(name: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = be((body.len() + 8) as u32).to_vec();
        out.extend_from_slice(name);
        out.extend_from_slice(body);
        out
    }
    fn atom64(name: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = be(1).to_vec();
        out.extend_from_slice(name);
        out.extend((body.len() as u64 + 16).to_be_bytes());
        out.extend_from_slice(body);
        out
    }
    fn data_atom(kind: u32, payload: &[u8]) -> Vec<u8> {
        let mut body = be(kind).to_vec();
        body.extend([0, 0, 0, 0]);
        body.extend_from_slice(payload);
        atom(b"data", &body)
    }
    fn ilst_text(name: &[u8; 4], text: &str) -> Vec<u8> {
        atom(name, &data_atom(1, text.as_bytes()))
    }
    fn m4a_fixture() -> Vec<u8> {
        let mut mvhd = vec![0u8; 4]; // version 0, flags
        mvhd.extend([0; 8]); // creation, modification
        mvhd.extend(be(1000)); // timescale
        mvhd.extend(be(2500)); // duration
        mvhd.extend([0; 80]);
        let mut entry = vec![0u8; 8]; // size placeholder + format filled below
        entry.extend([0; 6]);
        entry.extend([0, 1]); // data reference index
        entry.extend([0; 8]); // version, revision, vendor
        entry.extend([0, 2]); // channels
        entry.extend([0, 16]); // sample size
        entry.extend([0; 4]);
        entry.extend(be(44100 << 16));
        // esds: ES_Descriptor > DecoderConfig (avg bitrate 96 kbps) > AudioSpecificConfig
        // (AAC LC, 44100 Hz, one channel), then an SLConfig.
        let mut esds = vec![0u8; 4];
        esds.extend([0x03, 25, 0, 1, 0]);
        esds.extend([0x04, 17, 0x40, 0x15, 0, 0, 0]);
        esds.extend(be(128_000));
        esds.extend(be(96_000));
        esds.extend([0x05, 2, 0x12, 0x08]);
        esds.extend([0x06, 1, 2]);
        entry.extend(atom(b"esds", &esds));
        let entry_len = be(entry.len() as u32);
        entry[..4].copy_from_slice(&entry_len);
        entry[4..8].copy_from_slice(b"mp4a");
        let mut stsd = vec![0u8; 4];
        stsd.extend(be(1));
        stsd.extend(entry);
        let trak = atom(
            b"trak",
            &atom(
                b"mdia",
                &atom(b"minf", &atom(b"stbl", &atom(b"stsd", &stsd))),
            ),
        );
        let mut ilst = Vec::new();
        ilst.extend(ilst_text(b"\xA9nam", "Atom Title"));
        ilst.extend(ilst_text(b"\xA9ART", "Atom Artist"));
        ilst.extend(ilst_text(b"\xA9alb", "Atom Album"));
        ilst.extend(ilst_text(b"aART", "Atom Album Artist"));
        ilst.extend(atom(b"gnre", &data_atom(0, &[0, 18]))); // 1-based: Rock
        ilst.extend(ilst_text(b"\xA9day", "2019-01-01"));
        ilst.extend(atom(b"trkn", &data_atom(0, &[0, 0, 0, 3, 0, 12, 0, 0])));
        ilst.extend(atom(b"disk", &data_atom(0, &[0, 0, 0, 1, 0, 2])));
        ilst.extend(ilst_text(b"\xA9cmt", "Atom Comment"));
        ilst.extend(atom(b"covr", &data_atom(14, PNG)));
        let mut meta = vec![0u8; 4]; // full atom version/flags
        meta.extend(atom(b"hdlr", &[0; 24]));
        meta.extend(atom(b"ilst", &ilst));
        let udta = atom(b"udta", &atom(b"meta", &meta));
        let mut moov = atom(b"mvhd", &mvhd);
        moov.extend(trak);
        moov.extend(udta);
        let mut file = atom(b"ftyp", b"M4A \0\0\0\0M4A mp42isom");
        file.extend(atom(b"moov", &moov));
        file.extend(atom64(b"mdat", &vec![0u8; 10_000]));
        // A trailing atom whose size field is 0: "to the end of the file".
        let mut free = be(0).to_vec();
        free.extend_from_slice(b"free");
        free.extend([0; 16]);
        file.extend(free);
        file
    }

    fn vorbis_comments(vendor: &str, fields: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = le(vendor.len() as u32).to_vec();
        out.extend(vendor.bytes());
        out.extend(le(fields.len() as u32));
        for (k, v) in fields {
            let mut entry = k.as_bytes().to_vec();
            entry.push(b'=');
            entry.extend_from_slice(v);
            out.extend(le(entry.len() as u32));
            out.extend(entry);
        }
        out
    }
    fn flac_picture_block(kind: u32, mime: &str, data: &[u8]) -> Vec<u8> {
        let mut out = be(kind).to_vec();
        out.extend(be(mime.len() as u32));
        out.extend(mime.bytes());
        out.extend(be(4));
        out.extend(b"desc");
        out.extend([0; 16]);
        out.extend(be(data.len() as u32));
        out.extend_from_slice(data);
        out
    }
    fn flac_block(kind: u8, last: bool, body: &[u8]) -> Vec<u8> {
        let mut out = vec![kind | if last { 0x80 } else { 0 }];
        out.extend(&be(body.len() as u32)[1..]);
        out.extend_from_slice(body);
        out
    }
    fn streaminfo(sample_rate: u32, channels: u8, bits: u8, total_samples: u64) -> Vec<u8> {
        let mut out = vec![0x10, 0x00, 0x10, 0x00, 0, 0, 0, 0, 0, 0];
        let packed: u64 = ((sample_rate as u64) << 44)
            | (((channels - 1) as u64) << 41)
            | (((bits - 1) as u64) << 36)
            | (total_samples & 0xF_FFFF_FFFF);
        out.extend(packed.to_be_bytes());
        out.extend([0; 16]);
        assert_eq!(out.len(), 34);
        out
    }
    const FLAC_FIELDS: &[(&str, &[u8])] = &[
        ("TITLE", b"Flac Title"),
        ("ARTIST", b"Flac Artist"),
        ("ALBUM", b"Flac Album"),
        ("ALBUMARTIST", b"Flac Album Artist"),
        ("GENRE", b"Jazz"),
        ("DATE", b"1999-12"),
        ("TRACKNUMBER", b"5"),
        ("TRACKTOTAL", b"9"),
        ("DISCNUMBER", b"2"),
        ("DISCTOTAL", b"3"),
        ("COMMENT", b"Flac Comment"),
    ];
    fn flac_fixture() -> Vec<u8> {
        let mut out = b"fLaC".to_vec();
        out.extend(flac_block(0, false, &streaminfo(44100, 2, 16, 441_000)));
        out.extend(flac_block(4, false, &vorbis_comments("test", FLAC_FIELDS)));
        out.extend(flac_block(
            6,
            false,
            &flac_picture_block(0, "image/jpeg", JPEG),
        ));
        out.extend(flac_block(
            6,
            true,
            &flac_picture_block(3, "image/png", PNG),
        ));
        out.extend([0xFF, 0xF8, 0x69, 0x18]); // the first audio frame's sync code
        out
    }

    fn ogg_page(header_type: u8, granule: u64, seq: u32, lacing: &[u8], body: &[u8]) -> Vec<u8> {
        let mut out = b"OggS".to_vec();
        out.push(0);
        out.push(header_type);
        out.extend(granule.to_le_bytes());
        out.extend(le(0x1234)); // serial
        out.extend(le(seq));
        out.extend([0; 4]); // crc, unchecked
        out.push(lacing.len() as u8);
        out.extend_from_slice(lacing);
        out.extend_from_slice(body);
        out
    }
    /// Lays `packet` across pages: the first page ends in a 255 segment, a continuation page
    /// carries the rest.
    fn ogg_spanning(packet: &[u8], first_seq: u32, granule: u64) -> Vec<u8> {
        assert!(packet.len() > 255 && packet.len() < 510);
        let mut out = ogg_page(0, u64::MAX, first_seq, &[255], &packet[..255]);
        out.extend(ogg_page(
            0x01,
            granule,
            first_seq + 1,
            &[(packet.len() - 255) as u8],
            &packet[255..],
        ));
        out
    }
    fn ogg_vorbis_fixture() -> Vec<u8> {
        let mut ident = b"\x01vorbis".to_vec();
        ident.extend(le(0)); // version
        ident.push(2); // channels
        ident.extend(le(44100));
        ident.extend(le(0)); // max bitrate
        ident.extend(le(160_000)); // nominal
        ident.extend(le(0));
        ident.extend([0xB8, 1]); // blocksizes, framing
        let mut comments = b"\x03vorbis".to_vec();
        let vendor = "x".repeat(200);
        comments.extend(vorbis_comments(
            &vendor,
            &[
                ("TITLE", b"Ogg Title"),
                ("ARTIST", b"Ogg Artist"),
                ("TRACKNUMBER", b"4/10"),
                ("DATE", b"2015"),
            ],
        ));
        comments.push(1); // framing bit
        let mut out = ogg_page(0x02, 0, 0, &[ident.len() as u8], &ident);
        out.extend(ogg_spanning(&comments, 1, 0));
        out.extend(ogg_page(0, 44100 * 2, 3, &[10], &[0; 10]));
        out.extend(ogg_page(0x04, 44100 * 3, 4, &[10], &[0; 10]));
        out
    }
    fn base64_encode(data: &[u8]) -> Vec<u8> {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = Vec::new();
        for chunk in data.chunks(3) {
            let mut n = 0u32;
            for (i, &b) in chunk.iter().enumerate() {
                n |= (b as u32) << (16 - 8 * i);
            }
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]);
                } else {
                    out.push(b'=');
                }
            }
        }
        out
    }
    fn opus_fixture() -> Vec<u8> {
        let mut head = b"OpusHead".to_vec();
        head.push(1); // version
        head.push(2); // channels
        head.extend(312u16.to_le_bytes()); // pre-skip
        head.extend(le(44100)); // input sample rate
        head.extend([0, 0, 0]); // gain, mapping family
        let picture = base64_encode(&flac_picture_block(3, "image/jpeg", JPEG));
        let mut tags = b"OpusTags".to_vec();
        tags.extend(vorbis_comments(
            "libopus",
            &[
                ("TITLE", b"Opus Title"),
                ("ALBUM", b"Opus Album"),
                ("METADATA_BLOCK_PICTURE", &picture),
            ],
        ));
        let mut out = ogg_page(0x02, 0, 0, &[head.len() as u8], &head);
        out.extend(ogg_page(0, 0, 1, &[tags.len() as u8], &tags));
        out.extend(ogg_page(0x04, 48000 * 2 + 312, 2, &[3], &[0; 3]));
        out
    }

    fn riff_chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend(le(body.len() as u32));
        out.extend_from_slice(body);
        if body.len() % 2 == 1 {
            out.push(0);
        }
        out
    }
    fn wav_fixture(extra: &[u8]) -> Vec<u8> {
        let mut fmt = 1u16.to_le_bytes().to_vec(); // PCM
        fmt.extend(2u16.to_le_bytes());
        fmt.extend(le(44100));
        fmt.extend(le(176_400));
        fmt.extend(4u16.to_le_bytes());
        fmt.extend(16u16.to_le_bytes());
        let mut body = b"WAVE".to_vec();
        body.extend(riff_chunk(b"fmt ", &fmt));
        body.extend_from_slice(extra);
        body.extend(riff_chunk(b"data", &vec![0u8; 88_200]));
        let mut out = b"RIFF".to_vec();
        out.extend(le(body.len() as u32));
        out.extend(body);
        out
    }
    fn wav_info_fixture() -> Vec<u8> {
        let mut info = b"INFO".to_vec();
        for (id, text) in [
            (b"INAM", "Wav Title"),
            (b"IART", "Wav Artist"),
            (b"IPRD", "Wav Album"),
            (b"IGNR", "Blues"),
            (b"ICRD", "2001-07-04"),
            (b"ITRK", "7"),
            (b"ICMT", "Odd comment"), // 11 chars + null = 12; the pad rule is exercised by INAM (10)
        ] {
            let mut t = text.as_bytes().to_vec();
            t.push(0);
            info.extend(riff_chunk(id, &t));
        }
        wav_fixture(&riff_chunk(b"LIST", &info))
    }
    fn aiff_fixture() -> Vec<u8> {
        let mut comm = 1u16.to_be_bytes().to_vec();
        comm.extend(be(44100)); // frames
        comm.extend(16u16.to_be_bytes());
        comm.extend([0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]); // 44100.0
        fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
            let mut out = id.to_vec();
            out.extend(be(body.len() as u32));
            out.extend_from_slice(body);
            if body.len() % 2 == 1 {
                out.push(0);
            }
            out
        }
        let mut body = b"AIFF".to_vec();
        body.extend(chunk(b"COMM", &comm));
        body.extend(chunk(b"NAME", b"Aiff Title!")); // odd length
        body.extend(chunk(b"AUTH", b"Aiff Author"));
        body.extend(chunk(b"ANNO", b"Aiff Note"));
        body.extend(chunk(
            b"ID3 ",
            &id3v2(3, 0, &frame_v23(b"TALB", &text_latin1("Aiff Album"))),
        ));
        let mut ssnd = vec![0u8; 8];
        ssnd.extend([0u8; 64]);
        body.extend(chunk(b"SSND", &ssnd));
        let mut out = b"FORM".to_vec();
        out.extend(be(body.len() as u32));
        out.extend(body);
        out
    }

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 0.01)
    }

    // --- MP3 ---------------------------------------------------------------------------------

    #[test]
    fn id3v23_cbr_with_v1_fallback() {
        let mut body = Vec::new();
        body.extend(frame_v23(b"TIT2", &text_latin1("Title")));
        body.extend(frame_v23(b"TPE1", &text_latin1("Artist")));
        body.extend(frame_v23(b"TPE2", &text_latin1("Album Artist")));
        body.extend(frame_v23(b"TCON", &text_latin1("(17)")));
        body.extend(frame_v23(b"TYER", &text_latin1("2004")));
        body.extend(frame_v23(b"TRCK", &text_latin1("3/12")));
        body.extend(frame_v23(b"TPOS", &text_latin1("1/2")));
        body.extend(frame_v23(b"COMM", &comm("described", "Not this one")));
        body.extend(frame_v23(b"COMM", &comm("", "Hi")));
        body.extend(frame_v23(b"APIC", &apic("image/png", 0, PNG)));
        body.extend(frame_v23(b"APIC", &apic("image/jpeg", 3, JPEG)));
        body.extend([0u8; 32]); // padding
        let mut file = id3v2(3, 0, &body);
        file.extend(mp3_frames(10));
        file.extend(id3v1(
            "V1 Title",
            "V1 Artist",
            "V1 Album",
            "1999",
            "V1 comment",
            9,
            8,
        ));
        let t = read(&file);
        assert_eq!(t.format, Format::Mp3);
        assert_eq!(t.title.as_deref(), Some("Title"));
        assert_eq!(t.artist.as_deref(), Some("Artist"));
        assert_eq!(
            t.album.as_deref(),
            Some("V1 Album"),
            "v1 fills what v2 left empty"
        );
        assert_eq!(t.album_artist.as_deref(), Some("Album Artist"));
        assert_eq!(t.genre.as_deref(), Some("Rock"));
        assert_eq!(t.year, Some(2004));
        assert_eq!((t.track, t.track_total), (Some(3), Some(12)));
        assert_eq!((t.disc, t.disc_total), (Some(1), Some(2)));
        assert_eq!(
            t.comment.as_deref(),
            Some("Hi"),
            "the undescribed comment wins"
        );
        let pic = t.picture.expect("front cover");
        assert_eq!(
            (pic.mime.as_str(), pic.data.as_slice()),
            ("image/jpeg", JPEG)
        );
        assert_eq!(
            (t.sample_rate, t.channels, t.bitrate_kbps),
            (Some(44100), Some(2), Some(128))
        );
        assert!(
            close(t.duration_secs, 4170.0 * 8.0 / 128_000.0),
            "{:?}",
            t.duration_secs
        );
    }

    #[test]
    fn id3v24_utf8_xing_unsync_ext_header_footer() {
        let mut body = Vec::new();
        body.extend([0, 0, 0, 6, 1, 0]); // extended header: syncsafe size 6, one flag byte
        body.extend(frame_v24(b"TIT2", 0, &text_utf8("Tïtle ✓")));
        body.extend(frame_v24(b"TPE1", 0, &text_utf8("A\0B")));
        body.extend(frame_v24(b"TDRC", 0, &text_utf8("2010-03-04")));
        body.extend(frame_v24(b"TCON", 0, &text_utf8("17")));
        // UTF-16 with a BOM and UTF-16BE without.
        let mut utf16le = vec![1u8, 0xFF, 0xFE];
        utf16le.extend("Albüm".encode_utf16().flat_map(|u| u.to_le_bytes()));
        body.extend(frame_v24(b"TALB", 0, &utf16le));
        let mut utf16be = vec![2u8];
        utf16be.extend("Bänd".encode_utf16().flat_map(|u| u.to_be_bytes()));
        body.extend(frame_v24(b"TPE2", 0, &utf16be));
        // An unsynchronised APIC with a data length indicator.
        let raw = apic(
            "image/png",
            3,
            &[0x89, b'P', b'N', b'G', 0xFF, 0xE0, 0xFF, 0x00],
        );
        let mut stored = syncsafe_bytes(raw.len() as u32).to_vec();
        for &b in &raw {
            stored.push(b);
            if b == 0xFF {
                stored.push(0);
            }
        }
        // The two 0xFF bytes each gain a 00; the stored form must be longer than the raw one.
        assert_eq!(stored.len(), raw.len() + 4 + 2);
        body.extend(frame_v24(b"APIC", 0x03, &stored));
        let mut file = id3v2(4, 0x40 | 0x10, &body);
        let mut footer = b"3DI".to_vec();
        footer.extend(&file[3..10]);
        file.extend(footer);
        let mut xing = mp3_frame();
        xing[36..40].copy_from_slice(b"Xing");
        xing[40..44].copy_from_slice(&be(3));
        xing[44..48].copy_from_slice(&be(1000));
        xing[48..52].copy_from_slice(&be(417_000));
        file.extend(xing);
        file.extend(mp3_frames(2));
        let t = read(&file);
        assert_eq!(t.format, Format::Mp3);
        assert_eq!(t.title.as_deref(), Some("Tïtle ✓"));
        assert_eq!(t.artist.as_deref(), Some("A, B"));
        assert_eq!(t.album.as_deref(), Some("Albüm"));
        assert_eq!(t.album_artist.as_deref(), Some("Bänd"));
        assert_eq!(t.year, Some(2010));
        assert_eq!(t.genre.as_deref(), Some("Rock"));
        let pic = t.picture.expect("picture");
        assert_eq!(pic.mime, "image/png");
        assert_eq!(pic.data, [0x89, b'P', b'N', b'G', 0xFF, 0xE0, 0xFF, 0x00]);
        assert!(
            close(t.duration_secs, 1000.0 * 1152.0 / 44100.0),
            "{:?}",
            t.duration_secs
        );
        assert_eq!(t.bitrate_kbps, Some(128));
    }

    #[test]
    fn id3v22_and_tag_level_unsync() {
        let mut body = Vec::new();
        body.extend(frame_v22(b"TT2", &text_latin1("Old Title")));
        body.extend(frame_v22(b"TRK", &text_latin1("2")));
        let mut pic = vec![0u8];
        pic.extend(b"JPG");
        pic.push(3);
        pic.push(0);
        pic.extend(JPEG);
        body.extend(frame_v22(b"PIC", &pic));
        let t = read(&id3v2(2, 0, &body));
        assert_eq!(t.format, Format::Mp3);
        assert_eq!(t.title.as_deref(), Some("Old Title"));
        assert_eq!(t.track, Some(2));
        assert_eq!(
            t.picture.as_ref().map(|p| p.mime.as_str()),
            Some("image/jpeg")
        );

        // 2.3 with the tag-level unsynchronisation flag: FF 00 pairs collapse before parsing.
        let raw = frame_v23(
            b"APIC",
            &apic("image/jpeg", 3, &[0xFF, 0xE0, 0xFF, 0x00, 0x01]),
        );
        let mut stored = Vec::new();
        for &b in &raw {
            stored.push(b);
            if b == 0xFF {
                stored.push(0);
            }
        }
        let t = read(&id3v2(3, 0x80, &stored));
        assert_eq!(
            t.picture.map(|p| p.data),
            Some(vec![0xFF, 0xE0, 0xFF, 0x00, 0x01])
        );
    }

    #[test]
    fn id3v1_only_and_tlen() {
        let mut file = mp3_frames(3);
        file.extend(id3v1(
            "Only V1",
            "Someone",
            "Somewhere",
            "1987",
            "Comment here",
            4,
            17,
        ));
        let t = read(&file);
        assert_eq!(t.format, Format::Mp3);
        assert_eq!(t.title.as_deref(), Some("Only V1"));
        assert_eq!(t.artist.as_deref(), Some("Someone"));
        assert_eq!(t.album.as_deref(), Some("Somewhere"));
        assert_eq!(t.year, Some(1987));
        assert_eq!(t.comment.as_deref(), Some("Comment here"));
        assert_eq!(t.track, Some(4));
        assert_eq!(t.genre.as_deref(), Some("Rock"));
        assert!(close(t.duration_secs, 3.0 * 417.0 * 8.0 / 128_000.0));

        let mut file = id3v2(3, 0, &frame_v23(b"TLEN", &text_latin1("2500")));
        file.extend(mp3_frames(2));
        assert!(
            close(read(&file).duration_secs, 2.5),
            "TLEN beats the CBR estimate"
        );
    }

    #[test]
    fn genre_mapping() {
        assert_eq!(id3_genre("(17)").as_deref(), Some("Rock"));
        assert_eq!(id3_genre("17").as_deref(), Some("Rock"));
        assert_eq!(id3_genre("(17)Progressive").as_deref(), Some("Progressive"));
        assert_eq!(id3_genre("(RX)").as_deref(), Some("Remix"));
        assert_eq!(id3_genre("Shoegaze").as_deref(), Some("Shoegaze"));
        assert_eq!(id3_genre("(999)"), None);
        assert_eq!(id3_genre(""), None);
        assert_eq!(genre_name(133), None);
    }

    // --- MP4, FLAC, Ogg, WAV, AIFF ------------------------------------------------------------

    #[test]
    fn m4a_atoms() {
        let t = read(&m4a_fixture());
        assert_eq!(t.format, Format::Mp4);
        assert_eq!(t.title.as_deref(), Some("Atom Title"));
        assert_eq!(t.artist.as_deref(), Some("Atom Artist"));
        assert_eq!(t.album.as_deref(), Some("Atom Album"));
        assert_eq!(t.album_artist.as_deref(), Some("Atom Album Artist"));
        assert_eq!(t.genre.as_deref(), Some("Rock"));
        assert_eq!(t.year, Some(2019));
        assert_eq!((t.track, t.track_total), (Some(3), Some(12)));
        assert_eq!((t.disc, t.disc_total), (Some(1), Some(2)));
        assert_eq!(t.comment.as_deref(), Some("Atom Comment"));
        let pic = t.picture.expect("cover");
        assert_eq!((pic.mime.as_str(), pic.data.as_slice()), ("image/png", PNG));
        assert!(close(t.duration_secs, 2.5));
        assert_eq!(
            (t.sample_rate, t.channels),
            (Some(44100), Some(1)),
            "esds corrects the entry's 2 channels"
        );
        assert_eq!(
            t.bitrate_kbps,
            Some(32),
            "10 000 mdat bytes over 2.5 s beat the declared 96 kbps"
        );
    }

    #[test]
    fn flac_blocks() {
        let file = flac_fixture();
        let t = read(&file);
        assert_eq!(t.format, Format::Flac);
        assert_eq!(t.title.as_deref(), Some("Flac Title"));
        assert_eq!(t.artist.as_deref(), Some("Flac Artist"));
        assert_eq!(t.album.as_deref(), Some("Flac Album"));
        assert_eq!(t.album_artist.as_deref(), Some("Flac Album Artist"));
        assert_eq!(t.genre.as_deref(), Some("Jazz"));
        assert_eq!(t.year, Some(1999));
        assert_eq!((t.track, t.track_total), (Some(5), Some(9)));
        assert_eq!((t.disc, t.disc_total), (Some(2), Some(3)));
        assert_eq!(t.comment.as_deref(), Some("Flac Comment"));
        let pic = t.picture.expect("picture");
        assert_eq!(
            (pic.mime.as_str(), pic.data.as_slice()),
            ("image/png", PNG),
            "type 3 wins over type 0"
        );
        assert!(close(t.duration_secs, 10.0));
        assert_eq!((t.sample_rate, t.channels), (Some(44100), Some(2)));
        assert_eq!(
            t.bitrate_kbps,
            Some((file.len() as f64 * 8.0 / 10.0 / 1000.0).round() as u32)
        );

        // An ID3v2 tag in front of a FLAC stream.
        let mut prefixed = id3v2(
            3,
            0,
            &frame_v23(b"TIT2", &text_latin1("Ignored by FLAC readers")),
        );
        prefixed.extend(&file);
        let t = read(&prefixed);
        assert_eq!(t.format, Format::Flac);
        assert_eq!(
            t.title.as_deref(),
            Some("Ignored by FLAC readers"),
            "the earlier tag fills first"
        );
        assert!(close(t.duration_secs, 10.0));
    }

    #[test]
    fn ogg_vorbis_spanning_packets() {
        let t = read(&ogg_vorbis_fixture());
        assert_eq!(t.format, Format::OggVorbis);
        assert_eq!(t.title.as_deref(), Some("Ogg Title"));
        assert_eq!(t.artist.as_deref(), Some("Ogg Artist"));
        assert_eq!((t.track, t.track_total), (Some(4), Some(10)));
        assert_eq!(t.year, Some(2015));
        assert_eq!((t.sample_rate, t.channels), (Some(44100), Some(2)));
        assert!(close(t.duration_secs, 3.0), "{:?}", t.duration_secs);
        assert_eq!(t.bitrate_kbps, Some(160));
        assert_eq!(t.picture, None);
    }

    #[test]
    fn opus_with_base64_picture() {
        let t = read(&opus_fixture());
        assert_eq!(t.format, Format::Opus);
        assert_eq!(t.title.as_deref(), Some("Opus Title"));
        assert_eq!(t.album.as_deref(), Some("Opus Album"));
        assert_eq!((t.sample_rate, t.channels), (Some(44100), Some(2)));
        assert!(close(t.duration_secs, 2.0), "{:?}", t.duration_secs);
        let pic = t.picture.expect("picture");
        assert_eq!(
            (pic.mime.as_str(), pic.data.as_slice()),
            ("image/jpeg", JPEG)
        );
        assert!(t.bitrate_kbps.is_some());
    }

    #[test]
    fn base64_roundtrip_and_rejects() {
        for n in 0..40 {
            let data: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(
                base64_decode(&base64_encode(&data)).as_deref(),
                Some(data.as_slice())
            );
        }
        assert_eq!(
            base64_decode(b"QUJD\nREVG").as_deref(),
            Some(b"ABCDEF".as_slice())
        );
        assert_eq!(base64_decode(b"QU*D"), None);
    }

    #[test]
    fn wav_list_info() {
        let t = read(&wav_info_fixture());
        assert_eq!(t.format, Format::Wav);
        assert_eq!(t.title.as_deref(), Some("Wav Title"));
        assert_eq!(t.artist.as_deref(), Some("Wav Artist"));
        assert_eq!(t.album.as_deref(), Some("Wav Album"));
        assert_eq!(t.genre.as_deref(), Some("Blues"));
        assert_eq!(t.year, Some(2001));
        assert_eq!(t.track, Some(7));
        assert_eq!(t.comment.as_deref(), Some("Odd comment"));
        assert!(close(t.duration_secs, 0.5), "{:?}", t.duration_secs);
        assert_eq!(
            (t.sample_rate, t.channels, t.bitrate_kbps),
            (Some(44100), Some(2), Some(1411))
        );
    }

    #[test]
    fn wav_id3_chunk() {
        let mut body = frame_v23(b"TIT2", &text_latin1("Wav ID3 Title"));
        body.extend(frame_v23(b"TRCK", &text_latin1("2/8")));
        body.extend(frame_v23(b"APIC", &apic("image/jpeg", 3, JPEG)));
        let t = read(&wav_fixture(&riff_chunk(b"id3 ", &id3v2(3, 0, &body))));
        assert_eq!(t.format, Format::Wav);
        assert_eq!(t.title.as_deref(), Some("Wav ID3 Title"));
        assert_eq!((t.track, t.track_total), (Some(2), Some(8)));
        assert_eq!(t.picture.map(|p| p.data), Some(JPEG.to_vec()));
        assert!(close(t.duration_secs, 0.5));
    }

    #[test]
    fn aiff_chunks() {
        let t = read(&aiff_fixture());
        assert_eq!(t.format, Format::Aiff);
        assert_eq!(t.title.as_deref(), Some("Aiff Title!"));
        assert_eq!(t.artist.as_deref(), Some("Aiff Author"));
        assert_eq!(t.comment.as_deref(), Some("Aiff Note"));
        assert_eq!(t.album.as_deref(), Some("Aiff Album"));
        assert!(close(t.duration_secs, 1.0), "{:?}", t.duration_secs);
        assert_eq!(
            (t.sample_rate, t.channels, t.bitrate_kbps),
            (Some(44100), Some(1), Some(706))
        );
        assert_eq!(
            extended80(&[0x40, 0x0F, 0xBB, 0x80, 0, 0, 0, 0, 0, 0]),
            Some(96000.0)
        );
    }

    #[test]
    fn extensions() {
        assert!(is_audio_extension("mp3"));
        assert!(is_audio_extension(".FLAC"));
        assert!(is_audio_extension("M4b"));
        assert!(!is_audio_extension("txt"));
        assert!(!is_audio_extension(""));
        assert!(AUDIO_EXTENSIONS.iter().all(|e| {
            e.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        }));
    }

    // --- hostile input -----------------------------------------------------------------------

    fn all_fixtures() -> Vec<Vec<u8>> {
        let mut mp3 = id3v2(3, 0, &frame_v23(b"TIT2", &text_latin1("T")));
        mp3.extend(mp3_frames(3));
        mp3.extend(id3v1("a", "b", "c", "2000", "d", 1, 2));
        vec![
            mp3,
            m4a_fixture(),
            flac_fixture(),
            ogg_vorbis_fixture(),
            opus_fixture(),
            wav_info_fixture(),
            aiff_fixture(),
        ]
    }

    #[test]
    fn empty_input_is_unknown() {
        assert_eq!(read(&[]), Tags::default());
        assert_eq!(
            read(b"ID3"),
            Tags {
                format: Format::Unknown,
                ..Tags::default()
            }
        );
        assert_eq!(read(b"not an audio file at all").format, Format::Unknown);
    }

    #[test]
    fn truncated_at_every_length_never_panics() {
        for fixture in all_fixtures() {
            // Every cut point for the small fixtures; a stride through the WAV's sample data.
            let step = if fixture.len() > 8192 { 97 } else { 1 };
            for n in (0..fixture.len()).step_by(step) {
                let _ = read(&fixture[..n]);
            }
        }
    }

    #[test]
    fn frame_size_exceeding_tag() {
        let mut body = frame_v23(b"TIT2", &text_latin1("Kept"));
        body.extend(b"TALB");
        body.extend(be(0x00FF_FFFF));
        body.extend([0, 0, 0, b'x']);
        let mut file = id3v2(3, 0, &body);
        file.extend(mp3_frames(2));
        let t = read(&file);
        assert_eq!(t.title.as_deref(), Some("Kept"));
        assert_eq!(t.album, None);
        assert_eq!(t.format, Format::Mp3);

        // A tag header claiming far more than the file holds.
        let mut huge = b"ID3\x03\x00\x00\x7F\x7F\x7F\x7F".to_vec();
        huge.extend(frame_v23(b"TPE1", &text_latin1("Still read")));
        let t = read(&huge);
        assert_eq!(t.artist.as_deref(), Some("Still read"));
        assert_eq!(t.format, Format::Mp3);
    }

    #[test]
    fn picture_over_the_cap_is_skipped() {
        // A FLAC PICTURE block declaring 33 MiB of data, in a file that ends long before.
        let mut block = flac_picture_block(3, "image/jpeg", &[]);
        let at = block.len() - 4;
        block[at..].copy_from_slice(&be(33 * 1024 * 1024));
        block.extend([0xFF; 64]);
        let mut file = b"fLaC".to_vec();
        file.extend(flac_block(0, false, &streaminfo(48000, 1, 24, 48000)));
        let mut header = vec![0x80 | 6];
        header.extend(&be(33 * 1024 * 1024 + 40)[1..]);
        file.extend(header);
        file.extend(block);
        let t = read(&file);
        assert_eq!(t.format, Format::Flac);
        assert_eq!(t.picture, None);
        assert!(close(t.duration_secs, 1.0));

        // An MP4 `covr` item of 40 MiB in a truncated file: skipped, the rest of ilst too, but
        // the duration and the title before it survive.
        let mut ilst = ilst_text(b"\xA9nam", "Before");
        ilst.extend(be(40 * 1024 * 1024));
        ilst.extend(b"covr");
        ilst.extend([0; 32]);
        let mut meta = vec![0u8; 4];
        meta.extend(atom(b"ilst", &ilst));
        let mut mvhd = vec![0u8; 12];
        mvhd.extend(be(10));
        mvhd.extend(be(25));
        mvhd.extend([0; 80]);
        let mut moov = atom(b"mvhd", &mvhd);
        moov.extend(atom(b"udta", &atom(b"meta", &meta)));
        let mut file = atom(b"ftyp", b"M4A ");
        file.extend(atom(b"moov", &moov));
        let t = read(&file);
        assert_eq!(t.format, Format::Mp4);
        assert_eq!(t.title.as_deref(), Some("Before"));
        assert_eq!(t.picture, None);
        assert!(close(t.duration_secs, 2.5));

        // Text frames over the cap are skipped as well (checked on the slice parser directly,
        // so the test allocates one oversized frame and nothing else).
        let mut frame = vec![3u8];
        frame.resize(TEXT_CAP + 2, b'a');
        assert_eq!(id3_text_frame(&frame), None);
    }

    fn xorshift(len: usize) -> Vec<u8> {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn random_bytes_are_unknown_and_never_panic() {
        let noise = xorshift(4096);
        let t = read(&noise);
        assert_eq!(t, Tags::default());
        // The same noise behind every magic number the detector knows.
        let magics: &[&[u8]] = &[
            b"ID3\x04\x00\x00",
            b"ID3\x03\x00\x80",
            b"ID3\x02\x00\x00",
            b"\xFF\xFB\x90\x00",
            b"fLaC",
            b"OggS\x00\x02",
            b"RIFF\xFF\xFF\xFF\xFFWAVE",
            b"FORM\xFF\xFF\xFF\xFFAIFF",
            b"\x00\x00\x00\x20ftypM4A ",
            b"\x00\x00\x00\x01moov",
        ];
        for magic in magics {
            let mut file = magic.to_vec();
            file.extend(&noise);
            let _ = read(&file);
            for n in [0, 1, 7, 15, 64, 300] {
                let _ = read(&file[..magic.len().min(file.len()) + n]);
            }
        }
        // Noise inside the structure as well: a Vorbis comment block and an ilst of noise.
        let mut flac = b"fLaC".to_vec();
        flac.extend(flac_block(4, true, &noise));
        let _ = read(&flac);
        let mut ogg = ogg_page(2, 0, 0, &[30], &{
            let mut p = b"\x01vorbis".to_vec();
            p.extend(&noise[..23]);
            p
        });
        ogg.extend(ogg_page(0, 0, 1, &[255, 255, 200], &{
            let mut p = b"\x03vorbis".to_vec();
            p.extend(&noise[..703]);
            p
        }));
        let _ = read(&ogg);
        let m4a = {
            let mut moov = atom(b"ilst", &noise);
            moov.extend(atom(b"mvhd", &noise[..100]));
            let mut f = atom(b"ftyp", b"M4A ");
            f.extend(atom(b"moov", &moov));
            f
        };
        let _ = read(&m4a);
        let _ = read_from(&mut Cursor::new(&noise)).unwrap();
    }

    // --- a real file --------------------------------------------------------------------------

    /// Converts a generated WAV through macOS `afconvert` and reads the AAC result back. Skipped
    /// where the tool is absent.
    #[test]
    fn afconvert_m4a_cross_check() {
        use std::process::Command;
        if Command::new("afconvert").arg("-h").output().is_err() {
            eprintln!("afconvert not present; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("day-piece-media-tags-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav_path = dir.join("tone.wav");
        let m4a_path = dir.join("tone.m4a");
        // One second of a 440 Hz sine, 16-bit mono at 44.1 kHz.
        let samples: Vec<u8> = (0..44_100)
            .flat_map(|i| {
                let v = (i as f64 / 44_100.0 * 440.0 * std::f64::consts::TAU).sin() * 12_000.0;
                (v as i16).to_le_bytes()
            })
            .collect();
        let mut fmt = 1u16.to_le_bytes().to_vec();
        fmt.extend(1u16.to_le_bytes());
        fmt.extend(le(44_100));
        fmt.extend(le(88_200));
        fmt.extend(2u16.to_le_bytes());
        fmt.extend(16u16.to_le_bytes());
        let mut body = b"WAVE".to_vec();
        body.extend(riff_chunk(b"fmt ", &fmt));
        body.extend(riff_chunk(b"data", &samples));
        let mut wav = b"RIFF".to_vec();
        wav.extend(le(body.len() as u32));
        wav.extend(body);
        std::fs::write(&wav_path, &wav).unwrap();
        let wav_tags = read_file(&wav_path).unwrap();
        assert_eq!(wav_tags.format, Format::Wav);
        assert!(close(wav_tags.duration_secs, 1.0));

        let status = Command::new("afconvert")
            .args(["-f", "m4af", "-d", "aac"])
            .arg(&wav_path)
            .arg(&m4a_path)
            .status()
            .unwrap();
        assert!(status.success(), "afconvert failed: {status}");
        let t = read_file(&m4a_path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(t.format, Format::Mp4);
        let secs = t.duration_secs.expect("duration from mvhd");
        assert!((secs - 1.0).abs() < 0.2, "duration {secs}");
        assert_eq!(t.sample_rate, Some(44_100));
        assert_eq!(t.channels, Some(1));
        assert!(t.bitrate_kbps.is_some_and(|b| b > 0));
    }
}
