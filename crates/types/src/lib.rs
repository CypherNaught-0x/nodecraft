#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft types: ids, modalities, port types, values, hashes, money and the error base (pure data).
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

mod asset;
mod audio_use;
mod contract;
mod document_use;
mod error;
mod ids;
mod money;
mod provider;
mod scalar;
mod schema;
mod units;
mod value;
mod video_edit;

pub use asset::{AssetRef, Blake3, ColorSpace, MediaMeta, Mime};
pub use audio_use::{AudioDuration, AudioOverrun, AudioSound, AudioUse, MAX_AUDIO_MS, seconds};
pub use contract::{
    Affects, Check, Comparison, Condition, ConnectedText, Contract, ContractVersion, DefaultPolicy, DropReason, Dropped, Effective, HiddenPolicy, Issue,
    IssueCode, MAX_OPTION_BYTES, MAX_OPTION_LISTS, MAX_OPTIONS, Migration, NoOptions, OptionKey, OptionList, OptionListId, OptionLists, OptionLookup,
    OptionSource, ParamOptions, ParamOwner, ParamRule, ParamState, Resolution, Shared, TextPrecedence, Unit, compose_text, model_namespace, overlay,
    shared_value,
};
pub use document_use::{DocumentUse, MAX_DOCUMENT_PAGES, MAX_TOKENS_PER_PAGE};
pub use error::{Error, ErrorKind};
pub use ids::*;
pub use money::{Currency, Money, Rounding};
pub use provider::{EMPTY_TARGET_SELECTION, ProviderTarget, TargetChoice, TargetSelection};
pub use scalar::{Integer, Number, Unsigned};
pub use schema::{BorrowedResolvedParams, Choice, ParamControl, ParamSchema, ParamSpec, ParamValues, ResolvedParams, ValidatedParamSchema, VisibleIf};
pub use units::BillableUnits;
pub use value::{Modality, PortType, STYLE_MIME, Value, style_family};
pub use video_edit::{EditOperation, EditSound, MAX_EDIT_MS, VideoEdit};
