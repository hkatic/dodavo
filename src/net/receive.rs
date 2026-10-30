//! Receiving side of a transfer.

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::bail;
use iroh::endpoint::{Connection, ConnectionError};
use tokio::{
    io::{AsyncWriteExt, BufWriter},
    sync::oneshot,
};

use super::{
    CLOSE_CANCELLED, CLOSE_FAILED, CancelToken, Event, Outcome, Phase, Shared, Throttle,
    protocol::{self, Kind, Msg, Offer},
    send::AbortOnDrop,
    spawn_path_watch,
};
use crate::{archive, i18n::tr};

const CHUNK: usize = 256 * 1024;

fn closed_by_peer_with(conn: &Connection, code: u32) -> bool {
    matches!(
        conn.close_reason(),
        Some(ConnectionError::ApplicationClosed(close)) if close.error_code == code.into()
    )
}

/// The peer closed the connection because its user cancelled.
pub(crate) fn closed_by_peer_cancel(conn: &Connection) -> bool {
    closed_by_peer_with(conn, CLOSE_CANCELLED)
}

/// The peer closed the connection because the transfer failed on its side.
pub(crate) fn closed_by_peer_failure(conn: &Connection) -> bool {
    closed_by_peer_with(conn, CLOSE_FAILED)
}

/// A failure that was already reported to the sender over the stream. The
/// connection must stay open until the sender has read the report, so it is
/// not closed with an error code like other failures.
#[derive(Debug)]
struct Reported(String);

impl std::fmt::Display for Reported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Reported {}

/// Removes a partially written file or folder unless disarmed.
struct Cleanup(Option<PathBuf>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            if p.is_dir() {
                let _ = std::fs::remove_dir_all(&p);
            } else {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

pub(super) async fn handle(shared: Arc<Shared>, conn: Connection) {
    let from = conn.remote_id();
    let Ok((mut send, mut recv)) = conn.accept_bi().await else {
        return;
    };
    let offer =
        match tokio::time::timeout(Duration::from_secs(30), protocol::read_msg(&mut recv)).await {
            Ok(Ok(Msg::Offer(offer))) => offer,
            _ => return,
        };

    let id = super::next_transfer_id();
    let cancel = shared.register(id);
    let (tx, rx) = oneshot::channel();
    shared.pending.lock().unwrap().insert(id, tx);
    shared.emit(Event::Offer {
        id,
        from,
        offer: offer.clone(),
    });

    // Wait for the user's decision (or for the sender to give up).
    let decision = tokio::select! {
        answer = rx => answer.ok().flatten(),
        _ = conn.closed() => {
            shared.finish(id, Outcome::CancelledByPeer);
            return;
        }
        _ = cancel.cancelled() => None,
    };
    let Some(dest_dir) = decision else {
        let _ = protocol::write_msg(&mut send, &Msg::Decline).await;
        let _ = send.finish();
        let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
        shared.finish(id, Outcome::Declined);
        return;
    };

    let watch = AbortOnDrop(spawn_path_watch(shared.clone(), id, conn.clone()));
    let result = tokio::select! {
        r = receive(&shared, id, &offer, dest_dir, &mut send, &mut recv, &cancel, &conn) => r,
        _ = cancel.cancelled() => {
            conn.close(CLOSE_CANCELLED.into(), b"cancelled");
            shared.finish(id, Outcome::Cancelled);
            return;
        }
    };
    drop(watch);
    let outcome = match result {
        Ok(path) => Outcome::Completed(Some(path)),
        Err(_) if closed_by_peer_cancel(&conn) => Outcome::CancelledByPeer,
        Err(_) if closed_by_peer_failure(&conn) => {
            Outcome::Failed(tr!(error_sender_failed).into())
        }
        Err(_) if cancel.flag().load(std::sync::atomic::Ordering::Relaxed) => Outcome::Cancelled,
        Err(e) => {
            // A failure the sender does not know about yet: tell it with the
            // close code, so it reports a failure and not a cancellation. A
            // reported failure keeps the connection open until the sender has
            // read the report (closing now would discard it).
            if !e.is::<Reported>() {
                conn.close(CLOSE_FAILED.into(), b"failed");
            }
            Outcome::Failed(format!("{e:#}"))
        }
    };
    // Give the sender a moment to read our final message and close the connection.
    let _ = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await;
    shared.finish(id, outcome);
}

#[allow(clippy::too_many_arguments)]
async fn receive(
    shared: &Arc<Shared>,
    id: u64,
    offer: &Offer,
    dest_dir: PathBuf,
    send: &mut iroh::endpoint::SendStream,
    recv: &mut iroh::endpoint::RecvStream,
    cancel: &CancelToken,
    _conn: &Connection,
) -> anyhow::Result<PathBuf> {
    let name = archive::sanitize_file_name(&offer.name);
    tokio::fs::create_dir_all(&dest_dir).await?;

    // Folders arrive as a zip in the temp folder; files go straight to the destination folder.
    let part_path = match offer.kind {
        Kind::File => archive::unique_path(&dest_dir, &format!("{name}.part"), false),
        Kind::Folder => archive::temp_dir()?.join(format!("recv-{}-{id}.zip", std::process::id())),
    };
    let mut part = Cleanup(Some(part_path.clone()));

    protocol::write_msg(send, &Msg::Accept).await?;
    shared.emit(Event::Phase {
        id,
        phase: Phase::Transferring,
    });
    shared.emit(Event::Progress {
        id,
        done: 0,
        total: offer.size,
    });

    let mut out = BufWriter::with_capacity(1024 * 1024, tokio::fs::File::create(&part_path).await?);
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    let mut throttle = Throttle::new();
    while done < offer.size {
        let want = CHUNK.min((offer.size - done) as usize);
        let Some(n) = recv.read(&mut buf[..want]).await? else {
            bail!(tr!(error_sender_stopped));
        };
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).await?;
        done += n as u64;
        if throttle.ready() || done == offer.size {
            shared.emit(Event::Progress {
                id,
                done,
                total: offer.size,
            });
        }
    }
    out.flush().await?;
    drop(out);

    let Msg::Done { hash } = protocol::read_msg(recv).await? else {
        bail!(tr!(error_protocol_end));
    };
    if hash != *hasher.finalize().as_bytes() {
        let _ = protocol::write_msg(
            send,
            &Msg::Result {
                ok: false,
                message: tr!(error_checksum).into(),
            },
        )
        .await;
        let _ = send.finish();
        return Err(Reported(tr!(error_damaged).into()).into());
    }
    protocol::write_msg(
        send,
        &Msg::Result {
            ok: true,
            message: String::new(),
        },
    )
    .await?;
    let _ = send.finish();

    match offer.kind {
        Kind::File => {
            let final_path = archive::unique_path(&dest_dir, &name, false);
            tokio::fs::rename(&part_path, &final_path).await?;
            part.0 = None;
            Ok(final_path)
        }
        Kind::Folder => {
            shared.emit(Event::Phase {
                id,
                phase: Phase::Unpacking,
            });
            let final_path = archive::unique_path(&dest_dir, &name, true);
            let shared2 = shared.clone();
            let token = cancel.clone();
            let target = final_path.clone();
            tokio::task::spawn_blocking(move || {
                let mut partial = Cleanup(Some(target.clone()));
                let mut throttle = Throttle::new();
                archive::unzip(&part_path, &target, token.flag(), &mut |done, total| {
                    if throttle.ready() || done == total {
                        shared2.emit(Event::Progress { id, done, total });
                    }
                })?;
                partial.0 = None;
                anyhow::Ok(())
            })
            .await??;
            Ok(final_path)
        }
    }
}
