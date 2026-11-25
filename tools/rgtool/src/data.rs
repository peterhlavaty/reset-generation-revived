//! Data-extraction tools for the game's file formats (ported from the former `tools/*.py`).
//! All formats are little endian.
//!
//! * **`.pak`** (game data archive): `u32 header_size, u32 count`, then `count` 17-byte entries
//!   `{u32 name_hash, u32 stored_size, u32 raw_size, u8 compressed, u32 offset}`, then the data;
//!   entries with `compressed == 1` are zlib streams (a broken stream extracts as empty). The last
//!   entry lists the other entries' paths in entry order: `u32 n`, then `n` x `{u16 len, len bytes
//!   latin-1}`. When it parses exactly and `n == count - 1`, entries are named by it (backslashes
//!   become `/`; the list itself is `_namelist.bin`); otherwise `NNNN_hash8.ext` with a sniffed
//!   extension.
//! * **`.gfx`** (image, partial): 40-byte header of u32s: `0 magic=2, 1 format (0 = raw 32-bit
//!   ARGB, i.e. BGRA bytes), 2 width, 3 height, 4 frames, 5..8 ?, 9 data size`, then the pixels.
//!   Frames are stacked vertically in the PNG. Other formats are not decoded yet.
//! * **`.spc`** (audio, TENTATIVE): `u16 ?, u16 ?, u16 ?, u16 sample_rate, u16 ?, u16 bits (4),
//!   u32 sample_count`, zero padding to 0x22, a 16-entry i16 table of output levels at 0x22..0x41
//!   (ascending: a non-uniform 4-bit quantiser), then 4-bit codes from 0x42, high nibble first
//!   unless `--low-first`. Each code selects `table[code]` as the output sample. The header and
//!   table are certain; whether codes are absolute levels or deltas is not yet confirmed against
//!   the engine. Written as 16-bit mono WAV.
//! * **`.sis`** (Symbian 9 SISX installer, minimal): 16 bytes of UIDs, then the contents field. A
//!   field is `u32 type, u32 length` (if bit 31 of the length is set, a second u32 supplies bits
//!   31 and up), the data, padding to 4 bytes. An array is `u32 element_type`, then elements as
//!   fields without their type word. Compressed (3) = `u32 algorithm (1 = zlib), u64 size, data`.
//!   The contents hold the compressed controller (3) and the data (30). A controller (13) holds
//!   Info (14: uid 9, vendor string 1, name arrays 2, version 4), its data-unit index (40) and an
//!   install block (28): arrays (2) of file descriptions (24: target string first, u32 file index
//!   last), embedded controllers (13) and `If` blocks (26, with nested install blocks directly and
//!   in their else-if array). The data is an array of data units, each an array of file data whose
//!   compressed (3) blobs are the files. Files go to `OUTDIR/du<N>/<target>`, with the drive
//!   (`!:` or a letter) dropped.
//! * **`.n-gage`** (N-Gage 2.0 install file; its paks are DRM-encrypted): MIME multipart with the
//!   boundary `--KBoundary`; each part is written as `<Content-ID or partN>` + `.xml` (for
//!   `text/xml` parts) or `.sis`.

use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use flate2::{Decompress, FlushDecompress, Status};

use crate::util::R;

// ---- shared helpers ----

fn err<T>(msg: impl Into<String>) -> R<T> {
    let msg: String = msg.into();
    Err(msg.into())
}

/// `N` bytes at `o`, or an error if the buffer is too short.
fn bytes<const N: usize>(b: &[u8], o: usize) -> R<[u8; N]> {
    match o.checked_add(N).and_then(|e| b.get(o..e)).and_then(|s| <[u8; N]>::try_from(s).ok()) {
        Some(a) => Ok(a),
        None => err(format!("truncated data: {N} bytes wanted at offset {o:#x} of {}", b.len())),
    }
}

fn u16_at(b: &[u8], o: usize) -> R<u16> {
    bytes(b, o).map(u16::from_le_bytes)
}

fn i16_at(b: &[u8], o: usize) -> R<i16> {
    bytes(b, o).map(i16::from_le_bytes)
}

