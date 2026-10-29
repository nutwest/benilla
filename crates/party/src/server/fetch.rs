//! Downloads through Windows' own `curl.exe` and unpacking through its `tar.exe`, so the game
//! carries no HTTP or archive stack. Archives are pinned by SHA-256 ([`sha256`]).

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

/// The SHA-256 of `path`, lowercase hex.
pub fn sha256(path: &Path) -> anyhow::Result<String> {
    use sha2::Digest as _;
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
