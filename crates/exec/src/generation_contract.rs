//! Admission checks for generated results against the stored instance ports.
use nodecraft_graph::Node;
use nodecraft_nodes::JobSpec;

/// Check the complete step before publishing any of its jobs. Diagnostics deliberately
/// exclude route/provider/model strings supplied by programmatic plans.
pub(crate) fn validate_outputs(node: &Node, jobs: &[JobSpec]) -> Result<(), &'static str> {
    for job in jobs {
        let JobSpec::Generate { node: owner, outputs, .. } = job else { continue };
        if *owner != node.id {
            return Err("a generated job does not belong to its step node");
        }
        for route in outputs {
            let Some(port) = node.outputs.iter().find(|port| port.name == route.port) else {
                return Err("a generated output targets an unknown output port");
            };
            if port.port_type.modality != route.modality {
                return Err("a generated output modality does not match its output port");
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_targets(
    node: &nodecraft_graph::Node,
    jobs: &[nodecraft_nodes::JobSpec],
    targets: &nodecraft_types::TargetSelection,
) -> Result<(), &'static str> {
    for job in jobs {
        if let nodecraft_nodes::JobSpec::Generate { node: owner, provider, instance, .. } = job
            && (*owner != node.id || targets.get(node.id, provider).is_none_or(|target| target.instance() != *instance))
        {
            return Err("generated job does not match its selected provider target");
        }
    }
    Ok(())
}