fn u32_at(b: &[u8], o: usize) -> R<u32> {
    bytes(b, o).map(u32::from_le_bytes)
}

/// Python-style `b[o:o+l]`: clamped to the buffer, never fails.
fn py_slice(b: &[u8], o: usize, l: usize) -> &[u8] {
    let s = o.min(b.len());
    &b[s..o.saturating_add(l).min(b.len())]
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    if n.is_empty() {
        return Some(0);
    }
    h.windows(n.len()).position(|w| w == n)
}

/// Inflates one complete zlib stream (trailing bytes ignored); a truncated or corrupt stream is an
/// error, as with Python's `zlib.decompress`.
fn inflate(src: &[u8]) -> R<Vec<u8>> {
    let mut z = Decompress::new(true);
    let mut out = Vec::with_capacity(src.len().saturating_mul(2).max(256));
    loop {
        if out.len() == out.capacity() {
            out.reserve(out.len().max(256));
        }
        let (i0, o0) = (z.total_in(), z.total_out());
        let input = src.get(usize::try_from(i0)?..).unwrap_or(&[]);
        if matches!(z.decompress_vec(input, &mut out, FlushDecompress::None)?, Status::StreamEnd) {
            return Ok(out);
        }
        if z.total_in() == i0 && z.total_out() == o0 {
            return err("truncated zlib stream");
        }
    }
}

/// `base` joined with a `/`- or `\`-separated relative path, refusing anything that could leave
/// `base` (`..`, drive letters, alternate streams). Empty and `.` components are skipped.
fn safe_join(base: &Path, rel: &str) -> R<PathBuf> {
    let mut p = base.to_path_buf();
    let mut any = false;
    for c in rel.split(['/', '\\']) {
        match c {
            "" | "." => {}
            ".." => return err(format!("unsafe path {rel:?}")),
            _ if c.contains(':') => return err(format!("unsafe path {rel:?}")),
            _ => {
                p.push(c);
                any = true;
            }
        }
    }
    if !any {
        return err(format!("empty path {rel:?}"));
    }
    Ok(p)
}

fn write_file(p: &Path, data: &[u8]) -> R<()> {
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(p, data)?;
    Ok(())
}

// ---- .pak ----

struct PakEntry {
    hash: u32,
    raw_size: u32,
    compressed: u8,
    data: Vec<u8>,
}

fn read_pak(b: &[u8]) -> R<Vec<PakEntry>> {
    let count = u32_at(b, 4)?;
    let mut out = Vec::new();
    for i in 0..count as usize {
        let e = i
            .checked_mul(17)
            .and_then(|x| x.checked_add(8))
            .ok_or("pak entry table overflows")?;
        let hash = u32_at(b, e)?;
        let stored = u32_at(b, e + 4)? as usize;
        let raw_size = u32_at(b, e + 8)?;
        let [compressed] = bytes::<1>(b, e + 12)?;
        let off = u32_at(b, e + 13)? as usize;
        let Some(raw) = off.checked_add(stored).and_then(|end| b.get(off..end)) else {
            return err(format!(
                "pak entry {i}: data {off:#x}+{stored:#x} is beyond the end of the file ({:#x})",
                b.len()
            ));
        };
        let data = if compressed == 1 {
            inflate(raw).unwrap_or_else(|e| {
                println!("zlib fail {hash:#x} {e}");
                Vec::new()
            })
        } else {
            raw.to_vec()
        };
        out.push(PakEntry { hash, raw_size, compressed, data });
    }
    Ok(out)
}

/// Python's `str.isprintable` (approximate outside ASCII: control, separator and the common
/// format/private-use characters are non-printable).
fn py_printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    if c.is_control() || c.is_whitespace() {
        return false;
    }
    !matches!(c as u32,
        0xAD | 0x600..=0x605 | 0x61C | 0x6DD | 0x70F | 0x180E | 0x200B..=0x200F
        | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB
        | 0xE000..=0xF8FF | 0xF0000..=0x10FFFF)
}

