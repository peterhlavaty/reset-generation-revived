//! The recompiled (native) engine: `ghidra-export`, `recomp`, `build-native-test` (rgtest).
//!
//! Pipeline: the engine DLL -> headless Ghidra (analysis + tools/ghidra_scripts/ExportPcode.java)
//! -> decompiled/native/pcode.txt -> recomp (recomp.rs) -> C in build/native/gen -> compiled
//! together with native/rt and a platform host (native/android, native/host/desktop).
//! decompiled/ and build/ are git-ignored: the generated code is derived from the game binary.

use crate::recomp;
use crate::setup::need;
use crate::util::{root, run, R};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn dll() -> PathBuf {
    root().join("game/ResetGeneration/ResetGeneration-Win32-Web.dll")
}

pub fn pcode() -> PathBuf {
    root().join("decompiled/native/pcode.txt")
}

pub fn gen_dir() -> PathBuf {
    root().join("build/native/gen")
}

fn newer(a: &Path, b: &Path) -> bool {
    let t = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (t(a), t(b)) {
        (Some(x), Some(y)) => x > y,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Headless Ghidra: imports and analyses the DLL into build/ghidra (first run only, ~10 min),
/// then exports decompiled/native/pcode.txt (for recomp) and rg_win32.c (for reading).
pub fn ghidra_export(args: &[String]) -> R<()> {
    let ghidra = need("ghidra")?;
    make_executable(&ghidra);
    let jdk = need("jdk25")?;
    let r = root();
    let dll = args.first().map(PathBuf::from).unwrap_or_else(dll);
    let proj = r.join("build/ghidra");
    let out = r.join("decompiled/native");
    std::fs::create_dir_all(&proj)?;
    std::fs::create_dir_all(&out)?;
    let headless = ghidra.join("support").join(if cfg!(windows) { "analyzeHeadless.bat" } else { "analyzeHeadless" });
    let mut c = Command::new(headless);
    c.env("JAVA_HOME", &jdk).arg(&proj).arg("RG");
    if proj.join("RG.rep").is_dir() {
        c.arg("-process").arg(dll.file_name().ok_or("bad dll path")?).arg("-noanalysis");
    } else {
        c.arg("-import").arg(&dll);
    }
    c.arg("-scriptPath").arg(r.join("tools/ghidra_scripts"));
    c.arg("-postScript").arg("ExportPcode.java").arg(out.join("pcode.txt"));
    c.arg("-postScript").arg("ExportDecomp.java").arg(out.join("rg_win32.c"));
    let log = proj.join("export.log");
    eprintln!("Ghidra: analysing {} (log: {})", dll.display(), log.display());
    let f = std::fs::File::create(&log)?;
    let _ = c.stdout(f.try_clone()?).stderr(f).status()?;
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    if !text.contains("ExportPcode: wrote") {
        return Err(format!("the Ghidra export failed, see {}", log.display()).into());
    }
    println!("wrote {}", out.join("pcode.txt").display());
    Ok(())
}

/// Regenerates build/native/gen when the export or the recompiler is newer than it.
pub fn generate(extra: &[String]) -> R<PathBuf> {
    let gen = gen_dir();
    let table = gen.join("rg_gen_table.c");
    if !pcode().exists() {
        // one time: the engine's P-code comes from a Ghidra analysis of the original DLL
        eprintln!("no decompiled/native/pcode.txt yet: running the Ghidra export (one time, several minutes)");
        let missing: Vec<String> = ["jdk25", "ghidra"].iter().filter(|t| crate::setup::find(t).is_none()).map(|t| t.to_string()).collect();
        if !missing.is_empty() {
            crate::setup::setup(&missing)?;
        }
        ghidra_export(&[])?;
    }
    let me = std::env::current_exe()?;
    if !extra.is_empty() || !table.exists() || newer(&pcode(), &table) || newer(&me, &table) {
        let mut args = vec!["--dll".to_string(), dll().display().to_string()];
        args.extend(extra.iter().cloned());
        recomp::recompile(&pcode(), &gen, &recomp::Options::parse(&args)?)?;
    }
    Ok(gen)
}

/// Compiles C files to objects in parallel, skipping objects newer than their source (and than
/// `deps`, the headers they include).
pub fn compile_all(cc: &Path, flags: &[String], files: &[PathBuf], obj: &Path, deps: &[PathBuf]) -> R<Vec<PathBuf>> {
    std::fs::create_dir_all(obj)?;
    let newest_dep = deps.iter().filter_map(|d| std::fs::metadata(d).and_then(|m| m.modified()).ok()).max();
    let jobs: Vec<(PathBuf, PathBuf)> = files
        .iter()
        .map(|f| (f.clone(), obj.join(format!("{}.o", f.file_stem().unwrap().to_string_lossy()))))
        .collect();
    let todo: Vec<&(PathBuf, PathBuf)> = jobs
        .iter()
        .filter(|(src, o)| {
            let ot = std::fs::metadata(o).and_then(|m| m.modified()).ok();
            let st = std::fs::metadata(src).and_then(|m| m.modified()).ok();
            match (ot, st) {
                (Some(ot), Some(st)) => st > ot || newest_dep.is_some_and(|d| d > ot),
                _ => true,
            }
        })
        .collect();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let errors = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some((src, o)) = todo.get(i) else { break };
                let r = Command::new(cc).args(flags).arg("-c").arg(src).arg("-o").arg(o).output();
                match r {
                    Ok(out) if out.status.success() => {
                        if !out.stderr.is_empty() {
                            eprint!("{}", String::from_utf8_lossy(&out.stderr));
                        }
                    }
                    Ok(out) => errors.lock().unwrap().push(format!("{}:\n{}", src.display(), String::from_utf8_lossy(&out.stderr))),
                    Err(e) => errors.lock().unwrap().push(format!("{}: {e}", cc.display())),
                }
            });
        }
    });
    let errors = errors.into_inner().unwrap();
    if !errors.is_empty() {
        return Err(format!("compile errors:\n{}", errors.join("\n")).into());
    }
    Ok(jobs.into_iter().map(|(_, o)| o).collect())
}

