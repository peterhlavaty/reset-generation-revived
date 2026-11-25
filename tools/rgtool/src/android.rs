//! `cargo x build-android`: the Android APK (sideloadable, debug-signed) in dist/:
//! recompiled engine + native/rt + native/android host -> libresetgen.so per ABI, the game data
//! (pre-patched for the 640x360 engine size) as assets. No Gradle and no Java code in the app:
//! NativeActivity, packaged with aapt2 / zipalign / apksigner from the SDK.
//! `android-run`, `android-keys`, `android-shot`, `android-log` drive it in the emulator.

use crate::native::{c_files, compile_all, generate, patched_data, tool_jdk};
use crate::setup::{android_sdk, newest};
use crate::util::{files_with_ext, remove_dir, root, run, R};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PACKAGE: &str = "com.appliedflux.resetgeneration";

fn sdk() -> R<PathBuf> {
    android_sdk().ok_or_else(|| "no Android SDK found (install Android Studio, or set ANDROID_HOME)".into())
}


fn exe(p: PathBuf) -> PathBuf {
    if cfg!(windows) {
        p.with_extension("exe")
    } else {
        p
    }
}

fn adb() -> R<PathBuf> {
    Ok(exe(sdk()?.join("platform-tools/adb")))
}

/// The serial of the first connected, ready device (an emulator that is shutting down can be
/// listed "offline" next to it, and adb then refuses commands without -s).
fn device() -> Option<String> {
    let o = Command::new(adb().ok()?).arg("devices").stderr(Stdio::null()).output().ok()?;
    let text = String::from_utf8_lossy(&o.stdout).to_string();
    text.lines().skip(1).find_map(|l| l.strip_suffix("	device").map(String::from))
}

fn adb_cmd() -> R<Command> {
    let mut c = Command::new(adb()?);
    if let Some(s) = device() {
        c.args(["-s", &s]);
    }
    Ok(c)
}

/// 2.0.0 -> 20000
fn version_code(v: &str) -> u32 {
    v.split('.').take(3).fold(0, |acc, p| acc * 100 + p.parse::<u32>().unwrap_or(0))
}

/// The game data for the APK (assets/game), patched for 640x360.
fn assets(out: &Path) -> R<PathBuf> {
    patched_data(&out.join("assets/game"))?;
    Ok(out.join("assets"))
}

