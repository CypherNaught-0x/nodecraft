//! Source identity comes from the actual planned recipe, never the currently visible upstream
//! generation (which may have changed while the render was running).
use super::{Input, MAX_CLIPS, PORTS_PARAM, SOURCES_PARAM, SPEC_PARAM, Timeline, Wire};
use nodecraft_graph::SourceVersion;
use nodecraft_types::{Error, ErrorKind, ParamValues, Value};

/// The wires of a materialized Timeline job, including compatibility defaults for old recipes.
pub fn wires(params: &ParamValues, count: usize) -> Vec<Wire> {
    let text = |name: &str, index: usize| match params.get(name) {
        Some(Value::List(values)) => match values.get(index) {
            Some(Value::Text(text)) => Some(text.as_str()),
            _ => None,
        },
        _ => None,
    };
    (0..count).map(|i| Wire::new(text(PORTS_PARAM, i).and_then(Input::from_port).unwrap_or_default(), text(SOURCES_PARAM, i).unwrap_or_default())).collect()
}

/// Content versions of just the clips the job renders (disabled or unwired clips contribute
/// nothing). Called after success and when rebinding an equivalent cached output to new clips.
pub fn source_versions(params: &ParamValues, inputs: &[Value], item: Option<u32>) -> Result<Vec<SourceVersion>, Error> {
    let bad = || Error::new(ErrorKind::InvalidInput, "the rendered timeline's source versions can't be recorded");
    let spec = match params.get(SPEC_PARAM) {
        None => super::EMPTY,
        Some(Value::Text(spec)) => spec,
        _ => return Err(bad()),
    };
    let timeline = Timeline::parse(spec)?;
    let resolved = timeline.resolve(&wires(params, inputs.len()));
    if resolved.len() > MAX_CLIPS {
        return Err(bad());
    }
    resolved
        .into_iter()
        .map(|(input, clip)| {
            let Some(Value::Media(asset)) = inputs.get(input) else {
                return Err(bad());
            };
            Ok(SourceVersion { role: clip.id, hash: asset.hash, item })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_types::{AssetRef, Blake3, MediaMeta};
    #[test]
    fn versions_follow_split_clips_and_actual_ports_but_omit_disabled_and_unwired_clips() {
        let spec = r#"{"clips":[{"id":"part1","origin":"node","out_us":1000000},{"id":"part2","origin":"node","in_us":1000000},{"id":"off","origin":"sound","input":"audio","track":1,"enabled":false},{"id":"gone","origin":"missing"}],"tracks":[{"kind":"audio"}]}"#;
        let params = ParamValues::from([
            (SPEC_PARAM.into(), Value::Text(spec.into())),
            (SOURCES_PARAM.into(), Value::List(vec![Value::Text("node".into()), Value::Text("sound".into())])),
            (PORTS_PARAM.into(), Value::List(vec![Value::Text("clips".into()), Value::Text("audio".into())])),
        ]);
        let asset = |n| Value::Media(AssetRef { hash: Blake3::from_bytes([n; 32]), mime: "video/webm".parse().unwrap(), meta: MediaMeta::default() });
        let sources = source_versions(&params, &[asset(1), asset(2)], Some(3)).unwrap();
        assert_eq!(
            sources.iter().map(|s| (s.role.as_str(), s.hash, s.item)).collect::<Vec<_>>(),
            vec![("part1", Blake3::from_bytes([1; 32]), Some(3)), ("part2", Blake3::from_bytes([1; 32]), Some(3))]
        );
        assert!(source_versions(&params, &[Value::Text("bad source".into())], None).is_err());
    }
}
