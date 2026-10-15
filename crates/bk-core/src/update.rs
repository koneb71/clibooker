//! Updates for a standalone `bk`. The newest release is described by `latest.json`, the file
//! the desktop app's updater reads too. `bk update` fetches this platform's archive and its
//! signature, checks them against Clibooker's release key, and puts the new bk in its own
//! place. A bk that came with the app, a package or cargo is updated the way it was installed.

#[cfg(any(test, feature = "fake-releases"))]
pub mod fake;

use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
pub use semver::Version;
use serde::Deserialize;

/// Clibooker's release signing key, as Tauri writes it (base64 of a minisign public key).
/// The app trusts the same one (`plugins.updater.pubkey` in `app/src-tauri/tauri.conf.json`;
/// a test keeps them equal).
pub const RELEASE_PUBKEY: &str = "";

/// The platform this bk was built for, as release archives name it.
pub const TARGET: &str = env!("BK_TARGET");

const RELEASES: &str = "https://github.com/koneb71/clibooker/releases";

/// Where releases are published (`BK_RELEASES_URL` points tests at a local server).
pub fn releases_base() -> String {
    std::env::var("BK_RELEASES_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| RELEASES.to_string())
        .trim_end_matches('/')
        .to_string()
}

/// The key releases must be signed with. Debug builds take another one from `BK_RELEASE_KEY`,
/// so tests can sign releases of their own; release builds never do.
fn release_key() -> String {
    if cfg!(debug_assertions)
        && let Ok(k) = std::env::var("BK_RELEASE_KEY")
    {
        return k;
    }
    RELEASE_PUBKEY.to_string()
}

pub fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is semver")
}

fn get(url: &str, timeout: Duration, limit: u64) -> Result<Vec<u8>> {
    let agent: ureq::Agent =
        ureq::Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(false).build().into();
    let mut resp = agent.get(url).call().map_err(|e| anyhow!("can't reach {url}: {e}"))?;
    if resp.status() != 200 {
        bail!("{url}: HTTP {}", resp.status().as_u16());
    }
    resp.body_mut().with_config().limit(limit).read_to_vec().map_err(|e| anyhow!("downloading {url}: {e}"))
}

#[derive(Deserialize)]
struct Manifest {
    version: String,
}

/// The newest released version.
pub fn latest(timeout: Duration) -> Result<Version> {
    let url = format!("{}/latest/download/latest.json", releases_base());
    let manifest: Manifest = serde_json::from_slice(&get(&url, timeout, 1 << 20)?).context("reading latest.json")?;
    Version::parse(manifest.version.trim_start_matches('v'))
        .with_context(|| format!("latest.json has an odd version `{}`", manifest.version))
}

/// `bk-v0.4.0-x86_64-unknown-linux-gnu.tar.gz` (a `.zip` on Windows).
pub fn asset_name(version: &Version, target: &str) -> String {
    let ext = if target.contains("windows") { "zip" } else { "tar.gz" };
    format!("bk-v{version}-{target}.{ext}")
}

fn decode_b64(text: &str, what: &str) -> Result<String> {
    let bytes = STANDARD.decode(text.trim()).map_err(|_| anyhow!("{what} isn't valid base64"))?;
    String::from_utf8(bytes).map_err(|_| anyhow!("{what} isn't valid text"))
}

/// Check `data` against its `.sig` (Tauri's format: a base64 minisign signature) and `pubkey`.
/// The signature must also name this very file and version, so an older release or another
/// platform's archive can't stand in for it.
pub fn verify(data: &[u8], sig: &str, pubkey: &str, file: &str, version: &Version) -> Result<()> {
    if pubkey.trim().is_empty() {
        bail!("this bk was built without Clibooker's release key, so it can't check an update");
    }
    let key = minisign_verify::PublicKey::decode(&decode_b64(pubkey, "the release key")?)
        .map_err(|e| anyhow!("the release key: {e}"))?;
    let signature = minisign_verify::Signature::decode(&decode_b64(sig, "the signature")?)
        .map_err(|e| anyhow!("{file}.sig: {e}"))?;
    key.verify(data, &signature, false)
        .map_err(|_| anyhow!("{file} isn't signed with Clibooker's release key; not installing it"))?;
    let signed: BTreeMap<&str, &str> =
        signature.trusted_comment().split('\t').filter_map(|field| field.split_once(':')).collect();
    let version = version.to_string();
    if signed.get("file") != Some(&file) {
        bail!("the signature is for {}, not {file}", signed.get("file").unwrap_or(&"another file"));
    }
    if signed.get("version") != Some(&version.as_str()) {
        bail!("the signature is for version {}, not {version}", signed.get("version").unwrap_or(&"?"));
    }
    Ok(())
}

