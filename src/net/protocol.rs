//! Wire protocol (`dodavo/2`). Every connection carries a single
//! bidirectional stream. A transfer starts on one connection and may be
//! continued on later ones (after a lost connection or a restart):
//!
//! ```text
//! sender                              receiver
//!   Offer{…, token}  ─────────────▶
//!                    ◀─────────────  Accept | Decline
//!   Data{len} + len raw bytes ────▶   (repeated; Paused / Resumed may be
//!   Paused | Resumed ─────────────▶    sent by either side at any time)
//!                    ◀─────────────  Paused | Resumed
//!   Done{hash}       ─────────────▶
//!                    ◀─────────────  Result{ok, message}
//!
//! later connection (sender dials again):
//!   Continue{token}  ─────────────▶
//!                    ◀─────────────  ContinueFrom{have} | Gone
//!   … Data from byte `have` on, as above …
//!
//! receiver asking whether the sender still has the transfer:
//!   Query{token}     ◀─────────────
//!   Alive | Gone     ─────────────▶
//! ```
//!
//! Control messages are postcard encoded and prefixed with a little-endian
//! u32 length. The bytes of a `Data` frame follow its header unencoded.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::i18n::tr;

pub const ALPN: &[u8] = b"dodavo/2";
const MAX_MSG_LEN: usize = 64 * 1024;
/// The largest `Data` frame.
pub const CHUNK: usize = 256 * 1024;

/// Identifies a transfer on both sides, across connections and restarts.
pub type Token = [u8; 16];

pub fn new_token() -> Token {
    rand::random()
}

/// The token as a file name friendly hex string.
pub fn token_hex(token: &Token) -> String {
    token.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    File,
    /// A folder, transferred as a zip archive and extracted by the receiver.
    Folder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub kind: Kind,
    /// File or folder name (without any path).
    pub name: String,
    /// Number of bytes that will be streamed.
    pub size: u64,
    /// For folders: number of files and their total uncompressed size.
    pub file_count: u64,
    pub content_size: u64,
    /// The display name the sender chose for themselves.
    pub sender_name: String,
    pub token: Token,
    /// When the file or folder was last modified and when it was created, as
    /// Windows file times (100 ns since 1601, UTC). The receiver gives them to
    /// what it saves; the times inside a folder travel in its zip archive.
    /// (`default` keeps transfer records of earlier versions readable.)
    #[serde(default)]
    pub modified: Option<u64>,
    #[serde(default)]
    pub created: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Msg {
    // Sender to receiver.
    Offer(Offer),
    /// First message on a new connection for a transfer that was interrupted.
    Continue { token: Token },
    /// A frame of `len` raw bytes follows.
    Data { len: u32 },
    Done { hash: [u8; 32] },
    /// Answer to `Query`: the transfer is still pending here.
    Alive,
    // Receiver to sender.
    Accept,
    Decline,
    /// Answer to `Continue`: the receiver already has the first `have` bytes.
    ContinueFrom { have: u64 },
    /// First message on a connection the receiver opens to ask whether the
    /// sender still has the transfer.
    Query { token: Token },
    Result { ok: bool, message: String },
    // Either direction.
    /// Answer to `Continue` or `Query`: this side no longer has the transfer,
    /// because it was cancelled, failed or (for `completed`) finished.
    Gone { completed: bool },
    /// The user on this side paused the transfer.
    Paused,
    /// The user on this side resumed the transfer.
    Resumed,
}

/// A message as it goes onto the stream: length prefix and body.
pub fn encode(msg: &Msg) -> anyhow::Result<Vec<u8>> {
    let body = postcard::to_stdvec(msg)?;
    let mut bytes = Vec::with_capacity(body.len() + 4);
    bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&body);
    Ok(bytes)
}

pub async fn write_msg<W: AsyncWrite + Unpin>(w: &mut W, msg: &Msg) -> anyhow::Result<()> {
    w.write_all(&encode(msg)?).await?;
    Ok(())
}

pub async fn read_msg<R: AsyncRead + Unpin>(r: &mut R) -> anyhow::Result<Msg> {
    let len = r.read_u32_le().await? as usize;
    anyhow::ensure!(len <= MAX_MSG_LEN, tr!(error_message_too_large));
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(postcard::from_bytes(&buf)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let msgs = vec![
            Msg::Offer(Offer {
                kind: Kind::Folder,
                name: "Photos 2026".into(),
                size: 123_456_789_012,
                file_count: 42,
                content_size: 999,
                sender_name: "Ana".into(),
                token: [7; 16],
                modified: Some(133_000_000_000_000_000),
                created: None,
            }),
            Msg::Continue { token: [1; 16] },
            Msg::Data { len: 262_144 },
            Msg::Done { hash: [9; 32] },
            Msg::Alive,
            Msg::Accept,
            Msg::Decline,
            Msg::ContinueFrom { have: 1 << 40 },
            Msg::Query { token: [2; 16] },
            Msg::Result {
                ok: false,
                message: "nope".into(),
            },
            Msg::Gone { completed: true },
            Msg::Paused,
            Msg::Resumed,
        ];
        for m in &msgs {
            write_msg(&mut a, m).await.unwrap();
        }
        for m in &msgs {
            assert_eq!(&read_msg(&mut b).await.unwrap(), m);
        }
    }

    #[tokio::test]
    async fn rejects_oversized() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_u32_le(10_000_000).await.unwrap();
        assert!(read_msg(&mut b).await.is_err());
    }

    #[test]
    fn tokens() {
        assert_ne!(new_token(), new_token());
        assert_eq!(token_hex(&[0, 255, 16, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10]).len(), 32);
        assert!(token_hex(&[0xab; 16]).starts_with("abab"));
    }
}
