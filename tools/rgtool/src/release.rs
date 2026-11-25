//! `cargo x github-release DIR` (run by CI, `.github/workflows/native.yml`): the builds of a push
//! to main become the repository's rolling "latest" pre-release, so players download a working
//! game without building it. The release is created once and then updated; each file replaces
//! the asset of the same name. The asset names never change, so `…/releases/download/latest/<name>`
//! links (README, the browser page) stay valid. Needs GITHUB_TOKEN (contents: write),
//! GITHUB_REPOSITORY and GITHUB_SHA, as Actions sets them.

use crate::util::R;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const TAG: &str = "latest";
/// What players download, by the file name the builds give it; the APK's name carries its
/// version, its asset name does not.
const ASSETS: &[(&str, &str, &str)] = &[
    ("ResetGeneration-Revived-windows.zip", "Windows 10 or 11 (64-bit)", "application/zip"),
    ("ResetGeneration-Revived-linux.zip", "Linux (64-bit PC: x86-64)", "application/zip"),
    ("ResetGeneration-Revived-macos.zip", "macOS (Apple Silicon)", "application/zip"),
    ("ResetGeneration-Revived-android.apk", "Android 8 or newer (64-bit)", "application/vnd.android.package-archive"),
];

/// The asset name a build file is published under, if it is one of [`ASSETS`].
fn asset_name(file: &str) -> Option<&'static str> {
    if file.starts_with("ResetGeneration-Revived-") && file.ends_with(".apk") {
        return Some(ASSETS[3].0);
    }
    ASSETS.iter().map(|a| a.0).find(|a| *a == file)
}

/// Every build file under `dir` (the downloaded CI artifacts, one folder each), by asset name.
fn collect(dir: &Path) -> R<Vec<(&'static str, PathBuf)>> {
    let mut found: Vec<(&'static str, PathBuf)> = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for p in std::fs::read_dir(&d)?.flatten().map(|e| e.path()) {
            if p.is_dir() {
                stack.push(p);
            } else if let Some(name) = asset_name(&p.file_name().unwrap_or_default().to_string_lossy()) {
                if found.iter().any(|(n, _)| *n == name) {
                    return Err(format!("{name} is in {dir:?} twice").into());
                }
                found.push((name, p));
            }
        }
    }
    found.sort_by_key(|(n, _)| ASSETS.iter().position(|a| a.0 == *n));
    Ok(found)
}

/// The release page's text: which file is for which system, how to start it.
fn notes(repo: &str, sha: &str, files: &[&str]) -> String {
    let mut s = format!(
        "The newest build of Reset Generation Revived, made automatically from commit {sha}.\n\n\
         | System | Download |\n|---|---|\n"
    );
    for (name, system, _) in ASSETS.iter().filter(|a| files.contains(&a.0)) {
        s.push_str(&format!("| {system} | [{name}](https://github.com/{repo}/releases/download/{TAG}/{name}) |\n"));
    }
    s.push_str(&format!(
        "\nUnzip the file and start **ResetGeneration** in the folder (Windows: ResetGeneration.exe); \
         on Android open the APK to install it. Step by step, the controls and how to play: \
         [README](https://github.com/{repo}#play).\n\n\
         No download needed: play in the browser at https://resetgeneration.rustyapps.com/\n"
    ));
    s
}

struct Api {
    agent: ureq::Agent,
    token: String,
    repo: String,
}

impl Api {
    fn url(&self, path: &str) -> String {
        format!("https://api.github.com/repos/{}/{path}", self.repo)
    }

    /// One request; Ok(None) for a GET that finds nothing (404). An error carries GitHub's own
    /// message (e.g. why the token may not do something).
    fn call(&self, method: &str, url: &str, body: Option<Value>) -> R<Option<Value>> {
        let auth = format!("Bearer {}", self.token);
        let gh = "application/vnd.github+json";
        let res = match method {
            "GET" => self.agent.get(url).config().http_status_as_error(false).build().header("Authorization", &auth).header("Accept", gh).call(),
            "DELETE" => self.agent.delete(url).config().http_status_as_error(false).build().header("Authorization", &auth).header("Accept", gh).call(),
            "POST" => self.agent.post(url).config().http_status_as_error(false).build().header("Authorization", &auth).header("Accept", gh).send_json(body.unwrap_or_default()),
            "PATCH" => self.agent.patch(url).config().http_status_as_error(false).build().header("Authorization", &auth).header("Accept", gh).send_json(body.unwrap_or_default()),
            _ => unreachable!(),
        };
        let mut res = res.map_err(|e| format!("{method} {url}: {e}"))?;
        answer(method, url, &mut res)
    }

