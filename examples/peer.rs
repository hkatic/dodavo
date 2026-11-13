//! A peer without a user interface, for trying out transfers with other
//! copies of Dodavo. It accepts every offer, saving to `--out`, and takes
//! commands on its standard input:
//!
//!     cargo run --example peer -- --profile peer --out ~/Downloads/peer [--keep extract|zip|both]
//!
//!     send <contact id> <path>
//!     chat <contact id> <text>
//!     typing <contact id> [off]
//!     pause <id> | resume <id> | cancel <id>
//!     netchange   (tells it that the network changed, as an Android app does)
//!     quit
//!
//! Its Contact ID is printed at the start, and stays the same for a profile.
//! On Android (run through `adb shell`), `DODAVO_DATA_DIR`, `DODAVO_TEMP_DIR`
//! and `DODAVO_DOWNLOADS_DIR` give it the folders that the app would.

use std::{
    io::BufRead,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use dodavo_core::{
    archive, config,
    format::format_size,
    i18n, identity,
    net::{self, Accept, Command, Event, Kind, NetHandle, ReceivedFolder},
};

fn main() {
    let mut profile = "peer".to_string();
    let mut out = None;
    let mut folder = ReceivedFolder::Extract;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().expect("a value after the option");
        match arg.as_str() {
            "--profile" => profile = value(),
            "--out" => out = Some(PathBuf::from(value())),
            "--keep" => {
                folder = match value().as_str() {
                    "zip" => ReceivedFolder::KeepZip,
                    "both" => ReceivedFolder::Both,
                    _ => ReceivedFolder::Extract,
                }
            }
            other => panic!("unknown option {other}"),
        }
    }
    let out = out.unwrap_or_else(|| dodavo_core::platform::downloads_dir().join("peer"));
    i18n::init(None);
    let data_dir = config::data_dir(Some(&profile));
    // Its own log, in its profile's folder, as a copy of Dodavo keeps one.
    if let Some(log) = dodavo_core::logging::init(&data_dir, "test peer", &[]) {
        println!("Log: {}", log.display());
    }
    let secret = identity::load_or_create(&data_dir).expect("the identity");
    println!("Contact ID: {}", secret.public());

    let handle: Arc<Mutex<Option<NetHandle>>> = Arc::default();
    let responder = handle.clone();
    let chat = net::ChatPolicy {
        my_name: "Dodavo test peer".into(),
        ..Default::default()
    };
    let net = net::start_with_chat(
        secret,
        data_dir,
        archive::temp_dir_for(Some(&profile)),
        [],
        net::Limits::default(),
        Some(chat),
        move |event| match event {
            Event::Progress { id, done, total } => {
                println!("{id}: {} of {}", format_size(done), format_size(total));
            }
            Event::Offer { id, from, offer } => {
                println!(
                    "{id}: {from} ({}) offers {:?} {:?}, {}; accepting",
                    offer.sender_name,
                    offer.kind,
                    offer.name,
                    format_size(offer.size)
                );
                if let Some(net) = &*responder.lock().unwrap() {
                    net.send(Command::Respond {
                        id,
                        accept: Some(Accept {
                            dir: out.clone(),
                            folder,
                        }),
                    });
                }
            }
            Event::Chat { from, name, text } => println!("chat from {from} ({name}): {text}"),
            Event::Typing { from, on } => println!("typing {from}: {on}"),
            other => println!("{other:?}"),
        },
    );
    *handle.lock().unwrap() = Some(net);

    let send = |command| {
        if let Some(net) = &*handle.lock().unwrap() {
            net.send(command);
        }
    };
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let (verb, rest) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
        let id = || rest.trim().parse::<u64>().ok();
        match (verb, id()) {
            ("pause", Some(id)) => send(Command::Pause { id }),
            ("resume", Some(id)) => send(Command::Resume { id }),
            ("cancel", Some(id)) => send(Command::Cancel { id }),
            ("send", _) => {
                let Some((to, path)) = rest.trim().split_once(' ') else {
                    println!("send <contact id> <path>");
                    continue;
                };
                let (Some(to), path) = (identity::parse_contact_id(to), PathBuf::from(path.trim()))
                else {
                    println!("that is not a Contact ID");
                    continue;
                };
                let kind = if path.is_dir() {
                    Kind::Folder
                } else {
                    Kind::File
                };
                let id = net::next_transfer_id();
                println!("{id}: sending {}", path.display());
                send(Command::Send {
                    id,
                    to,
                    path,
                    kind,
                    my_name: "Dodavo test peer".into(),
                    peer_name: "peer".into(),
                });
            }
            ("chat" | "typing", _) => {
                let (to, text) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
                let Some(to) = identity::parse_contact_id(to) else {
                    println!("chat <contact id> <text>, typing <contact id> [off]");
                    continue;
                };
                if verb == "typing" {
                    send(Command::Typing {
                        to,
                        on: text.trim() != "off",
                    });
                } else {
                    let id = net::next_transfer_id();
                    println!("{id}: saying {text:?}");
                    send(Command::Chat {
                        to,
                        id,
                        text: text.to_string(),
                    });
                }
            }
            ("netchange", _) => send(Command::NetworkChange),
            ("quit", _) => break,
            ("", _) => {}
            _ => println!("send, chat, typing, pause, resume, cancel, netchange or quit"),
        }
    }
    if let Some(net) = &*handle.lock().unwrap() {
        net.shutdown(Duration::from_secs(3));
    }
    dodavo_core::logging::flush(Duration::from_secs(1));
}
