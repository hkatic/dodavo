//! Networking: an iroh endpoint running on a tokio runtime in a background
//! thread. The UI talks to it with [`Command`]s and receives [`Event`]s.

pub mod protocol;
mod receive;
mod send;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use iroh::{Endpoint, EndpointId, SecretKey, Watcher, endpoint::Connection, endpoint::presets};
use iroh_mdns_address_lookup::MdnsAddressLookup;
use tokio::sync::{mpsc, oneshot, watch};

pub use protocol::{Kind, Offer};

pub enum Command {
    Send {
        id: u64,
        to: EndpointId,
        path: PathBuf,
        kind: Kind,
        my_name: String,
    },
    /// Answer to an incoming offer: `Some(folder)` accepts and saves into that folder.
    Respond {
        id: u64,
        accept: Option<PathBuf>,
    },
    Cancel {
        id: u64,
    },
    /// Cancels every transfer, closes the endpoint and then signals `done`.
    Shutdown {
        done: std::sync::mpsc::Sender<()>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Preparing,
    Connecting,
    WaitingForAccept,
    Transferring,
    Unpacking,
}

#[derive(Debug)]
pub enum Outcome {
    /// Finished successfully; for received items, the final path.
    Completed(Option<PathBuf>),
    Declined,
    Cancelled,
    /// The other side cancelled.
    CancelledByPeer,
    Unreachable(String),
    Failed(String),
}

#[derive(Debug)]
pub enum Event {
    BindFailed(String),
    Online(bool),
    Offer {
        id: u64,
        from: EndpointId,
        offer: Offer,
    },
    Phase {
        id: u64,
        phase: Phase,
    },
    Progress {
        id: u64,
        done: u64,
        total: u64,
    },
    Path {
        id: u64,
        direct: bool,
    },
    Finished {
        id: u64,
        outcome: Outcome,
    },
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Allocates an id for a new transfer (used for both directions).
pub fn next_transfer_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

type Sink = Box<dyn Fn(Event) + Send + Sync>;

/// Cancellation signal usable from async code and from blocking threads.
#[derive(Clone)]
pub(crate) struct CancelToken {
    flag: Arc<AtomicBool>,
    tx: Arc<watch::Sender<bool>>,
}

impl CancelToken {
    fn new() -> Self {
        CancelToken {
            flag: Arc::new(AtomicBool::new(false)),
            tx: Arc::new(watch::channel(false).0),
        }
    }
    fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
        self.tx.send_replace(true);
    }
    pub(crate) fn flag(&self) -> &AtomicBool {
        &self.flag
    }
    pub(crate) async fn cancelled(&self) {
        let mut rx = self.tx.subscribe();
        let _ = rx.wait_for(|v| *v).await;
    }
}

pub(crate) struct Shared {
    sink: Sink,
    pending: Mutex<HashMap<u64, oneshot::Sender<Option<PathBuf>>>>,
    cancels: Mutex<HashMap<u64, CancelToken>>,
}

impl Shared {
    pub(crate) fn emit(&self, event: Event) {
        (self.sink)(event)
    }

    fn register(&self, id: u64) -> CancelToken {
        let token = CancelToken::new();
        self.cancels.lock().unwrap().insert(id, token.clone());
        token
    }

