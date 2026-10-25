//! Reliable byte-bounded mailboxes. A permit lives through the socket write,
//! so receiving an item cannot hide a stalled writer from backpressure.
use sauerteig_core::ServerMessage;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, OnceLock,
};
use tokio::sync::mpsc;

pub(crate) const PEER_BACKLOG_BYTES: usize = 128 * 1024 * 1024;
/// Space a change leaves free behind its patch, for the result that follows it
/// and the ordinary traffic of the next few ticks.
pub(crate) const FOLLOW_UP_BYTES: usize = 64 * 1024;

/// Conservative wire plus authored-memory cost of a reliable message, calculated
/// without JSON serialization. Valid integer geometry is below these bounds.
///
/// Packets are charged with `message`. A change reserves room before it commits
/// with the same per-message functions, so a queue that admits the reservation
/// admits the packet.
pub(crate) mod cost {
    use sauerteig_core::{RootEdit, ServerMessage};

    pub const ENVELOPE: usize = 4096;
    const ROOT: usize = 128;
    const FRAGMENT: usize = 768;
    const CELL_EDIT: usize = 512;
    const OBJECT: usize = 512;
    const VOLUME: usize = 256;

    pub fn roots_bytes(roots: &[RootEdit]) -> usize {
        roots
            .iter()
            .map(|r| ROOT + r.fragments.len() * FRAGMENT)
            .sum()
    }
    pub fn root_patch(roots: &[RootEdit]) -> usize {
        ENVELOPE + roots_bytes(roots)
    }
    /// `edits` legacy cell edits, one per changed root.
    pub fn world_patch(edits: usize) -> usize {
        ENVELOPE + edits * CELL_EDIT
    }
    pub fn objects_patch(objects: usize) -> usize {
        ENVELOPE + objects * OBJECT
    }
    /// `objects` is the whole list the patch carries; `volumes` is the whole
    /// replacement layer, when the patch has one.
    pub fn transaction_patch(roots: &[RootEdit], objects: usize, volumes: Option<usize>) -> usize {
        ENVELOPE + roots_bytes(roots) + objects * OBJECT + volumes.map_or(0, |v| v * VOLUME)
    }
    pub fn message(message: &ServerMessage) -> usize {
        match message {
            ServerMessage::RootPatch { roots, .. } => root_patch(roots),
            ServerMessage::TransactionPatch {
                roots,
                objects,
                volumes,
                ..
            } => transaction_patch(roots, objects.len(), volumes.as_ref().map(Vec::len)),
            ServerMessage::WorldPatch { edits, .. } => world_patch(edits.len()),
            ServerMessage::Objects { objects, .. } => objects_patch(objects.len()),
            ServerMessage::Snapshot {
                players,
                projectiles,
                ..
            } => ENVELOPE + players.len() * 1024 + projectiles.len() * 512,
            ServerMessage::Shots { shots } => ENVELOPE + shots.len() * 512,
            ServerMessage::Error { message } | ServerMessage::Notice { message } => {
                ENVELOPE + message.len() * 6
            }
            _ => ENVELOPE,
        }
    }
}

pub(crate) struct Packet {
    pub message: ServerMessage,
    pub bytes: usize,
    encoded: OnceLock<Result<String, String>>,
}
impl Packet {
    pub fn new(message: ServerMessage) -> Arc<Self> {
        Arc::new(Self {
            bytes: cost::message(&message),
            message,
            encoded: OnceLock::new(),
        })
    }
    /// Called only by transport workers, never by the simulation owner.
    pub fn encode(&self) -> Result<&str, String> {
        self.encoded
            .get_or_init(|| serde_json::to_string(&self.message).map_err(|e| e.to_string()))
            .as_deref()
            .map_err(Clone::clone)
    }
    /// A harmless message that is charged as `bytes`, for filling a queue.
    #[cfg(test)]
    pub fn ballast(bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            message: ServerMessage::Synced { request: 0 },
            bytes,
            encoded: OnceLock::new(),
        })
    }
    /// A message that cannot be encoded, so the writer that reaches it fails the
    /// way it would on a socket that has stopped working.
    #[cfg(test)]
    pub fn poison() -> Arc<Self> {
        let packet = Self::ballast(cost::ENVELOPE);
        let _ = packet.encoded.set(Err("injected write failure".into()));
        packet
    }
    /// Ballast whose writer cannot finish encoding it until the returned sender
    /// is used or dropped, like a peer whose socket has stopped draining.
    #[cfg(test)]
    pub fn stall(bytes: usize) -> (Arc<Self>, std::sync::mpsc::Sender<()>) {
        let packet = Self::ballast(bytes);
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let (started_tx, started) = std::sync::mpsc::channel::<()>();
        let held = packet.clone();
        std::thread::spawn(move || {
            held.encoded.get_or_init(|| {
                let _ = started_tx.send(());
                let _ = wait.recv();
                serde_json::to_string(&held.message).map_err(|e| e.to_string())
            });
        });
        // Whoever encodes this packet later must find the holder already inside.
        started.recv().unwrap();
        (packet, release)
    }
}

pub(crate) struct Queued {
    pub packet: Arc<Packet>,
    used: Arc<AtomicUsize>,
}
impl Drop for Queued {
    fn drop(&mut self) {
        self.used.fetch_sub(self.packet.bytes, Ordering::AcqRel);
    }
}
pub(crate) struct Events {
    sender: mpsc::UnboundedSender<Queued>,
    used: Arc<AtomicUsize>,
}
pub(crate) fn channel() -> (Events, mpsc::UnboundedReceiver<Queued>) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (
        Events {
            sender,
            used: Arc::default(),
        },
        receiver,
    )
}
impl Events {
    pub fn available(&self, bytes: usize) -> bool {
        !self.sender.is_closed()
            && self.used.load(Ordering::Acquire).saturating_add(bytes) <= PEER_BACKLOG_BYTES
    }
    pub fn send(&self, packet: Arc<Packet>) -> bool {
        if !self.available(packet.bytes) {
            return false;
        }
        self.used.fetch_add(packet.bytes, Ordering::AcqRel);
        self.sender
            .send(Queued {
                packet,
                used: self.used.clone(),
            })
            .is_ok()
    }
    pub fn closed(&self) -> bool {
        self.sender.is_closed()
    }
    #[cfg(test)]
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backlog_is_bytes_and_inflight_packets_keep_their_reservation() {
        let (events, mut receiver) = channel();
        let packet = Arc::new(Packet {
            message: ServerMessage::Synced { request: 1 },
            bytes: PEER_BACKLOG_BYTES / 2,
            encoded: OnceLock::new(),
        });
        assert!(events.send(packet.clone()));
        assert!(events.send(packet.clone()));
        assert!(!events.send(packet.clone()));
        let inflight = receiver.try_recv().unwrap();
        assert!(!events.available(packet.bytes));
        drop(inflight);
        assert!(events.send(packet));
        let (events, mut receiver) = channel();
        for request in 0..1000 {
            assert!(events.send(Packet::new(ServerMessage::Synced { request })));
        }
        for request in 0..1000 {
            assert!(
                matches!(receiver.try_recv().unwrap().packet.message,ServerMessage::Synced {request:r} if r==request)
            );
        }
    }
}
