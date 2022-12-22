#![forbid(unsafe_code)]

use crate::{
    block_forest::BlockTree,
    data::{BlockHash, VerifiedBlock, VerifiedPeerMessage, VerifiedTransaction},
    node::mining_service::MiningInfo,
    node::peer_service::{PeerCommand, PeerCommandKind, PeerEvent, PeerEventKind, SessionId},
};

use crossbeam::{
    channel::{self, Receiver, Sender},
    select,
};
use log::*;
use serde::{Deserialize, Serialize};

use std::ops::Deref;
use std::{collections::HashSet, thread, time::Duration};

////////////////////////////////////////////////////////////////////////////////

#[derive(Default, Serialize, Deserialize)]
pub struct GossipServiceConfig {
    #[serde(with = "humantime_serde")]
    pub eager_requests_interval: Duration,
}

pub struct GossipService {
    config: GossipServiceConfig,
    event_receiver: Receiver<PeerEvent>,
    command_sender: Sender<PeerCommand>,

    block_receiver: Receiver<VerifiedBlock>,
    mining_info_sender: Sender<MiningInfo>,
    block_forest: BlockTree,

    current_sessions: HashSet<SessionId>,
}

// TODO: we need to store session_ids that are currently connected in order to broadcast msgs

impl GossipService {
    pub fn new(
        config: GossipServiceConfig,
        event_receiver: Receiver<PeerEvent>,
        command_sender: Sender<PeerCommand>,
        block_receiver: Receiver<VerifiedBlock>,
        mining_info_sender: Sender<MiningInfo>,
    ) -> Self {
        Self {
            config,
            event_receiver,
            command_sender,
            block_receiver,
            mining_info_sender,
            block_forest: BlockTree::new(),
            current_sessions: HashSet::new(),
        }
    }

    fn send_head(&self) {
        let head = self.block_forest.head();
        self.mining_info_sender
            .send(MiningInfo {
                block_index: head.index + 1,
                prev_hash: *head.hash(),
                max_hash: self.block_forest.next_max_hash(),
                transactions: vec![],
            })
            .expect("Couldn't send a message to MiningService");
    }

    pub fn run(&mut self) {
        self.send_head();
        match self.spawn_ticker() {
            None => loop {
                select! {
                    recv(self.event_receiver) -> event => self.handle_peer_event(event.unwrap()),
                    recv(self.block_receiver) -> block => self.add_block(block.unwrap()),
                }
            },
            Some(ticker) => loop {
                select! {
                    recv(self.event_receiver) -> msg => self.handle_peer_event(msg.unwrap()),
                    recv(self.block_receiver) -> block => self.add_block(block.unwrap()),
                    recv(ticker) -> _msg => self.request_unknown_blocks(),
                }
            },
        }
    }

    /////////////////////////////////////////////////////////////////////////////////////

    fn spawn_ticker(&self) -> Option<Receiver<()>> {
        if self.config.eager_requests_interval.is_zero() {
            return None;
        }
        let (sender, receiver) = channel::bounded(1);
        let duration = self.config.eager_requests_interval;
        thread::spawn(move || loop {
            thread::sleep(duration);
            if sender.send(()).is_err() {
                break;
            }
        });
        Some(receiver)
    }

    fn request_unknown_blocks(&self) {
        for block_hash in self.block_forest.unknown_block_hashes() {
            for session_id in &self.current_sessions {
                self.command_sender
                    .send(PeerCommand {
                        session_id: *session_id,
                        command_kind: PeerCommandKind::SendMessage(VerifiedPeerMessage::Request {
                            block_hash: *block_hash,
                        }),
                    })
                    .expect("Couldn't send a message");
            }
        }
    }

    /////////////////////////////////////////////////////////////////////////////////////

    fn handle_peer_event(&mut self, event: PeerEvent) {
        info!("[GossipService]: Got PeerEvent {:?}", event);
        match event.event_kind {
            PeerEventKind::Connected => {
                self.current_sessions.insert(event.session_id);
                self.send_snapshot(event.session_id);
            }
            PeerEventKind::NewMessage(msg) => {
                match msg {
                    VerifiedPeerMessage::Request { block_hash } => {
                        self.send_block(event.session_id, block_hash)
                    }
                    VerifiedPeerMessage::Block(block) => {
                        self.receive_block(event.session_id, block)
                    }
                    VerifiedPeerMessage::Transaction(tx) => self.receive_tx(event.session_id, tx),
                };
            }
            PeerEventKind::Disconnected => {
                self.current_sessions.remove(&event.session_id);
            }
        }
    }

    // Peer Event Callbacks.
    /////////////////////////////////////////////////////////////////////////////////////////////