    fn finish(&self, id: u64, outcome: Outcome) {
        self.cancels.lock().unwrap().remove(&id);
        self.pending.lock().unwrap().remove(&id);
        self.emit(Event::Finished { id, outcome });
    }
}

/// Limits progress events to a few per second.
pub(crate) struct Throttle(Option<Instant>);

impl Throttle {
    pub(crate) fn new() -> Self {
        Throttle(None)
    }
    pub(crate) fn ready(&mut self) -> bool {
        let now = Instant::now();
        match self.0 {
            Some(last) if now.duration_since(last) < Duration::from_millis(100) => false,
            _ => {
                self.0 = Some(now);
                true
            }
        }
    }
}

/// Reports whether the connection currently uses a direct path or a relay.
pub(crate) fn spawn_path_watch(
    shared: Arc<Shared>,
    id: u64,
    conn: Connection,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last = None;
        loop {
            let paths = conn.paths();
            let selected = paths.iter().find(|p| p.is_selected()).map(|p| p.is_ip());
            if let Some(direct) = selected
                && selected != last
            {
                last = selected;
                shared.emit(Event::Path { id, direct });
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
}

pub struct NetHandle {
    tx: mpsc::UnboundedSender<Command>,
}

impl NetHandle {
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }

    /// Stops all transfers and closes connections so that peers learn about
    /// it, waiting at most `timeout`.
    pub fn shutdown(&self, timeout: Duration) {
        let (done, wait) = std::sync::mpsc::channel();
        self.send(Command::Shutdown { done });
        let _ = wait.recv_timeout(timeout);
    }
}

/// Starts the network thread. `sink` is called from network threads for every event.
pub fn start(secret: SecretKey, sink: impl Fn(Event) + Send + Sync + 'static) -> NetHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        sink: Box::new(sink),
        pending: Mutex::new(HashMap::new()),
        cancels: Mutex::new(HashMap::new()),
    });
    std::thread::Builder::new()
        .name("network".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(run(secret, shared, rx));
        })
        .expect("spawn network thread");
    NetHandle { tx }
}

async fn bind(secret: SecretKey) -> anyhow::Result<Endpoint> {
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(secret)
        .alpns(vec![protocol::ALPN.to_vec()])
        .address_lookup(MdnsAddressLookup::builder())
        .bind()
        .await?;
    Ok(endpoint)
}