pub fn c_files(dir: &Path) -> Vec<PathBuf> {
    crate::util::files_with_ext(dir, "c").into_iter().filter(|p| p.parent() == Some(dir)).collect()
}

/// `cargo x build-native-test [--trace a,b] [--calltrace]`: build/native/rgtest(.exe), the
/// headless test driver (recompiled engine + native/host/desktop/rgtest.c) for this PC.
pub fn build_test(args: &[String]) -> R<()> {
    let calltrace = args.iter().any(|a| a == "--calltrace");
    let recomp_args: Vec<String> = args.iter().filter(|a| *a != "--calltrace").cloned().collect();
    let gen = generate(&recomp_args)?;
    let r = root();
    let (cc, libs): (PathBuf, &[&str]) = if cfg!(windows) {
        (need("llvm-mingw")?.join("bin/x86_64-w64-mingw32-clang.exe"), &["-lws2_32", "-lpthread", "-lm", "-static"])
    } else {
        (PathBuf::from(std::env::var("CC").unwrap_or("cc".into())), &["-lpthread", "-lm"])
    };
    let rt = r.join("native/rt");
    let mut flags: Vec<String> = ["-O2", "-fno-strict-aliasing", "-fwrapv"].iter().map(|s| s.to_string()).collect();
    if calltrace {
        flags.push("-DRT_CALLTRACE".into());
    }
    flags.push(format!("-I{}", rt.display()));
    flags.push(format!("-I{}", gen.display()));
    let obj = r.join(if calltrace { "build/native/obj-test-trace" } else { "build/native/obj-test" });
    let headers = crate::util::files_with_ext(&rt, "h");
    let mut gen_flags = flags.clone();
    gen_flags.push("-w".into());
    let mut objs = compile_all(&cc, &gen_flags, &c_files(&gen), &obj, &headers)?;
    let mut own = c_files(&rt);
    own.push(r.join("native/host/desktop/rgtest.c"));
    let mut own_flags = flags.clone();
    own_flags.extend(["-g", "-Wall", "-Wno-unused-function"].iter().map(|s| s.to_string()));
    objs.extend(compile_all(&cc, &own_flags, &own, &obj, &headers)?);
    let exe = r.join("build/native").join(format!("rgtest{}", std::env::consts::EXE_SUFFIX));
    run(Command::new(&cc).arg("-o").arg(&exe).args(&objs).args(libs))?;
    println!("built {}", exe.display());
    Ok(())
}

