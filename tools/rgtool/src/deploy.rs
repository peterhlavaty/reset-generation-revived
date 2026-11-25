//! `cargo x deploy`: puts the relay (server/) on the public server, next to whatever else runs
//! there, without touching it: one static binary, one systemd unit and one nginx site for its own
//! subdomain (wss on 443 for the games and browsers, plain ws on port 80 for older builds and as
//! the games' fallback). `cargo x deploy-web`: the browser version (dist/web) into that site's
//! folder, served by nginx as static files, like the other web apps on that server.
//!
//! Settings: deploy.local.ini in the repository root (git-ignored, see deploy.example.ini).
//! SSH is pure Rust (russh): the key, even a passphrase-protected one, is decoded in memory.

use crate::util::{expand, read_ini, root, run, R};
use russh::keys::{decode_secret_key, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{client, ChannelMsg, Disconnect};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use std::io::{Read, Write};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const TARGET: &str = "x86_64-unknown-linux-musl";
const DIR: &str = "/opt/rg-relay";
const UNIT: &str = "/etc/systemd/system/rg-relay.service";
const SITE: &str = "/etc/nginx/sites-available/rg-relay.conf";
const ENABLED: &str = "/etc/nginx/sites-enabled/rg-relay.conf";
const MARK: &str = "# Reset Generation Revived relay (written by rgtool deploy)";
const PORT: u16 = 8787;
/// The browser version: static files of our site (outside the folders other tools manage).
const WEB: &str = "/var/www/resetgeneration";

pub struct Settings {
    host: String,
    user: String,
    key: String,
    key_pass_file: Option<String>,
    pub domain: String,
}

pub fn settings() -> R<Settings> {
    let path = root().join("deploy.local.ini");
    if !path.exists() {
        return Err(format!(
            "{} is missing: copy deploy.example.ini there and fill it in",
            path.display()
        )
        .into());
    }
    let kv = read_ini(&path)?;
    let get = |k: &str| kv.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
    let need = |k: &str| get(k).ok_or_else(|| format!("deploy.local.ini: {k} is missing"));
    Ok(Settings {
        host: need("host")?,
        user: get("user").unwrap_or_else(|| "root".into()),
        key: need("key")?,
        key_pass_file: get("key_pass_file"),
        domain: need("domain")?,
    })
}

struct Accept;

impl client::Handler for Accept {
    type Error = russh::Error;
    // The host key is not pinned: the box was trusted when it was provisioned (rusty-apps).
    async fn check_server_key(&mut self, _key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

pub struct Ssh {
    handle: client::Handle<Accept>,
}

impl Ssh {
    pub async fn connect(s: &Settings) -> R<Ssh> {
        let pem = std::fs::read_to_string(expand(&s.key)).map_err(|e| format!("key {}: {e}", s.key))?;
        let pass = match &s.key_pass_file {
            Some(f) => Some(std::fs::read_to_string(expand(f)).map_err(|e| format!("{f}: {e}"))?.trim().to_string()),
            None => None,
        };
        let key = decode_secret_key(&pem, pass.as_deref())?;
        let cfg = Arc::new(client::Config { inactivity_timeout: Some(Duration::from_secs(120)), ..Default::default() });
        let mut handle = client::connect(cfg, (s.host.as_str(), 22u16), Accept).await?;
        let hash = handle.best_supported_rsa_hash().await?.flatten();
        let auth = handle.authenticate_publickey(&s.user, PrivateKeyWithHashAlg::new(Arc::new(key), hash)).await?;
        if !auth.success() {
            return Err(format!("SSH key authentication failed for {}@{}", s.user, s.host).into());
        }
        Ok(Ssh { handle })
    }

    /// Runs a shell command; returns (exit code, stdout + stderr).
    pub async fn exec(&self, cmd: &str) -> R<(i32, String)> {
        let mut ch = self.handle.channel_open_session().await?;
        ch.exec(true, cmd.as_bytes().to_vec()).await?;
        let (mut code, mut out) = (-1, Vec::new());
        while let Some(msg) = ch.wait().await {
            match msg {
                ChannelMsg::Data { ref data } | ChannelMsg::ExtendedData { ref data, .. } => out.extend_from_slice(data),
                ChannelMsg::ExitStatus { exit_status } => code = exit_status as i32,
                _ => {}
            }
        }
        Ok((code, String::from_utf8_lossy(&out).into_owned()))
    }

    /// Runs a command, printing its output; fails on a non-zero exit.
    pub async fn run(&self, cmd: &str) -> R<String> {
        let (code, out) = self.exec(cmd).await?;
        print!("{out}");
        if code != 0 {
            return Err(format!("remote command failed ({code}): {cmd}").into());
        }
        Ok(out)
    }

    pub async fn upload(&self, path: &str, data: &[u8]) -> R<()> {
        let ch = self.handle.channel_open_session().await?;
        ch.request_subsystem(true, "sftp").await?;
        let sftp = SftpSession::new(ch.into_stream()).await?;
        let mut f = sftp.open_with_flags(path, OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE).await?;
        f.write_all(data).await?;
        f.shutdown().await?;
        Ok(())
    }

    /// Several files under `dir` in one SFTP session (their folders must exist).
    pub async fn upload_all(&self, dir: &str, files: &[(String, Vec<u8>)]) -> R<()> {
        let ch = self.handle.channel_open_session().await?;
        ch.request_subsystem(true, "sftp").await?;
        let sftp = SftpSession::new(ch.into_stream()).await?;
        for (name, data) in files {
            let mut f = sftp.open_with_flags(format!("{dir}/{name}"), OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE).await?;
            f.write_all(data).await?;
            f.shutdown().await?;
        }
        Ok(())
    }

    pub async fn close(&self) {
        let _ = self.handle.disconnect(Disconnect::ByApplication, "", "en").await;
    }
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio").block_on(f)
}

fn unit() -> String {
    format!(
        "{MARK}
[Unit]
Description=Reset Generation Revived relay
After=network.target

[Service]
ExecStart={DIR}/rg-relay --listen 127.0.0.1:{PORT}
Restart=always
RestartSec=2
DynamicUser=yes
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectControlGroups=yes
RestrictAddressFamilies=AF_INET AF_INET6
MemoryMax=256M
LimitNOFILE=8192

[Install]
WantedBy=multi-user.target
"
    )
}

fn site(domain: &str, cert: Option<(&str, &str)>) -> String {
    let proxy = format!(
        "        proxy_pass http://127.0.0.1:{PORT};
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $rg_relay_connection;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_buffering off;
        proxy_read_timeout 120s;
        proxy_send_timeout 120s;
"
    );
    let relay = format!("    location = /ws {{\n{proxy}    }}\n    location = /status {{\n{proxy}    }}\n");
    // the browser version: precompressed (deploy-web writes a .gz next to what compresses well),
    // and "no-cache": its file names do not change between builds, so a browser asks every time
    // (the ETag makes that a 304)
    let web = format!(
        "    root {WEB};
    index index.html;
    types {{ application/wasm wasm; }}
    include /etc/nginx/mime.types;
    gzip_static on;
    gzip_vary on;
    location / {{
        add_header Cache-Control \"no-cache\";
        try_files $uri $uri/ =404;
    }}
"
    );
    // port 80: the relay for plain ws; the page itself only over https when there is a certificate
    let plain = if cert.is_some() { "    location / {\n        return 301 https://$host$request_uri;\n    }\n".to_string() } else { web.clone() };
    let mut s = format!(
        "{MARK}
# Only this subdomain: everything else on this server keeps its own configuration.
map $http_upgrade $rg_relay_connection {{
    default upgrade;
    ''      close;
}}

# Plain ws for older game builds and the games' TLS fallback: no redirect to https for /ws.
server {{
    listen 80;
    listen [::]:80;
    server_name {domain};
{relay}{plain}}}
"
    );
    if let Some((crt, key)) = cert {
        s += &format!(
            "
server {{
    listen 443 ssl http2;
    listen [::]:443 ssl http2;
    server_name {domain};
    ssl_certificate     {crt};
    ssl_certificate_key {key};
{relay}{web}}}
"
        );
    }
    s
}

/// Builds the static Linux binary.
pub fn build_linux() -> R<std::path::PathBuf> {
    let r = root();
    run(Command::new("rustup").args(["target", "add", TARGET]))?;
    run(Command::new("cargo").current_dir(&r).args(["build", "-q", "--release", "-p", "rg-server", "--target", TARGET]))?;
    Ok(r.join("target").join(TARGET).join("release").join("rg-relay"))
}

pub fn deploy() -> R<()> {
    let s = settings()?;
    let bin = std::fs::read(build_linux()?)?;
    println!("rg-relay for Linux: {} KB", bin.len() / 1024);
    block_on(async {
        let ssh = Ssh::connect(&s).await?;
        // 1. Look before touching anything.
        let (_, os) = ssh.exec("uname -sm; nginx -v 2>&1; systemctl --version | head -1").await?;
        print!("{os}");
        let (_, listeners) = ssh.exec(&format!("ss -ltnpH 'sport = :{PORT}'")).await?;
        if !listeners.trim().is_empty() && !listeners.contains("rg-relay") {
            return Err(format!("port {PORT} on the server is used by something else:\n{listeners}").into());
        }
        for f in [SITE, UNIT] {
            let (_, head) = ssh.exec(&format!("test -e {f} && head -1 {f} || true")).await?;
            if !head.trim().is_empty() && head.trim() != MARK {
                return Err(format!("{f} exists and was not written by rgtool: not touching it").into());
            }
        }
        let (_, clash) = ssh
            .exec(&format!(
                "grep -lw 'server_name.*{}' /etc/nginx/sites-enabled/* 2>/dev/null | grep -v rg-relay.conf || true",
                s.domain.replace('.', "\\.")
            ))
            .await?;
        if !clash.trim().is_empty() {
            return Err(format!("another nginx site already names {}: {clash}", s.domain).into());
        }
        // the wildcard certificate the main site uses (if HTTPS is on)
        let (_, certs) = ssh
            .exec("grep -hE '^\\s*ssl_certificate(_key)?\\s' $(ls /etc/nginx/sites-enabled/* | grep -v rg-relay) 2>/dev/null | head -2")
            .await?;
        let mut crt = None;
        let mut key = None;
        for l in certs.lines() {
            let mut it = l.split_whitespace();
            match (it.next(), it.next()) {
                (Some("ssl_certificate"), Some(v)) => crt = Some(v.trim_end_matches(';').to_string()),
                (Some("ssl_certificate_key"), Some(v)) => key = Some(v.trim_end_matches(';').to_string()),
                _ => {}
            }
        }
        let cert = crt.as_deref().zip(key.as_deref());
        if cert.is_none() {
            println!("no TLS certificate found: serving plain ws only");
        }
        // 2. The binary and the service, swapped in atomically; the relay is restarted (which
        //    drops everyone connected) only when one of them changed.
        ssh.run(&format!("mkdir -p {DIR}")).await?;
        ssh.upload(&format!("{DIR}/rg-relay.new"), &bin).await?;
        ssh.upload(&format!("{DIR}/rg-relay.service.new"), unit().as_bytes()).await?;
        let (_, changed) = ssh
            .exec(&format!(
                "cd {DIR} && n=0; cmp -s rg-relay.new rg-relay || n=1; cmp -s rg-relay.service.new {UNIT} || n=1; \
                 chmod 755 rg-relay.new && mv -f rg-relay.new rg-relay && mv -f rg-relay.service.new {UNIT} && echo $n"
            ))
            .await?;
        let (_, active) = ssh.exec("systemctl is-active rg-relay").await?;
        if changed.trim() == "0" && active.trim() == "active" {
            println!("rg-relay unchanged and running: not restarted");
        } else {
            ssh.run("systemctl daemon-reload && systemctl enable -q rg-relay && systemctl restart rg-relay").await?;
        }
        // 4. The nginx site; on a config error it is removed again and nginx is left as it was.
        ssh.upload(SITE, site(&s.domain, cert).as_bytes()).await?;
        ssh.run(&format!("ln -sf {SITE} {ENABLED}")).await?;
        let (code, test) = ssh.exec("nginx -t 2>&1").await?;
        if code != 0 {
            ssh.exec(&format!("rm -f {ENABLED} {SITE}")).await?;
            return Err(format!("nginx rejected the site (removed again):\n{test}").into());
        }
        ssh.run("systemctl reload nginx").await?;
        tokio::time::sleep(Duration::from_millis(800)).await;
        ssh.run("systemctl is-active rg-relay").await?;
        ssh.close().await;
        Ok::<(), Box<dyn std::error::Error>>(())
    })?;
    status()
}

/// GET /status of the public server over plain HTTP.
pub fn status() -> R<()> {
    let s = settings()?;
    let body = http_get(&s.domain, 80, "/status")?;
    println!("http://{}/status: {body}", s.domain);
    if !body.contains("\"players\"") {
        return Err("the relay does not answer".into());
    }
    Ok(())
}

pub fn http_get(host: &str, port: u16, path: &str) -> R<String> {
    use std::net::ToSocketAddrs;
    let addr = (host, port).to_socket_addrs()?.next().ok_or("cannot resolve")?;
    let mut c = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    c.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(c, "GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n")?;
    let mut resp = String::new();
    c.read_to_string(&mut resp)?;
    Ok(resp.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or(resp))
}

pub fn logs(lines: usize) -> R<()> {
    let s = settings()?;
    block_on(async {
        let ssh = Ssh::connect(&s).await?;
        ssh.run(&format!("journalctl -u rg-relay -n {lines} --no-pager -o short-iso")).await?;
        ssh.close().await;
        Ok(())
    })
}

/// Runs one command on the server (for looking around).
pub fn remote(cmd: &str) -> R<()> {
    let s = settings()?;
    block_on(async {
        let ssh = Ssh::connect(&s).await?;
        let (code, out) = ssh.exec(cmd).await?;
        print!("{out}");
        ssh.close().await;
        if code != 0 {
            return Err(format!("exit {code}").into());
        }
        Ok(())
    })
}

/// `deploy-web`: builds the browser version and puts it on the server (into WEB, swapped in at
/// once), with a gzip copy of each file that compresses by a tenth or more (gzip_static).
pub fn deploy_web() -> R<()> {
    use flate2::{write::GzEncoder, Compression};
    let s = settings()?;
    crate::web::build(&[])?;
    let dist = root().join("dist/web");
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut dirs = vec![String::new()];
    let mut todo = vec![dist.clone()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d)?.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(&dist)?.to_string_lossy().replace('\\', "/");
            if p.is_dir() {
                dirs.push(rel);
                todo.push(p);
                continue;
            }
            let data = std::fs::read(&p)?;
            let mut gz = GzEncoder::new(Vec::new(), Compression::best());
            gz.write_all(&data)?;
            let gz = gz.finish()?;
            if gz.len() < data.len() / 10 * 9 {
                files.push((format!("{rel}.gz"), gz));
            }
            files.push((rel, data));
        }
    }
    let total: usize = files.iter().map(|f| f.1.len()).sum();
    println!("uploading {} files, {} MB", files.len(), total >> 20);
    block_on(async {
        let ssh = Ssh::connect(&s).await?;
        let stage = format!("{WEB}.new");
        let mk: Vec<String> = dirs.iter().map(|d| format!("'{stage}/{d}'")).collect();
        ssh.run(&format!("rm -rf {stage} && mkdir -p {}", mk.join(" "))).await?;
        ssh.upload_all(&stage, &files).await?;
        ssh.run(&format!(
            "chmod -R a+rX {stage} && rm -rf {WEB}.old && {{ test ! -e {WEB} || mv {WEB} {WEB}.old; }} && mv {stage} {WEB} && rm -rf {WEB}.old"
        ))
        .await?;
        // through nginx, as a browser would get it
        let d = &s.domain;
        let (_, code) = ssh
            .exec(&format!("curl -s -o /dev/null -w '%{{http_code}} %{{size_download}}' -H 'Accept-Encoding: gzip' --resolve {d}:443:127.0.0.1 https://{d}/rg_web_bg.wasm"))
            .await?;
        println!("https://{d}/rg_web_bg.wasm: HTTP {code} bytes (gzip)");
        ssh.close().await;
        if !code.starts_with("200") {
            return Err("nginx does not serve the browser version (run `cargo x deploy` for the site)".into());
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    })?;
    println!("the browser version is at https://{}/", s.domain);
    Ok(())
}

/// Removes everything deploy and deploy-web added (binary, unit, site, the browser version).
pub fn undeploy() -> R<()> {
    let s = settings()?;
    block_on(async {
        let ssh = Ssh::connect(&s).await?;
        ssh.run(&format!(
            "systemctl disable --now rg-relay 2>/dev/null; rm -f {ENABLED} {SITE} {UNIT}; rm -rf {DIR} {WEB}; systemctl daemon-reload; nginx -t && systemctl reload nginx"
        ))
        .await?;
        ssh.close().await;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_routes() {
        let tls = site("rg.example.com", Some(("/c.pem", "/k.pem")));
        // the relay on both ports, the page over https only
        assert_eq!(tls.matches("location = /ws {").count(), 2);
        assert_eq!(tls.matches("location = /status {").count(), 2);
        assert_eq!(tls.matches(&format!("root {WEB};")).count(), 1);
        assert!(tls.contains("return 301 https://$host$request_uri;"));
        assert_eq!(tls.matches('{').count(), tls.matches('}').count());
        let plain = site("rg.example.com", None);
        assert!(plain.contains(&format!("root {WEB};")) && !plain.contains("return 301"));
        println!("{tls}");
    }
}
