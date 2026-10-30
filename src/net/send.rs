//! Sending side of a transfer.

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, bail};
use iroh::{Endpoint, EndpointId, endpoint::Connection};
use tokio::io::AsyncReadExt;

use super::{
    CLOSE_CANCELLED, CLOSE_FAILED, CLOSE_OK, CancelToken, Event, Outcome, Phase, Shared, Throttle,
    protocol::{self, Kind, Msg, Offer},
    receive::{closed_by_peer_cancel, closed_by_peer_failure},
    spawn_path_watch,
};
use crate::{archive, i18n::tr};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(40);
const CHUNK: usize = 256 * 1024;

/// Deletes the temporary zip of a folder transfer when dropped.
struct TempFile(Option<PathBuf>);

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    shared: &Arc<Shared>,
    endpoint: &Endpoint,
    id: u64,
    to: EndpointId,
    path: PathBuf,
    kind: Kind,
    my_name: String,
    cancel: CancelToken,
) -> Outcome {
    let conn_slot = std::sync::Mutex::new(None::<Connection>);
    let result = tokio::select! {
        r = transfer(shared, endpoint, id, to, path, kind, my_name, &cancel, &conn_slot) => r,
        _ = cancel.cancelled() => {
            if let Some(conn) = conn_slot.lock().unwrap().take() {
                conn.close(CLOSE_CANCELLED.into(), b"cancelled");
            }
            return Outcome::Cancelled;
        }
    };
    let conn = conn_slot.lock().unwrap().take();
    match result {
        Ok(outcome) => {
            if let Some(conn) = conn {
                conn.close(CLOSE_OK.into(), b"done");
            }
            outcome
        }
        Err(e) => {
            if cancel.flag().load(std::sync::atomic::Ordering::Relaxed) {
                return Outcome::Cancelled;
            }
            if let Some(conn) = &conn {
                if closed_by_peer_cancel(conn) {
                    return Outcome::CancelledByPeer;
                }
                if closed_by_peer_failure(conn) {
                    return Outcome::Failed(tr!(error_recipient_failed).into());
                }
                // Our own failure: let the receiver report it as such.
                conn.close(CLOSE_FAILED.into(), b"failed");
            }
            match e.downcast::<Unreachable>() {
                Ok(u) => Outcome::Unreachable(u.0),
                Err(e) => Outcome::Failed(format!("{e:#}")),
            }
        }
    }
}

#[derive(Debug)]
struct Unreachable(String);

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unreachable {}

#[allow(clippy::too_many_arguments)]
async fn transfer(
    shared: &Arc<Shared>,
    endpoint: &Endpoint,
    id: u64,
    to: EndpointId,
    path: PathBuf,
    kind: Kind,
    my_name: String,
    cancel: &CancelToken,
    conn_slot: &std::sync::Mutex<Option<Connection>>,
) -> anyhow::Result<Outcome> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| tr!(untitled).into());

    // 1. Prepare the data (zip folders). `size` is what gets streamed.
    let mut temp = TempFile(None);
    let (data_path, file_count, content_size, size) = match kind {
        Kind::File => {
            let size = tokio::fs::metadata(&path)
                .await
                .with_context(|| tr!(error_cannot_read, path = path.display()))?
                .len();
            (path.clone(), 1, size, size)
        }
        Kind::Folder => {
            shared.emit(Event::Phase {
                id,
                phase: Phase::Preparing,
            });
            let zip_path =
                archive::temp_dir()?.join(format!("send-{}-{id}.zip", std::process::id()));
            temp.0 = Some(zip_path.clone());
            let shared2 = shared.clone();
            let token = cancel.clone();
            let src = path.clone();
            let zip_dst = zip_path.clone();
            let (file_count, content_size) = tokio::task::spawn_blocking(move || {
                let mut throttle = Throttle::new();
                archive::zip_folder(&src, &zip_dst, token.flag(), &mut |done, total| {
                    if throttle.ready() || done == total {
                        shared2.emit(Event::Progress { id, done, total });
                    }
                })
            })
            .await??;
            let size = tokio::fs::metadata(&zip_path).await?.len();
            (zip_path, file_count, content_size, size)
        }
    };

    // 2. Connect.
    shared.emit(Event::Phase {
        id,
        phase: Phase::Connecting,
    });
    let conn =
        match tokio::time::timeout(CONNECT_TIMEOUT, endpoint.connect(to, protocol::ALPN)).await {
            Ok(Ok(conn)) => conn,
            Ok(Err(e)) => return Err(Unreachable(e.to_string()).into()),
            Err(_) => return Err(Unreachable(tr!(error_timeout).into()).into()),
        };
    *conn_slot.lock().unwrap() = Some(conn.clone());
    let watch = spawn_path_watch(shared.clone(), id, conn.clone());
    let _abort = AbortOnDrop(watch);

    // 3. Offer and wait for the answer.
    let (mut send, mut recv) = conn.open_bi().await?;
    let offer = Offer {
        kind,
        name,
        size,
        file_count,
        content_size,
        sender_name: my_name,
    };
    protocol::write_msg(&mut send, &Msg::Offer(offer)).await?;
    shared.emit(Event::Phase {
        id,
        phase: Phase::WaitingForAccept,
    });
    match protocol::read_msg(&mut recv).await? {
        Msg::Accept => {}
        Msg::Decline => return Ok(Outcome::Declined),
        other => bail!(tr!(error_unexpected_answer, answer = format!("{other:?}"))),
    }

    // 4. Stream the data.
    shared.emit(Event::Phase {
        id,
        phase: Phase::Transferring,
    });
    shared.emit(Event::Progress {
        id,
        done: 0,
        total: size,
    });
    let mut file = tokio::fs::File::open(&data_path).await?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    let mut throttle = Throttle::new();
    while done < size {
        let want = CHUNK.min((size - done) as usize);
        let n = file.read(&mut buf[..want]).await?;
        if n == 0 {
            bail!(tr!(error_file_shorter));
        }
        hasher.update(&buf[..n]);
        send.write_all(&buf[..n]).await?;
        done += n as u64;
        if throttle.ready() || done == size {
            shared.emit(Event::Progress {
                id,
                done,
                total: size,
            });
        }
    }
    protocol::write_msg(
        &mut send,
        &Msg::Done {
            hash: *hasher.finalize().as_bytes(),
        },
    )
    .await?;
    send.finish()?;

    // 5. Wait for confirmation.
    match protocol::read_msg(&mut recv).await? {
        Msg::Result { ok: true, .. } => Ok(Outcome::Completed(None)),
        Msg::Result { ok: false, message } => {
            bail!(tr!(error_peer_reported, message = message))
        }
        other => bail!(tr!(error_unexpected_answer, answer = format!("{other:?}"))),
    }
}

pub(crate) struct AbortOnDrop(pub tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
