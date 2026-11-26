//! `cargo x oracle [FRAMES] [rgtest options…]`: the recompiled engine checked against the
//! original. Builds two test drivers that differ only in how the engine runs:
//!
//! * build/oracle/rg-oracle.exe: the original DLL on the real CPU (32-bit, native/oracle);
//! * build/oracle/rgtest-pure.exe: the recompiled engine without our layout patches and hooks
//!   (recomp --pure, RT_PURE) in the same memory map (RT_COMPACT_MAP);
//!
//! runs both on the same input (virtual clock, `--keys`) and compares every frame's hash and
//! every Win32/JNI call's result (RT_API_TRACE). The first difference points at the bug: a
//! screenshot of the first different frame from each side lands in build/oracle/{orig,reco}/.
//! Windows only (the original is a Win32 DLL).

use crate::native::{generate_into, link_driver, ppm_to_png, test_data, WIN_LIBS};
use crate::util::{root, R};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The two runs' directories: the original, the recompiled engine (the same length: the engine sees
/// its path, e.g. GetModuleFileNameA's result).
const SIDES: [&str; 2] = ["orig", "reco"];

pub fn oracle(args: &[String]) -> R<()> {
    if !cfg!(windows) {
        return Err("the oracle runs the original 32-bit Windows DLL: Windows only".into());
    }
    let r = root();
    let dir = r.join("build/oracle");
    let bin = crate::setup::ensure("llvm-mingw")?.join("bin");
    let rgtest = r.join("native/host/desktop/rgtest.c");
    let exes = [dir.join("rg-oracle.exe"), dir.join("rgtest-pure.exe")];
    let mut libs32 = WIN_LIBS.to_vec();
    libs32.push("-Wl,--large-address-aware"); // the stubs live at 0xF0000000
    let sources = [r.join("native/oracle/oracle.c"), rgtest.clone()];
    link_driver(&bin.join("i686-w64-mingw32-clang.exe"), &["RT_ORACLE", "RT_PURE"], None, &sources, &dir.join("obj-oracle"), &exes[0], &libs32)?;
    let gen = generate_into(&dir.join("gen"), &["--pure".to_string()])?;
    link_driver(&bin.join("x86_64-w64-mingw32-clang.exe"), &["RT_PURE", "RT_COMPACT_MAP"], Some(&gen), &[rgtest], &dir.join("obj-pure"), &exes[1], WIN_LIBS)?;

    let data = dir.join("data");
    test_data(&data)?;
    let (frames, rest) = match args.first().and_then(|a| a.parse::<u32>().ok()) {
        Some(n) => (n, &args[1..]),
        None => (400, args),
    };
    run_both(&dir, &exes, &data, frames, rest)?;

    let mut same = true;
    let hashes: Vec<Vec<String>> = SIDES.iter().map(|s| lines(&dir.join(s).join("hashes.txt"))).collect::<R<_>>()?;
    let n = hashes[0].len().min(hashes[1].len());
    match (0..n).find(|&i| hashes[0][i] != hashes[1][i]) {
        None if hashes[0].len() == hashes[1].len() => println!("frames: all {n} identical"),
        None => {
            same = false;
            println!("frames: the first {n} identical, then one side stopped ({} vs {} frames)", hashes[0].len(), hashes[1].len());
        }
        Some(i) => {
            same = false;
            let f = hashes[0][i].split(' ').next().unwrap_or("0").to_string();
            println!("frames: {i} identical, the first different is frame {f}");
            // run again up to that frame with a screenshot of it (the runs are deterministic)
            let mut again: Vec<String> = rest.to_vec();
            again.extend(["--shots".to_string(), f.clone()]);
            run_both(&dir, &exes, &data, f.parse::<u32>()? + 1, &again)?;
            for s in SIDES {
                let png = dir.join(s).join(format!("frame_{:05}.png", f.parse::<u32>()?));
                println!("  {s}: {}", png.display());
            }
        }
    }
    let api: Vec<Vec<String>> = SIDES.iter().map(|s| lines(&dir.join(s).join("api.txt"))).collect::<R<_>>()?;
    let n = api[0].len().min(api[1].len());
    match (0..n).find(|&i| api[0][i] != api[1][i]) {
        None if api[0].len() == api[1].len() => println!("Win32/JNI calls: all {n} identical (name and result)"),
        None => {
            same = false;
            println!("Win32/JNI calls: the first {n} identical, then {} vs {} calls", api[0].len(), api[1].len());
        }
        Some(i) => {
            same = false;
            println!("Win32/JNI calls: {i} identical, then (call #{}):", i + 1);
            for (s, a) in SIDES.iter().zip(&api) {
                let ctx: Vec<&str> = a[i.saturating_sub(3)..=i].iter().map(|l| l.as_str()).collect();
                println!("  {s}: {}", ctx.join(" | "));
            }
        }
    }
    if !same {
        return Err("the recompiled engine differs from the original (logs: build/oracle/{orig,reco}/log.txt)".into());
    }
    Ok(())
}

/// Runs both drivers at once, each on its own copy of the data (the engine writes settings),
/// with the API trace on.
fn run_both(dir: &Path, exes: &[PathBuf; 2], data: &Path, frames: u32, args: &[String]) -> R<()> {
    let mut children = Vec::new();
    for (side, exe) in SIDES.iter().zip(exes) {
        let out = dir.join(side);
        crate::util::remove_dir(&out)?;
        let side_data = out.join("data");
        std::fs::create_dir_all(&side_data)?;
        for e in std::fs::read_dir(data)?.flatten() {
            std::fs::copy(e.path(), side_data.join(e.file_name()))?;
        }
        let log = std::fs::File::create(out.join("log.txt"))?;
        let mut c = Command::new(exe);
        c.arg(side_data.join("ResetGeneration-Win32-Web.dll")).arg(&side_data).arg(frames.to_string()).arg(&out).args(args);
        c.env("RT_API_TRACE", out.join("api.txt")).stdout(log.try_clone()?).stderr(log);
        crate::util::no_inherit();
        children.push((side, out, c.spawn()?));
    }
    let mut failed = Vec::new();
    for (side, out, mut c) in children {
        if !c.wait()?.success() {
            failed.push(format!("{side} failed: {}", tail(&out.join("log.txt"))));
        }
        for p in crate::util::files_with_ext(&out, "ppm") {
            ppm_to_png(&p)?;
        }
    }
    if !failed.is_empty() {
        return Err(failed.join("\n").into());
    }
    Ok(())
}

fn lines(p: &Path) -> R<Vec<String>> {
    Ok(std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?.lines().map(String::from).collect())
}

fn tail(p: &Path) -> String {
    let s = std::fs::read_to_string(p).unwrap_or_default();
    let l: Vec<&str> = s.lines().collect();
    l[l.len().saturating_sub(8)..].join("\n    ")
}
