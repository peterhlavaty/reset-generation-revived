//! Adapts the game's data paks to the engine size of the native builds (was `rg.PakPatcher` in
//! the retired Java host). The builds copy `game/ResetGeneration` and patch the copy; `game/`
//! itself stays pristine.
//!
//! * base.pak: the screen-transition masks (`data/base/gfx/fadeWipe*.gfx`) are resized. The wipe
//!   composites the new screen through a mask and loops over the full screen while indexing the
//!   mask with its own stride: the shipped 240x320 masks make any taller screen read past the
//!   mask (crash) and any wider one show stripes.
//! * intro.pak / story.pak: cutscene scripts place sprites in absolute 240x320 coordinates; they
//!   are shifted so the scene sits in the centre of the screen. (Not the rest of menus.pak: its
//!   win/lose screens are shifted by the recompiler, and fight.pak's cut-ins render into a fixed
//!   240x320 surface.)
//! * menus.pak: the error / question dialog's scene (panel, speech bubble, hedgehog: "Connection
//!   lost", "Could not connect", …) is shifted to the centre like a cutscene, under the dialog's
//!   text, which the recompiler lays out 240 px wide at x + 200 (`recomp.rs`, `FUN_100cc530`).
//!   The end screen's "View replay at www.ResetGeneration.com" line is dropped (the site is gone).
//!
//! Pak layout (little endian): `u32 header_size, u32 count`, then `count` entries of
//! `{u32 name_hash, u32 stored_size, u32 raw_size, u8 zlib, u32 offset}`; the last entry lists
//! every other entry's path, in order.

use crate::data::inflate;
use crate::util::R;
use flate2::{write::ZlibEncoder, Compression};
use std::io::Write;
use std::path::Path;

const MASK_PREFIX: &str = "data\\base\\gfx\\fadewipe";
const GFX_HEADER: usize = 40;
const SCENE_PAKS: [&str; 2] = ["intro", "story"];
/// The scene behind the engine's error / question dialog (FUN_100cc350 plays it, FUN_100cc530 draws
/// it with the text); `_bg2` is an unused variant, shifted alike in case a code path loads it.
const ERROR_SCENES: [&str; 2] = ["data\\menus\\errorscreen_bg.xml", "data\\menus\\errorscreen_bg2.xml"];
const END_MENU: &str = "data\\menus\\end_arena_menu.xml";
/// The info line of the end screen's buttons: "View replay at www.ResetGeneration.com" (every
/// language). The key is only used there, so dropping the `<info>` lines removes it.
const DEAD_SITE_INFO: &str = "<info>REPLAY_SAVED_TXT</info>";

struct Entry {
    hash: u32,
    zlib: bool,
    raw: Vec<u8>,
}

fn u32_at(b: &[u8], o: usize) -> R<u32> {
    Ok(u32::from_le_bytes(b.get(o..o + 4).ok_or("pak: truncated")?.try_into()?))
}

fn read(b: &[u8]) -> R<Vec<Entry>> {
    let count = u32_at(b, 4)? as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let p = 8 + i * 17;
        let (hash, stored, raw_size) = (u32_at(b, p)?, u32_at(b, p + 4)? as usize, u32_at(b, p + 8)? as usize);
        let zlib = *b.get(p + 12).ok_or("pak: truncated")? != 0;
        let off = u32_at(b, p + 13)? as usize;
        let data = b.get(off..off + stored).ok_or("pak: entry beyond the end of the file")?;
        let raw = if zlib { inflate(data)? } else { data.to_vec() };
        if raw.len() != raw_size {
            return Err(format!("pak entry {i}: {} bytes, expected {raw_size}", raw.len()).into());
        }
        out.push(Entry { hash, zlib, raw });
    }
    Ok(out)
}

