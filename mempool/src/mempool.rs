pub struct Mempool;
use crate::batch_maker::BatchMaker;
use crate::batch_maker::Transaction;
use crate::processor::Processor;
use crate::quorum_waiter::QuorumWaiter;
use crate::{Committee, Parameters};
use async_trait::async_trait;
use bytes::Bytes;
use crypto::PublicKey;
use log::info;
use network::{MessageHandler, Receiver, Writer};
use std::error::Error;
use store::Store;
use tokio::sync::mpsc::{channel, Sender};

/// The default channel capacity for each channel of the mempool.
pub const CHANNEL_CAPACITY: usize = 1_000;

/// Indicates a serialized `WorkerMessage::Batch` message.
pub type SerializedBatch = Vec<u8>;
pub type Payload = SerializedBatch;

impl Mempool {
    pub fn spawn(
        // The public key of this authority.
        name: PublicKey,
        // The committee information.
        committee: Committee,
        // The configuration parameters.
        parameters: Parameters,
        // The persistent storage.
        store: Store,
        // Output serialize batches to the consensus.
        tx_consensus: Sender<SerializedBatch>,
    ) {
        let (tx_batch_maker, rx_batch_maker) = channel(CHANNEL_CAPACITY);
        let (tx_quorum_waiter, rx_quorum_waiter) = channel(CHANNEL_CAPACITY);
        let (tx_processor, rx_processor) = channel(CHANNEL_CAPACITY);

        // We first receive clients' transactions from the network.
        let mut address = committee
            .address(&name)
            .expect("Our public key is not in the committee");
        address.set_ip("0.0.0.0".parse().unwrap());
        Receiver::spawn(
            address,
            /* handler */ TxReceiverHandler { tx_batch_maker },
        );

        // The transactions are sent to the `BatchMaker` that assembles them into batches. It then broadcasts
        // (in a reliable manner) the batches to all other nodes. Finally, it gathers the 'cancel handlers'
        // of the messages and send them to the `QuorumWaiter`.
        BatchMaker::spawn(
            parameters.batch_size,
            parameters.max_batch_delay,
            /* rx_transaction */ rx_batch_maker,
            /* tx_message */ tx_quorum_waiter,
            /* workers_addresses */
            committee
                .broadcast_addresses(&name)
                .iter()
                .map(|(name, addresses)| (*name, *addresses))
                .collect(),
        );

        // The `QuorumWaiter` waits for 2f authorities to acknowledge reception of the batch. It then forwards
        // the batch to the `Processor`.
        QuorumWaiter::spawn(
            committee.clone(),
            /* stake */ committee.stake(&name),
            /* rx_message */ rx_quorum_waiter,
            /* tx_batch */ tx_processor,
        );

        // The `Processor` hashes and stores the batch. It then forwards the batch's digest to the `PrimaryConnector`
        // that will send it to our primary machine.
        Processor::spawn(
            store.clone(),
            /* rx_batch */ rx_processor,
            /* tx_digest */ tx_consensus,
        );

        info!(
            "Mempool {} listening to client transactions on {}",
            name, address
        );
    }
}

/// Defines how the network receiver handles incoming transactions.
#[derive(Clone)]
struct TxReceiverHandler {
    tx_batch_maker: Sender<Transaction>,
}

#[async_trait]
impl MessageHandler for TxReceiverHandler {
    async fn dispatch(&self, _writer: &mut Writer, message: Bytes) -> Result<(), Box<dyn Error>> {
        // Send the transaction to the batch maker.
        self.tx_batch_maker
            .send(message.to_vec())
            .await
            .expect("Failed to send transaction");

        // Give the change to schedule other tasks.
        tokio::task::yield_now().await;
        Ok(())
    }
}
