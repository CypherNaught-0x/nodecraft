//! Who else is in a shared project, and where (#120): each peer tells the relay which canvas it
//! has open, where its pointer is on it and which cards it has selected; the relay names the
//! peer and passes that on to everyone else. None of it is part of the project: it is never
//! merged, saved or undone, and a peer that leaves takes it with it.
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId};
use serde::{Deserialize, Serialize};

use crate::role::Role;

/// The most selected cards one peer shows the others; past it the rest go unshown.
pub const MAX_SELECTION: usize = 256;
/// The farthest a pointer may be from the canvas origin, in canvas points.
const MAX_COORD: f32 = 1.0e7;

/// Where one peer is: the canvas it has open, its pointer on that canvas (in canvas points at
/// 100% zoom) and the cards it has selected there.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Presence {
    #[serde(default)]
    pub canvas: Option<CanvasId>,
    #[serde(default)]
    pub cursor: Option<[f32; 2]>,
    #[serde(default)]
    pub selection: Vec<NodeId>,
}

impl Presence {
    /// Refuses a pointer that isn't a finite point near the canvas, more than
    /// [`MAX_SELECTION`] selected cards, and a pointer or selection with no canvas.
    pub fn validate(&self) -> Result<(), Error> {
        if self.cursor.is_some_and(|point| point.iter().any(|c| !c.is_finite() || c.abs() > MAX_COORD)) {
            return Err(Error::new(ErrorKind::InvalidInput, "the pointer isn't on the canvas"));
        }
        if self.selection.len() > MAX_SELECTION {
            return Err(Error::new(ErrorKind::Overflow, "too many selected cards to show"));
        }
        if self.canvas.is_none() && (self.cursor.is_some() || !self.selection.is_empty()) {
            return Err(Error::new(ErrorKind::InvalidInput, "a pointer or selection needs a canvas"));
        }
        Ok(())
    }
}

/// Another peer in the room, as the relay names it: the share link's name, or "Owner" for
/// whoever joined by the room id. `peer` tells two people of the same name apart.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Person {
    pub peer: u64,
    pub name: String,
    pub role: Role,
    pub presence: Presence,
}

/// What the owner is called to the others: they joined by the room id, which names nobody.
pub const OWNER_NAME: &str = "Owner";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_presence_must_be_a_finite_point_and_a_bounded_selection_on_a_canvas() {
        let canvas = Some(CanvasId::from_bytes([1; 16]));
        assert!(Presence::default().validate().is_ok());
        assert!(Presence { canvas, cursor: Some([1.0, -2.0]), selection: vec![NodeId::from_bytes([2; 16])] }.validate().is_ok());
        for cursor in [[f32::NAN, 0.0], [0.0, f32::INFINITY], [2.0e7, 0.0]] {
            assert!(Presence { canvas, cursor: Some(cursor), ..Presence::default() }.validate().is_err());
        }
        assert!(Presence { canvas, selection: vec![NodeId::from_bytes([2; 16]); MAX_SELECTION + 1], ..Presence::default() }.validate().is_err());
        assert!(Presence { cursor: Some([0.0, 0.0]), ..Presence::default() }.validate().is_err());
        assert!(Presence { selection: vec![NodeId::from_bytes([2; 16])], ..Presence::default() }.validate().is_err());
    }
}