async fn run(secret: SecretKey, shared: Arc<Shared>, mut rx: mpsc::UnboundedReceiver<Command>) {
    let endpoint = loop {
        match bind(secret.clone()).await {
            Ok(ep) => break ep,
            Err(e) => {
                shared.emit(Event::BindFailed(e.to_string()));
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    };

    // Online indicator: connected to the home relay server.
    {
        let endpoint = endpoint.clone();
        let shared = shared.clone();
        tokio::spawn(async move {
            let mut status = endpoint.home_relay_status();
            let mut last = None;
            loop {
                let online = status.get().iter().any(|s| s.is_connected());
                if last != Some(online) {
                    last = Some(online);
                    shared.emit(Event::Online(online));
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
    }

    // Incoming connections.
    {
        let endpoint = endpoint.clone();
        let shared = shared.clone();
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let shared = shared.clone();
                tokio::spawn(async move {
                    let Ok(accepting) = incoming.accept() else {
                        return;
                    };
                    let Ok(conn) = accepting.await else { return };
                    receive::handle(shared, conn).await;
                });
            }
        });
    }

    while let Some(cmd) = rx.recv().await {
        match cmd {
            Command::Send {
                id,
                to,
                path,
                kind,
                my_name,
            } => {
                let token = shared.register(id);
                let shared = shared.clone();
                let endpoint = endpoint.clone();
                tokio::spawn(async move {
                    let outcome =
                        send::run(&shared, &endpoint, id, to, path, kind, my_name, token).await;
                    shared.finish(id, outcome);
                });
            }
            Command::Respond { id, accept } => {
                if let Some(tx) = shared.pending.lock().unwrap().remove(&id) {
                    let _ = tx.send(accept);
                }
            }
            Command::Cancel { id } => {
                if let Some(token) = shared.cancels.lock().unwrap().get(&id) {
                    token.cancel();
                }
            }
            Command::Shutdown { done } => {
                for token in shared.cancels.lock().unwrap().values() {
                    token.cancel();
                }
                // Let the transfer tasks send their "cancelled" close first.
                tokio::time::sleep(Duration::from_millis(200)).await;
                endpoint.close().await;
                let _ = done.send(());
                return;
            }
        }
    }
}

/// Application error codes used when closing connections.
pub(crate) const CLOSE_OK: u32 = 0;
/// The user on that side cancelled the transfer.
pub(crate) const CLOSE_CANCELLED: u32 = 1;
/// The transfer failed on that side (for example, the file could not be
/// written), so the peer reports a failure rather than a cancellation.
pub(crate) const CLOSE_FAILED: u32 = 2;

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dodavo-net-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn wait_finished(rx: &mpsc::Receiver<Event>, id: u64) -> Outcome {
        loop {
            match rx.recv_timeout(Duration::from_secs(90)).expect("timed out") {
                Event::Finished { id: i, outcome } if i == id => return outcome,
                _ => {}
            }
        }
    }

    /// Two endpoints in one process: sends a file and a folder from A to B,
    /// then checks that a declined offer is reported to the sender.
    #[test]
    fn end_to_end() {
        let base = scratch("e2e");
        let (tx_a, rx_a) = mpsc::channel();
        let (tx_b, rx_b) = mpsc::channel();
        let tx_a = Mutex::new(tx_a);
        let tx_b = Mutex::new(tx_b);
        let key_b = SecretKey::generate();
        let id_b = key_b.public();
        let a = start(SecretKey::generate(), move |e| {
            tx_a.lock().unwrap().send(e).unwrap()
        });
        let b = start(key_b, move |e| tx_b.lock().unwrap().send(e).unwrap());

        // Receiver: accept everything except offers named "decline.txt".
        let recv_dir = base.join("received");
        let b_thread = {
            let recv_dir = recv_dir.clone();
            std::thread::spawn(move || {
                let mut done = Vec::new();
                while done.len() < 3 {
                    match rx_b
                        .recv_timeout(Duration::from_secs(90))
                        .expect("receiver timed out")
                    {
                        Event::Offer { id, offer, .. } => {
                            let accept = (offer.name != "decline.txt").then(|| recv_dir.clone());
                            b.send(Command::Respond { id, accept });
                        }
                        Event::Finished { outcome, .. } => done.push(outcome),
                        _ => {}
                    }
                }
                done
            })
        };

        // A file.
        let file = base.join("hello.bin");
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(&file, &data).unwrap();
        let id = next_transfer_id();
        a.send(Command::Send {
            id,
            to: id_b,
            path: file,
            kind: Kind::File,
            my_name: "A".into(),
        });
        assert!(matches!(wait_finished(&rx_a, id), Outcome::Completed(_)));
        assert_eq!(std::fs::read(recv_dir.join("hello.bin")).unwrap(), data);

        // A folder.
        let folder = base.join("My Folder");
        std::fs::create_dir_all(folder.join("sub")).unwrap();
        std::fs::write(folder.join("a.txt"), b"alpha").unwrap();
        std::fs::write(folder.join("sub").join("b.txt"), b"beta").unwrap();
        let id = next_transfer_id();
        a.send(Command::Send {
            id,
            to: id_b,
            path: folder,
            kind: Kind::Folder,
            my_name: "A".into(),
        });
        assert!(matches!(wait_finished(&rx_a, id), Outcome::Completed(_)));

        // A declined offer.
        let declined = base.join("decline.txt");
        std::fs::write(&declined, b"no").unwrap();
        let id = next_transfer_id();
        a.send(Command::Send {
            id,
            to: id_b,
            path: declined,
            kind: Kind::File,
            my_name: "A".into(),
        });
        assert!(matches!(wait_finished(&rx_a, id), Outcome::Declined));

        let results = b_thread.join().unwrap();
        assert_eq!(
            results
                .iter()
                .filter(|o| matches!(o, Outcome::Completed(_)))
                .count(),
            2
        );
        // Give the receiver a moment to finish unpacking before checking (it reports Finished afterwards).
        assert_eq!(
            std::fs::read(recv_dir.join("My Folder").join("sub").join("b.txt")).unwrap(),
            b"beta"
        );
        assert_eq!(
            std::fs::read(recv_dir.join("My Folder").join("a.txt")).unwrap(),
            b"alpha"
        );
        assert!(!recv_dir.join("decline.txt").exists());
        let leftovers: Vec<_> = std::fs::read_dir(&recv_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(leftovers.len(), 2, "unexpected files: {leftovers:?}");
        std::fs::remove_dir_all(&base).ok();
    }
}
