//! A hosted endpoint preserves the Workflow card's logical provider and server instance while
//! delegating its transport to the audited Comfy Cloud adapter. Pricing belongs to the binding.
use super::*;
use nodecraft_providers::{BoxFuture, JobHandle, JobProgress, ProviderError, ProviderJob, Submission, SubmitContext};

pub(crate) enum CatalogProvider {
    Local(Box<nodecraft_providers::comfyui::ComfyProvider>),
    Cloud(nodecraft_providers::comfy_cloud::ComfyCloud),
}
impl CatalogProvider {
    pub(crate) async fn catalog(&self) -> Result<Arc<nodecraft_comfy::NodeCatalog>, ProviderError> {
        match self {
            Self::Local(provider) => provider.catalog().await,
            Self::Cloud(provider) => provider.catalog().await,
        }
    }
}
fn cloud(env: &HostEnv<'_>) -> Result<nodecraft_providers::comfy_cloud::ComfyCloud, Readiness> {
    Ok(nodecraft_providers::comfy_cloud::ComfyCloud::new(needs_key(env.keys, "comfy-cloud")?, env.http.clone(), env.now_ms.clone()))
}
pub(crate) fn catalog_provider(env: &HostEnv<'_>) -> Result<CatalogProvider, Readiness> {
    if env.settings.default_comfy().is_some_and(crate::settings::ComfyServer::is_cloud) {
        cloud(env).map(CatalogProvider::Cloud)
    } else {
        comfyui(env).map(|provider| CatalogProvider::Local(Box::new(provider)))
    }
}
pub(super) fn make(server: &crate::settings::ComfyServer, env: &HostEnv<'_>) -> Result<Arc<dyn Provider>, Readiness> {
    if server.is_cloud() { Ok(Arc::new(CloudProfile(cloud(env)?))) } else { Ok(Arc::new(comfyui_server(env, server)?)) }
}
struct CloudProfile(nodecraft_providers::comfy_cloud::ComfyCloud);
impl Provider for CloudProfile {
    fn id(&self) -> &str {
        "comfyui"
    }
    fn metered_comfy_workflows(&self) -> bool {
        true
    }
    fn retry_ambiguous_submit(&self) -> bool {
        false
    }
    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        self.0.submit(job)
    }
    fn submit_with_delivery<'a>(&'a self, job: &'a ProviderJob, context: &'a SubmitContext) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        self.0.submit_with_delivery(job, context)
    }
    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        self.0.poll(handle)
    }
    fn cancel<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        self.0.cancel(handle)
    }
    fn poll_interval_ms(&self, progress: &JobProgress) -> u64 {
        self.0.poll_interval_ms(progress)
    }
}