/// File extension guessed from the content.
fn sniff(d: &[u8]) -> String {
    let at = |o: usize, m: &[u8]| d.get(o..o + m.len()) == Some(m);
    let k = if d.starts_with(b"\x89PNG\r\n\x1a\n") {
        "png"
    } else if d.starts_with(b"RIFF") {
        "wav"
    } else if d.starts_with(b"OggS") {
        "ogg"
    } else if d.starts_with(b"ID3") || d.starts_with(b"\xff\xfb") {
        "mp3"
    } else if d.starts_with(b"\xff\xd8") {
        "jpg"
    } else if d.starts_with(b"<?xml") {
        "xml"
    } else if d.starts_with(b"MThd") {
        "mid"
    } else if d.starts_with(b"IMPM") {
        "it"
    } else if d.starts_with(b"Extended Module: ") {
        "xm"
    } else if d.len() > 1080 && (at(1080, b"M.K.") || at(1080, b"4CHN") || at(1080, b"8CHN")) {
        "mod"
    } else {
        let head = &d[..d.len().min(4)];
        if !head.is_empty() && head.iter().all(u8::is_ascii_alphabetic) {
            return format!("tag_{}", String::from_utf8_lossy(head));
        }
        match std::str::from_utf8(&d[..d.len().min(200)]) {
            Ok(t) if t.chars().all(|c| py_printable(c) || "\r\n\t".contains(c)) => "txt",
            _ => "bin",
        }
    };
    k.to_string()
}

/// The last entry of each pak lists the other entries' paths, in entry order.
fn namelist(ents: &[PakEntry]) -> Option<Vec<String>> {
    let d = &ents.last()?.data;
    let n = u32_at(d, 0).ok()? as usize;
    if n != ents.len() - 1 {
        return None;
    }
    let mut o = 4;
    let mut names = Vec::with_capacity(n);
    for _ in 0..n {
        let l = usize::from(u16_at(d, o).ok()?);
        o += 2;
        names.push(d.get(o..o + l)?.iter().map(|&c| char::from(c)).collect());
        o += l;
    }
    if o != d.len() {
        return None;
    }
    names.push("_namelist.bin".to_string());
    Some(names)
}

/// `pak list|extract FILE.pak [OUTDIR]`
pub fn pak(args: &[String]) -> R<()> {
    const USAGE: &str = "usage: pak list|extract FILE.pak [OUTDIR]";
    let [cmd, path, rest @ ..] = args else { return err(USAGE) };
    let outdir = match cmd.as_str() {
        "list" => None,
        "extract" => Some(Path::new(rest.first().ok_or(USAGE)?)),
        _ => return err(USAGE),
    };
    let ents = read_pak(&fs::read(path)?)?;
    if let Some(od) = outdir {
        fs::create_dir_all(od)?;
    }
    let names = namelist(&ents);
    let mut kinds: Vec<(String, usize)> = Vec::new();
    for (i, e) in ents.iter().enumerate() {
        let k = sniff(&e.data);
        match kinds.iter_mut().find(|(n, _)| *n == k) {
            Some((_, c)) => *c += 1,
            None => kinds.push((k.clone(), 1)),
        }
        let name = match &names {
            Some(n) => n[i].replace('\\', "/"),
            None => format!("{i:04}_{:08x}.{k}", e.hash),
        };
        match outdir {
            None => println!("{i:5} {:08x} {:9} {} {k:<10} {name}", e.hash, e.raw_size, e.compressed),
            Some(od) => write_file(&safe_join(od, &name)?, &e.data)?,
        }
    }
    let kinds: Vec<String> = kinds.iter().map(|(k, c)| format!("'{k}': {c}")).collect();
    let base = Path::new(path).file_name().map_or_else(|| path.into(), |f| f.to_string_lossy());
    println!("{base} {} {{{}}}", ents.len(), kinds.join(", "));
    Ok(())
}

// ---- .gfx ----

