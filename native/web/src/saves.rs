//! Files the engine writes (settings, saved games, achievements) kept in the browser's
//! localStorage, base64 encoded, under "rg:" + their path.

const PREFIX: &str = "rg:";
const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

pub fn encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            s.push(if i <= c.len() { B64[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    s
}

pub fn decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for b in s.bytes() {
        if b == b'=' {
            break;
        }
        acc = acc << 6 | B64.iter().position(|&c| c == b)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// A file the engine wrote (None: removed).
pub fn store(path: &str, data: Option<&[u8]>) {
    let Some(st) = storage() else { return };
    let key = format!("{PREFIX}{path}");
    let r = match data {
        Some(d) => st.set_item(&key, &encode(d)),
        None => st.remove_item(&key),
    };
    if r.is_err() {
        web_sys::console::warn_1(&format!("cannot keep {path} (browser storage full or blocked)").into());
    }
}

/// Every kept file: (path, contents).
pub fn all() -> Vec<(String, Vec<u8>)> {
    let Some(st) = storage() else { return Vec::new() };
    let n = st.length().unwrap_or(0);
    (0..n)
        .filter_map(|i| st.key(i).ok().flatten())
        .filter_map(|k| {
            let path = k.strip_prefix(PREFIX)?.to_string();
            let data = decode(&st.get_item(&k).ok()??)?;
            Some((path, data))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_round_trip() {
        for n in 0..10 {
            let d: Vec<u8> = (0..n as u8).map(|i| i.wrapping_mul(37)).collect();
            assert_eq!(super::decode(&super::encode(&d)).unwrap(), d);
        }
        assert_eq!(super::encode(b"Man"), "TWFu");
        assert_eq!(super::encode(b"Ma"), "TWE=");
    }
}
