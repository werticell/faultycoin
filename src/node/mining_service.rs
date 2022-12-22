#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::{
    data::{
        Block, BlockAttributes, BlockHash, VerifiedBlock, VerifiedTransaction, WalletId, MAX_REWARD,
    },
    util::{deserialize_wallet_id, serialize_wallet_id},
};

use chrono::Utc;
use crossbeam::channel::{Receiver, Sender};
use crossbeam::{channel, select};
use log::*;
use rayon::{ThreadPool, ThreadPoolBuilder};
use serde::{Deserialize, Serialize};

////////////////////////////////////////////////////////////////////////////////

#[derive(Serialize, Deserialize)]
pub struct MiningServiceConfig {
    pub thread_count: usize,
    pub max_tx_per_block: usize,

    #[serde(
        serialize_with = "serialize_wallet_id",
        deserialize_with = "deserialize_wallet_id"
    )]
    pub public_key: WalletId,
}

impl Default for MiningServiceConfig {
    fn default() -> Self {
        Self {
            thread_count: 0,
            max_tx_per_block: 0,
            public_key: WalletId::of_genesis(),
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

#[derive(Clone, Debug)]
pub struct MiningInfo {
    pub block_index: u64,
    pub prev_hash: BlockHash,
    pub max_hash: BlockHash,
    pub transactions: Vec<VerifiedTransaction>,
}

pub struct MiningService {
    config: MiningServiceConfig,
    info_receiver: Receiver<MiningInfo>,
    block_sender: Sender<VerifiedBlock>,

    pool: ThreadPool,
}

impl MiningService {
    pub fn new(
        config: MiningServiceConfig,
        info_receiver: Receiver<MiningInfo>,
        block_sender: Sender<VerifiedBlock>,
    ) -> Self {
        Self {
            pool: ThreadPoolBuilder::new()
                .num_threads(config.thread_count)
                .build()
                .unwrap(),
            config,
            info_receiver,
            block_sender,
        }
    }

    pub fn run(&mut self) {
        info!("[MiningService]: Starting mining service");
        let (ready_sender, ready_receiver) = channel::unbounded();
        let mut work_needed = Arc::new(AtomicBool::new(true));
        loop {
            select! {
                recv(self.info_receiver) -> mining_info => {
                    work_needed.store(false, Ordering::SeqCst);
                    work_needed = Arc::new(AtomicBool::new(true));
                    self.start_miners(mining_info.unwrap(), ready_sender.clone(), work_needed.clone())
                },
                recv(ready_receiver) -> ready_block => self.block_sender.send(ready_block.unwrap()).expect("Couldn't send verified block to GossipService"),
            }
        }
    }

    fn start_miners(
        &self,
        mining_info: MiningInfo,
        ready_sender: Sender<VerifiedBlock>,
        work_needed: Arc<AtomicBool>,
    ) {
        info!("[MiningService]: Starting new miners");
        for _ in 0..self.config.thread_count {
            let block = self.make_block_to_mine(mining_info.clone());
            let sender_clone = ready_sender.clone();
            let work_needed_clone = work_needed.clone();
            self.pool.spawn(move || {
                Self::miner_routine(block, sender_clone, work_needed_clone);
            })
        }
    }

    fn make_block_to_mine(&self, mining_info: MiningInfo) -> Block {
        Block {
            attrs: BlockAttributes {
                index: mining_info.block_index,
                timestamp: Utc::now(),
                reward: MAX_REWARD,
                nonce: 0,
                issuer: self.config.public_key.clone(),
                max_hash: mining_info.max_hash,
                prev_hash: mining_info.prev_hash,
            },
            transactions: mining_info
                .transactions
                .into_iter()
                .map(|tx| tx.into())
                .collect::<Vec<_>>(),
        }
    }

    fn miner_routine(
        mut block: Block,
        ready_sender: Sender<VerifiedBlock>,
        work_needed: Arc<AtomicBool>,
    ) {
        while work_needed.load(Ordering::SeqCst) {
            info!("[MiningService]: Mining loop");
            if block.compute_hash() <= block.max_hash {
                if work_needed.swap(false, Ordering::SeqCst) {
                    let verified_block =
                        block.verified().expect("Unverified block in miner routine");
                    ready_sender
                        .send(verified_block)
                        .expect("Couldn't send new block in MinerService");
                }
                break;
            }
            block.nonce += 1;
        }
    }
}
