//! One live host for every command that acts on the window's view rather than only the document
//! (#360): navigation, search, the tag filter, the discussions panel and Format selection. The
//! control channel and the MCP bridge route these ids here, so an agent reaches the same scoped
//! capabilities a button does, refreshed for the one call.
use super::*;
use serde_json::Value as Json;

impl NodeCraftApp {
    /// Whether `id` needs this live host. Routing metadata only: automation still applies its
    /// own classification, and the engine its own enabled checks.
    pub fn is_host_command(id: &str) -> bool {
        Self::is_navigation_command(id) || Self::is_search_command(id) || tags::is_command(id) || comments::view_command(id) || id == LAYOUT
    }
    /// Fresh availability of a host command, with its capability scoped to this one query.
    pub fn host_enabled(&mut self, ctx: &egui::Context, id: &str, params: &Json) -> Result<(), Error> {
        if Self::is_navigation_command(id) {
            self.navigation_enabled(ctx, id, params)
        } else if Self::is_search_command(id) {
            self.search_enabled(ctx, id, params)
        } else if tags::is_command(id) {
            self.tag_filter_enabled(ctx, id, params)
        } else if comments::view_command(id) {
            self.comments_enabled(ctx, id, params)
        } else if id == LAYOUT {
            self.layout_enabled(ctx, params)
        } else {
            Err(Error::new(ErrorKind::InvalidInput, "not a live view command"))
        }
    }
    /// Dispatch a host command through the same engine path its button uses.
    pub fn dispatch_host(&mut self, ctx: &egui::Context, id: &str, params: &Json) -> Result<Json, Error> {
        if Self::is_navigation_command(id) {
            self.dispatch_navigation(ctx, id, params)
        } else if Self::is_search_command(id) {
            self.dispatch_search(ctx, id, params)
        } else if tags::is_command(id) {
            self.dispatch_tag_filter(ctx, id, params)
        } else if comments::view_command(id) {
            self.dispatch_comments(ctx, id, params)
        } else if id == LAYOUT {
            self.dispatch_layout(ctx, params)
        } else {
            Err(Error::new(ErrorKind::InvalidInput, "not a live view command"))
        }
    }
    /// What a host command would do. Only Format selection edits the document; the rest change
    /// what the window shows, which is not the document.
    pub fn host_impact(&mut self, ctx: &egui::Context, id: &str, params: &Json) -> Result<nodecraft_engine::impact::Impact, Error> {
        if id == LAYOUT {
            self.layout_impact(ctx, params)
        } else {
            self.host_enabled(ctx, id, params)?;
            Ok(nodecraft_engine::impact::Impact::Read)
        }
    }
}

const LAYOUT: &str = "canvas.auto_format";
