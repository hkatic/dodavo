//! Wire protocol (`dodavo/3`). Every connection carries a single
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
//!                    ◀─────────────  PartInUse{in_use}     (at any time)
//!   Done{hash}       ─────────────▶
//!                    ◀─────────────  Verified{unpacking}   (if the hash matches)
//!                    ◀─────────────  Result{ok, message}   (once the item is saved)
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

pub const ALPN: &[u8] = b"dodavo/3";
/// The protocol of the chat (see `net::chat`), which an endpoint takes
/// beside `ALPN` only where its user interface can show messages. A name of
/// its own, so that it can change without touching the transfers.
pub const CHAT_ALPN: &[u8] = b"dodavo-chat/1";
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
    /// For folders: the size of the largest file in it, which tells whether
    /// a file system that limits the size of a file (FAT32) can hold the
    /// folder unpacked. For files: their size. (`default`: records of
    /// earlier versions.)
    #[serde(default)]
    pub largest_file: u64,
    /// The display name the sender chose for themselves.
    pub sender_name: String,
    pub token: Token,
    /// When the file or folder was last modified and when it was created, as
    /// Windows file times (100 ns since 1601, UTC). The receiver gives them to
    /// what it saves; the times inside a folder travel in its zip archive.
    /// (`default` keeps transfer records of earlier versions readable. It
    /// does nothing on the wire: postcard reads a struct as a fixed sequence
    /// of fields, so adding these two needed a new ALPN.)
    #[serde(default)]
    pub modified: Option<u64>,
    #[serde(default)]
    pub created: Option<u64>,
}

/// How a transfer ended on a side that no longer has it (see `Msg::Gone`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ended {
    /// The receiver has the item.
    Completed,
    /// The user on that side cancelled it (or the other side's cancel ended
    /// it there).
    Cancelled,
    /// It failed there, or that side knows nothing of it (any more), so it
    /// cannot go on.
    Failed,
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
    /// After `Done`: all of the data arrived intact. The receiver now puts
    /// the item in place (unpacking the folder first, if `unpacking`) and
    /// then sends `Result`. From now on only the receiver can end the
    /// transfer without the item; a cancel on the sending side comes too
    /// late.
    Verified { unpacking: bool },
    /// The last message: whether the item was saved, or why it was not.
    Result { ok: bool, message: String },
    /// Another program keeps the partly received file out (`in_use`), so
    /// the receiver waits for it, or it has let go and the transfer goes
    /// on. While the connection opens the file, and in the last step.
    PartInUse { in_use: bool },
    // Either direction.
    /// Answer to `Continue` or `Query`: this side no longer has the transfer,
    /// and how it ended here.
    Gone { ended: Ended },
    /// The user on this side paused the transfer.
    Paused,
    /// The user on this side resumed the transfer.
    Resumed,
}

impl Msg {
    /// The message's name without what it carries, for the log (see
    /// `logging`): an `Offer` holds names, a `Result` the peer's own text.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Msg::Offer(_) => "Offer",
            Msg::Continue { .. } => "Continue",
            Msg::Data { .. } => "Data",
            Msg::Done { .. } => "Done",
            Msg::Alive => "Alive",
            Msg::Accept => "Accept",
            Msg::Decline => "Decline",
            Msg::ContinueFrom { .. } => "ContinueFrom",
            Msg::Query { .. } => "Query",
            Msg::Verified { .. } => "Verified",
            Msg::Result { ok: true, .. } => "Result(ok)",
            Msg::Result { ok: false, .. } => "Result(failed)",
            Msg::PartInUse { .. } => "PartInUse",
            Msg::Gone { ended } => match ended {
                Ended::Completed => "Gone(completed)",
                Ended::Cancelled => "Gone(cancelled)",
                Ended::Failed => "Gone(failed)",
            },
            Msg::Paused => "Paused",
            Msg::Resumed => "Resumed",
        }
    }
}

/// What the two sides of a chat connection say to each other
/// (`CHAT_ALPN`), on its one bidirectional stream, framed as [`Msg`] is:
///
/// ```text
/// who dials                           who is dialed
///   Hello{name}      ─────────────▶
///                    ◀─────────────  Welcome{name} | Refused
///   Text{id, text}   ◀────────────▶  (either side, at any time)
///   Received{id}     ◀────────────▶  (the answer to each `Text`)
///   Typing{on}       ◀────────────▶
/// ```
///
/// A change to it needs a new ALPN, as a change to [`Msg`] does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatMsg {
    /// The first message, with the display name the dialer chose for
    /// themselves.
    Hello { name: String },
    /// The answer to `Hello`, with this side's name.
    Welcome { name: String },
    /// This side takes no messages from the other: the answer to `Hello`,
    /// or to a `Text` once that has changed.
    Refused,
    /// A message, with a number that its `Received` repeats.
    Text { id: u64, text: String },
    /// The `Text` with this number is with the user.
    Received { id: u64 },
    /// The user on this side is writing a message, or has stopped.
    Typing { on: bool },
}

impl ChatMsg {
    /// The message's name without its names and text, for the log.
    pub fn kind_name(&self) -> &'static str {
        match self {
            ChatMsg::Hello { .. } => "Hello",
            ChatMsg::Welcome { .. } => "Welcome",
            ChatMsg::Refused => "Refused",
            ChatMsg::Text { .. } => "Text",
            ChatMsg::Received { .. } => "Received",
            ChatMsg::Typing { .. } => "Typing",
        }
    }
}

/// The longest text of a chat message, in bytes.
pub const MAX_CHAT_TEXT: usize = 16 * 1024;

/// A message as it goes onto the stream: length prefix and body.
pub fn encode<T: Serialize>(msg: &T) -> anyhow::Result<Vec<u8>> {
    let body = postcard::to_stdvec(msg)?;
    let mut bytes = Vec::with_capacity(body.len() + 4);
    bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&body);
    Ok(bytes)
}

pub async fn write_msg<W: AsyncWrite + Unpin>(w: &mut W, msg: &Msg) -> anyhow::Result<()> {
    write_as(w, msg).await
}

pub async fn read_msg<R: AsyncRead + Unpin>(r: &mut R) -> anyhow::Result<Msg> {
    read_as(r).await
}

/// Writes any of the protocols' messages (see [`encode`]).
pub async fn write_as<W: AsyncWrite + Unpin, T: Serialize>(
    w: &mut W,
    msg: &T,
) -> anyhow::Result<()> {
    w.write_all(&encode(msg)?).await?;
    Ok(())
}

/// Reads any of the protocols' messages.
pub async fn read_as<R: AsyncRead + Unpin, T: serde::de::DeserializeOwned>(
    r: &mut R,
) -> anyhow::Result<T> {
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
                largest_file: 999,
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
            Msg::Verified { unpacking: true },
            Msg::Result {
                ok: false,
                message: "nope".into(),
            },
            Msg::Gone {
                ended: Ended::Completed,
            },
            Msg::Gone {
                ended: Ended::Failed,
            },
            Msg::PartInUse { in_use: true },
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
    async fn chat_roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let msgs = vec![
            ChatMsg::Hello { name: "Ana".into() },
            ChatMsg::Welcome { name: "Željko".into() },
            ChatMsg::Refused,
            ChatMsg::Text {
                id: u64::MAX,
                text: "Stiže ti mapa sa slikama.".into(),
            },
            ChatMsg::Received { id: 7 },
            ChatMsg::Typing { on: true },
        ];
        for m in &msgs {
            write_as(&mut a, m).await.unwrap();
        }
        for m in &msgs {
            assert_eq!(&read_as::<_, ChatMsg>(&mut b).await.unwrap(), m);
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
