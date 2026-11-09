//! Soul IDs as provider-bound handles (#180, `docs/decisions/2026-11-04-soul-ids-as-styles.md`).
//!
//! A trained Soul ID is kept as a style descriptor of family `soul-2`, but unlike a style whose
//! weights are a file, it names an identity that only the Higgsfield account that trained it can
//! use. This module is what tells the two apart: whether a style is account-bound, the id it
//! names, whether the account whose list was last fetched has it, and how many such handles a
//! project holds. Exports refuse to write one out as a file ([`ACCOUNT_BOUND`]), the Library and
//! the share panel say what it is, and nothing here changes the document.
use nodecraft_format::AssetStore;
use nodecraft_graph::{Canvas, Project};
use nodecraft_nodes::generation::{MODEL, STYLE_KIND, is_generation};
use nodecraft_types::{AssetRef, OptionKey, OptionLists, OptionLookup, Value};

/// The style family of a trained Soul ID.
pub const FAMILY: &str = nodecraft_providers::higgsfield::SOUL_FAMILY;
/// The catalog model that takes a Soul ID, and the parameter that names it. Its option list is
/// the account's, shared by every model in [`SOUL_MODELS`].
pub const SOUL_MODEL: &str = "higgsfield.soul-2";
/// Every catalog model that takes a Soul ID: SOUL 2 text to image and image to image (#1043).
pub const SOUL_MODELS: [&str; 2] = [SOUL_MODEL, "higgsfield.soul-2-i2i"];

/// Whether catalog model `model` takes a Soul ID.
pub fn takes(model: &str) -> bool {
    SOUL_MODELS.contains(&model)
}
pub const PARAM: &str = "custom_reference_id";
/// Why a Soul ID is never written out as a file.
pub const ACCOUNT_BOUND: &str = "a Soul ID works only on the Higgsfield account that trained it: the project keeps a handle to that account's identity, \
     not a portable file. Share the project itself, or train the identity again on the other account";

/// Whether `style` is a Soul ID: a style descriptor of the Soul ID family. Read from its type, so
/// it holds even when the descriptor's bytes are missing.
pub fn bound(style: &AssetRef) -> bool {
    style.mime.essence() == nodecraft_types::STYLE_MIME && style.mime.param("family") == Some(FAMILY)
}

/// Whether `value` is a Soul ID descriptor.
pub fn bound_value(value: &Value) -> bool {
    matches!(value, Value::Media(asset) if bound(asset))
}

/// The id a Soul ID descriptor names, when its bytes are here and read as the trainer writes them.
pub fn id(assets: &AssetStore, style: &AssetRef) -> Option<String> {
    if !bound(style) {
        return None;
    }
    let decoded = nodecraft_media::style::decode(assets.get(&style.hash)?).ok()?;
    nodecraft_providers::higgsfield::soul_id(&decoded)
}

/// Whether the Higgsfield account whose Soul IDs were last fetched has an identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    /// It is on the fetched list: ready on this account.
    Listed,
    /// The fetched list doesn't have it: another account's, still training, failed or deleted.
    /// Cards keep it; a run is refused before anything is sent.
    Unlisted,
    /// No list has been fetched (no key, offline, or not refreshed yet). The adapter still checks
    /// it before paying.
    Unchecked,
}

impl Availability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Listed => "listed",
            Self::Unlisted => "unlisted",
            Self::Unchecked => "unchecked",
        }
    }
}

/// Where `id` stands against the lists the host fetched.
pub fn availability(lists: Option<&OptionLists>, id: &str) -> Availability {
    let key = OptionKey { owner: SOUL_MODEL, source: nodecraft_providers::higgsfield::SOUL_IDS, scope: None };
    match lists.and_then(|lists| lists.contains(key, id)) {
        Some(true) => Availability::Listed,
        Some(false) => Availability::Unlisted,
        None => Availability::Unchecked,
    }
}

/// Which fetch of the Soul ID list `lists` holds: a new value each time a list is installed, so
/// a view that shows availability knows to read it again (a refresh changes no revision).
pub fn lists_stamp(lists: Option<&OptionLists>) -> Option<nodecraft_types::OptionListId> {
    let key = OptionKey { owner: SOUL_MODEL, source: nodecraft_providers::higgsfield::SOUL_IDS, scope: None };
    lists.and_then(|lists| lists.view(key)).map(nodecraft_types::OptionList::identity)
}

/// The account-bound handles a project holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Handles {
    /// Train style cards whose active result is a Soul ID.
    pub identities: usize,
    /// SOUL 2 cards that name a Soul ID.
    pub uses: usize,
}

impl Handles {
    pub fn any(self) -> bool {
        self.identities > 0 || self.uses > 0
    }
}

/// Counts the Soul ID handles on every canvas of `project`.
pub fn handles(project: &Project) -> Handles {
    project.canvases().values().fold(Handles::default(), |found, canvas| {
        let here = canvas_handles(canvas);
        Handles { identities: found.identities.saturating_add(here.identities), uses: found.uses.saturating_add(here.uses) }
    })
}

/// Counts the Soul ID handles on one canvas, such as a technique version.
pub fn canvas_handles(canvas: &Canvas) -> Handles {
    let mut found = Handles::default();
    for node in canvas.nodes().values() {
        if node.kind == STYLE_KIND {
            if crate::style_cmds::active_style(node).is_some_and(bound) {
                found.identities = found.identities.saturating_add(1);
            }
        } else if is_generation(&node.kind)
            && matches!(node.params.get(MODEL), Some(Value::Text(model)) if takes(model))
            && matches!(node.params.get(PARAM), Some(Value::Text(id)) if !id.trim().is_empty())
        {
            found.uses = found.uses.saturating_add(1);
        }
    }
    found
}