    /// Uploads a file as an asset of the release; returns the new asset's id.
    fn upload(&self, release: u64, name: &str, data: &[u8], kind: &str) -> R<u64> {
        let url = format!("https://uploads.github.com/repos/{}/releases/{release}/assets?name={name}", self.repo);
        let mut res = self
            .agent
            .post(&url)
            .config()
            .http_status_as_error(false)
            .build()
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("Content-Type", kind)
            .send(data)
            .map_err(|e| format!("uploading {name}: {e}"))?;
        let asset = answer("POST", &url, &mut res)?.unwrap_or_default();
        asset["id"].as_u64().ok_or_else(|| format!("uploading {name}: no asset id in {asset}").into())
    }

    fn assets(&self, release: u64) -> R<Vec<Value>> {
        let list = self.call("GET", &self.url(&format!("releases/{release}/assets?per_page=100")), None)?;
        Ok(list.and_then(|l| l.as_array().cloned()).unwrap_or_default())
    }

    fn delete_assets(&self, release: u64, name: &str) -> R<()> {
        for a in self.assets(release)?.iter().filter(|a| a["name"] == name) {
            self.call("DELETE", &self.url(&format!("releases/assets/{}", a["id"])), None)?;
        }
        Ok(())
    }

    /// Points the tag at `sha`. GitHub does not let the workflow's token create or move a tag
    /// over a commit that changes workflow files: then a new tag goes on the nearest parent that
    /// takes it and an existing one stays where it is, with a warning. Players are not affected:
    /// the download links only name the tag, and the notes name the commit the builds come from.
    fn tag(&self, sha: &str) -> R<()> {
        if self.call("GET", &self.url(&format!("git/ref/tags/{TAG}")), None)?.is_some() {
            let moved = self.call("PATCH", &self.url(&format!("git/refs/tags/{TAG}")), Some(json!({"sha": sha, "force": true})));
            if let Err(e) = moved {
                eprintln!("warning: the tag {TAG} stays where it is: {e}");
            }
            return Ok(());
        }
        let mut commit = sha.to_string();
        for _ in 0..20 {
            match self.call("POST", &self.url("git/refs"), Some(json!({"ref": format!("refs/tags/{TAG}"), "sha": commit}))) {
                Ok(_) => return Ok(()),
                Err(e) => {
                    eprintln!("warning: no tag {TAG} at {commit}: {e}");
                    let c = self.call("GET", &self.url(&format!("commits/{commit}")), None)?.ok_or("commit not found")?;
                    commit = c["parents"][0]["sha"].as_str().ok_or("no parent commit")?.to_string();
                }
            }
        }
        Err(format!("could not create the tag {TAG}").into())
    }
}

/// A response as JSON; Ok(None) for a GET answered 404, an error with GitHub's text for 4xx/5xx.
fn answer(method: &str, url: &str, res: &mut ureq::http::Response<ureq::Body>) -> R<Option<Value>> {
    let status = res.status().as_u16();
    let text = res.body_mut().read_to_string().unwrap_or_default();
    match status {
        404 if method == "GET" => Ok(None),
        s if s >= 400 => Err(format!("{method} {url}: HTTP {s}: {}", text.trim()).into()),
        _ if text.trim().is_empty() => Ok(Some(Value::Null)),
        _ => Ok(Some(serde_json::from_str(&text)?)),
    }
}

