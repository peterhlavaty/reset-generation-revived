//! `cargo x build-windows`: dist/ResetGeneration-Revived, a self-contained Windows game folder
//! (32-bit Java runtime + launcher + game data + rghost.dll + rg-relay.exe + LAN-Setup.exe) and the
//! same folder as a zip. Needs `cargo x setup` (jdk17-x86, llvm-mingw).

use crate::setup::need;
use crate::util::{copy_dir, files_with_ext, remove_dir, root, run, R};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const APP: &str = "ResetGeneration-Revived";

pub fn dist() -> PathBuf {
    root().join("dist").join(APP)
}

pub fn exe() -> PathBuf {
    dist().join(format!("{APP}.exe"))
}

/// Compiles the launcher and the patched classes (src/java) against the original jars.
pub fn javac(jdk: &Path, out: &Path) -> R<()> {
    let r = root();
    let g = r.join("game");
    remove_dir(out)?;
    std::fs::create_dir_all(out)?;
    let sources = files_with_ext(&r.join("src/java"), "java");
    let list = out.with_extension("sources.txt");
    let text: String = sources.iter().map(|p| format!("\"{}\"\n", p.display().to_string().replace('\\', "/"))).collect();
    std::fs::write(&list, text)?;
    let sep = if cfg!(windows) { ";" } else { ":" };
    let cp = format!("{}{sep}{}", g.join("ResetGeneration.jar").display(), g.join("lib/rg-signed.jar").display());
    run(Command::new(jdk.join("bin/javac"))
        .args(["-nowarn", "--release", "17", "-encoding", "UTF-8", "-cp", &cp, "-d"])
        .arg(out)
        .arg(format!("@{}", list.display())))
}

/// The game jar: the original jar's contents (without its signature), overlaid with our classes
/// and the icon, with our manifest.
fn make_jar(classes: &Path, dest: &Path) -> R<()> {
    let r = root();
    let mut out = zip::ZipWriter::new(std::fs::File::create(dest)?);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    out.start_file("META-INF/MANIFEST.MF", opts)?;
    out.write_all(b"Manifest-Version: 1.0\r\nMain-Class: rg.Revived\r\nClass-Path: rg-signed.jar\r\n\r\n")?;
    let ours: Vec<(String, PathBuf)> = files_with_ext(classes, "class")
        .into_iter()
        .map(|p| (p.strip_prefix(classes).unwrap().to_string_lossy().replace('\\', "/"), p))
        .collect();
    let mut orig = zip::ZipArchive::new(std::fs::File::open(r.join("game/ResetGeneration.jar"))?)?;
    for i in 0..orig.len() {
        let mut f = orig.by_index(i)?;
        let name = f.name().to_string();
        if name.starts_with("META-INF/") || f.is_dir() || ours.iter().any(|(n, _)| *n == name) {
            continue;
        }
        let mut data = Vec::new();
        f.read_to_end(&mut data)?;
        out.start_file(name, opts)?;
        out.write_all(&data)?;
    }
    for (name, p) in &ours {
        out.start_file(name.as_str(), opts)?;
        out.write_all(&std::fs::read(p)?)?;
    }
    out.start_file("revived/icon.png", opts)?;
    out.write_all(&std::fs::read(r.join("src/assets/icon.png"))?)?;
    out.finish()?;
    Ok(())
}

/// rghost.dll: "host a game" (F5), 32-bit like the engine.
pub fn rghost(jdk: &Path, dest: &Path) -> R<()> {
    let cc = need("llvm-mingw")?.join("bin/i686-w64-mingw32-clang.exe");
    run(Command::new(cc)
        .args(["-O2", "-Wall", "-Wno-ignored-attributes", "-shared", "-static"])
        .arg(format!("-I{}", jdk.join("include").display()))
        .arg(format!("-I{}", jdk.join("include/win32").display()))
        .arg(root().join("native/win32/rghost.c"))
        .arg("-o")
        .arg(dest)
        .args(["-lws2_32", "-Wl,--kill-at"]))
}

/// A release binary of one workspace package (for this PC).
pub fn cargo_bin(package: &str, bin: &str) -> R<PathBuf> {
    run(Command::new("cargo").current_dir(root()).args(["build", "-q", "--release", "-p", package, "--bin", bin]))?;
    Ok(root().join("target/release").join(format!("{bin}{}", std::env::consts::EXE_SUFFIX)))
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

pub fn build() -> R<()> {
    if !cfg!(windows) {
        return Err("the Windows game build runs on Windows (jpackage makes a launcher for the OS it runs on)".into());
    }
    let r = root();
    let jdk = need("jdk17-x86")?;
    let b = r.join("build/win");
    let out = r.join("dist");
    let app = dist();
    remove_dir(&b)?;
    remove_dir(&app)?;
    std::fs::create_dir_all(b.join("input"))?;
    std::fs::create_dir_all(&out)?;
    // 1. classes + jar
    javac(&jdk, &b.join("classes"))?;
    make_jar(&b.join("classes"), &b.join("input/ResetGenerationRevived.jar"))?;
    std::fs::copy(r.join("game/lib/rg-signed.jar"), b.join("input/rg-signed.jar"))?;
    // 2. the app image: launcher exe + trimmed 32-bit runtime
    run(Command::new(jdk.join("bin/jpackage"))
        .args(["--type", "app-image", "--name", APP, "--dest"])
        .arg(&out)
        .arg("--input")
        .arg(b.join("input"))
        .args(["--main-jar", "ResetGenerationRevived.jar", "--main-class", "rg.Revived", "--icon"])
        .arg(r.join("src/assets/icon.ico"))
        .args(["--app-version", env!("CARGO_PKG_VERSION"), "--vendor", "Reset Generation Revived"])
        .args(["--description", "Reset Generation (RedLynx/Nokia, 2008) for modern Windows"])
        .args(["--add-modules", "java.base,java.desktop,java.net.http,jdk.jsobject,java.logging,jdk.unsupported"])
        .args(["--jlink-options", "--strip-debug --no-header-files --no-man-pages --compress=2"])
        .args(["--java-options", "-Dsun.java2d.uiScale=1", "--java-options", "-Xss1m"]))?;
    // 3. game data next to the jar (the engine looks in <jar dir>/ResetGeneration)
    let a = app.join("app");
    let data = a.join("ResetGeneration");
    std::fs::create_dir_all(&data)?;
    for e in std::fs::read_dir(r.join("game/ResetGeneration"))?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.ends_with(".dylib") || name == "file.lock" {
            continue; // the macOS build / a stale lock file
        }
        if e.path().is_dir() {
            copy_dir(&e.path(), &data.join(&name))?;
        } else {
            std::fs::copy(e.path(), data.join(&name))?;
        }
    }
    std::fs::copy(r.join("src/dist/revived.ini"), a.join("revived.ini"))?;
    // 4. native parts: hosting (rghost.dll), the LAN relay and the firewall helper
    rghost(&jdk, &a.join("rghost.dll"))?;
    std::fs::copy(cargo_bin("rg-server", "rg-relay")?, a.join("rg-relay.exe"))?;
    std::fs::copy(cargo_bin("lan-setup", "lan-setup")?, app.join("LAN-Setup.exe"))?;
    std::fs::copy(r.join("src/dist/README.txt"), app.join("README.txt"))?;
    println!("built {}", app.display());
    // 5. the same folder as one zip to copy to other PCs
    let z = out.join(format!("{APP}.zip"));
    zip_dir(&app, &z)?;
    println!("packed {} ({} MB)", z.display(), std::fs::metadata(&z)?.len() >> 20);
    Ok(())
}
