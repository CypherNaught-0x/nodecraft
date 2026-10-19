use crate::{NodeDescriptor, NodeKind};
use nodecraft_types::{Error, ErrorKind, PortType};
use std::collections::BTreeMap;

fn shortcut_key(key: &str) -> String {
    key.chars().flat_map(char::to_lowercase).collect()
}

#[derive(Default)]
pub struct Registry {
    kinds: BTreeMap<String, Box<dyn NodeKind>>,
}

impl Registry {
    pub fn builtins() -> Result<Self, Error> {
        let mut registry = Self::default();
        for kind in [crate::text::kind(), crate::image::kind(), crate::video::kind(), crate::audio::kind()] {
            registry.register(Box::new(kind))?;
        }
        registry.register(Box::new(crate::router::kind()))?;
        registry.register(Box::new(crate::note::kind()))?;
        for kind in crate::action::kinds()? {
            registry.register(Box::new(kind))?;
        }
        Ok(registry)
    }

    pub fn register(&mut self, kind: Box<dyn NodeKind>) -> Result<(), Error> {
        let descriptor = kind.descriptor();
        descriptor.validate()?;
        if self.kinds.contains_key(&descriptor.id) {
            return Err(Error::new(ErrorKind::Conflict, "node kind is already registered"));
        }
        if let Some(shortcut) = &descriptor.quick_add
            && self.kinds.values().any(|existing| existing.descriptor().quick_add.as_ref().is_some_and(|key| shortcut_key(key) == shortcut_key(shortcut)))
        {
            return Err(Error::new(ErrorKind::Conflict, "quick-add shortcut is already registered"));
        }
        self.kinds.insert(descriptor.id.clone(), kind);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<&dyn NodeKind, Error> {
        self.kinds.get(id).map(Box::as_ref).ok_or_else(|| Error::new(ErrorKind::NotFound, "unknown node kind"))
    }

    pub fn descriptors(&self) -> impl Iterator<Item = &NodeDescriptor> {
        self.kinds.values().map(|kind| kind.descriptor())
    }

    /// The Turn-into picker uses the same mode predicate as planning. Source kinds appear for
    /// an empty canvas selection, never as a destination for an existing wire.
    pub fn compatible(&self, inputs: &[PortType]) -> impl Iterator<Item = &NodeDescriptor> {
        self.kinds.values().filter(move |kind| kind.mode(inputs).is_some()).map(|kind| kind.descriptor())
    }
}
