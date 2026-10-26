use crate::{NodeDescriptor, NodeKind};
use nodecraft_types::{Error, ErrorKind, PortType};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

fn shortcut_key(key: &str) -> String {
    key.chars().flat_map(char::to_lowercase).collect()
}

/// Kinds are shared, so a copy with other plugin kinds costs one count per kind.
#[derive(Clone, Default)]
pub struct Registry {
    kinds: BTreeMap<String, Arc<dyn NodeKind>>,
    public_errors: BTreeSet<String>,
}

impl Registry {
    pub fn builtins() -> Result<Self, Error> {
        let mut registry = Self::default();
        for kind in
            [crate::text::kind(), crate::image::kind(), crate::video::kind(), crate::audio::kind(), crate::mesh::kind_source(), crate::document::kind_source()]
        {
            registry.register(Box::new(kind))?;
        }
        registry.register(Box::new(crate::router::kind()))?;
        registry.register(Box::new(crate::batch::kind()))?;
        registry.register(Box::new(crate::folder::kind()))?;
        registry.register(Box::new(crate::note::kind()))?;
        registry.register(Box::new(crate::deck::kind()))?;
        registry.register(Box::new(crate::technique::kind()))?;
        registry.register(Box::new(crate::comfy_workflow::kind()))?;
        for kind in crate::action::kinds()? {
            registry.register(Box::new(kind))?;
        }
        for kind in crate::video_edit::kinds()? {
            registry.register(Box::new(kind))?;
        }
        for kind in crate::video_frames::kinds()? {
            registry.register(Box::new(kind))?;
        }
        for kind in crate::video_look::kinds()? {
            registry.register(Box::new(kind))?;
        }
        for kind in crate::video_split::kinds()? {
            registry.register(Box::new(kind))?;
        }
        registry.register(Box::new(crate::shader::kind()?))?;
        registry.register(Box::new(crate::mask::kind()))?;
        registry.register(Box::new(crate::mesh::convert_kind()))?;
        registry.register(Box::new(crate::mesh::photo_kind()?))?;
        registry.register(Box::new(crate::mesh::views_kind()?))?;
        registry.register(Box::new(crate::mesh::turntable::kind()?))?;
        registry.register(Box::new(crate::document::pages_kind()))?;
        registry.register(Box::new(crate::document::text_kind()))?;
        registry.register(Box::new(crate::layers::kind()))?;
        registry.register(Box::new(crate::timeline::kind()))?;
        registry.register(Box::new(crate::element::kind()))?;
        registry.public_errors.extend(registry.kinds.keys().cloned());
        Ok(registry)
    }

    /// Register the built-in, data-defined generation implementation. Accepting the concrete
    /// type keeps arbitrary native callbacks from claiming public-safe validation messages.
    pub fn register_generation(&mut self, kind: crate::generation::GenerationKind) -> Result<(), Error> {
        let id = kind.descriptor().id.clone();
        self.register(Box::new(kind))?;
        self.public_errors.insert(id);
        Ok(())
    }

    /// Whether validation text comes from an implementation this crate owns. Generic
    /// register/with_replaced callbacks remain private, even under a built-in kind's id.
    pub fn has_public_errors(&self, id: &str) -> bool {
        self.public_errors.contains(id)
    }

    pub fn register(&mut self, kind: Box<dyn NodeKind>) -> Result<(), Error> {
        let descriptor = kind.descriptor();
        descriptor.validate()?;
        kind.contract(descriptor).validate(descriptor)?;
        if self.kinds.contains_key(&descriptor.id) {
            return Err(Error::new(ErrorKind::Conflict, "node kind is already registered"));
        }
        if let Some(shortcut) = &descriptor.quick_add
            && self.kinds.values().any(|existing| existing.descriptor().quick_add.as_ref().is_some_and(|key| shortcut_key(key) == shortcut_key(shortcut)))
        {
            return Err(Error::new(ErrorKind::Conflict, "quick-add shortcut is already registered"));
        }
        self.kinds.insert(descriptor.id.clone(), Arc::from(kind));
        Ok(())
    }

    /// A copy whose catalog generation kinds are exactly `kinds` (#174: a catalog that gained or
    /// lost plugin models); refused whole if any is invalid.
    pub fn with_generation(&self, kinds: Vec<crate::generation::GenerationKind>) -> Result<Self, Error> {
        let keep = |id: &String| !crate::generation::is_generation(id);
        let mut next = Self {
            kinds: self.kinds.iter().filter(|(id, _)| keep(id)).map(|(id, k)| (id.clone(), Arc::clone(k))).collect(),
            public_errors: self.public_errors.iter().filter(|id| keep(id)).cloned().collect(),
        };
        for kind in kinds {
            next.register_generation(kind)?;
        }
        Ok(next)
    }

    /// A copy whose kinds under `prefix` are exactly `kinds`; refused whole if any kind is
    /// invalid, outside `prefix`, or clashes with another.
    pub fn with_replaced(&self, prefix: &str, kinds: Vec<Box<dyn NodeKind>>) -> Result<Self, Error> {
        let mut next = Self {
            kinds: self.kinds.iter().filter(|(id, _)| !id.starts_with(prefix)).map(|(id, k)| (id.clone(), Arc::clone(k))).collect(),
            public_errors: self.public_errors.iter().filter(|id| !id.starts_with(prefix)).cloned().collect(),
        };
        for kind in kinds {
            if !kind.descriptor().id.starts_with(prefix) {
                return Err(Error::new(ErrorKind::InvalidInput, "a plugin node kind must be named under its prefix"));
            }
            next.register(kind)?;
        }
        Ok(next)
    }

    pub fn get(&self, id: &str) -> Result<&dyn NodeKind, Error> {
        self.kinds.get(id).map(Arc::as_ref).ok_or_else(|| Error::new(ErrorKind::NotFound, "unknown node kind"))
    }

    /// The checked static contract of a registered kind.
    pub fn contract(&self, id: &str) -> Result<crate::NodeContract, Error> {
        let kind = self.get(id)?;
        let contract = kind.contract(kind.descriptor());
        contract.validate(kind.descriptor())?;
        Ok(contract)
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

#[cfg(test)]
mod tests {
    #[test]
    fn builtins_leave_prompt_overlay_letter_unbound() {
        let registry = super::Registry::builtins().unwrap();
        for kind in registry.descriptors() {
            assert!(
                kind.quick_add.as_deref().is_none_or(|key| !matches!(super::shortcut_key(key).as_str(), "h" | "shift+h")),
                "{} shadows the canvas overlay shortcut",
                kind.id
            );
        }
    }
}