/// The `bk` (or `bk.exe`) inside a release archive.
pub fn extract_bk(archive: &[u8], name: &str) -> Result<Vec<u8>> {
    let is_bk = |path: &str| matches!(path.rsplit(['/', '\\']).next(), Some("bk" | "bk.exe"));
    let mut out = Vec::new();
    if name.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(Cursor::new(archive)).with_context(|| format!("opening {name}"))?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            if entry.is_file() && is_bk(&entry.name()?) {
                entry.read_to_end(&mut out)?;
                return Ok(out);
            }
        }
    } else {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
        for entry in tar.entries().with_context(|| format!("opening {name}"))? {
            let mut entry = entry?;
            if entry.header().entry_type().is_file() && is_bk(&entry.path()?.to_string_lossy()) {
                entry.read_to_end(&mut out)?;
                return Ok(out);
            }
        }
    }
    bail!("{name} has no bk in it")
}

/// How a bk was installed, which says how to update it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    /// From a release archive: `bk update` replaces it.
    Standalone,
    /// Inside the desktop app (also the .deb and .rpm packages): the app updates it.
    App,
    /// `cargo install`.
    Cargo,
}

pub fn install_kind(exe: &Path) -> InstallKind {
    let exe = dunce::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    let dir = exe.parent().unwrap_or(Path::new(""));
    if ["Clibooker", "Clibooker.exe", "clibooker"].iter().any(|app| dir.join(app).is_file()) {
        return InstallKind::App;
    }
    let cargo_home =
        std::env::var_os("CARGO_HOME").map(PathBuf::from).unwrap_or_else(|| crate::paths::home().join(".cargo"));
    let cargo_bin = dunce::canonicalize(cargo_home.join("bin")).unwrap_or_else(|_| cargo_home.join("bin"));
    if exe.starts_with(cargo_bin) { InstallKind::Cargo } else { InstallKind::Standalone }
}

/// Fetch release `version`'s bk for this platform, check its signature, and unpack it.
pub fn download(version: &Version) -> Result<Vec<u8>> {
    let name = asset_name(version, TARGET);
    let base = format!("{}/download/v{version}", releases_base());
    let archive = get(&format!("{base}/{name}"), Duration::from_secs(300), 256 << 20)?;
    let sig = get(&format!("{base}/{name}.sig"), Duration::from_secs(30), 64 << 10)?;
    verify(&archive, &String::from_utf8_lossy(&sig), &release_key(), &name, version)?;
    extract_bk(&archive, &name)
}