pub fn publish(args: &[String]) -> R<()> {
    let dir = PathBuf::from(args.first().ok_or("usage: cargo x github-release DIR")?);
    let env = |k: &str| std::env::var(k).map_err(|_| format!("{k} is not set (github-release runs in GitHub Actions)"));
    let api = Api { agent: crate::setup::agent(), token: env("GITHUB_TOKEN")?, repo: env("GITHUB_REPOSITORY")? };
    let sha = env("GITHUB_SHA")?;
    let files = collect(&dir)?;
    let names: Vec<&str> = files.iter().map(|(n, _)| *n).collect();
    let missing: Vec<&str> = ASSETS.iter().map(|a| a.0).filter(|a| !names.contains(a)).collect();
    if !missing.is_empty() {
        // all or nothing: the release says every file comes from this commit
        return Err(format!("not in {}: {}", dir.display(), missing.join(", ")).into());
    }

    api.tag(&sha)?;
    let body = json!({
        "tag_name": TAG,
        "name": "Latest build",
        "body": notes(&api.repo, &sha, &names),
        "prerelease": true,
        "make_latest": "false",
    });
    let release = match api.call("GET", &api.url(&format!("releases/tags/{TAG}")), None)? {
        Some(r) => api.call("PATCH", &api.url(&format!("releases/{}", r["id"])), Some(body))?,
        None => api.call("POST", &api.url("releases"), Some(body))?,
    }
    .unwrap_or_default();
    let id = release["id"].as_u64().ok_or("no release id")?;

    // Each file goes up under a temporary name and replaces the old asset only once it is there,
    // so the download link never breaks (and a failed upload leaves the old file).
    for (name, path) in &files {
        let temp = format!("{name}.new");
        api.delete_assets(id, &temp)?; // left over from a run that was stopped
        let data = std::fs::read(path)?;
        let kind = ASSETS.iter().find(|a| a.0 == *name).map_or("application/octet-stream", |a| a.2);
        let new = api.upload(id, &temp, &data, kind)?;
        api.delete_assets(id, name)?;
        api.call("PATCH", &api.url(&format!("releases/assets/{new}")), Some(json!({"name": name})))?;
        println!("uploaded {name} ({} MB)", data.len() >> 20);
    }
    println!("https://github.com/{}/releases/tag/{TAG}", api.repo);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_names() {
        assert_eq!(asset_name("ResetGeneration-Revived-2.0.0.apk"), Some("ResetGeneration-Revived-android.apk"));
        assert_eq!(asset_name("ResetGeneration-Revived-macos.zip"), Some("ResetGeneration-Revived-macos.zip"));
        assert_eq!(asset_name("rg_web_bg.wasm"), None);
        assert_eq!(asset_name("ResetGeneration-Revived-freebsd.zip"), None);
    }

    #[test]
    fn notes_link_only_what_was_built() {
        let n = notes("me/rg", "abc123", &["ResetGeneration-Revived-linux.zip"]);
        assert!(n.contains("abc123"));
        assert!(n.contains("https://github.com/me/rg/releases/download/latest/ResetGeneration-Revived-linux.zip"));
        assert!(!n.contains("windows.zip"));
    }

    /// The layout actions/download-artifact gives: one folder per artifact, the files as
    /// upload-artifact stored them under their common folder (dist/).
    #[test]
    fn collects_the_ci_artifacts() {
        let tmp = std::env::temp_dir().join(format!("rgtool-release-{}", std::process::id()));
        for (dir, file) in [
            ("ResetGeneration-Linux", "ResetGeneration-Revived-linux.zip"),
            ("ResetGeneration-Linux", "ResetGeneration-Revived-2.0.0.apk"),
            ("ResetGeneration-Linux/web", "rg_web_bg.wasm"),
            ("ResetGeneration-macOS", "ResetGeneration-Revived-macos.zip"),
            ("ResetGeneration-Windows", "ResetGeneration-Revived-windows.zip"),
        ] {
            std::fs::create_dir_all(tmp.join(dir)).unwrap();
            std::fs::write(tmp.join(dir).join(file), b"x").unwrap();
        }
        let names: Vec<&str> = collect(&tmp).unwrap().into_iter().map(|(n, _)| n).collect();
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(names, ASSETS.iter().map(|a| a.0).collect::<Vec<_>>());
    }
}
