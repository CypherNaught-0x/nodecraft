//! A commenter's change to one discussion, as the relay applies it (#121). The commenter says
//! what the thread was when they changed it and what they made of it; the relay applies only
//! that difference to the thread as it is now, so a reply someone else sent meanwhile is kept.
//! Each part of the difference must be the commenter's to make: their own new messages, edits
//! and deletions of their own messages, moving or deleting a thread they started, and resolving
//! or reopening any thread.
use nodecraft_graph::{Comment, CommentReply};

/// What to write: `None` when the change changes nothing now, `Some(None)` to delete.
pub type Merged = Option<Option<Comment>>;

pub fn merge(name: &str, now: Option<&Comment>, base: Option<&Comment>, made: Option<&Comment>) -> Result<Merged, &'static str> {
    let mine = |author: &Option<String>| author.as_deref() == Some(name);
    match (base, made) {
        (None, None) => Ok(None),
        (None, Some(made)) => {
            if !mine(&made.author) || !made.replies.iter().all(|reply| mine(&reply.author)) {
                return Err("a new discussion carries its writer's name");
            }
            match now {
                None => Ok(Some(Some(made.clone()))),
                // Their own thread is there already: this is a put sent again after a reconnect,
                // and what others added since stays.
                Some(now) if now.author == made.author => Ok(None),
                Some(_) => Err("that discussion already exists"),
            }
        }
        (Some(_), None) => match now {
            None => Ok(None),
            Some(now) if mine(&now.author) => Ok(Some(None)),
            Some(_) => Err("only who started a discussion may delete it"),
        },
        (Some(base), Some(made)) => {
            let Some(now) = now else { return Ok(None) };
            let mut next = now.clone();
            if made.author != base.author {
                return Err("a message keeps its writer");
            }
            if made.text != base.text || made.pos != base.pos || made.node != base.node {
                if !mine(&now.author) {
                    return Err("only its writer may edit or move a discussion");
                }
                if made.text != base.text {
                    next.text.clone_from(&made.text);
                }
                if made.pos != base.pos || made.node != base.node {
                    next.pos = made.pos;
                    next.node = made.node;
                }
            }
            if made.resolved != base.resolved {
                next.resolved = made.resolved;
            }
            for old in &base.replies {
                match made.replies.iter().find(|reply| reply.id == old.id) {
                    Some(new) if new == old => {}
                    Some(new) => {
                        if new.author != old.author {
                            return Err("a message keeps its writer");
                        }
                        if let Some(at) = next.replies.iter_mut().find(|reply| reply.id == old.id) {
                            if !mine(&at.author) {
                                return Err("only its writer may edit a reply");
                            }
                            at.text.clone_from(&new.text);
                        }
                    }
                    None => {
                        if let Some(at) = next.replies.iter().position(|reply| reply.id == old.id) {
                            if !next.replies.get(at).is_some_and(|reply| mine(&reply.author)) {
                                return Err("only its writer may delete a reply");
                            }
                            next.replies.remove(at);
                        }
                    }
                }
            }
            for new in made.replies.iter().filter(|reply| !base.replies.iter().any(|old| old.id == reply.id)) {
                if !mine(&new.author) {
                    return Err("a new reply carries its writer's name");
                }
                if !next.replies.iter().any(|reply| reply.id == new.id) {
                    next.replies.push(CommentReply { id: new.id, text: new.text.clone(), author: new.author.clone() });
                }
            }
            if next == *now {
                return Ok(None);
            }
            next.validate_payload().map_err(|_| "the discussion would grow too large")?;
            Ok(Some(Some(next)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_graph::Position;
    use nodecraft_types::CommentId;

    fn id(n: u8) -> CommentId {
        CommentId::from_bytes([n; 16])
    }

    fn thread(author: &str) -> Comment {
        let mut comment = Comment::new(id(1), "Look here", Position { x: 0.0, y: 0.0 });
        comment.author = Some(author.into());
        comment
    }

    fn reply(n: u8, author: &str, text: &str) -> CommentReply {
        CommentReply { id: id(n), text: text.into(), author: Some(author.into()) }
    }

    #[test]
    fn a_commenter_starts_threads_in_their_own_name_only() {
        assert_eq!(merge("ana", None, None, Some(&thread("ana"))).unwrap(), Some(Some(thread("ana"))));
        assert!(merge("ana", None, None, Some(&thread("bo"))).is_err());
        let mut unsigned = thread("ana");
        unsigned.author = None;
        assert!(merge("ana", None, None, Some(&unsigned)).is_err());
    }

    #[test]
    fn a_new_thread_sent_again_takes_nothing_from_what_came_since() {
        let mut now = thread("ana");
        now.replies.push(reply(2, "bo", "Agreed"));
        assert_eq!(merge("ana", Some(&now), None, Some(&thread("ana"))).unwrap(), None);
        assert!(merge("cy", Some(&now), None, Some(&thread("cy"))).is_err());
    }

    #[test]
    fn a_reply_sent_meanwhile_is_kept() {
        let base = thread("bo");
        let mut now = base.clone();
        now.replies.push(reply(2, "cy", "Agreed"));
        let mut made = base.clone();
        made.replies.push(reply(3, "ana", "Me too"));
        let merged = merge("ana", Some(&now), Some(&base), Some(&made)).unwrap().unwrap().unwrap();
        assert_eq!(merged.replies.iter().map(|r| r.id).collect::<Vec<_>>(), vec![id(2), id(3)]);
    }

    #[test]
    fn only_a_writer_edits_or_deletes_their_messages() {
        let mut base = thread("bo");
        base.replies.push(reply(2, "cy", "Agreed"));
        let mut edited = base.clone();
        edited.text = "Changed".into();
        assert!(merge("ana", Some(&base), Some(&base), Some(&edited)).is_err());
        assert!(merge("bo", Some(&base), Some(&base), Some(&edited)).unwrap().is_some());
        let mut gone = base.clone();
        gone.replies.clear();
        assert!(merge("ana", Some(&base), Some(&base), Some(&gone)).is_err());
        assert!(merge("cy", Some(&base), Some(&base), Some(&gone)).unwrap().is_some());
        assert!(merge("ana", Some(&base), Some(&base), None).is_err());
        assert_eq!(merge("bo", Some(&base), Some(&base), None).unwrap(), Some(None));
        // Claiming another's message as one's own is refused too.
        let mut stolen = base.clone();
        stolen.author = Some("ana".into());
        assert!(merge("ana", Some(&base), Some(&base), Some(&stolen)).is_err());
    }

    #[test]
    fn anyone_may_resolve_and_a_gone_thread_takes_nothing() {
        let base = thread("bo");
        let mut resolved = base.clone();
        resolved.resolved = true;
        assert_eq!(merge("ana", Some(&base), Some(&base), Some(&resolved)).unwrap(), Some(Some(resolved.clone())));
        assert_eq!(merge("ana", None, Some(&base), Some(&resolved)).unwrap(), None);
        assert_eq!(merge("ana", Some(&resolved), Some(&base), Some(&resolved)).unwrap(), None);
    }
}