    // Callback when new connection is established.
    fn send_snapshot(&self, session_id: SessionId) {
        let head_block = Box::new(self.block_forest.head().deref().clone());

        self.command_sender
            .send(PeerCommand {
                session_id,
                command_kind: PeerCommandKind::SendMessage(VerifiedPeerMessage::Block(head_block)),
            })
            .expect("Couldn't send a message");

        for tx in self.block_forest.pending_transactions().values() {
            self.command_sender
                .send(PeerCommand {
                    session_id,
                    command_kind: PeerCommandKind::SendMessage(VerifiedPeerMessage::Transaction(
                        Box::new(tx.clone()),
                    )),
                })
                .expect("Couldn't send a message");
        }
        info!(
            "[GossipService]: Sent current snapshot for session_id {}",
            session_id
        );
    }

    // Callback when a peer asks for a block with a particular hash.
    fn send_block(&self, session_id: SessionId, block_hash: BlockHash) {
        if let Some(block) = self.block_forest.find_block(&block_hash) {
            self.command_sender
                .send(PeerCommand {
                    session_id,
                    command_kind: PeerCommandKind::SendMessage(VerifiedPeerMessage::Block(
                        Box::new(block.deref().clone()),
                    )),
                })
                .expect("Couldn't send a message");
        }
        info!(
            "[GossipService]: Sent a block for session id {} with BlockHash {:?}",
            session_id, block_hash
        );
    }

    // Callback when a peer sends us a new block.
    fn receive_block(&mut self, cur_session_id: SessionId, block: Box<VerifiedBlock>) {
        // Add it to our BlockTree.
        if self.block_forest.add_block(block.deref().clone()).is_err() {
            return;
        }

        // Broadcast this block to all peers.
        for session_id in &self.current_sessions {
            if *session_id != cur_session_id {
                self.command_sender
                    .send(PeerCommand {
                        session_id: *session_id,
                        command_kind: PeerCommandKind::SendMessage(VerifiedPeerMessage::Block(
                            block.clone(),
                        )),
                    })
                    .expect("Couldn't send a message");
            }
        }

        // If we don't ancestor for this block we should send a request for it.
        if self.block_forest.find_block(&block.prev_hash).is_none() {
            self.command_sender
                .send(PeerCommand {
                    session_id: cur_session_id,
                    command_kind: PeerCommandKind::SendMessage(VerifiedPeerMessage::Request {
                        block_hash: block.prev_hash,
                    }),
                })
                .expect("Couldn't send a message");
        }
        info!(
            "[GossipService]: Received new block from session_id {} with BlochHash {:?}",
            cur_session_id,
            block.hash()
        );
    }

    fn send_mining_info(&self) {
        // TODO: maybe we need to somehow filter when doing this.
        // Try to mine a new block with currently pending transactions.
        let mut transactions: Vec<VerifiedTransaction> = self
            .block_forest
            .pending_transactions()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        transactions.truncate(2);
        let head = self.block_forest.head();
        self.mining_info_sender
            .send(MiningInfo {
                block_index: head.index + 1,
                prev_hash: *head.hash(),
                max_hash: self.block_forest.next_max_hash(),
                transactions,
            })
            .expect("Couldn't send a message to MiningService");
    }

    // Callback when a peer sends us a new transaction.
    fn receive_tx(&mut self, cur_session_id: SessionId, tx: Box<VerifiedTransaction>) {
        if self
            .block_forest
            .add_transaction(tx.deref().clone())
            .is_err()
        {
            return;
        }

        self.send_mining_info();

        // Forward this tx to all peers.
        for session_id in &self.current_sessions {
            if *session_id != cur_session_id {
                self.command_sender
                    .send(PeerCommand {
                        session_id: *session_id,
                        command_kind: PeerCommandKind::SendMessage(
                            VerifiedPeerMessage::Transaction(tx.clone()),
                        ),
                    })
                    .expect("Couldn't send a message");
            }
        }
        info!("[GossipService]: Received a new transaction from session_id");
    }

    ///////////////////////////////////////////////////////////////////////////////////////////

    // Called when we receive a new block from a miner service.
    fn add_block(&mut self, block: VerifiedBlock) {
        self.block_forest.add_block(block.clone()).unwrap();
        for session_id in &self.current_sessions {
            self.command_sender
                .send(PeerCommand {
                    session_id: *session_id,
                    command_kind: PeerCommandKind::SendMessage(VerifiedPeerMessage::Block(
                        Box::new(block.clone()),
                    )),
                })
                .expect("Couldn't send a message");
        }
        self.send_mining_info();
        info!("[GossipService]: Got new block from MinerService");
    }
}
