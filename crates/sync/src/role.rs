//! What a share link lets its holder do (#121). The room id is the owner's key: whoever has it
//! may do anything, including making and taking back links. A link carries one role.
use nodecraft_graph::validate_author;
use nodecraft_types::{Error, ErrorKind};
use serde::{Deserialize, Serialize};

/// What a peer may do in a room, from least to most.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// See the project, its discussions and its media; change nothing.
    View,
    /// See everything, and start, answer, edit and resolve discussions (only one's own messages
    /// may be edited or deleted).
    Comment,
    /// Change the project as the owner does.
    Edit,
    /// Joined by the room id: edits, and manages the room's links.
    Owner,
}

impl Role {
    /// May write the shared document directly.
    pub fn edits(self) -> bool {
        matches!(self, Self::Edit | Self::Owner)
    }
    /// May take part in discussions.
    pub fn comments(self) -> bool {
        self != Self::View
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Comment => "comment",
            Self::Edit => "edit",
            Self::Owner => "owner",
        }
    }
}

/// One link's terms, as the owner sets them and the relay keeps them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub role: Role,
    /// The person the link is for: their name on the discussions they write.
    pub name: String,
    /// When the link stops working, in seconds since 1970 (UTC); none for never.
    #[serde(default)]
    pub expires: Option<u64>,
}

impl Grant {
    pub fn validate(&self) -> Result<(), Error> {
        if self.role == Role::Owner {
            return Err(Error::new(ErrorKind::InvalidInput, "a share link can't make its holder the owner"));
        }
        validate_author(&self.name)
    }
    /// Still good at `now` (seconds since 1970).
    pub fn live(&self, now: u64) -> bool {
        self.expires.is_none_or(|expires| now < expires)
    }
}
