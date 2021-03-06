//! The `sigverify_stage` implements the signature verification stage of the TPU. It
//! receives a list of lists of packets and outputs the same list, but tags each
//! top-level list with a list of booleans, telling the next stage whether the
//! signature in that packet is valid. It assumes each packet contains one
//! transaction. All processing is done on the CPU by default and on a GPU
//! if the `cuda` feature is enabled with `--features=cuda`.

use packet::SharedPackets;
use rand::{thread_rng, Rng};
use result::{Error, Result};
use service::Service;
use sigverify;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, spawn, JoinHandle};
use std::time::Instant;
use streamer::{self, PacketReceiver};
use timing;

pub type VerifiedPackets = Vec<(SharedPackets, Vec<u8>)>;

pub struct SigVerifyStage {
    thread_hdls: Vec<JoinHandle<()>>,
}

impl SigVerifyStage {
    pub fn new(packet_receiver: Receiver<SharedPackets>) -> (Self, Receiver<VerifiedPackets>) {
        sigverify::init();
        let (verified_sender, verified_receiver) = channel();
        let thread_hdls = Self::verifier_services(packet_receiver, verified_sender);
        (SigVerifyStage { thread_hdls }, verified_receiver)
    }

    fn verify_batch(batch: Vec<SharedPackets>) -> VerifiedPackets {
        let r = sigverify::ed25519_verify(&batch);
        batch.into_iter().zip(r).collect()
    }

    fn verifier(
        recvr: &Arc<Mutex<PacketReceiver>>,
        sendr: &Arc<Mutex<Sender<VerifiedPackets>>>,
    ) -> Result<()> {
        let (batch, len) =
            streamer::recv_batch(&recvr.lock().expect("'recvr' lock in fn verifier"))?;

        let now = Instant::now();
        let batch_len = batch.len();
        let rand_id = thread_rng().gen_range(0, 100);
        info!(
            "@{:?} verifier: verifying: {} id: {}",
            timing::timestamp(),
            batch.len(),
            rand_id
        );

        let verified_batch = Self::verify_batch(batch);
        sendr
            .lock()
            .expect("lock in fn verify_batch in tpu")
            .send(verified_batch)?;

        let total_time_ms = timing::duration_as_ms(&now.elapsed());
        let total_time_s = timing::duration_as_s(&now.elapsed());
        info!(
            "@{:?} verifier: done. batches: {} total verify time: {:?} id: {} verified: {} v/s {}",
            timing::timestamp(),
            batch_len,
            total_time_ms,
            rand_id,
            len,
            (len as f32 / total_time_s)
        );
        Ok(())
    }

    fn verifier_service(
        packet_receiver: Arc<Mutex<PacketReceiver>>,
        verified_sender: Arc<Mutex<Sender<VerifiedPackets>>>,
    ) -> JoinHandle<()> {
        spawn(move || loop {
            if let Err(e) = Self::verifier(&packet_receiver.clone(), &verified_sender.clone()) {
                match e {
                    Error::RecvTimeoutError(RecvTimeoutError::Disconnected) => break,
                    Error::RecvTimeoutError(RecvTimeoutError::Timeout) => (),
                    _ => error!("{:?}", e),
                }
            }
        })
    }

    fn verifier_services(
        packet_receiver: PacketReceiver,
        verified_sender: Sender<VerifiedPackets>,
    ) -> Vec<JoinHandle<()>> {
        let sender = Arc::new(Mutex::new(verified_sender));
        let receiver = Arc::new(Mutex::new(packet_receiver));
        (0..4)
            .map(|_| Self::verifier_service(receiver.clone(), sender.clone()))
            .collect()
    }
}

impl Service for SigVerifyStage {
    fn thread_hdls(self) -> Vec<JoinHandle<()>> {
        self.thread_hdls
    }

    fn join(self) -> thread::Result<()> {
        for thread_hdl in self.thread_hdls() {
            thread_hdl.join()?;
        }
        Ok(())
    }
}