/// `gfx FILE.gfx OUT.png`
pub fn gfx(args: &[String]) -> R<()> {
    let [src, dst, ..] = args else { return err("usage: gfx FILE.gfx OUT.png") };
    let d = fs::read(src)?;
    if d.len() < 40 {
        return err(format!("{src}: truncated gfx header ({} bytes)", d.len()));
    }
    let (fmt, w, h, frames) = (u32_at(&d, 4)?, u32_at(&d, 8)?, u32_at(&d, 12)?, u32_at(&d, 16)?);
    if fmt != 0 {
        return err(format!("format {fmt} not supported yet"));
    }
    let rows = h.checked_mul(frames).ok_or("gfx too large")?;
    let px = (w as usize)
        .checked_mul(rows as usize)
        .and_then(|n| n.checked_mul(4))
        .and_then(|n| d.get(40..40 + n))
        .ok_or("not enough image data")?;
    let rgba: Vec<u8> = px.as_chunks::<4>().0.iter().flat_map(|&[b, g, r, a]| [r, g, b, a]).collect();
    let mut enc = png::Encoder::new(BufWriter::new(fs::File::create(dst)?), w, rows);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header()?;
    wr.write_image_data(&rgba)?;
    wr.finish()?;
    println!("{src} {fmt} {w} {h} {frames}");
    Ok(())
}

// ---- .spc ----

struct Spc {
    rate: u16,
    bits: u16,
    count: u32,
    pcm: Vec<i16>,
}

fn spc_decode(d: &[u8], low_first: bool) -> R<Spc> {
    let (rate, bits, count) = (u16_at(d, 6)?, u16_at(d, 10)?, u32_at(d, 12)?);
    let mut table = [0i16; 16];
    for (k, t) in table.iter_mut().enumerate() {
        *t = i16_at(d, 0x22 + 2 * k)?;
    }
    let pcm = d
        .get(0x42..)
        .unwrap_or(&[])
        .iter()
        .flat_map(|&b| if low_first { [b & 15, b >> 4] } else { [b >> 4, b & 15] })
        .take(count as usize)
        .map(|c| table[usize::from(c)])
        .collect();
    Ok(Spc { rate, bits, count, pcm })
}

/// 16-bit mono PCM WAV (44-byte header).
fn wav(rate: u16, pcm: &[i16]) -> R<Vec<u8>> {
    let data_len = pcm
        .len()
        .checked_mul(2)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|&n| n <= u32::MAX - 36)
        .ok_or("too many samples for a WAV file")?;
    let rate = u32::from(rate);
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&1u16.to_le_bytes()); // mono
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 2).to_le_bytes()); // bytes per second
    v.extend_from_slice(&2u16.to_le_bytes()); // block align
    v.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        v.extend_from_slice(&s.to_le_bytes());
    }
    Ok(v)
}

/// `spc FILE.spc OUT.wav [--low-first]`
pub fn spc(args: &[String]) -> R<()> {
    let low_first = args.iter().any(|a| a == "--low-first");
    let pos: Vec<&String> = args.iter().filter(|a| a.as_str() != "--low-first").collect();
    let [src, dst, ..] = pos.as_slice() else {
        return err("usage: spc FILE.spc OUT.wav [--low-first]");
    };
    let s = spc_decode(&fs::read(src)?, low_first)?;
    if s.rate == 0 {
        return err(format!("{src}: sample rate is 0"));
    }
    fs::write(dst, wav(s.rate, &s.pcm)?)?;
    let clipped = s.pcm.iter().filter(|x| x.unsigned_abs() >= 32767).count();
    let mean = s.pcm.iter().map(|&x| i64::from(x)).sum::<i64>() as f64 / s.pcm.len().max(1) as f64;
    println!(
        "{src}: {} Hz, {}-bit codes, {} samples ({:.1}s), clipped {clipped}, mean {mean:.0}",
        s.rate,
        s.bits,
        s.count,
        f64::from(s.count) / f64::from(s.rate)
    );
    Ok(())
}

// ---- .sis (SISX) ----

/// Recursion limit for nested controllers / install blocks (malformed input).
const MAX_NEST: usize = 64;

/// (data unit index, file index, target path)
type FileDesc = (usize, u32, String);

#[derive(Default)]
struct Meta {
    uid: Option<u32>,
    strings: Vec<String>,
    names: Vec<String>,
    version: Option<[i32; 3]>,
}

