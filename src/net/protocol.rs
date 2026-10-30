//! Wire protocol. Every transfer uses its own connection and a single
//! bidirectional stream:
//!
//! ```text
//! sender                         receiver
//!   Offer      ───────────────▶
//!              ◀───────────────  Accept | Decline
//!   <size raw bytes> ──────────▶
//!   Done{hash} ───────────────▶
//!              ◀───────────────  Result{ok, message}
//! ```
//!
//! Control messages are postcard encoded and prefixed with a little-endian
//! u32 length.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::i18n::tr;

pub const ALPN: &[u8] = b"dodavo/1";
const MAX_MSG_LEN: usize = 64 * 1024;

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Msg {
    Offer(Offer),
    Accept,
    Decline,
    Done { hash: [u8; 32] },
    Result { ok: bool, message: String },
}

pub async fn write_msg<W: AsyncWrite + Unpin>(w: &mut W, msg: &Msg) -> anyhow::Result<()> {
    let bytes = postcard::to_stdvec(msg)?;
    w.write_u32_le(bytes.len() as u32).await?;
    w.write_all(&bytes).await?;
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
            }),
            Msg::Accept,
            Msg::Decline,
            Msg::Done { hash: [9; 32] },
            Msg::Result {
                ok: false,
                message: "nope".into(),
            },
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
}
