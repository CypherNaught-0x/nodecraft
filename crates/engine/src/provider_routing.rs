//! Host endpoint authority. Bindings never retain their registry or another binding.
use nodecraft_providers::Provider;
use nodecraft_types::{Error, ErrorKind, NodeId, ProviderInstanceId, ProviderTarget, TargetSelection};
use std::{collections::BTreeMap, mem::size_of, sync::Arc};
const LIMIT: usize = 64 * 1024 * 1024;
const COUNT: usize = nodecraft_exec::run::MAX_RUN_JOBS;
fn invalid() -> Error {
    Error::new(ErrorKind::InvalidInput, "provider routing is missing, duplicate or mismatched")
}
fn cap() -> Error {
    Error::new(ErrorKind::Budget, "provider routing ownership exceeds its limit")
}
struct Binding {
    target: ProviderTarget,
    provider: Arc<dyn Provider>,
    serial: [u8; 32],
    /// Most of a run's jobs sent to this target at once; `None` keeps the scheduler's default.
    limit: Option<usize>,
    /// Hosted execution is paid even when the logical ComfyUI catalog model is local/free.
    metered_comfy: bool,
}
/// Captured immutable physical adapter and its typed logical identity.
#[derive(Clone)]
pub struct RegisteredProvider(Arc<Binding>);
impl RegisteredProvider {
    pub fn default_instance(provider: Arc<dyn Provider>) -> Result<Self, Error> {
        Self::new(provider, None)
    }
    pub fn named(provider: Arc<dyn Provider>, instance: ProviderInstanceId) -> Result<Self, Error> {
        Self::new(provider, Some(instance))
    }
    fn new(provider: Arc<dyn Provider>, instance: Option<ProviderInstanceId>) -> Result<Self, Error> {
        let target = ProviderTarget::new(provider.id(), instance)?;
        let serial = crate::execution_identity::nonce()?;
        let metered_comfy = provider.metered_comfy_workflows();
        Ok(Self(Arc::new(Binding { target, provider, serial, limit: None, metered_comfy })))
    }
    /// A named instance that runs at most `limit` of a run's jobs at once (#864), such as a
    /// ComfyUI server with one GPU.
    pub fn named_with_limit(provider: Arc<dyn Provider>, instance: ProviderInstanceId, limit: Option<usize>) -> Result<Self, Error> {
        let target = ProviderTarget::new(provider.id(), Some(instance))?;
        let serial = crate::execution_identity::nonce()?;
        let metered_comfy = provider.metered_comfy_workflows();
        Ok(Self(Arc::new(Binding { target, provider, serial, limit: limit.map(|n| n.max(1)), metered_comfy })))
    }
    pub(crate) fn metered_comfy(&self) -> bool {
        self.0.metered_comfy
    }
    /// Most of a run's jobs sent to this target at once, when it sets its own.
    pub fn limit(&self) -> Option<usize> {
        self.0.limit
    }
    pub fn target(&self) -> &ProviderTarget {
        &self.0.target
    }
    pub fn provider(&self) -> &Arc<dyn Provider> {
        &self.0.provider
    }
    /// Same captured registration, including its immutable binding incarnation.
    pub fn same_binding(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) && self.0.serial == other.0.serial
    }
    pub(crate) fn retained_bytes(&self) -> usize {
        size_of::<Self>() + size_of::<Binding>() + 2 * size_of::<usize>() + self.target().adapter().len()
    }
}
/// Mutable host builder. Session installation consumes it and mints a fresh routing nonce.
/// Configuration ownership is separately bounded; preparation retains selected bindings only.
pub struct ProviderRouting {
    bindings: BTreeMap<String, BTreeMap<Option<ProviderInstanceId>, RegisteredProvider>>,
    targets: TargetSelection,
    bytes: usize,
    count: usize,
}
impl Default for ProviderRouting {
    fn default() -> Self {
        Self::new()
    }
}
impl ProviderRouting {
    pub const fn new() -> Self {
        Self { bindings: BTreeMap::new(), targets: TargetSelection::empty(), bytes: size_of::<RoutingAuthority>(), count: 0 }
    }
    fn bind(&mut self, binding: RegisteredProvider) -> Result<RegisteredProvider, Error> {
        let adapter = binding.target().adapter();
        let instance = binding.target().instance();
        if self.bindings.get(adapter).is_some_and(|instances| instances.contains_key(&instance)) {
            return Err(invalid());
        }
        let new_adapter = !self.bindings.contains_key(adapter);
        // Registry payload/link ownership, with a single compact outer adapter key.
        let mut charge = binding.retained_bytes() + size_of::<(Option<ProviderInstanceId>, RegisteredProvider)>() - size_of::<RegisteredProvider>() + 48;
        if new_adapter {
            charge =
                charge.checked_add(size_of::<(String, BTreeMap<Option<ProviderInstanceId>, RegisteredProvider>)>() + 48 + adapter.len()).ok_or_else(cap)?;
        }
        let bytes = self.bytes.checked_add(charge).ok_or_else(cap)?;
        if self.count.saturating_add(self.targets.len()) >= COUNT || bytes > LIMIT {
            return Err(cap());
        }
        if let Some(instances) = self.bindings.get_mut(adapter) {
            instances.insert(instance, binding.clone());
        } else {
            let mut key = String::new();
            key.try_reserve_exact(adapter.len()).map_err(|_| cap())?;
            if key.capacity() > nodecraft_exec::cost::MAX_TEXT {
                return Err(cap());
            }
            key.push_str(adapter);
            self.bindings.insert(key, BTreeMap::from([(instance, binding.clone())]));
        }
        self.count += 1;
        self.bytes = bytes;
        Ok(binding)
    }
    pub fn bind_default(&mut self, provider: Arc<dyn Provider>) -> Result<RegisteredProvider, Error> {
        self.bind(RegisteredProvider::default_instance(provider)?)
    }
    pub fn bind_named(&mut self, provider: Arc<dyn Provider>, instance: ProviderInstanceId) -> Result<RegisteredProvider, Error> {
        self.bind(RegisteredProvider::named(provider, instance)?)
    }
    /// [`Self::bind_named`] with a cap on the jobs a run sends it at once.
    pub fn bind_named_with_limit(
        &mut self,
        provider: Arc<dyn Provider>,
        instance: ProviderInstanceId,
        limit: Option<usize>,
    ) -> Result<RegisteredProvider, Error> {
        self.bind(RegisteredProvider::named_with_limit(provider, instance, limit)?)
    }
    pub fn select(&mut self, node: NodeId, adapter: &str, target: &ProviderTarget) -> Result<(), Error> {
        if adapter != target.adapter() || self.find_parts(target.adapter(), target.instance()).is_err() {
            return Err(invalid());
        }
        let bytes = self.bytes.checked_add(size_of::<nodecraft_types::TargetChoice>() + adapter.len()).ok_or_else(cap)?;
        if bytes > LIMIT || self.count.saturating_add(self.targets.len()) >= COUNT {
            return Err(cap());
        }
        self.targets.insert(node, target)?;
        self.bytes = bytes;
        Ok(())
    }
    pub(crate) fn targets(&self) -> &TargetSelection {
        &self.targets
    }
    pub(crate) fn find_parts(&self, adapter: &str, instance: Option<ProviderInstanceId>) -> Result<&RegisteredProvider, Error> {
        self.bindings.get(adapter).and_then(|instances| instances.get(&instance)).ok_or_else(invalid)
    }
}
#[derive(Default)]
pub(crate) struct RoutingAuthority {
    pub(crate) routing: ProviderRouting,
    pub(crate) nonce: Option<[u8; 32]>,
}
impl RoutingAuthority {
    pub(crate) fn bind(&self, hash: &mut blake3::Hasher) {
        hash.update(&self.nonce.unwrap_or([0; 32]));
    }
}
