//! Shared helpers: paths, running programs, small file utilities.

use std::path::{Path, PathBuf};
use std::process::Command;

pub type R<T> = Result<T, Box<dyn std::error::Error>>;

/// The repository root (rgtool is always run from its workspace through `cargo x`).
pub fn root() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    p.canonicalize().map(strip_unc).unwrap_or(p)
}

/// `\\?\C:\x` -> `C:\x`: canonical Windows paths confuse javac, jpackage and batch files.
pub fn strip_unc(p: PathBuf) -> PathBuf {
    match p.to_str() {
        Some(s) if s.starts_with(r"\\?\") && !s.starts_with(r"\\?\UNC\") => PathBuf::from(&s[4..]),
        _ => p,
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `~/x` -> `<home>/x`.
pub fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        Some(rest) => home().join(rest),
        None => PathBuf::from(p),
    }
}

/// Runs a program, echoing it; fails on a non-zero exit.
pub fn run(cmd: &mut Command) -> R<()> {
    let shown = format!("{cmd:?}").replace('"', "");
    let shown = if shown.len() > 300 { format!("{}…", &shown[..300]) } else { shown };
    eprintln!("> {shown}");
    let st = cmd.status().map_err(|e| format!("cannot run {:?}: {e}", cmd.get_program()))?;
    if !st.success() {
        return Err(format!("{:?} failed ({st})", cmd.get_program()).into());
    }
    Ok(())
}

/// All files under `dir` (recursively) with this extension.
pub fn files_with_ext(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case(ext)) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Removes a directory tree if it exists.
pub fn remove_dir(p: &Path) -> R<()> {
    if p.exists() {
        std::fs::remove_dir_all(p).map_err(|e| format!("cannot remove {}: {e}", p.display()))?;
    }
    Ok(())
}

/// `key = value` lines (# comments), as used by deploy.local.ini.
pub fn read_ini(p: &Path) -> R<Vec<(String, String)>> {
    let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect())
}

/// Long-running programs we start (the game, the emulator) must not inherit our stdout / stderr:
/// a caller reading them through a pipe would wait until those programs end. Windows passes every
/// inheritable handle to a child, and under `cargo x` we hold more copies of the caller's pipe than
/// our std handles (cargo's own, inherited from its parent), so all handles lose the flag.
pub fn no_inherit() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT};
        // handle values are multiples of 4; a value that is no handle just fails
        for h in (4..0x10000usize).step_by(4) {
            unsafe {
                SetHandleInformation(h as HANDLE, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Packs a folder into a zip (the folder itself at the top).
pub fn zip_dir(dir: &Path, dest: &Path) -> R<()> {
    let base = dir.parent().unwrap();
    let mut out = zip::ZipWriter::new(std::fs::File::create(dest)?);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(false);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&d)?.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            let name = p.strip_prefix(base)?.to_string_lossy().replace('\\', "/");
            if p.is_dir() {
                out.add_directory(format!("{name}/"), opts)?;
                stack.push(p);
            } else {
                out.start_file(name, opts)?;
                std::io::copy(&mut std::fs::File::open(&p)?, &mut out)?;
            }
        }
    }
    out.finish()?;
    Ok(())
}
