//! The browser build: `build-web` (dist/web: the recompiled engine as WebAssembly with the Rust
//! host native/web, a generated page and the game data) and `serve-web` (a local web server to
//! try it: pages that fetch files do not work from file://).
//!
//! The C (generated engine, native/rt without the socket code, native/web/c and the small C
//! library native/web/libc) is built with any clang that has the wasm32 target: RG_WASM_CC, the
//! Android NDK's, or `clang` on PATH (llvm-mingw has no wasm32 target).

use crate::native::{c_files, compile_all, generate, patched_data};
use crate::util::{root, run, R};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const TARGET: &str = "wasm32-unknown-unknown";

fn exe(p: PathBuf) -> PathBuf {
    if cfg!(windows) { p.with_extension("exe") } else { p }
}

/// clang and llvm-ar for wasm32.
fn wasm_cc() -> R<(PathBuf, PathBuf)> {
    if let Some(cc) = std::env::var_os("RG_WASM_CC").map(PathBuf::from) {
        let ar = std::env::var_os("RG_WASM_AR").map(PathBuf::from).unwrap_or_else(|| cc.with_file_name(exe(PathBuf::from("llvm-ar")).file_name().unwrap()));
        return Ok((cc, ar));
    }
    if let Some(ndk) = crate::setup::android_sdk().and_then(|s| crate::setup::newest(&s.join("ndk"))) {
        let host = if cfg!(windows) { "windows-x86_64" } else if cfg!(target_os = "macos") { "darwin-x86_64" } else { "linux-x86_64" };
        let bin = ndk.join("toolchains/llvm/prebuilt").join(host).join("bin");
        if exe(bin.join("clang")).exists() {
            return Ok((exe(bin.join("clang")), exe(bin.join("llvm-ar"))));
        }
    }
    Ok((PathBuf::from("clang"), PathBuf::from("llvm-ar")))
}

/// The wasm-bindgen CLI matching the crate version in Cargo.lock: on PATH, or installed into
/// .toolchains/wasm-bindgen-<version> on first use.
fn wasm_bindgen() -> R<PathBuf> {
    let lock = std::fs::read_to_string(root().join("Cargo.lock"))?;
    let version = lock
        .split("[[package]]")
        .find(|p| p.contains("name = \"wasm-bindgen\"\n"))
        .and_then(|p| p.lines().find_map(|l| l.strip_prefix("version = \"")?.strip_suffix('"').map(String::from)))
        .ok_or("wasm-bindgen is not in Cargo.lock")?;
    let on_path = Command::new("wasm-bindgen").arg("--version").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if on_path.as_deref() == Some(&format!("wasm-bindgen {version}")) {
        return Ok(PathBuf::from("wasm-bindgen"));
    }
    let dir = crate::setup::dir().join(format!("wasm-bindgen-{version}"));
    let bin = exe(dir.join("bin/wasm-bindgen"));
    if !bin.exists() {
        eprintln!("installing wasm-bindgen-cli {version} into {}", dir.display());
        run(Command::new("cargo").args(["install", "-q", "--locked", "wasm-bindgen-cli", "--version", &version, "--root"]).arg(&dir))?;
    }
    Ok(bin)
}