fn write(entries: &[Entry]) -> R<Vec<u8>> {
    let header = 8 + entries.len() * 17;
    let stored: Vec<Vec<u8>> = entries
        .iter()
        .map(|e| {
            if !e.zlib {
                return Ok(e.raw.clone());
            }
            let mut z = ZlibEncoder::new(Vec::new(), Compression::best());
            z.write_all(&e.raw)?;
            z.finish()
        })
        .collect::<Result<_, std::io::Error>>()?;
    let mut out = Vec::with_capacity(header + stored.iter().map(Vec::len).sum::<usize>());
    out.extend((header as u32).to_le_bytes());
    out.extend((entries.len() as u32).to_le_bytes());
    let mut off = header;
    for (e, s) in entries.iter().zip(&stored) {
        out.extend(e.hash.to_le_bytes());
        out.extend((s.len() as u32).to_le_bytes());
        out.extend((e.raw.len() as u32).to_le_bytes());
        out.push(e.zlib as u8);
        out.extend((off as u32).to_le_bytes());
        off += s.len();
    }
    stored.iter().for_each(|s| out.extend(s));
    Ok(out)
}

fn names(d: &[u8]) -> Option<Vec<String>> {
    let n = u32_at(d, 0).ok()? as usize;
    let mut o = 4;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let l = u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?) as usize;
        out.push(d.get(o + 2..o + 2 + l)?.iter().map(|&c| char::from(c)).collect());
        o += 2 + l;
    }
    Some(out)
}

/// Rewrites `dir/<base>.pak`: `t(lower-case path, raw)` returns the new contents of an entry, or
/// None to keep it. Returns the number of entries changed.
fn patch_pak(dir: &Path, base: &str, t: impl Fn(&str, &[u8]) -> Option<Vec<u8>>) -> R<usize> {
    let path = dir.join(format!("{base}.pak"));
    let mut entries = read(&std::fs::read(&path)?)?;
    let names = entries.last().and_then(|e| names(&e.raw)).ok_or_else(|| format!("{base}.pak: no name list"))?;
    let mut changed = 0;
    for (e, n) in entries.iter_mut().zip(&names) {
        if let Some(raw) = t(&n.to_ascii_lowercase(), &e.raw) {
            e.raw = raw;
            changed += 1;
        }
    }
    std::fs::write(&path, write(&entries)?)?;
    Ok(changed)
}

/// Patches the paks in `dir` (a copy of game/ResetGeneration) for a `w`x`h` engine.
pub fn patch_dir(dir: &Path, w: u32, h: u32) -> R<()> {
    let n = patch_pak(dir, "base", |name, raw| name.starts_with(MASK_PREFIX).then(|| resize_mask(raw, w, h)))?;
    eprintln!("patched {n} entries in base.pak ({w}x{h})");
    let (dx, dy) = (w.saturating_sub(240) as i32 / 2, h.saturating_sub(320) as i32 / 2);
    for base in SCENE_PAKS {
        let n = patch_pak(dir, base, |name, raw| if name.ends_with(".xml") { offset_scene(raw, dx, dy) } else { None })?;
        eprintln!("patched {n} entries in {base}.pak ({dx},{dy})");
    }
    let n = patch_pak(dir, "menus", |name, raw| menus_entry(name, raw, dx))?;
    eprintln!("patched {n} entries in menus.pak");
    Ok(())
}

/// menus.pak: the error dialog's scene moves by `dx` only. The engine places the dialog's text at a
/// constant y of the 240x320 layout (centred on y 125, inside the bubble), so moving the bubble
/// down would leave the text at its top edge.
fn menus_entry(name: &str, raw: &[u8], dx: i32) -> Option<Vec<u8>> {
    if ERROR_SCENES.contains(&name) {
        offset_scene(raw, dx, 0)
    } else if name == END_MENU {
        drop_lines(raw, DEAD_SITE_INFO)
    } else {
        None
    }
}

/// Removes every line that contains `text`; None when there is none.
fn drop_lines(raw: &[u8], text: &str) -> Option<Vec<u8>> {
    let t = text.as_bytes();
    let out: Vec<u8> =
        raw.split_inclusive(|&c| c == b'\n').filter(|l| !l.windows(t.len()).any(|w| w == t)).flatten().copied().collect();
    (out.len() != raw.len()).then_some(out)
}

