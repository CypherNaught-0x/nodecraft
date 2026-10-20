//! What a host keeps to record a model job's result after a restart (#508), when the run that
//! sent it is gone: the generation the step would have recorded, minus its outputs.
//!
//! Only a step with exactly one job and no fan-out has one. Its result is that job's alone, so
//! it can be assembled without the run; a fanned or multi-job step needs its siblings, and a
//! resumed job of one is followed to its end (so its charge settles) but not recorded.
use super::{JobId, JobState, Run, StepState, recordable};
use nodecraft_graph::{Generation, JobInput, JobSettings};
use nodecraft_nodes::{JobSpec, Registry};
use nodecraft_types::{Blake3, GenerationId, NodeId, Value};
use serde::{Deserialize, Serialize};

/// Everything but the outputs of the generation a single-job step records.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeRecipe {
    pub node: NodeId,
    pub generation: GenerationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_hash: Option<Blake3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<JobSettings>,
    /// The node's output ports in order, and whether each is a list.
    pub ports: Vec<RecipePort>,
}

/// One output port of a [`ResumeRecipe`]'s node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipePort {
    pub name: String,
    pub many: bool,
}

impl Run {
    /// The recipe for `job`'s step, if it is a model call and the step's only job. Call when the
    /// job is queued: its inputs are known then, so the input hash is the one the run would stamp.
    pub fn resume_recipe(&mut self, kinds: &Registry, job: JobId) -> Option<ResumeRecipe> {
        let JobState { step: node, work, .. } = self.jobs.get(&job)?;
        let node = *node;
        let JobSpec::Generate { provider, instance, model, params, inputs, .. } = work else { return None };
        let settings = JobSettings {
            provider: provider.clone(),
            instance: *instance,
            model: model.clone(),
            params: params.clone(),
            inputs: inputs.iter().map(|input| JobInput { role: input.role.clone(), value: input.value.clone() }).collect(),
            item: None,
        };
        match self.steps.get(&node) {
            Some(StepState { jobs, step, .. }) if jobs.as_slice() == [job] && step.fanout.is_none() => {}
            _ => return None,
        }
        let target = self.canvas.nodes().get(&node)?;
        let ports = target.outputs.iter().map(|port| RecipePort { name: port.name.clone(), many: port.port_type.many }).collect();
        let input_hash = self.hasher.hash(&self.canvas, kinds, node, &self.produced).ok().flatten();
        Some(ResumeRecipe { node, generation: self.generation_id(node), input_hash, settings: Some(settings), ports })
    }
}

impl ResumeRecipe {
    /// The generation to record from the job's `outputs` (`(output port, value)`), like the run
    /// assembles a single-job step.
    pub fn assemble(&self, outputs: Vec<(String, Value)>) -> Result<Generation, String> {
        let mut per_port: Vec<Vec<Value>> = vec![Vec::new(); self.ports.len()];
        for (port, value) in outputs {
            let index = self.ports.iter().position(|p| p.name == port).ok_or_else(|| format!("a job produced an unknown output {port}"))?;
            let slot = per_port.get_mut(index).ok_or("output index out of range")?;
            if slot.len() >= crate::planner::MAX_LIST_ITEMS {
                return Err(format!("its {port} output has more than {} items", crate::planner::MAX_LIST_ITEMS));
            }
            slot.push(value);
        }
        let mut assembled = Vec::with_capacity(self.ports.len());
        for (port, values) in self.ports.iter().zip(per_port) {
            if port.many {
                assembled.push(Value::List(values));
                continue;
            }
            let mut values = values.into_iter();
            match (values.next(), values.next()) {
                (Some(value), None) => assembled.push(value),
                (None, _) => return Err(format!("it produced nothing on its {} output", port.name)),
                _ => return Err(format!("it produced several values on its single {} output", port.name)),
            }
        }
        let generation = Generation {
            id: self.generation,
            outputs: assembled,
            input_hash: self.input_hash,
            items: Vec::new(),
            settings: self.settings.iter().cloned().collect(),
            incomplete: None,
        };
        recordable(generation)
    }
}
