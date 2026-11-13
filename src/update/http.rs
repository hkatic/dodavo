//! Fetching the manifest and downloading the installer.
//!
//! The TLS roots are the bundled Mozilla list (webpki-roots), like iroh's own
//! connections, so the update check works on a Windows 7 whose certificate
//! store was never updated.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use reqwest::Client;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use super::{Error, manifest::MAX_MANIFEST_BYTES};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const READ_TIMEOUT: Duration = Duration::from_secs(60);

pub fn client() -> Result<Client, Error> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| Error::Network)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Client::builder()
        .tls_backend_preconfigured(tls)
        .user_agent(concat!("Dodavo/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()
        .map_err(|_| Error::Network)
}

/// What a request that failed is to the user: a server that did not answer
/// in time, or one that could not be reached at all. The library's own
/// words for it ("error sending request", "dns error") are in English and
/// say no more to who reads them.
fn network(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        Error::Timeout
    } else {
        Error::Network
    }
}

fn io(e: std::io::Error) -> Error {
    Error::io(&e)
}

/// Reads the manifest (at most `MAX_MANIFEST_BYTES`).
pub async fn fetch_manifest(client: &Client, url: &str) -> Result<Vec<u8>, Error> {
    let mut response = client
        .get(url)
        .header("Cache-Control", "no-cache")
        .send()
        .await
        .map_err(network)?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Http(status.as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|len| len > MAX_MANIFEST_BYTES as u64)
    {
        return Err(Error::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        if body.len() + chunk.len() > MAX_MANIFEST_BYTES {
            return Err(Error::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Downloads `url` to `dest`, which must be exactly `expected_size` bytes.
/// Returns the SHA-256 of what was written. Nothing is left behind on
/// failure or cancellation; `progress` is called with (done, total).
pub async fn download(
    client: &Client,
    url: &str,
    dest: &Path,
    expected_size: u64,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &CancellationToken,
) -> Result<[u8; 32], Error> {
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let result = download_to(client, url, &part, expected_size, progress, cancel).await;
    match result {
        Ok(hash) => {
            let _ = tokio::fs::remove_file(dest).await;
            tokio::fs::rename(&part, dest).await.map_err(io)?;
            Ok(hash)
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            Err(e)
        }
    }
}

async fn download_to(
    client: &Client,
    url: &str,
    part: &Path,
    expected_size: u64,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &CancellationToken,
) -> Result<[u8; 32], Error> {
    let mut file = tokio::fs::File::create(part).await.map_err(io)?;
    let mut response = client.get(url).send().await.map_err(network)?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Http(status.as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|len| len != expected_size)
    {
        return Err(Error::Size);
    }
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    progress(0, expected_size);
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Err(Error::Cancelled),
            chunk = response.chunk() => chunk.map_err(network)?,
        };
        let Some(chunk) = chunk else {
            break;
        };
        done += chunk.len() as u64;
        if done > expected_size {
            return Err(Error::Size);
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(io)?;
        progress(done, expected_size);
    }
    if done != expected_size {
        return Err(Error::Size);
    }
    file.flush().await.map_err(io)?;
    file.sync_all().await.map_err(io)?;
    drop(file);
    Ok(hasher.finalize().into())
}