/// Nearest-neighbour horizontal stretch, vertical tiling: keeps the halftone look.
fn resize_mask(gfx: &[u8], w: u32, h: u32) -> Vec<u8> {
    let get = |o: usize| u32_at(gfx, o).unwrap_or(0);
    let (format, sw, sh) = (get(4), get(8) as usize, get(12) as usize);
    if format != 0 || sw == 0 || sh == 0 || gfx.len() < GFX_HEADER + sw * sh * 4 {
        return gfx.to_vec();
    }
    let (w, h) = (w as usize, h as usize);
    let mut out = gfx[..GFX_HEADER].to_vec();
    out.resize(GFX_HEADER + w * h * 4, 0);
    for (at, v) in [(8, w), (12, h), (16, 1), (36, w * h * 4)] {
        out[at..at + 4].copy_from_slice(&(v as u32).to_le_bytes());
    }
    for y in 0..h {
        let sy = y % sh;
        for x in 0..w {
            let s = GFX_HEADER + (sy * sw + x * sw / w) * 4;
            let d = GFX_HEADER + (y * w + x) * 4;
            let px: [u8; 4] = gfx[s..s + 4].try_into().unwrap();
            out[d..d + 4].copy_from_slice(&px);
        }
    }
    out
}

/// Shifts absolute positions in a scene script by (dx, dy): `positionX/Y` of
/// `<physics … type="RESET" …>` and `posX/Y` of `<effect … name="PARTICLE" …>`.
fn offset_scene(raw: &[u8], dx: i32, dy: i32) -> Option<Vec<u8>> {
    if dx == 0 && dy == 0 {
        return None;
    }
    let xml: String = raw.iter().map(|&c| char::from(c)).collect(); // ISO-8859-1
    let out = shift(&xml, "physics", "type=\"RESET\"", "positionX", "positionY", dx, dy);
    let out = shift(&out, "effect", "name=\"PARTICLE\"", "posX", "posY", dx, dy);
    (out != xml).then(|| out.chars().map(|c| c as u8).collect())
}

fn word(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Every `<tag …>` (no `>` inside) that contains `must` gets `d` added to its attributes.
fn shift(xml: &str, tag: &str, must: &str, ax: &str, ay: &str, dx: i32, dy: i32) -> String {
    let open = format!("<{tag}");
    let mut out = String::with_capacity(xml.len() + 64);
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        let Some(end) = after.find('>') else { break };
        out.push_str(&rest[..i]);
        let whole = &rest[i..i + open.len() + end + 1];
        if word(after.chars().next()) || !whole.contains(must) {
            out.push_str(&rest[i..i + open.len()]);
            rest = after;
            continue;
        }
        out.push_str(&add_to(&add_to(whole, ax, dx), ay, dy));
        rest = &rest[i + whole.len()..];
    }
    out.push_str(rest);
    out
}

