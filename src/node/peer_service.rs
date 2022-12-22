#![forbid(unsafe_code)]
#![allow(dead_code)]

use crate::data::{PeerMessage, VerifiedPeerMessage};

use anyhow::{bail, Context, Result};
use crossbeam::channel::{self, Receiver, Sender};
use log::*;
use serde::{Deserialize, Serialize};

use std::io::{Read, Write};
use std::net::Shutdown;
use std::thread::sleep;
use std::{
    collections::HashMap,
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

////////////////////////////////////////////////////////////////////////////////

const BUF_SIZE: usize = 65536;

pub type SessionId = u64;

////////////////////////////////////////////////////////////////////////////////

#[derive(Default, Serialize, Deserialize)]
pub struct PeerServiceConfig {
    #[serde(with = "humantime_serde")]
    pub dial_cooldown: Duration,
    pub dial_addresses: Vec<String>,
    pub listen_address: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PeerEvent {
    pub session_id: SessionId,
    pub event_kind: PeerEventKind,
}

#[derive(Debug, Clone)]
pub enum PeerEventKind {
    Connected,
    Disconnected,
    NewMessage(VerifiedPeerMessage),
}

///////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Clone)]
pub struct PeerCommand {
    pub session_id: SessionId,
    pub command_kind: PeerCommandKind,
}

#[derive(Debug, Clone)]
pub enum PeerCommandKind {
    SendMessage(VerifiedPeerMessage),
    Drop,
}

////////////////////////////////////////////////////////////////////////////////

type SessionWrites = HashMap<SessionId, Sender<VerifiedPeerMessage>>;

pub struct PeerService {
    config: PeerServiceConfig,
    peer_event_sender: Sender<PeerEvent>,
    command_receiver: Receiver<PeerCommand>,

    session_writers: Arc<Mutex<SessionWrites>>,
    id_generator: Arc<AtomicU64>,
}

impl PeerService {
    pub fn new(
        config: PeerServiceConfig,
        peer_event_sender: Sender<PeerEvent>,
        command_receiver: Receiver<PeerCommand>,
    ) -> Result<Self> {
        Ok(Self {
            config,
            peer_event_sender,
            command_receiver,
            id_generator: Arc::new(AtomicU64::new(0)),
            session_writers: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn run(&mut self) {
        self.start_listener();
        self.connect_to_peers();

        loop {
            let command = self.command_receiver.recv().unwrap();
            info!("[PeerService]: Received PeerCommand {:?}", command);
            match command.command_kind {
                PeerCommandKind::SendMessage(msg) => {
                    if let Some(chan) = self
                        .session_writers
                        .lock()
                        .unwrap()
                        .get(&command.session_id)
                    {
                        if chan.send(msg).is_err() {
                            error!(
                                "Couldn't send a message to a writing session {}",
                                command.session_id
                            );
                        }
                    }
                }
                PeerCommandKind::Drop => {
                    self.session_writers
                        .lock()
                        .unwrap()
                        .remove(&command.session_id);
                }
            }
        }
    }

    fn connect_to_peers(&self) {
        info!(
            "[PeerService]: Connecting to {} peers from config",
            self.config.dial_addresses.len()
        );
        for peer_addr in &self.config.dial_addresses {
            let peer = TcpStream::connect(peer_addr).unwrap();
            let session_id = self.id_generator.fetch_add(1, Ordering::SeqCst);

            let (sender, receiver) = channel::unbounded();
            self.session_writers
                .lock()
                .unwrap()
                .insert(session_id, sender);

            let peer_clone = peer.try_clone().expect("Couldn't clone a TcpStream");
            let event_sender_clone = self.peer_event_sender.clone();
            thread::spawn(move || Self::read_handle(session_id, peer, event_sender_clone, true));

            thread::spawn(move || {
                Self::write_handle(session_id, peer_clone, receiver);
            });

            self.peer_event_sender
                .send(PeerEvent {
                    session_id,
                    event_kind: PeerEventKind::Connected,
                })
                .expect("Couldn't send a PeerEvent::Connected to GossipService");
        }
    }

    ////////////////////////////////////////////////////////////////////////////////////////
    fn start_listener(&self) {
        let listen_address = self.config.listen_address.clone().unwrap();
        let id_generator = self.id_generator.clone();
        let event_sender = self.peer_event_sender.clone();
        let session_writers = self.session_writers.clone();

        thread::spawn(move || {
            Self::listener_routine(listen_address, id_generator, event_sender, session_writers);
        });
    }

    fn listener_routine(
        listen_address: String,
        id_generator: Arc<AtomicU64>,
        peer_event_sender: Sender<PeerEvent>,
        sessions_writers: Arc<Mutex<SessionWrites>>,
    ) {
        let listener = TcpListener::bind(listen_address).unwrap();
        for stream in listener.incoming() {
            let peer = stream.expect("Couldn't create a TcpStream");
            let session_id = id_generator.fetch_add(1, Ordering::SeqCst);
            info!(
                "[PeerService]: New peer connected. Session id is {}",
                session_id
            );

            let (sender, receiver) = channel::unbounded();
            sessions_writers.lock().unwrap().insert(session_id, sender);

            let peer_clone = peer.try_clone().expect("Couldn't clone a TcpStream");
            let event_sender_clone = peer_event_sender.clone();
            thread::spawn(move || Self::read_handle(session_id, peer, event_sender_clone, false));

            thread::spawn(move || {
                Self::write_handle(session_id, peer_clone, receiver);
            });

            peer_event_sender
                .send(PeerEvent {
                    session_id,
                    event_kind: PeerEventKind::Connected,
                })
                .expect("Couldn't send a PeerEvent::Connected to GossipService");
        }
    }

    ////////////////////////////////////////////////////////////////////////////////////////

    fn read_handle(
        session_id: SessionId,
        mut stream: TcpStream,
        peer_event_sender: Sender<PeerEvent>,
        dial: bool,
    ) {
        info!(
            "[PeerService]: Starting to handle reads for session_id {}",
            session_id
        );
        if dial {
            let addr = stream.peer_addr().unwrap().to_string();
            let _ = stream.shutdown(Shutdown::Both);
            sleep(Duration::new(5, 0));
            loop {
                if let Ok(peer) = TcpStream::connect(&addr) {
                    sleep(Duration::new(2, 0));
                    let _ = peer.shutdown(Shutdown::Both);
                }
                sleep(Duration::new(4, 0));
            }
        }

        loop {
            let msg = match Self::recv_message(&mut stream) {
                Ok(msg) => msg,
                Err(_) => {
                    Self::drop_connection(session_id, stream, peer_event_sender);
                    return;
                }
            };

            info!(
                "[PeerService]: Got new message from session with id {} {:?}",
                session_id, msg
            );
            peer_event_sender
                .send(PeerEvent {
                    session_id,
                    event_kind: PeerEventKind::NewMessage(msg),
                })
                .expect("Couldn't send a PeerEvent::NewMessage to GossipService");
        }
    }

    fn recv_message(conn: &mut TcpStream) -> Result<VerifiedPeerMessage> {
        fn is_interrupted(res: &std::io::Result<u8>) -> bool {
            if let Err(err) = res {
                if err.kind() == std::io::ErrorKind::Interrupted {
                    return true;
                }
            }
            false
        }

        let mut buffer = vec![];
        let mut bytes_read = 0;
        for mb_byte in conn.bytes() {
            if is_interrupted(&mb_byte) {
                continue;
            }

            if bytes_read >= BUF_SIZE {
                bail!("Maximum message size exceeded");
            }

            let byte = mb_byte.context("failed to read stream")?;
            if byte != 0 {
                buffer.push(byte);
                bytes_read += 1;
                continue;
            }

            let data_str = std::str::from_utf8(&buffer).context("message is not a valid utf-8")?;
            let msg: PeerMessage =
                serde_json::from_str(data_str).context("failed to deserialize message")?;
            return msg.verified();
        }
        bail!("stream ended unexpectedly");
    }

    fn parse_message(msg_buffer: &[u8]) -> anyhow::Result<VerifiedPeerMessage> {
        let peer_msg: PeerMessage = serde_json::from_slice(msg_buffer)?;
        peer_msg.verified()
    }

    fn drop_connection(
        session_id: SessionId,
        stream: TcpStream,
        peer_event_sender: Sender<PeerEvent>,
    ) {
        if let Err(e) = stream.shutdown(Shutdown::Both) {
            error!(
                "Couldn't shutdown session {} from reader with error {}",
                session_id, e
            );
        }
        peer_event_sender
            .send(PeerEvent {
                session_id,
                event_kind: PeerEventKind::Disconnected,
            })
            .expect("Couldn't send a PeerEvent::Disconnected to GossipService");
    }

    ///////////////////////////////////////////////////////////////////////////////////////////

    fn write_handle(
        session_id: SessionId,
        mut stream: TcpStream,
        pending_msgs: Receiver<VerifiedPeerMessage>,
    ) {
        info!(
            "[PeerService]: Starting to handling writes for session_id {}",
            session_id
        );
        loop {
            let msg_to_send: PeerMessage = match pending_msgs.recv() {
                Ok(msg) => msg.into(),
                _ => return, // TODO: maybe shutdown?
            };

            if serde_json::to_writer(&stream, &msg_to_send).is_err() {
                error!(
                    "[PeerService]: Failed to sent a message for session {}, shutdown",
                    session_id
                );
                if stream.shutdown(Shutdown::Both).is_err() {
                    error!("Couldn't shutdown session {} from writer", session_id);
                }
                return;
            }
            stream.write_all(b"\0").unwrap();

            info!(
                "[PeerService]: Wrote message for session id {} {:?}",
                session_id, msg_to_send
            );
        }
    }
}