/// `build-android [--abis arm64-v8a,x86_64] [--diag]`
pub fn build(args: &[String]) -> R<()> {
    let r = root();
    let sdk = sdk()?;
    let ndk = newest(&sdk.join("ndk")).ok_or("no NDK in the Android SDK (install one with the SDK manager)")?;
    let bt = newest(&sdk.join("build-tools")).ok_or("no build-tools in the Android SDK")?;
    let platform = ["android-36"]
        .iter()
        .map(|p| sdk.join("platforms").join(p))
        .find(|p| p.join("android.jar").exists())
        .or_else(|| newest(&sdk.join("platforms")))
        .ok_or("no platform in the Android SDK")?
        .join("android.jar");
    let host = if cfg!(windows) { "windows-x86_64" } else if cfg!(target_os = "macos") { "darwin-x86_64" } else { "linux-x86_64" };
    let clang = exe(ndk.join("toolchains/llvm/prebuilt").join(host).join("bin/clang"));
    let abis: Vec<String> = args
        .iter()
        .find_map(|a| a.strip_prefix("--abis=").map(|v| v.split(',').map(String::from).collect()))
        .unwrap_or_else(|| vec!["arm64-v8a".into(), "x86_64".into()]);
    let diag = args.iter().any(|a| a == "--diag");
    let version = env!("CARGO_PKG_VERSION");
    let out = r.join("build/android");
    std::fs::create_dir_all(&out)?;
    let gen = generate(&[])?;
    let rt = r.join("native/rt");
    let glue = ndk.join("sources/android/native_app_glue");
    let headers = files_with_ext(&rt, "h");
    let stamp = format!("{version}-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs());
    let libs = out.join("apk");
    remove_dir(&libs)?;
    for abi in &abis {
        let triple = match abi.as_str() {
            "arm64-v8a" => "aarch64-linux-android26",
            "x86_64" => "x86_64-linux-android26",
            "armeabi-v7a" => "armv7a-linux-androideabi26",
            _ => return Err(format!("unknown ABI {abi}").into()),
        };
        eprintln!("== {abi}");
        let mut cf: Vec<String> = vec![format!("--target={triple}")];
        cf.extend(["-O2", "-fPIC", "-fno-strict-aliasing", "-fwrapv", "-ffunction-sections", "-fdata-sections"].map(String::from));
        for i in [&rt, &gen, &glue] {
            cf.push(format!("-I{}", i.display()));
        }
        let obj = out.join(format!("obj-{abi}"));
        let mut gf = cf.clone();
        gf.push("-w".into());
        let mut objs = compile_all(&clang, &gf, &c_files(&gen), &obj, &headers)?;
        let mut own = c_files(&rt);
        own.push(r.join("native/android/main.c"));
        own.push(glue.join("android_native_app_glue.c"));
        // always rebuilt: the data stamp below changes with every build
        for f in &own {
            let _ = std::fs::remove_file(obj.join(format!("{}.o", f.file_stem().unwrap().to_string_lossy())));
        }
        let mut of = cf.clone();
        of.extend(["-Wall", "-Wno-unused-function"].map(String::from));
        if diag {
            of.push("-DRG_DIAG".into());
        }
        of.push(format!("-DRG_DATA_STAMP=\"{stamp}\""));
        objs.extend(compile_all(&clang, &of, &own, &obj, &headers)?);
        let so = libs.join("lib").join(abi).join("libresetgen.so");
        std::fs::create_dir_all(so.parent().unwrap())?;
        run(Command::new(&clang)
            .arg(format!("--target={triple}"))
            .arg("-shared")
            .arg("-o")
            .arg(&so)
            .args(&objs)
            .args(["-u", "ANativeActivity_onCreate", "-Wl,--gc-sections", "-Wl,-z,max-page-size=16384", "-Wl,--build-id=sha1", "-s"])
            .args(["-landroid", "-llog", "-laaudio", "-lm"]))?;
        eprintln!("{} ({} KB)", so.display(), std::fs::metadata(&so)?.len() >> 10);
    }
    let assets = assets(&out)?;
    // package
    let manifest = out.join("AndroidManifest.xml");
    let m = std::fs::read_to_string(r.join("native/android/AndroidManifest.xml"))?
        .replace("@VERSION_CODE@", &version_code(version).to_string())
        .replace("@VERSION_NAME@", version);
    std::fs::write(&manifest, m)?;
    let res = out.join("res-compiled");
    remove_dir(&res)?;
    std::fs::create_dir_all(&res)?;
    let aapt2 = exe(bt.join("aapt2"));
    run(Command::new(&aapt2).args(["compile", "--dir"]).arg(r.join("native/android/res")).arg("-o").arg(res.join("res.zip")))?;
    let linked = out.join("linked.apk");
    run(Command::new(&aapt2)
        .args(["link", "-o"])
        .arg(&linked)
        .arg("-I")
        .arg(&platform)
        .arg("--manifest")
        .arg(&manifest)
        .arg("-A")
        .arg(&assets)
        .arg(res.join("res.zip")))?;
    // aapt2's entries copied as they are (raw) into a new zip, plus the native libraries
    let unsigned = out.join("unsigned.apk");
    {
        let mut z = zip::ZipWriter::new(std::fs::File::create(&unsigned)?);
        copy_entries(&linked, &mut z)?;
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for p in files_with_ext(&libs.join("lib"), "so") {
            let name = p.strip_prefix(&libs)?.to_string_lossy().replace('\\', "/");
            z.start_file(name, opts)?;
            z.write_all(&std::fs::read(&p)?)?;
        }
        z.finish()?;
    }
    let aligned = out.join("aligned.apk");
    run(Command::new(exe(bt.join("zipalign"))).args(["-f", "-p", "4"]).arg(&unsigned).arg(&aligned))?;
    let jdk = tool_jdk()?;
    let ks = out.join("debug.keystore");
    if !ks.exists() {
        run(Command::new(jdk.join("bin/keytool"))
            .args(["-genkeypair", "-keystore"])
            .arg(&ks)
            .args(["-storepass", "android", "-keypass", "android", "-alias", "rgdebug", "-keyalg", "RSA", "-keysize", "2048"])
            .args(["-validity", "10000", "-dname", "CN=Reset Generation Revived debug"])
            .stdout(Stdio::null()))?;
    }
    std::fs::create_dir_all(r.join("dist"))?;
    let apk = r.join("dist").join(format!("ResetGeneration-Revived-{version}.apk"));
    let signer = bt.join(if cfg!(windows) { "apksigner.bat" } else { "apksigner" });
    run(Command::new(&signer)
        .env("JAVA_HOME", &jdk)
        .args(["sign", "--ks"])
        .arg(&ks)
        .args(["--ks-pass", "pass:android", "--ks-key-alias", "rgdebug", "--out"])
        .arg(&apk)
        .arg(&aligned))?;
    run(Command::new(&signer).env("JAVA_HOME", &jdk).arg("verify").arg(&apk))?;
    println!("built {} ({} MB)", apk.display(), std::fs::metadata(&apk)?.len() >> 20);
    Ok(())
}

fn adb_out(args: &[&str]) -> R<String> {
    let o = adb_cmd()?.args(args).stderr(Stdio::null()).output()?;
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Starts the emulator (first AVD, or --avd=NAME) when no device is connected, waits for it to
/// boot, installs the APK and starts the game.
pub fn run_emulator(args: &[String]) -> R<()> {
    let sdk = sdk()?;
    let devices = adb_out(&["devices"])?;
    if !devices.lines().skip(1).any(|l| l.ends_with("\tdevice")) {
        let emu = exe(sdk.join("emulator/emulator"));
        let avd = match args.iter().find_map(|a| a.strip_prefix("--avd=")) {
            Some(a) => a.to_string(),
            None => {
                let o = Command::new(&emu).arg("-list-avds").output()?;
                String::from_utf8_lossy(&o.stdout).lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("INFO")).ok_or("no AVD: create one in Android Studio")?.to_string()
            }
        };
        eprintln!("starting emulator {avd}");
        crate::util::no_inherit();
        Command::new(&emu).args(["-avd", &avd, "-no-snapshot-save", "-no-boot-anim"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
        let t = Instant::now();
        loop {
            if adb_out(&["shell", "getprop", "sys.boot_completed"]).unwrap_or_default() == "1" {
                break;
            }
            if t.elapsed() > Duration::from_secs(240) {
                return Err("the emulator did not boot within 4 minutes".into());
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    let apk = root().join("dist").join(format!("ResetGeneration-Revived-{}.apk", env!("CARGO_PKG_VERSION")));
    if !apk.exists() {
        return Err("build first: cargo x build-android".into());
    }
    run(adb_cmd()?.args(["install", "-r"]).arg(&apk))?;
    let _ = adb_out(&["logcat", "-c"]);
    run(adb_cmd()?.args(["shell", "am", "start", "-n", &format!("{PACKAGE}/android.app.NativeActivity")]))?;
    println!("started; `cargo x android-log`, `cargo x android-shot NAME`, `cargo x android-keys ENTER,DOWN`");
    Ok(())
}

/// Key presses: ENTER ESC UP DOWN LEFT RIGHT CTRL (or Android KEYCODE_ names), WAITms.
pub fn keys(args: &[String]) -> R<()> {
    for k in args.join(",").split(',').map(|k| k.trim().to_ascii_uppercase()).filter(|k| !k.is_empty()) {
        if let Some(ms) = k.strip_prefix("WAIT") {
            std::thread::sleep(Duration::from_millis(ms.parse().unwrap_or(500)));
            continue;
        }
        let code = match k.as_str() {
            "ENTER" | "RETURN" => "KEYCODE_ENTER".to_string(),
            "ESC" | "BACK" => "KEYCODE_ESCAPE".into(),
            "UP" | "DOWN" | "LEFT" | "RIGHT" => format!("KEYCODE_DPAD_{k}"),
            "CTRL" => "KEYCODE_CTRL_LEFT".into(),
            k => k.to_string(),
        };
        adb_out(&["shell", "input", "keyevent", &code])?;
        std::thread::sleep(Duration::from_millis(300));
    }
    Ok(())
}

pub fn shot(args: &[String]) -> R<()> {
    let name = args.first().map(String::as_str).unwrap_or("android");
    let d = root().join("build/shots");
    std::fs::create_dir_all(&d)?;
    let p = d.join(format!("{name}.png"));
    let o = adb_cmd()?.args(["exec-out", "screencap", "-p"]).output()?;
    if !o.status.success() || o.stdout.len() < 100 {
        return Err("screencap failed (is a device connected?)".into());
    }
    std::fs::write(&p, &o.stdout)?;
    println!("{}", p.display());
    Ok(())
}

pub fn log(args: &[String]) -> R<()> {
    let n = args.first().cloned().unwrap_or("200".into());
    let text = adb_out(&["logcat", "-d", "-t", &n, "-s", "ResetGeneration:*", "AndroidRuntime:E", "DEBUG:*", "libc:F"])?;
    println!("{text}");
    Ok(())
}

/// Copies a zip's entries as they are (compressed data untouched) into `z`, with '/' in their
/// names: aapt2 on Windows names assets "assets/game\\x.pak", which Android does not find.
fn copy_entries<W: std::io::Write + std::io::Seek>(src: &Path, z: &mut zip::ZipWriter<W>) -> R<()> {
    let mut a = zip::ZipArchive::new(std::fs::File::open(src)?)?;
    for i in 0..a.len() {
        let f = a.by_index_raw(i)?;
        let name = f.name().replace('\\', "/");
        if name == f.name() {
            z.raw_copy_file(f)?;
        } else {
            z.raw_copy_file_rename(f, name)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: assets packed by aapt2 on Windows had backslashes in their names, so the app
    /// found no game data on the device.
    #[test]
    fn apk_entry_names_use_forward_slashes() {
        let dir = std::env::temp_dir().join(format!("rgtool-apk-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("linked.apk");
        {
            let mut z = zip::ZipWriter::new(std::fs::File::create(&src).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("assets/game\\base.pak", o).unwrap();
            z.write_all(b"pak").unwrap();
            z.start_file("AndroidManifest.xml", o).unwrap();
            z.finish().unwrap();
        }
        let out = dir.join("out.apk");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&out).unwrap());
        copy_entries(&src, &mut z).unwrap();
        z.finish().unwrap();
        let mut a = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        let names: Vec<String> = a.file_names().map(String::from).collect();
        assert!(names.contains(&"assets/game/base.pak".to_string()), "{names:?}");
        let mut s = String::new();
        std::io::Read::read_to_string(&mut a.by_name("assets/game/base.pak").unwrap(), &mut s).unwrap();
        assert_eq!(s, "pak");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