fn to_usize(l: u64) -> usize {
    usize::try_from(l).unwrap_or(usize::MAX)
}

fn pad(l: usize) -> usize {
    (4 - l % 4) % 4
}

/// A field length at `o`: u32, and if its bit 31 is set a second u32 holding bits 31 and up.
/// Returns the length and the offset after it.
fn rlen(b: &[u8], o: usize) -> R<(u64, usize)> {
    let mut l = u64::from(u32_at(b, o)?);
    let mut o = o + 4;
    if l & 0x8000_0000 != 0 {
        let hi = u64::from(u32_at(b, o)?);
        o += 4;
        l = (l & 0x7fff_ffff) | (hi << 31);
    }
    Ok((l, o))
}

/// The (type, data) fields packed in `b`.
fn fields(b: &[u8]) -> R<Vec<(u32, &[u8])>> {
    let mut v = Vec::new();
    let mut o = 0usize;
    while o.checked_add(8).is_some_and(|e| e <= b.len()) {
        let t = u32_at(b, o)?;
        let (l, d) = rlen(b, o + 4)?;
        let l = to_usize(l);
        v.push((t, py_slice(b, d, l)));
        o = d.saturating_add(l).saturating_add(pad(l));
    }
    Ok(v)
}

/// An array: (element type, element data).
fn array(d: &[u8]) -> R<(u32, Vec<&[u8]>)> {
    let et = u32_at(d, 0)?;
    let mut v = Vec::new();
    let mut o = 4usize;
    while o.checked_add(4).is_some_and(|e| e <= d.len()) {
        let (l, s) = rlen(d, o)?;
        let l = to_usize(l);
        v.push(py_slice(d, s, l));
        o = s.saturating_add(l).saturating_add(pad(l));
    }
    Ok((et, v))
}

/// The payload of a compressed field.
fn sis_decompress(d: &[u8]) -> R<Vec<u8>> {
    let body = d.get(12..).unwrap_or(&[]);
    if u32_at(d, 0)? == 1 { inflate(body) } else { Ok(body.to_vec()) }
}

fn sstr(d: &[u8]) -> String {
    let units: Vec<u16> = d.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
    let mut s = String::from_utf16_lossy(&units);
    if d.len() % 2 == 1 {
        s.push('\u{FFFD}');
    }
    s
}

fn exact<const N: usize>(d: &[u8]) -> R<[u8; N]> {
    <[u8; N]>::try_from(d).or_else(|_| err(format!("expected {N} bytes, got {}", d.len())))
}

/// Collects (data unit, file index, target) from a controller; its metadata goes to `out`.
fn parse_controller(
    c: &[u8],
    out: &mut Vec<(usize, Meta)>,
    depth: usize,
    nest: usize,
) -> R<Vec<FileDesc>> {
    if nest > MAX_NEST {
        return err("SIS controllers nested too deeply");
    }
    let fl = fields(c)?;
    let mut data_index = 0usize;
    for &(t, d) in &fl {
        if t == 40 {
            data_index = u32_at(d, 0)? as usize;
        }
    }
    let mut files = Vec::new();
    let mut meta = Meta::default();
    for &(t, d) in &fl {
        if t == 14 {
            for (t2, d2) in fields(d)? {
                match t2 {
                    9 => meta.uid = Some(u32::from_le_bytes(exact(d2)?)),
                    1 => meta.strings.push(sstr(d2)),
                    2 => {
                        for s in array(d2)?.1 {
                            meta.names.push(sstr(s));
                        }
                    }
                    4 => {
                        let v: [u8; 12] = exact(d2)?;
                        meta.version = Some([0, 4, 8].map(|k| {
                            i32::from_le_bytes([v[k], v[k + 1], v[k + 2], v[k + 3]])
                        }));
                    }
                    _ => {}
                }
            }
        }
        if t == 28 {
            install_block(d, data_index, &mut files, out, depth, nest + 1)?;
        }
    }
    out.push((depth, meta));
    Ok(files)
}