/// `attr="N"` (first occurrence at a word boundary) becomes `attr="N+d"`; Java's number formats
/// (an integer stays one, a decimal keeps a `.0`).
fn add_to(t: &str, attr: &str, d: i32) -> String {
    let key = format!("{attr}=\"");
    let mut from = 0;
    while let Some(i) = t[from..].find(&key).map(|i| i + from) {
        from = i + 1;
        if word(t[..i].chars().last()) {
            continue;
        }
        let vs = i + key.len();
        let len = t[vs..].find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(0);
        let v = &t[vs..vs + len];
        if len == 0 || !t[vs + len..].starts_with('"') || v[1..].contains('-') {
            continue;
        }
        let nv = if v.contains('.') {
            let Ok(f) = v.parse::<f64>() else { continue };
            let r = f + d as f64;
            if r.fract() == 0.0 && r.abs() < 1e7 { format!("{r:.1}") } else { format!("{r}") }
        } else {
            let Ok(n) = v.parse::<i64>() else { continue };
            (n + d as i64).to_string()
        };
        return format!("{}{nv}{}", &t[..vs], &t[vs + len..]);
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_offsets() {
        let x = br#"<a><physics type="RESET" positionX="10" positionY="2.5"/><physics type="MOVE" positionX="1"/><effect name="PARTICLE" posX="-3" posY="4.0"><physicsX type="RESET" positionX="7"/></a>"#;
        let out = String::from_utf8(offset_scene(x, 200, 20).unwrap()).unwrap();
        assert_eq!(
            out,
            r#"<a><physics type="RESET" positionX="210" positionY="22.5"/><physics type="MOVE" positionX="1"/><effect name="PARTICLE" posX="197" posY="24.0"><physicsX type="RESET" positionX="7"/></a>"#
        );
        assert!(offset_scene(x, 0, 0).is_none());
        assert_eq!(add_to(r#"<p xpositionX="1" positionX="2">"#, "positionX", 5), r#"<p xpositionX="1" positionX="7">"#);
    }

    #[test]
    fn mask_and_pak_round_trip() {
        let mut gfx = vec![0u8; GFX_HEADER];
        gfx[8..12].copy_from_slice(&2u32.to_le_bytes());
        gfx[12..16].copy_from_slice(&1u32.to_le_bytes());
        gfx.extend([1, 1, 1, 1, 2, 2, 2, 2]);
        let m = resize_mask(&gfx, 4, 3);
        assert_eq!(u32_at(&m, 8).unwrap(), 4);
        assert_eq!(&m[GFX_HEADER..GFX_HEADER + 16], &[1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2]);
        assert_eq!(m.len(), GFX_HEADER + 4 * 3 * 4);

        let mut list = 1u32.to_le_bytes().to_vec();
        list.extend(3u16.to_le_bytes());
        list.extend(b"a.x");
        let entries = vec![Entry { hash: 7, zlib: true, raw: b"hello hello hello".to_vec() }, Entry { hash: 9, zlib: false, raw: list }];
        let back = read(&write(&entries).unwrap()).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!((back[0].hash, back[0].zlib, back[0].raw.as_slice()), (7, true, &b"hello hello hello"[..]));
        assert_eq!(names(&back[1].raw).unwrap(), vec!["a.x".to_string()]);
    }

    #[test]
    fn menus_entries() {
        let end = b"<b>\r\n\t\t<info>REPLAY_SAVED_TXT</info>\r\n\t\t<text>ARENA</text>\r\n</b>";
        assert_eq!(menus_entry(END_MENU, end, 200).unwrap(), b"<b>\r\n\t\t<text>ARENA</text>\r\n</b>");
        assert!(menus_entry(END_MENU, b"<b>\r\n</b>", 200).is_none());
        let scene = br#"<physics start="0" type="RESET" positionX="-50" positionY="160" velocityX="35.4"/>"#;
        let moved = br#"<physics start="0" type="RESET" positionX="150" positionY="160" velocityX="35.4"/>"#;
        assert_eq!(menus_entry(ERROR_SCENES[0], scene, 200).unwrap(), moved);
        assert!(menus_entry("data\\menus\\hero01_win.xml", scene, 200).is_none());
        assert!(menus_entry(ERROR_SCENES[0], scene, 0).is_none());
    }

    /// The names above must match the shipped menus.pak, or the transform silently does nothing.
    #[test]
    fn menus_pak_has_the_patched_entries() {
        let pak = crate::util::root().join("game/ResetGeneration/menus.pak");
        let entries = read(&std::fs::read(pak).unwrap()).unwrap();
        let names = names(&entries.last().unwrap().raw).unwrap();
        let mut changed = Vec::new();
        for (e, n) in entries.iter().zip(&names) {
            let n = n.to_ascii_lowercase();
            if let Some(raw) = menus_entry(&n, &e.raw, 200) {
                let text: String = raw.iter().map(|&c| char::from(c)).collect();
                assert!(!text.contains("REPLAY_SAVED_TXT"), "{n}");
                if n == ERROR_SCENES[0] {
                    // the speech bubble ends up centred on 640 (it slides in from x 240 to about 120)
                    assert!(text.contains(r#"type="RESET" positionX="440""#), "{text}");
                }
                changed.push(n);
            }
        }
        changed.sort();
        assert_eq!(changed, [END_MENU, ERROR_SCENES[0], ERROR_SCENES[1]]);
    }
}

/// `pak-patch DIR [W H]` (a debugging aid): patches the paks in DIR in place.
pub fn cmd(args: &[String]) -> R<()> {
    let dir = args.first().ok_or("usage: pak-patch DIR [W H]")?;
    let w = args.get(1).map_or(Ok(640), |s| s.parse())?;
    let h = args.get(2).map_or(Ok(360), |s| s.parse())?;
    patch_dir(Path::new(dir), w, h)
}
