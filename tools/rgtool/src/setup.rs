//! `cargo x setup [what…]`: downloads the toolchains the builds need into `.toolchains/` in the
//! repository (git-ignored; RG_TOOLCHAINS overrides the place). Each download is checked against
//! the SHA-256 its publisher lists. Already installed ones are kept.
//!
//! * jdk17-x86: Temurin 17, 32-bit (the engine DLL is 32-bit, so is the JVM that loads it)
//! * llvm-mingw: clang for Windows targets (rghost.dll is i686, needs thiscall + GNU asm)
//! * jdk25, ghidra: the headless Ghidra export the native builds start from (Ghidra 12.2+ needs
//!   JDK 25); jdk25 also runs the build-time Java tools on other OSes
//!
//! Android SDK / NDK: an existing installation (ANDROID_HOME, ANDROID_SDK_ROOT or the Android
//! Studio default) is used; `cargo x doctor` shows what was found.

use crate::util::{root, R};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

pub fn dir() -> PathBuf {
    std::env::var_os("RG_TOOLCHAINS").map(PathBuf::from).unwrap_or_else(|| root().join(".toolchains"))
}

pub const ALL: &[&str] = &["jdk17-x86", "llvm-mingw", "jdk25", "ghidra"];

fn os() -> &'static str {
    match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "mac",
        _ => "linux",
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .user_agent("rgtool")
        .build()
        .into()
}

fn get_json(url: &str) -> R<serde_json::Value> {
    let mut resp = agent().get(url).call().map_err(|e| format!("{url}: {e}"))?;
    Ok(resp.body_mut().read_json()?)
}

/// Downloads to `dest`, checking the SHA-256 (hex).
fn download(url: &str, sha256: &str, dest: &Path) -> R<()> {
    eprintln!("downloading {url}");
    let resp = agent().get(url).call().map_err(|e| format!("{url}: {e}"))?;
    let total: u64 = resp.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse().ok()).unwrap_or(0);
    let mut body = resp.into_body().into_reader();
    let part = dest.with_extension("part");
    let mut out = std::fs::File::create(&part)?;
    let mut hash = Sha256::new();
    let (mut buf, mut got, mut shown) = (vec![0u8; 1 << 16], 0u64, 0u64);
    loop {
        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        std::io::Write::write_all(&mut out, &buf[..n])?;
        got += n as u64;
        if got - shown > 20 << 20 {
            shown = got;
            if total > 0 {
                eprintln!("  {} / {} MB", got >> 20, total >> 20);
            }
        }
    }
    drop(out);
    let have: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if !have.eq_ignore_ascii_case(sha256) {
        let _ = std::fs::remove_file(&part);
        return Err(format!("{url}: SHA-256 mismatch (got {have}, expected {sha256})").into());
    }
    std::fs::rename(&part, dest)?;
    Ok(())
}

/// Unpacks a .zip or .tar.gz into `dest` (which must not exist); returns the single top folder
/// inside it when there is one.
fn unpack(archive: &Path, dest: &Path) -> R<PathBuf> {
    eprintln!("unpacking {}", archive.display());
    let tmp = dest.with_extension("unpack");
    crate::util::remove_dir(&tmp)?;
    std::fs::create_dir_all(&tmp)?;
    let name = archive.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
    if name.ends_with(".zip") {
        zip::ZipArchive::new(std::fs::File::open(archive)?)?.extract(&tmp)?;
    } else {
        let gz = flate2::read::GzDecoder::new(std::fs::File::open(archive)?);
        tar::Archive::new(gz).unpack(&tmp)?;
    }
    std::fs::rename(&tmp, dest)?;
    let entries: Vec<_> = std::fs::read_dir(dest)?.flatten().collect();
    Ok(if entries.len() == 1 && entries[0].path().is_dir() { entries[0].path() } else { dest.to_path_buf() })
}

/// The installed folder of a toolchain (the archive's top folder), if present.
pub fn find(name: &str) -> Option<PathBuf> {
    let d = dir().join(name);
    let marker = std::fs::read_to_string(d.join(".home")).ok()?;
    let p = d.join(marker.trim());
    // a macOS JDK is a bundle: its home (bin/java, bin/javac) is Contents/Home
    let bundle = p.join("Contents/Home");
    let p = if bundle.join("bin").is_dir() { bundle } else { p };
    p.is_dir().then_some(p)
}

/// Like [`find`], or an error that says how to get it.
pub fn need(name: &str) -> R<PathBuf> {
    find(name).ok_or_else(|| format!("{name} is not installed: run `cargo x setup {name}`").into())
}

fn install(name: &str, url: &str, sha: &str, file: &str) -> R<()> {
    let d = dir().join(name);
    crate::util::remove_dir(&d)?;
    std::fs::create_dir_all(dir())?;
    let archive = dir().join(file);
    if !archive.exists() {
        download(url, sha, &archive)?;
    }
    let home = unpack(&archive, &d.join("x"))?;
    let rel = home.strip_prefix(&d)?.to_string_lossy().replace('\\', "/");
    std::fs::write(d.join(".home"), rel)?;
    std::fs::remove_file(&archive)?;
    println!("{name}: {}", home.display());
    Ok(())
}

