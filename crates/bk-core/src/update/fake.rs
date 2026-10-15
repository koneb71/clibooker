//! A local stand-in for GitHub releases, for tests: `latest.json`, one bk archive and its
//! signature, made with a key of its own. Point `BK_RELEASES_URL` at `url()` and (debug builds)
//! `BK_RELEASE_KEY` at `public_key()`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Cursor, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use semver::Version;

use super::asset_name;

/// Signs like `tauri signer sign --app-version`.
pub struct Signer {
    keys: minisign::KeyPair,
}

impl Default for Signer {
    fn default() -> Self {
        Signer::new()
    }
}

impl Signer {
    pub fn new() -> Signer {
        Signer { keys: minisign::KeyPair::generate_unencrypted_keypair().expect("a key pair") }
    }

    /// As Tauri writes it: base64 of the minisign public key file.
    pub fn public_key(&self) -> String {
        STANDARD.encode(self.keys.pk.to_box().expect("public key").into_string())
    }

    /// A `.sig` file's content for `data` published as `file` in `version`.
    pub fn sign(&self, data: &[u8], file: &str, version: &Version) -> String {
        let comment = format!("timestamp:0\tfile:{file}\tversion:{version}");
        let sig = minisign::sign(Some(&self.keys.pk), &self.keys.sk, Cursor::new(data), Some(&comment), None)
            .expect("signing");
        STANDARD.encode(sig.into_string())
    }
}

/// A release archive named `name` holding `inner` (a path inside it) with `content`.
pub fn archive_with(name: &str, inner: &str, content: &[u8]) -> Vec<u8> {
    if name.ends_with(".zip") {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file(inner, zip::write::SimpleFileOptions::default()).expect("zip entry");
        zip.write_all(content).expect("zip write");
        return zip.finish().expect("zip").into_inner();
    }
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    tar.append_data(&mut header, inner, content).expect("tar entry");
    tar.into_inner().expect("tar").finish().expect("gzip")
}

/// A release archive as the release workflow makes it: `bk-vX-target/bk` (`bk.exe` in a zip).
pub fn archive(name: &str, bk: &[u8]) -> Vec<u8> {
    let dir = name.trim_end_matches(".zip").trim_end_matches(".tar.gz");
    let exe = if name.ends_with(".zip") { "bk.exe" } else { "bk" };
    archive_with(name, &format!("{dir}/{exe}"), bk)
}

pub struct FakeReleases {
    port: u16,
    signer: Signer,
}

impl FakeReleases {
    /// Release `version`, whose bk for `target` is `bk`.
    pub fn start(version: &Version, target: &str, bk: &[u8]) -> FakeReleases {
        let signer = Signer::new();
        let name = asset_name(version, target);
        let data = archive(&name, bk);
        let mut files = BTreeMap::new();
        files.insert(
            "/latest/download/latest.json".to_string(),
            serde_json::json!({ "version": version.to_string(), "platforms": {} }).to_string().into_bytes(),
        );
        files.insert(format!("/download/v{version}/{name}.sig"), signer.sign(&data, &name, version).into_bytes());
        files.insert(format!("/download/v{version}/{name}"), data);
        let files = Arc::new(files);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake releases");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let files = files.clone();
                std::thread::spawn(move || {
                    let _ = serve(stream, &files);
                });
            }
        });
        FakeReleases { port, signer }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn public_key(&self) -> String {
        self.signer.public_key()
    }
}

fn serve(mut stream: TcpStream, files: &BTreeMap<String, Vec<u8>>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
    }
    let (status, body) = match files.get(&path) {
        Some(b) => ("200 OK", b.as_slice()),
        None => ("404 Not Found", b"Not Found".as_slice()),
    };
    write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())?;
    stream.write_all(body)
}
