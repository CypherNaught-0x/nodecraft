//! Run-time option lists consulted when a run is planned (#162): the checkpoints a ComfyUI server
//! has installed, the voices an account owns. The host fetches the lists and installs them in the
//! session as `OptionLists`; planning only reads what it has.
//!
//! The rule is not decided here. It is `Contract::resolve_with` asked with `ready` set, which is
//! the same resolver the edit, bulk-edit, describe and `node.options` paths use, so a parameter
//! that draws from no list while it is hidden or disabled, and a scoped list keyed by its scope
//! value, mean one thing everywhere. What planning adds is only the consequence: a value off a
//! *known* list blocks its node before anything is admitted, quoted or charged.
//!
//! Unknown is not empty. A list the host has not fetched, could not fetch, or whose scope
//! parameter has no value refuses nothing, so a run is never refused against a list the host
//! never refreshed and the provider stays the authority when the job is submitted.
//!
//! The cost is bounded and usually zero: a step whose contract names no list stops at one pass
//! over its rules. A step that does name one pays a second resolution next to the one
//! [`NodeKind::validate_instance`] already made (`resolve_with` validates its own groups through
//! [`NodeContract::resolve_with`]), which is a pass over that node's parameters, not over the
//! canvas, and is paid once per option-sourced step per plan.

use nodecraft_graph::Node;
use nodecraft_nodes::NodeContract;
use nodecraft_types::{IssueCode, OptionLookup};

/// Whether some parameter of this step is off a list the host has fetched for it. `false` when
/// every option-sourced value is on its known list, draws from a list the host does not have, or
/// belongs to a hidden or disabled parameter.
///
/// Which parameter it was is deliberately not reported here: the blocker carries no string, so no
/// contract name reaches a report built from native hooks (see
/// [`Reason::NotAnOption`](crate::planner::Reason::NotAnOption)). A caller that needs the
/// parameter asks `node.describe` or `node.options`, which report the same issue on the same node.
///
/// A contract that will not resolve reports `false` rather than a second complaint:
/// [`NodeKind::validate_instance`](nodecraft_nodes::NodeKind::validate_instance) has already
/// refused the step for that, and the node names its first issue.
pub(super) fn off_list(contract: &NodeContract, node: &Node, options: &dyn OptionLookup) -> bool {
    // The common case is a contract whose every list is fixed: nothing to ask, so a canvas of
    // such nodes costs one pass over its rules rather than a resolution per step.
    if !contract.parameter_groups().any(|group| group.rules.iter().any(|rule| rule.options.is_some())) {
        return false;
    }
    let Ok(resolution) = contract.resolve_with(&node.params, true, options) else {
        return false;
    };
    resolution.issues.iter().any(|issue| issue.code == IssueCode::NotAnOption)
}
