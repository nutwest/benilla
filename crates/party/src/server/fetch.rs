//! Downloads through Windows' own `curl.exe` and unpacking through its `tar.exe`, so the game
//! carries no HTTP or archive stack. Releases are resolved through the GitHub API.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{bail, Context as _};

use super::procs::hidden;

const USER_AGENT: &str = "benilla-party";

/// A file to download, with its size when the source states it.
pub struct Asset {
    pub url: String,
    pub name: String,
    pub size: Option<u64>,
}

fn curl() -> Command {
    let mut cmd = Command::new("curl.exe");
    cmd.args(["-fsSL", "--retry", "3", "-A", USER_AGENT]);
    hidden(&mut cmd, 0);
    cmd
}

/// The body at `url`, as text.
pub fn text(url: &str) -> anyhow::Result<String> {
    let out = curl()
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .context("running curl.exe")?;
    if !out.status.success() {
        bail!(
            "fetching {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8(out.stdout)?)
}

fn json(url: &str) -> anyhow::Result<serde_json::Value> {
    serde_json::from_str(&text(url)?).with_context(|| format!("reading {url}"))
}

/// The asset of release `tag` of `repo` whose name `pick` accepts.
pub fn release_asset(repo: &str, tag: &str, pick: impl Fn(&str) -> bool) -> anyhow::Result<Asset> {
    let release = json(&format!(
        "https://api.github.com/repos/{repo}/releases/tags/{tag}"
    ))?;
    let assets = release["assets"]
        .as_array()
        .context("release has no assets")?;
    assets
        .iter()
        .find_map(|a| {
            let name = a["name"].as_str()?;
            let url = a["browser_download_url"].as_str()?;
            pick(name).then(|| Asset {
                url: url.to_string(),
                name: name.to_string(),
                size: a["size"].as_u64(),
            })
        })
        .with_context(|| format!("no matching asset in {repo} release {tag}"))
}

/// The full sha of a commit named by a short one.
pub fn commit_sha(repo: &str, short: &str) -> anyhow::Result<String> {
    let commit = json(&format!(
        "https://api.github.com/repos/{repo}/commits/{short}"
    ))?;
    Ok(commit["sha"].as_str().context("no sha")?.to_string())
}

/// The names of the files in directory `path` of `repo` at commit `sha`, walked down the trees,
/// since the contents API stops at a thousand entries.
pub fn list_dir(repo: &str, sha: &str, path: &str) -> anyhow::Result<Vec<String>> {
    let mut tree = sha.to_string();
    for part in path.split('/') {
        let listing = json(&format!(
            "https://api.github.com/repos/{repo}/git/trees/{tree}"
        ))?;
        tree = listing["tree"]
            .as_array()
            .and_then(|t| t.iter().find(|e| e["path"] == part))
            .and_then(|e| e["sha"].as_str())
            .with_context(|| format!("{path} not in {repo}@{sha}"))?
            .to_string();
    }
    let listing = json(&format!(
        "https://api.github.com/repos/{repo}/git/trees/{tree}"
    ))?;
    Ok(listing["tree"]
        .as_array()
        .context("not a directory")?
        .iter()
        .filter_map(|e| e["path"].as_str().map(str::to_string))
        .collect())
}

/// A raw file of `repo` at `sha`.
pub fn raw(repo: &str, sha: &str, path: &str) -> anyhow::Result<String> {
    text(&format!(
        "https://raw.githubusercontent.com/{repo}/{sha}/{path}"
    ))
}

/// Downloads `asset` to `dest`, reporting the fraction done while it runs. A file already at
/// `dest` is kept: downloads land under a `.part` name and are renamed only when complete.
pub fn download(
    asset: &Asset,
    dest: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f32),
) -> anyhow::Result<()> {
    if dest.exists() {
        return Ok(());
    }
    let part = dest.with_extension("part");
    let mut child = curl()
        .arg("-o")
        .arg(&part)
        .arg(&asset.url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("running curl.exe")?;
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                let mut err = String::new();
                if let Some(mut e) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut e, &mut err);
                }
                bail!("downloading {}: {}", asset.url, err.trim());
            }
            break;
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            bail!("cancelled");
        }
        if let (Some(size), Ok(meta)) = (asset.size, std::fs::metadata(&part)) {
            progress((meta.len() as f32 / size.max(1) as f32).min(1.0));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    std::fs::rename(&part, dest)?;
    Ok(())
}

/// The size `url` states, following redirects.
pub fn content_length(url: &str) -> Option<u64> {
    let out = curl()
        .args(["-I", url])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse().ok())
                .flatten()
        })
        // The final response's, after any redirects.
        .next_back()
}

/// Unpacks a zip into `dir` with `tar.exe`.
pub fn unzip(zip: &Path, dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut cmd = Command::new("tar.exe");
    cmd.arg("-xf").arg(zip).arg("-C").arg(dir);
    let status = hidden(&mut cmd, 0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("running tar.exe")?;
    if !status.success() {
        bail!("unpacking {} failed ({status})", zip.display());
    }
    Ok(())
}
