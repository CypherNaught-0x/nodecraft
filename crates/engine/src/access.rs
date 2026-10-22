//! What this session may do to its document (#121). A local project, and the owner of a shared
//! one, may do anything. A session that joined through a share link takes the link's role: a
//! viewer changes nothing, and a commenter changes only discussions. Remote changes from the
//! shared document are not checked here: the relay checked them.
use nodecraft_graph::{Change, Changeset, validate_author};
use nodecraft_sync::Role;
use nodecraft_types::{Error, ErrorKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Access {
    pub role: Role,
    /// The name this session's discussions are written under; none for a local note.
    author: Option<String>,
}

impl Default for Access {
    fn default() -> Self {
        Self::owner()
    }
}

impl Access {
    /// Anything goes: a local project, or the owner of a shared one.
    pub fn owner() -> Self {
        Self { role: Role::Owner, author: None }
    }

    pub fn new(role: Role, author: Option<String>) -> Result<Self, Error> {
        if let Some(author) = &author {
            validate_author(author)?;
        }
        if role == Role::Comment && author.is_none() {
            return Err(Error::new(ErrorKind::InvalidInput, "a commenter writes under a name"));
        }
        Ok(Self { role, author })
    }

    pub fn author(&self) -> Option<&str> {
        self.author.as_deref()
    }

    /// Refused unless this session may make any change at all.
    pub fn changes(&self) -> Result<(), Error> {
        if self.role.comments() { Ok(()) } else { Err(view_only()) }
    }

    /// Refused unless this session may make every change in `changes`.
    pub fn permits(&self, changes: &Changeset) -> Result<(), Error> {
        match self.role {
            Role::Edit | Role::Owner => Ok(()),
            Role::View => Err(view_only()),
            Role::Comment if changes.changes.iter().all(discussion) => Ok(()),
            Role::Comment => Err(Error::new(ErrorKind::Forbidden, "this share link lets you discuss the project, not edit it")),
        }
    }

    /// Refused unless this session may start runs, which write their results into the project.
    pub fn runs(&self) -> Result<(), Error> {
        if self.role.edits() { Ok(()) } else { Err(Error::new(ErrorKind::Forbidden, "this share link doesn't let you run cards")) }
    }

    /// Whether this session may edit, move or delete a message written by `author`: its own, or
    /// (to delete or move one) anyone's when it may edit the project.
    pub fn owns(&self, author: Option<&str>) -> bool {
        author == self.author.as_deref()
    }
}

fn view_only() -> Error {
    Error::new(ErrorKind::Forbidden, "this share link only lets you view the project")
}

fn discussion(change: &Change) -> bool {
    matches!(
        change,
        Change::PutComment { .. }
            | Change::SetCommentText { .. }
            | Change::SetCommentPin { .. }
            | Change::SetCommentResolved { .. }
            | Change::InsertCommentReply { .. }
            | Change::RemoveCommentReply { .. }
            | Change::SetCommentReplyText { .. }
            | Change::RemoveComment { .. }
    )
}