fn temurin(name: &str, version: u32, arch: &str) -> R<()> {
    let url = format!(
        "https://api.adoptium.net/v3/assets/latest/{version}/hotspot?architecture={arch}&image_type=jdk&os={}&vendor=eclipse",
        os()
    );
    let v = get_json(&url)?;
    let pkg = &v[0]["binary"]["package"];
    let (link, sha, file) = (pkg["link"].as_str(), pkg["checksum"].as_str(), pkg["name"].as_str());
    match (link, sha, file) {
        (Some(l), Some(s), Some(f)) => install(name, l, s, f),
        _ => Err(format!("no Temurin {version} {arch} build for {}", os()).into()),
    }
}

fn github(name: &str, repo: &str, pick: impl Fn(&str) -> bool) -> R<()> {
    let v = get_json(&format!("https://api.github.com/repos/{repo}/releases/latest"))?;
    let assets = v["assets"].as_array().ok_or("no assets")?;
    let a = assets
        .iter()
        .find(|a| a["name"].as_str().is_some_and(&pick))
        .ok_or_else(|| format!("{repo}: no fitting download in the latest release"))?;
    let file = a["name"].as_str().unwrap();
    let url = a["browser_download_url"].as_str().ok_or("no url")?;
    let sha = a["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).ok_or("no sha256 digest published")?;
    install(name, url, sha, file)
}

pub fn setup(what: &[String]) -> R<()> {
    let list: Vec<&str> = if what.is_empty() {
        // what the Windows game build needs; the rest on request
        if cfg!(windows) { vec!["jdk17-x86", "llvm-mingw"] } else { vec![] }
    } else if what.iter().any(|w| w == "all") {
        ALL.to_vec()
    } else {
        what.iter().map(String::as_str).collect()
    };
    for name in list {
        if find(name).is_some() {
            println!("{name}: already installed ({})", find(name).unwrap().display());
            continue;
        }
        match name {
            "jdk17-x86" => temurin(name, 17, "x32")?,
            "jdk25" => temurin(name, 25, if cfg!(target_arch = "aarch64") { "aarch64" } else { "x64" })?,
            "llvm-mingw" => {
                let host = if cfg!(windows) { "ucrt-x86_64.zip" } else if cfg!(target_os = "macos") { "macos-universal.tar.xz" } else { "ucrt-ubuntu-22.04-x86_64.tar.xz" };
                if !cfg!(windows) {
                    return Err("llvm-mingw: install it yourself on this OS (only needed for Windows targets)".into());
                }
                github(name, "mstorsjo/llvm-mingw", |n| n.starts_with("llvm-mingw-") && n.ends_with(host))?
            }
            "ghidra" => github(name, "NationalSecurityAgency/ghidra", |n| n.starts_with("ghidra_") && n.ends_with(".zip"))?,
            _ => return Err(format!("unknown toolchain {name} (known: {})", ALL.join(", ")).into()),
        }
    }
    Ok(())
}

/// The Android SDK, if one is installed.
pub fn android_sdk() -> Option<PathBuf> {
    let mut tries: Vec<PathBuf> = ["ANDROID_HOME", "ANDROID_SDK_ROOT"].iter().filter_map(std::env::var_os).map(PathBuf::from).collect();
    if let Some(l) = std::env::var_os("LOCALAPPDATA") {
        tries.push(PathBuf::from(l).join("Android").join("Sdk"));
    }
    tries.push(crate::util::home().join("Library/Android/sdk"));
    tries.push(crate::util::home().join("Android/Sdk"));
    tries.into_iter().find(|p| p.join("platforms").is_dir())
}

/// The newest subfolder of `dir` by version-like name.
pub fn newest(dir: &Path) -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    v.sort_by_key(|p| {
        p.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|s| s.parse::<u64>().ok())
            .collect::<Vec<_>>()
    });
    v.pop()
}

pub fn doctor() -> R<()> {
    println!("toolchains in {}", dir().display());
    for name in ALL {
        match find(name) {
            Some(p) => println!("  {name:11} {}", p.display()),
            None => println!("  {name:11} -  (cargo x setup {name})"),
        }
    }
    match android_sdk() {
        Some(sdk) => {
            println!("  android-sdk {}", sdk.display());
            let ndk = newest(&sdk.join("ndk")).map(|p| p.display().to_string()).unwrap_or("- (install an NDK in the SDK manager)".into());
            println!("  android-ndk {ndk}");
        }
        None => println!("  android-sdk -  (install Android Studio or the command-line tools; set ANDROID_HOME)"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Regression: on macOS the JDK's bin/ is under Contents/Home, so `javac` was not found.
    #[test]
    fn macos_jdk_bundle_home() {
        let root = std::env::temp_dir().join(format!("rgtool-setup-test-{}", std::process::id()));
        let tc = root.join("jdk25");
        std::fs::create_dir_all(tc.join("x/jdk-25.jdk/Contents/Home/bin")).unwrap();
        std::fs::write(tc.join(".home"), "x/jdk-25.jdk").unwrap();
        std::env::set_var("RG_TOOLCHAINS", &root);
        let home = super::find("jdk25").unwrap();
        std::env::remove_var("RG_TOOLCHAINS");
        assert!(home.ends_with("Contents/Home"), "{}", home.display());
        let _ = std::fs::remove_dir_all(&root);
    }
}