fn install_block(
    d: &[u8],
    du: usize,
    files: &mut Vec<FileDesc>,
    out: &mut Vec<(usize, Meta)>,
    depth: usize,
    nest: usize,
) -> R<()> {
    if nest > MAX_NEST {
        return err("SIS install blocks nested too deeply");
    }
    for (t2, d2) in fields(d)? {
        if t2 != 2 || d2.len() < 4 {
            continue;
        }
        let (et, items) = array(d2)?;
        for e in items {
            match et {
                24 => {
                    let Some(&(_, target)) = fields(e)?.first() else {
                        return err("SIS file description without a target");
                    };
                    let idx = u32_at(e, e.len().saturating_sub(4))?;
                    files.push((du, idx, sstr(target)));
                }
                13 => files.extend(parse_controller(e, out, depth + 1, nest + 1)?),
                26 => {
                    // If: expression, install block, else-ifs
                    for (t3, d3) in fields(e)? {
                        if t3 == 28 {
                            install_block(d3, du, files, out, depth, nest + 1)?;
                        }
                        if t3 == 2 {
                            for ei in array(d3)?.1 {
                                for (t4, d4) in fields(ei)? {
                                    if t4 == 28 {
                                        install_block(d4, du, files, out, depth, nest + 1)?;
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn meta_repr(depth: usize, m: &Meta) -> String {
    let mut parts = Vec::new();
    if let Some(u) = m.uid {
        parts.push(format!("'uid': '{u:#x}'"));
    }
    if !m.strings.is_empty() {
        parts.push(format!("'strings': {:?}", m.strings));
    }
    if !m.names.is_empty() {
        parts.push(format!("'names': {:?}", m.names));
    }
    if let Some([a, b, c]) = m.version {
        parts.push(format!("'version': ({a}, {b}, {c})"));
    }
    format!("({depth}, {{{}}})", parts.join(", "))
}

/// Output path of a file under `du<N>/`: drive dropped, `/` separators.
fn sis_name(target: &str, du: usize, idx: u32) -> String {
    let name = target.replace("!:", "").replace('\\', "/");
    let mut name = name.trim_start_matches('/');
    let b = name.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        name = name[2..].trim_start_matches('/');
    }
    let mut name = name.to_string();
    if name.is_empty() || name.ends_with('/') {
        name.push_str(&format!("unnamed{du}_{idx}"));
    }
    name
}

/// `sisx FILE.sis OUTDIR`
pub fn sisx(args: &[String]) -> R<()> {
    let [path, outdir, ..] = args else { return err("usage: sisx FILE.sis OUTDIR") };
    let b = fs::read(path)?;
    let uids = (0..4)
        .map(|k| u32_at(&b, 4 * k).map(|u| format!("'{u:#x}'")))
        .collect::<R<Vec<_>>>()?;
    println!("UIDs [{}]", uids.join(", "));
    u32_at(&b, 16)?; // contents field type
    let (l, o) = rlen(&b, 20)?;
    let contents = py_slice(&b, o, to_usize(l));
    let (mut ctrl_buf, mut data) = (None, None);
    for (t, d) in fields(contents)? {
        if t == 3 {
            ctrl_buf = Some(sis_decompress(d)?);
        }
        if t == 30 {
            data = Some(d);
        }
    }
    fs::create_dir_all(outdir)?;
    let ctrl_buf = ctrl_buf.ok_or("SIS contents have no compressed controller (field 3)")?;
    // The compressed blob is the Controller field itself.
    let ctrl_fields = fields(&ctrl_buf)?;
    let ctrl = match ctrl_fields.first() {
        Some(&(13, c)) => c,
        _ => &ctrl_buf[..],
    };
    let mut meta = Vec::new();
    let files = parse_controller(ctrl, &mut meta, 0, 0)?;
    for (depth, m) in &meta {
        println!("controller {}", meta_repr(*depth, m));
    }
    let data = data.ok_or("SIS contents have no data (field 30)")?;
    let mut units: Vec<Vec<&[u8]>> = Vec::new();
    for (_, d) in fields(data)? {
        for du in array(d)?.1 {
            let mut blobs = Vec::new();
            for (_, d2) in fields(du)? {
                for fdata in array(d2)?.1 {
                    for (t3, d3) in fields(fdata)? {
                        if t3 == 3 {
                            blobs.push(d3);
                        }
                    }
                }
            }
            units.push(blobs);
        }
    }
    let counts: Vec<usize> = units.iter().map(Vec::len).collect();
    println!("data units {counts:?} file descs {}", files.len());
    let root = Path::new(outdir);
    for (du, idx, target) in &files {
        let name = sis_name(target, *du, *idx);
        let res = match units.get(*du).and_then(|u| u.get(*idx as usize)) {
            Some(blob) => sis_decompress(blob),
            None => err("no such data unit / file index"),
        }
        .and_then(|raw| Ok((safe_join(&root.join(format!("du{du}")), &name)?, raw)));
        let (p, raw) = match res {
            Ok(x) => x,
            Err(e) => {
                println!("fail {target} {e}");
                continue;
            }
        };
        write_file(&p, &raw)?;
        println!("{du} {idx:4} {:10} {target}", raw.len());
    }
    Ok(())
}

// ---- .n-gage ----

/// The value of `Content-ID: <...>` (non-empty, as the regex `Content-ID: <([^>]+)>`).
fn content_id(hdr: &[u8]) -> Option<&[u8]> {
    const PRE: &[u8] = b"Content-ID: <";
    let mut from = 0;
    while let Some(k) = find(&hdr[from..], PRE) {
        let s = from + k + PRE.len();
        match hdr[s..].iter().position(|&c| c == b'>') {
            Some(0) => from += k + 1,
            Some(e) => return Some(&hdr[s..s + e]),
            None => return None,
        }
    }
    None
}

/// The value of `Content-Transfer-Encoding: (\S+)`.
fn transfer_encoding(hdr: &[u8]) -> Option<&[u8]> {
    const PRE: &[u8] = b"Content-Transfer-Encoding: ";
    let space = |c: &u8| b" \t\n\r\x0b\x0c".contains(c);
    let mut from = 0;
    while let Some(k) = find(&hdr[from..], PRE) {
        let s = from + k + PRE.len();
        let n = hdr[s..].iter().take_while(|c| !space(c)).count();
        if n > 0 {
            return Some(&hdr[s..s + n]);
        }
        from += k + 1;
    }
    None
}

/// `split_ngage FILE.n-gage OUTDIR`
pub fn split_ngage(args: &[String]) -> R<()> {
    let [src, outdir, ..] = args else { return err("usage: split_ngage FILE.n-gage OUTDIR") };
    let data = fs::read(src)?;
    fs::create_dir_all(outdir)?;
    const BOUNDARY: &[u8] = b"--KBoundary";
    let mut parts = Vec::new();
    let mut rest = &data[..];
    while let Some(k) = find(rest, BOUNDARY) {
        parts.push(&rest[..k]);
        rest = &rest[k + BOUNDARY.len()..];
    }
    parts.push(rest);
    for (i, p) in parts.iter().skip(1).enumerate() {
        if p.starts_with(b"--") {
            break;
        }
        let (hdr, body) = match find(p, b"\r\n\r\n") {
            Some(k) => (&p[..k], &p[k + 4..]),
            None => (*p, &[][..]),
        };
        let body = body.strip_suffix(b"\r\n").unwrap_or(body);
        let name = match content_id(hdr) {
            Some(id) => String::from_utf8(id.to_vec())?,
            None => format!("part{i}"),
        };
        let enc = transfer_encoding(hdr).map_or("None".into(), String::from_utf8_lossy);
        let hdr_text: String = hdr.iter().map(|&c| char::from(c)).collect();
        println!("{i} {name} {} {} {enc}", body.len(), hdr_text.trim().replace("\r\n", " | "));
        let ext = if find(hdr, b"text/xml").is_some() { ".xml" } else { ".sis" };
        fs::write(safe_join(Path::new(outdir), &format!("{name}{ext}"))?, body)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A pak with the given (raw data, compress?) entries.
    fn pak_bytes(entries: &[(&[u8], bool)]) -> Vec<u8> {
        let base = 8 + 17 * entries.len();
        let (mut table, mut data) = (Vec::new(), Vec::new());
        for (i, &(raw, z)) in entries.iter().enumerate() {
            let stored = if z {
                let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                e.write_all(raw).unwrap();
                e.finish().unwrap()
            } else {
                raw.to_vec()
            };
            table.extend((0x1111 * i as u32).to_le_bytes());
            table.extend((stored.len() as u32).to_le_bytes());
            table.extend((raw.len() as u32).to_le_bytes());
            table.push(z as u8);
            table.extend(((base + data.len()) as u32).to_le_bytes());
            data.extend(stored);
        }
        let mut b = (base as u32).to_le_bytes().to_vec();
        b.extend((entries.len() as u32).to_le_bytes());
        b.extend(table);
        b.extend(data);
        b
    }

    fn names_blob(names: &[&str]) -> Vec<u8> {
        let mut b = (names.len() as u32).to_le_bytes().to_vec();
        for n in names {
            b.extend((n.len() as u16).to_le_bytes());
            b.extend(n.as_bytes());
        }
        b
    }

    #[test]
    fn pak_namelist_and_zlib() {
        let png = b"\x89PNG\r\n\x1a\nxx";
        let list = names_blob(&["gfx\\a.png", "b.txt"]);
        let ents = read_pak(&pak_bytes(&[(png, true), (b"hello", false), (&list, false)])).unwrap();
        assert_eq!(ents[0].data, png);
        assert_eq!(sniff(&ents[0].data), "png");
        assert_eq!(sniff(&ents[1].data), "tag_hell");
        assert_eq!(namelist(&ents).unwrap(), ["gfx\\a.png", "b.txt", "_namelist.bin"]);

        // A list that does not name every other entry is ignored.
        let list = names_blob(&["x", "y"]);
        let ents = read_pak(&pak_bytes(&[(b"hello", false), (&list, false)])).unwrap();
        assert!(namelist(&ents).is_none());

        // A truncated zlib stream is an error (the pak entry extracts as empty).
        let mut pak = pak_bytes(&[(&[7u8; 100], true)]);
        pak.truncate(pak.len() - 3);
        let e = 8;
        let stored = u32::from_le_bytes(pak[e + 4..e + 8].try_into().unwrap()) - 3;
        pak[e + 4..e + 8].copy_from_slice(&stored.to_le_bytes());
        assert!(read_pak(&pak).unwrap()[0].data.is_empty());
    }

    #[test]
    fn sis_lengths_and_fields() {
        // Bit 31 set: a second word supplies bits 31 and up.
        assert_eq!(rlen(&[5, 0, 0, 0x80, 2, 0, 0, 0], 0).unwrap(), ((2u64 << 31) | 5, 8));
        assert_eq!(rlen(&[7, 0, 0, 0], 0).unwrap(), (7, 4));
        assert!(rlen(&[0, 0, 0, 0x80], 0).is_err());
        // Fields are padded to 4 bytes.
        let f = [1, 0, 0, 0, 3, 0, 0, 0, b'a', b'b', b'c', 0, 2, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(fields(&f).unwrap(), [(1, &b"abc"[..]), (2, &b""[..])]);
        assert_eq!(sis_name("!:\\sys\\bin\\", 1, 2), "sys/bin/unnamed1_2");
        assert_eq!(sis_name("c:\\private\\x.dat", 0, 0), "private/x.dat");
    }

    #[test]
    fn spc_nibbles_and_count() {
        let mut d = vec![0u8; 0x42];
        d[6..8].copy_from_slice(&8000u16.to_le_bytes());
        d[12..16].copy_from_slice(&3u32.to_le_bytes());
        for k in 0..16 {
            d[0x22 + 2 * k..0x24 + 2 * k].copy_from_slice(&(k as i16 * 100 - 800).to_le_bytes());
        }
        d.extend([0x12, 0x34, 0x56]);
        assert_eq!(spc_decode(&d, false).unwrap().pcm, [-700, -600, -500]);
        assert_eq!(spc_decode(&d, true).unwrap().pcm, [-600, -700, -400]);
        assert_eq!(wav(8000, &[1, -1]).unwrap().len(), 48);
    }
}