/// Put `binary` in place of the running bk. Returns where it is.
pub fn replace_self(binary: &[u8]) -> Result<PathBuf> {
    let exe = std::env::current_exe().context("finding this bk")?;
    let dir = exe.parent().context("finding this bk's folder")?;
    let cant_write =
        |e: std::io::Error| anyhow!("can't write to {}: {e} (run it as the user who owns that folder)", dir.display());
    let mut new = tempfile::Builder::new().prefix(".bk-update-").tempfile_in(dir).map_err(cant_write)?;
    new.write_all(binary)?;
    new.as_file().sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(new.path(), std::fs::Permissions::from_mode(0o755))?;
    }
    self_replace::self_replace(new.path()).map_err(cant_write)?;
    Ok(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "bk-v1.2.3-x86_64-unknown-linux-gnu.tar.gz";

    fn testdata(name: &str) -> Vec<u8> {
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/update").join(name)).unwrap()
    }

    #[test]
    fn asset_names() {
        let v = Version::new(0, 4, 0);
        assert_eq!(asset_name(&v, "aarch64-apple-darwin"), "bk-v0.4.0-aarch64-apple-darwin.tar.gz");
        assert_eq!(asset_name(&v, "x86_64-pc-windows-msvc"), "bk-v0.4.0-x86_64-pc-windows-msvc.zip");
        assert!(Version::new(0, 10, 0) > Version::new(0, 9, 3), "compared as versions, not text");
        assert!(!TARGET.is_empty());
    }

    /// A signature made by Tauri's own signer (`tauri signer sign --app-version`), with a
    /// throwaway key whose private half was deleted.
    #[test]
    fn checks_signatures_from_tauri_signer() {
        let (data, sig) = (testdata(FIXTURE), String::from_utf8(testdata(&format!("{FIXTURE}.sig"))).unwrap());
        let key = String::from_utf8(testdata("test-key.pub")).unwrap();
        let v = Version::new(1, 2, 3);
        verify(&data, &sig, &key, FIXTURE, &v).unwrap();

        let mut tampered = data.clone();
        tampered[20] ^= 1;
        assert!(verify(&tampered, &sig, &key, FIXTURE, &v).unwrap_err().to_string().contains("isn't signed"));
        let err = |file: &str, v: &Version| verify(&data, &sig, &key, file, v).unwrap_err().to_string();
        assert!(err("bk-v1.2.3-aarch64-apple-darwin.tar.gz", &v).contains("signature is for bk-v1.2.3-x86_64"));
        assert!(err(FIXTURE, &Version::new(1, 2, 4)).contains("version 1.2.3, not 1.2.4"));
        let other = fake::Signer::new();
        assert!(verify(&data, &sig, &other.public_key(), FIXTURE, &v).is_err(), "another key");
        assert!(verify(&data, &sig, "", FIXTURE, &v).unwrap_err().to_string().contains("without"));
        assert!(verify(&data, "not base64!", &key, FIXTURE, &v).is_err());
    }

    #[test]
    fn unpacks_both_archive_kinds() {
        assert_eq!(extract_bk(&testdata(FIXTURE), FIXTURE).unwrap(), b"#!/bin/sh\necho \"bk 1.2.3\"\n");
        let zip = fake::archive("bk-v1.2.3-x86_64-pc-windows-msvc.zip", b"MZ new");
        assert_eq!(extract_bk(&zip, "x.zip").unwrap(), b"MZ new");
        let tgz = fake::archive("bk-v1.2.3-aarch64-apple-darwin.tar.gz", b"new");
        assert_eq!(extract_bk(&tgz, "x.tar.gz").unwrap(), b"new");
        let empty = fake::archive_with("x.tar.gz", "bk-v1/README.md", b"hi");
        assert!(extract_bk(&empty, "x.tar.gz").unwrap_err().to_string().contains("no bk"));
    }

    #[test]
    fn install_kinds() {
        let d = tempfile::tempdir().unwrap();
        let app = d.path().join("Clibooker.app/Contents/MacOS");
        std::fs::create_dir_all(&app).unwrap();
        for f in ["Clibooker", "bk"] {
            std::fs::write(app.join(f), "").unwrap();
        }
        assert_eq!(install_kind(&app.join("bk")), InstallKind::App);
        let alone = d.path().join("bin");
        std::fs::create_dir_all(&alone).unwrap();
        std::fs::write(alone.join("bk"), "").unwrap();
        assert_eq!(install_kind(&alone.join("bk")), InstallKind::Standalone);
        #[cfg(unix)]
        {
            // `~/.local/bin/bk` linked to the app's copy is the app's.
            std::os::unix::fs::symlink(app.join("bk"), d.path().join("linked")).unwrap();
            assert_eq!(install_kind(&d.path().join("linked")), InstallKind::App);
        }
    }

    #[test]
    fn release_key_matches_the_apps() {
        let conf =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/src-tauri/tauri.conf.json"))
                .unwrap();
        let conf: serde_json::Value = serde_json::from_str(&conf).unwrap();
        assert_eq!(conf["plugins"]["updater"]["pubkey"].as_str().unwrap_or_default(), RELEASE_PUBKEY);
    }

    /// The whole path against a local release server: latest.json, archive, signature.
    #[test]
    fn downloads_and_checks_a_release() {
        let server = fake::FakeReleases::start(&Version::new(9, 9, 9), TARGET, b"the new bk");
        // SAFETY: only this test reads these variables.
        unsafe {
            std::env::set_var("BK_RELEASES_URL", server.url());
            std::env::set_var("BK_RELEASE_KEY", server.public_key());
        }
        let v = latest(Duration::from_secs(5)).unwrap();
        assert_eq!(v, Version::new(9, 9, 9));
        assert_eq!(download(&v).unwrap(), b"the new bk");
        assert!(download(&Version::new(9, 9, 8)).unwrap_err().to_string().contains("HTTP 404"));
    }
}