/// A JDK for the build-time Java tools (javac, keytool, apksigner, PatchTool).
pub fn tool_jdk() -> R<PathBuf> {
    crate::setup::find("jdk17-x86")
        .or_else(|| crate::setup::find("jdk25"))
        .ok_or_else(|| "no JDK: run `cargo x setup` (or `cargo x setup jdk25`)".into())
}

/// The game data for the recompiled engine in `dest` (replaced): a copy of game/ResetGeneration
/// patched by the game's own PatchTool (src/java/rg) for the 640x360 engine size, with the
/// original DLL (the recompiled engine uses its data sections and imports at run time).
pub fn patched_data(dest: &Path) -> R<()> {
    let r = root();
    let jdk = tool_jdk()?;
    crate::util::remove_dir(dest)?;
    std::fs::create_dir_all(dest)?;
    for e in std::fs::read_dir(r.join("game/ResetGeneration"))?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if e.path().is_file() && !name.ends_with(".dylib") && name != "zlib1.dll" && name != "file.lock" {
            std::fs::copy(e.path(), dest.join(&name))?;
        }
    }
    let cls = r.join("build/native/patchtool");
    crate::util::remove_dir(&cls)?;
    let jar = r.join("game/ResetGeneration.jar");
    let src = r.join("src/java/rg");
    run(Command::new(jdk.join("bin/javac"))
        .args(["-nowarn", "--release", "17", "-cp"])
        .arg(&jar)
        .arg("-d")
        .arg(&cls)
        .args(["PatchTool.java", "PakPatcher.java", "DllPatcher.java"].map(|f| src.join(f))))?;
    let sep = if cfg!(windows) { ";" } else { ":" };
    run(Command::new(jdk.join("bin/java"))
        .arg("-cp")
        .arg(format!("{}{sep}{}", cls.display(), jar.display()))
        .arg("rg.PatchTool")
        .arg(dest)
        .args(["640", "360"]))?;
    let d = dest.join("ResetGeneration-Win32-Web.dll");
    std::fs::rename(d.with_extension("dll.orig"), &d)?;
    for e in std::fs::read_dir(dest)?.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if n.ends_with(".orig") || n.ends_with(".revived") {
            std::fs::remove_file(e.path())?;
        }
    }
    Ok(())
}

/// `cargo x native-test [FRAMES] [rgtest options…]`: runs build/native/rgtest headless on a
/// virtual clock against patched data in build/native/data (screenshots, frame hashes in
/// build/native/test).
pub fn run_test(args: &[String]) -> R<()> {
    let r = root();
    let exe = r.join("build/native").join(format!("rgtest{}", std::env::consts::EXE_SUFFIX));
    if !exe.exists() {
        return Err("build first: cargo x build-native-test".into());
    }
    let data = r.join("build/native/data");
    if !data.join("ResetGeneration-Win32-Web.dll").exists() {
        patched_data(&data)?;
    }
    let out = r.join("build/native/test");
    std::fs::create_dir_all(&out)?;
    let (frames, rest) = match args.first().and_then(|a| a.parse::<u32>().ok()) {
        Some(n) => (n.to_string(), &args[1..]),
        None => ("400".to_string(), args),
    };
    for p in crate::util::files_with_ext(&out, "ppm") {
        let _ = std::fs::remove_file(&p);
    }
    run(Command::new(&exe).arg(data.join("ResetGeneration-Win32-Web.dll")).arg(&data).arg(frames).arg(&out).args(rest))?;
    for p in crate::util::files_with_ext(&out, "ppm") {
        ppm_to_png(&p)?;
    }
    Ok(())
}

