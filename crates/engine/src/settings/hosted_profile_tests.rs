use super::*;

#[test]
fn hosted_profiles_roundtrip_while_old_and_unknown_backends_are_safe() {
    let old: ComfyServer = serde_json::from_str(r#"{"name":"Local","url":"http://localhost:8188"}"#).unwrap();
    assert_eq!(old.backend, ComfyBackend::Local);
    let cloud = ComfyServer::cloud("Hosted");
    let copy: ComfyServer = serde_json::from_value(serde_json::to_value(&cloud).unwrap()).unwrap();
    assert_eq!(copy, cloud);
    let future: ComfyServer = serde_json::from_str(r#"{"name":"Future","url":"https://cloud.comfy.org","backend":"future"}"#).unwrap();
    assert_eq!(future.backend, ComfyBackend::Unsupported);
    let bad_origin = ComfyServer { name: "Wrong origin".into(), url: "https://example.invalid".into(), ..cloud.clone() };
    let settings = ProviderSettings { comfyui: vec![old, cloud, future, bad_origin], ..Default::default() };
    assert_eq!(settings.comfy_servers().iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Local", "Hosted"]);
}