/// `build-web`: dist/web, ready to be served as static files.
pub fn build(_args: &[String]) -> R<()> {
    let gen = generate(&[])?;
    let r = root();
    let rt = r.join("native/rt");
    let web = r.join("native/web");
    let (cc, ar) = wasm_cc()?;
    let mut flags: Vec<String> = ["--target=wasm32-unknown-unknown", "-O2", "-fno-strict-aliasing", "-fwrapv", "-mbulk-memory", "-mnontrapping-fptoint", "-nostdlibinc"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    flags.push(format!("-isystem{}", web.join("libc/include").display()));
    flags.push(format!("-I{}", rt.display()));
    flags.push(format!("-I{}", gen.display()));
    let obj = r.join("build/web/obj");
    let mut headers = crate::util::files_with_ext(&rt, "h");
    headers.extend(crate::util::files_with_ext(&web.join("libc"), "h"));
    let mut gen_flags = flags.clone();
    gen_flags.push("-w".into());
    let mut objs = compile_all(&cc, &gen_flags, &c_files(&gen), &obj.join("gen"), &headers)?;
    flags.extend(["-Wall", "-Wno-unused-function"].iter().map(|s| s.to_string()));
    // no sockets in a page: link.c is replaced by native/web/c/web.c
    let mut own: Vec<PathBuf> = c_files(&rt).into_iter().filter(|p| !p.ends_with("link.c")).collect();
    own.extend(c_files(&web.join("c")));
    objs.extend(compile_all(&cc, &flags, &own, &obj.join("rt"), &headers)?);
    // the C library itself: no builtins, or the compiler turns its loops into calls to itself
    let mut libc_flags = flags.clone();
    libc_flags.push("-fno-builtin".into());
    objs.extend(compile_all(&cc, &libc_flags, &c_files(&web.join("libc/src")), &obj.join("libc"), &headers)?);
    let lib = r.join("build/web/librgengine.a");
    let _ = std::fs::remove_file(&lib);
    run(Command::new(&ar).arg("rcs").arg(&lib).args(&objs))?;

    run(Command::new("rustup").args(["target", "add", TARGET]))?;
    // not stripped: wasm-bindgen reads the target_features section (it strips the rest itself)
    run(Command::new("cargo").current_dir(&r).args(["build", "-q", "--release", "-p", "rg-web", "--target", TARGET]).env("RG_ENGINE_LIB", &lib).env("CARGO_PROFILE_RELEASE_STRIP", "false"))?;
    let wasm = r.join("target").join(TARGET).join("release/rg_web.wasm");

    let dist = r.join("dist/web");
    crate::util::remove_dir(&dist)?;
    std::fs::create_dir_all(&dist)?;
    run(Command::new(wasm_bindgen()?).arg("--target").arg("web").arg("--no-typescript").arg("--out-dir").arg(&dist).arg(&wasm))?;
    let data = dist.join("data");
    patched_data(&data)?;
    let mut list = String::new();
    let mut names: Vec<_> = std::fs::read_dir(&data)?.flatten().filter(|e| e.path().is_file()).collect();
    names.sort_by_key(|e| e.file_name());
    for e in names {
        list.push_str(&format!("{} {}\n", e.file_name().to_string_lossy(), e.metadata()?.len()));
    }
    std::fs::write(data.join("files.txt"), list)?;
    std::fs::write(dist.join("index.html"), PAGE)?;
    let size = std::fs::metadata(dist.join("rg_web_bg.wasm"))?.len();
    println!("built {} (engine {} MB); try it with `cargo x serve-web`", dist.display(), size >> 20);
    Ok(())
}

/// The page. The script is the one line wasm-bindgen's generated module needs to start.
const PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, user-scalable=no">
<title>Reset Generation</title>
<style>
  html, body { margin: 0; height: 100%; background: #000; color: #ccc; font: 16px sans-serif; overflow: hidden; }
  body { display: flex; align-items: center; justify-content: center; }
  canvas { width: min(100vw, 177.78vh); height: min(56.25vw, 100vh); image-rendering: pixelated; image-rendering: crisp-edges; }
  #status { position: fixed; top: 45%; width: 100%; text-align: center; }
  #pad { display: none; position: fixed; inset: 0; pointer-events: none; }
  #pad button { pointer-events: auto; position: absolute; width: 64px; height: 64px; border-radius: 32px; border: 2px solid #fff6;
                background: #fff2; color: #fffa; font: bold 20px sans-serif; touch-action: none; user-select: none; -webkit-user-select: none; }
  @media (pointer: coarse) { #pad { display: block; } }
</style>
</head>
<body>
<canvas id="screen" width="640" height="360"></canvas>
<div id="status">Loading...</div>
<div id="pad">
  <button data-key="38" style="left:80px; bottom:144px">&#9650;</button>
  <button data-key="37" style="left:12px; bottom:76px">&#9664;</button>
  <button data-key="39" style="left:148px; bottom:76px">&#9654;</button>
  <button data-key="40" style="left:80px; bottom:8px">&#9660;</button>
  <button data-key="10" style="right:12px; bottom:60px; width:80px; height:80px; border-radius:40px">OK</button>
  <button data-key="27" style="right:12px; top:12px">X</button>
</div>
<script type="module">import init from "./rg_web.js"; init();</script>
</body>
</html>
"#;

fn mime(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript",
        "wasm" => "application/wasm",
        "txt" | "cfg" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// `serve-web [PORT]`: serves dist/web on http://localhost:PORT (default 8080). The page's test
/// aid (`?shots=`) POSTs screenshots to /shot/NAME: saved as build/shots/NAME.png.
pub fn serve(args: &[String]) -> R<()> {
    let port: u16 = args.first().and_then(|a| a.parse().ok()).unwrap_or(8080);
    let dist = root().join("dist/web");
    if !dist.join("index.html").exists() {
        return Err("dist/web is missing: run `cargo x build-web` first".into());
    }
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
    println!("serving {} on http://localhost:{port}/ (Ctrl+C ends)", dist.display());
    for conn in listener.incoming().flatten() {
        let dist = dist.clone();
        std::thread::spawn(move || {
            let _ = handle(conn, &dist);
        });
    }
    Ok(())
}

fn handle(mut conn: std::net::TcpStream, dist: &Path) -> R<()> {
    let mut rd = BufReader::new(conn.try_clone()?);
    let mut line = String::new();
    rd.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let path = target.split('?').next().unwrap_or("/").to_string();
    let mut length = 0usize;
    loop {
        let mut h = String::new();
        if rd.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let rel = path.trim_start_matches('/');
    if rel.split('/').any(|c| c == ".." || c.contains('\\')) {
        conn.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
        return Ok(());
    }
    if method == "POST" {
        if let Some(name) = rel.strip_prefix("shot/").filter(|n| !n.is_empty() && !n.contains('/')) {
            let mut body = vec![0u8; length];
            std::io::Read::read_exact(&mut rd, &mut body)?;
            let (w, h) = (640u32, 360u32);
            if body.len() == (w * h * 4) as usize {
                let dir = root().join("build/shots");
                std::fs::create_dir_all(&dir)?;
                let mut enc = png::Encoder::new(std::fs::File::create(dir.join(format!("{name}.png")))?, w, h);
                enc.set_color(png::ColorType::Rgba);
                enc.write_header()?.write_image_data(&body)?;
                println!("screenshot build/shots/{name}.png");
            }
        }
        conn.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")?;
        return Ok(());
    }
    let file = if rel.is_empty() { dist.join("index.html") } else { dist.join(rel) };
    match std::fs::read(&file).ok().filter(|_| file.is_file()) {
        Some(body) => {
            conn.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n", mime(&file), body.len()).as_bytes())?;
            conn.write_all(&body)?;
        }
        None => conn.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?,
    }
    Ok(())
}