/// A binary PPM (P6, maxval 255) as PNG next to it.
fn ppm_to_png(p: &Path) -> R<()> {
    let d = std::fs::read(p)?;
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 && i < d.len() {
        while i < d.len() && d[i].is_ascii_whitespace() {
            i += 1;
        }
        let s = i;
        while i < d.len() && !d[i].is_ascii_whitespace() {
            i += 1;
        }
        fields.push(String::from_utf8_lossy(&d[s..i]).to_string());
    }
    let (w, h): (u32, u32) = (fields[1].parse()?, fields[2].parse()?);
    let px = d.get(i + 1..i + 1 + (w * h * 3) as usize).ok_or("short PPM")?;
    let mut enc = png::Encoder::new(std::fs::File::create(p.with_extension("png"))?, w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.write_header()?.write_image_data(px)?;
    std::fs::remove_file(p)?;
    Ok(())
}

/// The native desktop game for this OS: dist/ResetGeneration-Native-<os>/ (+ .zip), a window
/// program (native/desktop) linked with the recompiled engine, and its game data.
pub fn build_desktop(_args: &[String]) -> R<()> {
    let gen = generate(&[])?;
    let r = root();
    let rt = r.join("native/rt");
    let (cc, ar, target) = if cfg!(windows) {
        let bin = need("llvm-mingw")?.join("bin");
        (bin.join("x86_64-w64-mingw32-clang.exe"), bin.join("llvm-ar.exe"), Some("x86_64-pc-windows-gnullvm"))
    } else {
        let env = |k: &str, d: &str| PathBuf::from(std::env::var(k).unwrap_or(d.into()));
        (env("CC", "cc"), env("AR", "ar"), None)
    };
    let mut flags: Vec<String> = vec!["-O2".into(), "-fno-strict-aliasing".into(), "-fwrapv".into()];
    if !cfg!(windows) {
        flags.push("-fPIC".into()); // linked into a position-independent executable
    }
    flags.push(format!("-I{}", rt.display()));
    flags.push(format!("-I{}", gen.display()));
    let obj = r.join("build/native/obj-desktop");
    let headers = crate::util::files_with_ext(&rt, "h");
    let mut gen_flags = flags.clone();
    gen_flags.push("-w".into());
    let mut objs = compile_all(&cc, &gen_flags, &c_files(&gen), &obj, &headers)?;
    let mut own = c_files(&rt);
    own.push(r.join("native/host/desktop/host.c"));
    flags.extend(["-Wall", "-Wno-unused-function"].iter().map(|s| s.to_string()));
    objs.extend(compile_all(&cc, &flags, &own, &obj, &headers)?);
    let lib = r.join("build/native/desktop/librgengine.a");
    std::fs::create_dir_all(lib.parent().unwrap())?;
    let _ = std::fs::remove_file(&lib);
    run(Command::new(&ar).arg("rcs").arg(&lib).args(&objs))?;

    let mut cargo = Command::new("cargo");
    cargo.current_dir(&r).args(["build", "-q", "--release", "-p", "rg-native"]).env("RG_ENGINE_LIB", &lib);
    let mut out = r.join("target");
    if let Some(t) = target {
        run(Command::new("rustup").args(["target", "add", t]))?;
        let var = t.to_uppercase().replace('-', "_");
        cargo.args(["--target", t]).env(format!("CARGO_TARGET_{var}_RUSTFLAGS"), "-C target-feature=+crt-static -C link-arg=-Wno-unused-command-line-argument").env(format!("CARGO_TARGET_{var}_LINKER"), &cc).env(format!("CC_{}", t.replace('-', "_")), &cc).env(format!("AR_{}", t.replace('-', "_")), &ar);
        out = out.join(t);
    }
    run(&mut cargo)?;
    let exe_name = format!("ResetGeneration{}", std::env::consts::EXE_SUFFIX);
    let exe = out.join("release").join(&exe_name);

    let name = format!("ResetGeneration-Native-{}", std::env::consts::OS);
    let dist = r.join("dist").join(&name);
    crate::util::remove_dir(&dist)?;
    std::fs::create_dir_all(&dist)?;
    std::fs::copy(&exe, dist.join(&exe_name))?;
    patched_data(&dist.join("data"))?;
    let zip = r.join("dist").join(format!("{name}.zip"));
    crate::windows::zip_dir(&dist, &zip)?;
    println!("built {} (+ {})", dist.display(), zip.display());
    Ok(())
}

/// Ghidra's launch scripts and native helpers (support/, */os/*) must be executable on Unix; not
/// every unzip keeps the permission bits.
fn make_executable(ghidra: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut stack = vec![ghidra.to_path_buf()];
        while let Some(d) = stack.pop() {
            for p in std::fs::read_dir(&d).into_iter().flatten().flatten().map(|e| e.path()) {
                let s = p.to_string_lossy().to_string();
                if p.is_dir() {
                    stack.push(p);
                } else if s.contains("/support/") || s.contains("/os/") || s.ends_with(".sh") {
                    let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
                }
            }
        }
    }
    #[cfg(not(unix))]
    let _ = ghidra;
}
