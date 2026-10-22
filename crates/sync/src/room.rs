//! One shared project at the relay: the merged document, so a late joiner gets all of it, and the
//! assets its cards use, passed from the peers that have them to the peers that ask. The room
//! never reads the project; it only merges and forwards. Every byte it keeps is capped.
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};

use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, ReadTxn, StateVector, Transact, Update};

use crate::frame::{Frame, MAX_UPDATE_BYTES};

/// One connection, numbered by the relay.
pub type Peer = u64;

/// The most a room merges in all, so one room can't fill the relay's memory.
const MAX_ROOM_UPDATE_BYTES: usize = 512 << 20;
/// The most asset bytes a room keeps.
const MAX_ROOM_ASSET_BYTES: usize = 512 << 20;
/// The most peers in one room.
pub const MAX_PEERS: usize = 32;
/// The most assets asked for and not yet sent; later asks wait until some arrive.
const MAX_WANTED: usize = 4096;

/// What the relay should do after a frame.
#[derive(Debug, PartialEq, Eq)]
pub enum Out {
    Send(Peer, Frame),
    /// Send the frame, then hang up on the peer.
    Close(Peer, Frame),
}

pub struct Room {
    doc: Doc,
    peers: BTreeSet<Peer>,
    merged: usize,
    assets: BTreeMap<[u8; 32], Vec<u8>>,
    asset_bytes: usize,
    /// Who asked for an asset nobody has sent yet.
    wanted: BTreeMap<[u8; 32], BTreeSet<Peer>>,
    damaged: bool,
}

impl Default for Room {
    fn default() -> Self {
        Self::new()
    }
}

impl Room {
    pub fn new() -> Self {
        Self { doc: Doc::new(), peers: BTreeSet::new(), merged: 0, assets: BTreeMap::new(), asset_bytes: 0, wanted: BTreeMap::new(), damaged: false }
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// The bytes the room has taken in: merged updates and kept assets.
    pub fn held(&self) -> usize {
        self.merged.saturating_add(self.asset_bytes)
    }

    /// Lets `peer` in, and asks what it has: its answer brings in what it changed while away.
    pub fn join(&mut self, peer: Peer) -> Vec<Out> {
        if self.peers.len() >= MAX_PEERS {
            return vec![Out::Close(peer, Frame::Refused("the room is full".into()))];
        }
        match self.state_vector() {
            Some(sv) => {
                self.peers.insert(peer);
                let mut out = vec![Out::Send(peer, Frame::Step1(sv))];
                // Whoever asked for an asset may be waiting on someone who left: the newcomer
                // may have it.
                let wanted: Vec<[u8; 32]> = self.wanted.keys().copied().collect();
                if !wanted.is_empty() {
                    out.push(Out::Send(peer, Frame::AssetGet(wanted)));
                }
                out
            }
            None => vec![Out::Close(peer, Frame::Refused(DAMAGED.into()))],
        }
    }

    pub fn leave(&mut self, peer: Peer) {
        self.peers.remove(&peer);
        for askers in self.wanted.values_mut() {
            askers.remove(&peer);
        }
        self.wanted.retain(|_, askers| !askers.is_empty());
    }

    /// Takes one frame from `from`, a peer in the room.
    pub fn handle(&mut self, from: Peer, frame: Frame) -> Vec<Out> {
        if !self.peers.contains(&from) {
            return vec![Out::Close(from, Frame::Refused("not in the room".into()))];
        }
        match frame {
            Frame::Step1(sv) => match self.update_for(&sv) {
                Some(update) if update.len() > MAX_UPDATE_BYTES => self.refuse(from, "the project is too large to send"),
                Some(update) => vec![Out::Send(from, Frame::Step2(update))],
                None => self.refuse(from, "the state vector isn't readable"),
            },
            Frame::Step2(update) | Frame::Update(update) => {
                if self.merged.saturating_add(update.len()) > MAX_ROOM_UPDATE_BYTES {
                    return self.refuse(from, "the room has grown too large");
                }
                if let Err(message) = self.merge(&update) {
                    return self.refuse(from, message);
                }
                self.merged = self.merged.saturating_add(update.len());
                self.others(from).map(|peer| Out::Send(peer, Frame::Update(update.clone()))).collect()
            }
            Frame::AssetPut { hash, bytes } => {
                // The frame checked the hash when it was read.
                let mut out: Vec<Out> = self
                    .wanted
                    .remove(&hash)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|peer| *peer != from && self.peers.contains(peer))
                    .map(|peer| Out::Send(peer, Frame::AssetPut { hash, bytes: bytes.clone() }))
                    .collect();
                if !self.assets.contains_key(&hash) && self.asset_bytes.saturating_add(bytes.len()) <= MAX_ROOM_ASSET_BYTES {
                    self.asset_bytes = self.asset_bytes.saturating_add(bytes.len());
                    self.assets.insert(hash, bytes);
                } else if out.is_empty() && !self.assets.contains_key(&hash) {
                    out.push(Out::Send(from, Frame::Refused("the room keeps no more assets".into())));
                }
                out
            }
            Frame::AssetGet(hashes) => {
                let mut out = Vec::new();
                let mut missing = Vec::new();
                for hash in hashes {
                    match self.assets.get(&hash) {
                        Some(bytes) => out.push(Out::Send(from, Frame::AssetPut { hash, bytes: bytes.clone() })),
                        None if self.wanted.len() < MAX_WANTED || self.wanted.contains_key(&hash) => {
                            self.wanted.entry(hash).or_default().insert(from);
                            missing.push(hash);
                        }
                        None => {}
                    }
                }
                if !missing.is_empty() {
                    let others: Vec<Peer> = self.others(from).collect();
                    out.extend(others.into_iter().map(|peer| Out::Send(peer, Frame::AssetGet(missing.clone()))));
                }
                out
            }
            Frame::Hello { .. } | Frame::Refused(_) => self.refuse(from, "a frame a peer doesn't send"),
        }
    }

    fn others(&self, from: Peer) -> impl Iterator<Item = Peer> + '_ {
        self.peers.iter().copied().filter(move |peer| *peer != from)
    }

    fn refuse(&mut self, peer: Peer, message: &str) -> Vec<Out> {
        self.leave(peer);
        vec![Out::Close(peer, Frame::Refused(message.into()))]
    }

    fn state_vector(&self) -> Option<Vec<u8>> {
        if self.damaged {
            return None;
        }
        Some(self.doc.try_transact().ok()?.state_vector().encode_v1())
    }

    fn update_for(&self, sv: &[u8]) -> Option<Vec<u8>> {
        if self.damaged {
            return None;
        }
        let sv = StateVector::decode_v1(sv).ok()?;
        Some(self.doc.try_transact().ok()?.encode_diff_v1(&sv))
    }

    fn merge(&mut self, update: &[u8]) -> Result<(), &'static str> {
        if self.damaged {
            return Err(DAMAGED);
        }
        let update = Update::decode_v1(update).map_err(|_| "the update isn't readable")?;
        let doc = &self.doc;
        let merged = catch_unwind(AssertUnwindSafe(|| {
            let mut txn = doc.try_transact_mut().map_err(|_| "the room is busy")?;
            txn.apply_update(update).map_err(|_| "the update doesn't fit the room")
        }));
        merged.unwrap_or_else(|_| {
            self.damaged = true;
            Err(DAMAGED)
        })
    }
}

const DAMAGED: &str = "the room is damaged: share the project again";

#[cfg(test)]
mod tests;
