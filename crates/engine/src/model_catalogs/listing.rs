//! Turning provider model listings into catalog models. Listings are untrusted: every field is
//! bounded and checked, and an entry that doesn't make a valid model is skipped on its own.
use super::price;
use nodecraft_catalog::ModelDescriptor;
use nodecraft_types::{Error, ErrorKind};
use serde_json::{Value, json};

/// Most entries read from one listing, whatever it holds.
pub(crate) const MAX_ENTRIES: usize = 2048;
/// Longest label and description kept from a listing.
const MAX_LABEL: usize = 120;
const MAX_DESCRIPTION: usize = 400;
/// The editorial tier listed models get: below the curated catalog's, so automatic selection
/// keeps preferring models NodeCraft has checked.
const LISTED_TIER: u8 = 2;
/// Most images a listed chat model is sent with one prompt (the chat client's own cap).
const MAX_CHAT_IMAGES: u64 = 16;
/// The longest reply asked for by default, and the chat client's own ceiling.
const DEFAULT_REPLY_TOKENS: u64 = 8192;
const MAX_REPLY_TOKENS: u64 = 128_000;

fn malformed(what: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, format!("the model list isn't readable: {what}"))
}

/// The catalog id for a provider's model: `prefix.` and the remote id in the catalog's alphabet
/// (`:` becomes `.`, anything else outside it `-`). `None` when it can't make a valid id.
pub(crate) fn catalog_id(prefix: &str, remote: &str) -> Option<String> {
    if remote.is_empty() || remote.len() > 120 {
        return None;
    }
    let mapped: String = remote
        .chars()
        .map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9' | '.' | '-' | '_' | '/') => c,
            ':' => '.',
            _ => '-',
        })
        .collect();
    let id = format!("{prefix}.{mapped}");
    nodecraft_catalog::model::is_id(&id).then_some(id)
}

/// Printable text of at most `max` characters, or `None` when nothing printable is left.
pub(crate) fn shown(text: &str, max: usize) -> Option<String> {
    let flat: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let words: Vec<&str> = flat.split_whitespace().collect();
    let joined = words.join(" ");
    if joined.is_empty() {
        return None;
    }
    if joined.chars().count() <= max {
        return Some(joined);
    }
    let cut: String = joined.chars().take(max.saturating_sub(1)).collect();
    Some(format!("{}…", cut.trim_end()))
}

/// The first sentence (or so) of a listing's description.
fn summary(text: Option<&str>) -> Option<String> {
    let text = text?;
    let first = text.split_inclusive(". ").next().unwrap_or(text);
    shown(first, MAX_DESCRIPTION)
}

/// The `data` array of a listing reply.
pub(crate) fn entries(body: &[u8]) -> Result<Vec<Value>, Error> {
    let value: Value = serde_json::from_slice(body).map_err(|_| malformed("not JSON"))?;
    let Value::Object(mut object) = value else { return Err(malformed("not an object")) };
    match object.remove("data") {
        Some(Value::Array(mut list)) => {
            list.truncate(MAX_ENTRIES);
            Ok(list)
        }
        _ => Err(malformed("no `data` list")),
    }
}

fn strings(value: Option<&Value>) -> Vec<&str> {
    value.and_then(Value::as_array).map(|list| list.iter().filter_map(Value::as_str).take(16).collect()).unwrap_or_default()
}

fn descriptor(value: Value) -> Option<ModelDescriptor> {
    let model: ModelDescriptor = serde_json::from_value(value).ok()?;
    model.validate().ok()?;
    Some(model)
}

/// What a listed chat model costs, as the catalog prices it.
pub(crate) enum ChatPrice {
    /// Listed as free for both prompt and reply.
    Free,
    /// Per 1000 prompt and reply tokens.
    Tokens { input: nodecraft_catalog::Price, output: nodecraft_catalog::Price },
    /// Unknown before the run: the provider reports what it charged.
    Measured,
}

/// One chat model to list.
pub(crate) struct Chat<'a> {
    pub prefix: &'a str,
    pub provider: &'a str,
    pub remote: &'a str,
    pub label: String,
    pub description: String,
    pub images: bool,
    pub reply_tokens: Option<u64>,
    pub price: ChatPrice,
}

/// A chat model on a text card: text to text, and image to text when it reads images. The quote
/// is the reply-token price on Max output tokens, as for the curated chat models; prompt tokens
/// are charged once the reply reports them.
pub(crate) fn chat(model: Chat<'_>) -> Option<ModelDescriptor> {
    let id = catalog_id(model.prefix, model.remote)?;
    let max = model.reply_tokens.filter(|n| *n > 0).map_or(MAX_REPLY_TOKENS, |n| n.min(MAX_REPLY_TOKENS));
    let default = DEFAULT_REPLY_TOKENS.min(max);
    let (pricing, usage, cost) = match model.price {
        ChatPrice::Free => (json!({"type": "free"}), Value::Null, "Free on this provider.".to_owned()),
        ChatPrice::Tokens { input, output } => (
            json!({"type": "per_unit", "unit": "thousand_tokens", "price": price_text(output), "count": "max_tokens"}),
            json!({"input": price_text(input), "output": price_text(output)}),
            format!("{} / M input, {} / M output tokens.", price::per_million_label(input), price::per_million_label(output)),
        ),
        ChatPrice::Measured => (json!({"type": "by_usage", "currency": "USD"}), Value::Null, "Price unknown until the run reports it.".to_owned()),
    };
    let prompt = json!({"role": "prompt", "modality": "text", "min": 1, "max": 1, "inline": true});
    let mut modes = vec![json!({"id": "text-to-text", "output": "text", "inputs": [prompt]})];
    if model.images {
        modes.push(
            json!({"id": "image-to-text", "output": "text", "inputs": [prompt, {"role": "image", "modality": "image", "min": 1, "max": MAX_CHAT_IMAGES}]}),
        );
    }
    let description = shown(&format!("{} {cost}", model.description), MAX_DESCRIPTION)?;
    let mut value = json!({
        "id": id, "provider": model.provider, "provider_model": model.remote,
        "label": shown(&model.label, MAX_LABEL)?, "description": description,
        "latency_s": 20, "quality": LISTED_TIER, "pricing": pricing, "modes": modes,
        "params": {"params": [{
            "name": "max_tokens", "label": "Max output tokens",
            "description": "Upper limit on the reply, reasoning tokens included. The reply stops when it is reached.",
            "control": {"type": "unsigned", "min": "1", "max": max.to_string()},
            "default": {"type": "unsigned", "value": default.to_string()}
        }]},
    });
    if !usage.is_null()
        && let Some(object) = value.as_object_mut()
    {
        object.insert("usage".into(), usage);
    }
    descriptor(value)
}

fn price_text(price: nodecraft_catalog::Price) -> String {
    format!("{}.{:06} USD", price.0.micros / 1_000_000, price.0.micros % 1_000_000)
}

/// OpenRouter's chat models (`GET /api/v1/models`): every model that writes text only. Routers
/// with variable prices (`openrouter/…`, priced `-1`) are left out.
pub(crate) fn openrouter_chat(body: &[u8]) -> Result<Vec<ModelDescriptor>, Error> {
    let mut models = Vec::new();
    for entry in entries(body)? {
        let Some(remote) = entry.get("id").and_then(Value::as_str) else { continue };
        if remote.starts_with("openrouter/") {
            continue;
        }
        let architecture = entry.get("architecture");
        if strings(architecture.and_then(|a| a.get("output_modalities"))) != ["text"] {
            continue;
        }
        let mut images = strings(architecture.and_then(|a| a.get("input_modalities"))).contains(&"image");
        let Some(pricing) = entry.get("pricing").and_then(Value::as_object) else { continue };
        let field = |name: &str| pricing.get(name);
        let (Some(prompt), Some(completion)) = (field("prompt"), field("completion")) else { continue };
        let paid = |name: &str| field(name).is_some_and(|value| !price::is_zero(value));
        // Fees a token quote can't hold, on what NodeCraft sends: a per-request fee skips the model
        // (its charge would be understated); a per-image fee drops reading images.
        if paid("request") || pricing.values().any(|value| price::per_thousand_tokens(value).is_none() && !price::is_zero(value)) {
            continue;
        }
        if paid("image") {
            images = false;
        }
        let price = if price::is_zero(prompt) && price::is_zero(completion) && !paid("internal_reasoning") {
            ChatPrice::Free
        } else {
            // Reasoning tokens are reply tokens; the higher of the two rates bounds them both.
            let reasoning = field("internal_reasoning").and_then(price::per_thousand_tokens);
            match (price::per_thousand_tokens(prompt), price::per_thousand_tokens(completion)) {
                (Some(input), Some(output)) => {
                    let output = reasoning.filter(|r| r.0.micros > output.0.micros).unwrap_or(output);
                    ChatPrice::Tokens { input, output }
                }
                _ => continue,
            }
        };
        let reply_tokens = entry.get("top_provider").and_then(|t| t.get("max_completion_tokens")).and_then(Value::as_u64);
        let name = entry.get("name").and_then(Value::as_str).unwrap_or(remote);
        let description = summary(entry.get("description").and_then(Value::as_str)).unwrap_or_else(|| "Listed by OpenRouter.".into());
        let chat = Chat { prefix: "openrouter", provider: "openrouter", remote, label: name.to_owned(), description, images, reply_tokens, price };
        models.extend(self::chat(chat));
    }
    Ok(models)
}

/// The values a listing's enum setting offers, bounded and in the adapter's alphabet.
fn enum_values(spec: Option<&Value>) -> Vec<String> {
    let Some(spec) = spec.filter(|s| s.get("type").and_then(Value::as_str) == Some("enum")) else { return Vec::new() };
    let mut values: Vec<String> = Vec::new();
    for value in spec.get("values").and_then(Value::as_array).into_iter().flatten().take(32) {
        let Some(text) = value.as_str() else { continue };
        if !text.is_empty() && text.len() <= 16 && text.bytes().all(|b| b.is_ascii_alphanumeric() || b".:x".contains(&b)) && !values.iter().any(|v| v == text) {
            values.push(text.to_owned());
        }
    }
    values
}

fn choice_param(name: &str, label: &str, values: &[String], preferred: &str) -> Option<Value> {
    let first = values.first()?;
    let default = if values.iter().any(|v| v == preferred) { preferred } else { first.as_str() };
    let options: Vec<Value> = values.iter().map(|v| json!({"value": v, "label": v})).collect();
    Some(json!({"name": name, "label": label, "control": {"type": "choice", "options": options}, "default": {"type": "text", "value": default}}))
}

/// OpenRouter's image models (`GET /api/v1/images/models`), run through its Images API. Their
/// price depends on the image (per output token, per image, by resolution), so a run is priced
/// by what OpenRouter reports; `prices` (the `/api/v1/models?output_modalities=image` listing)
/// adds the listed rate to the description when it has one.
pub(crate) fn openrouter_images(body: &[u8], prices: Option<&[u8]>) -> Result<Vec<ModelDescriptor>, Error> {
    let rates: Vec<Value> = prices.and_then(|body| entries(body).ok()).unwrap_or_default();
    let rate = |remote: &str| -> Option<String> {
        let entry = rates.iter().find(|entry| entry.get("id").and_then(Value::as_str) == Some(remote))?;
        let per_token = entry.get("pricing").and_then(|p| p.get("image_output")).and_then(price::per_thousand_tokens)?;
        (per_token.0.micros > 0).then(|| format!("Listed at {} per million image tokens.", price::per_million_label(per_token)))
    };
    let mut models = Vec::new();
    for entry in entries(body)? {
        let Some(remote) = entry.get("id").and_then(Value::as_str) else { continue };
        if !strings(entry.get("architecture").and_then(|a| a.get("output_modalities"))).contains(&"image") {
            continue;
        }
        let Some(id) = catalog_id("openrouter", remote) else { continue };
        let supported = entry.get("supported_parameters");
        let param = |name: &str| supported.and_then(|s| s.get(name));
        let mut params = Vec::new();
        params.extend(choice_param("resolution", "Resolution", &enum_values(param("resolution")), "1K"));
        params.extend(choice_param("aspect_ratio", "Aspect ratio", &enum_values(param("aspect_ratio")), "1:1"));
        if param("seed").and_then(|s| s.get("type")).and_then(Value::as_str) == Some("boolean") {
            params
                .push(json!({"name": "seed", "label": "Fixed seed (optional)", "control": {"type": "unsigned", "min": "0", "max": "2147483647", "step": "1"}}));
        }
        let prompt = json!({"role": "prompt", "modality": "text", "min": 1, "max": 1, "inline": true});
        let mut modes = vec![json!({"id": "text-to-image", "output": "image", "inputs": [prompt]})];
        let references = param("input_references").and_then(|r| r.get("max")).and_then(Value::as_u64).unwrap_or(0).min(16);
        if references > 0 && strings(entry.get("architecture").and_then(|a| a.get("input_modalities"))).contains(&"image") {
            modes.push(
                json!({"id": "image-to-image", "output": "image", "inputs": [prompt, {"role": "image", "modality": "image", "min": 1, "max": references}]}),
            );
        }
        let name = entry.get("name").and_then(Value::as_str).unwrap_or(remote);
        let about = summary(entry.get("description").and_then(Value::as_str)).unwrap_or_else(|| "Listed by OpenRouter.".into());
        let cost = rate(remote).unwrap_or_else(|| "Priced per image by OpenRouter.".into());
        let Some(description) = shown(&format!("{about} {cost} The charge is known once the image is made."), MAX_DESCRIPTION) else { continue };
        let Some(label) = shown(name, MAX_LABEL) else { continue };
        models.extend(descriptor(json!({
            "id": id, "provider": nodecraft_providers::openrouter_images::ID, "provider_model": remote,
            "label": label, "description": description, "latency_s": 30, "quality": LISTED_TIER,
            "pricing": {"type": "by_usage", "currency": "USD"}, "modes": modes, "params": {"params": params},
        })));
    }
    Ok(models)
}

/// A LiteLLM proxy's chat models: `GET /v1/models` names them, `GET /model/info` (when the key
/// may read it) says which are chat models, what they cost and how long a reply may be. Without
/// a known price a model is priced by what the proxy reports for the run.
pub(crate) fn litellm(models: &[u8], info: Option<&[u8]>) -> Result<Vec<ModelDescriptor>, Error> {
    let info: Vec<Value> = info.and_then(|body| entries(body).ok()).unwrap_or_default();
    let mut listed = Vec::new();
    for entry in entries(models)? {
        let Some(remote) = entry.get("id").and_then(Value::as_str) else { continue };
        let details = info.iter().find(|i| i.get("model_name").and_then(Value::as_str) == Some(remote)).and_then(|i| i.get("model_info"));
        let field = |name: &str| details.and_then(|d| d.get(name));
        // Embedding, image, speech and rerank deployments aren't chat models.
        if field("mode").and_then(Value::as_str).is_some_and(|mode| !matches!(mode, "chat" | "completion" | "responses")) {
            continue;
        }
        let price = match (field("input_cost_per_token"), field("output_cost_per_token")) {
            (Some(input), Some(output)) => match (price::per_thousand_tokens(input), price::per_thousand_tokens(output)) {
                (Some(input), Some(output)) if input.0.micros > 0 || output.0.micros > 0 => ChatPrice::Tokens { input, output },
                // A proxy's zero is its own bookkeeping, not a promise; the run reports the charge.
                _ => ChatPrice::Measured,
            },
            _ => ChatPrice::Measured,
        };
        let images = field("supports_vision").and_then(Value::as_bool).unwrap_or(false);
        let reply_tokens = field("max_output_tokens").and_then(Value::as_u64);
        let chat = Chat {
            prefix: "litellm",
            provider: "litellm",
            remote,
            label: remote.to_owned(),
            description: "Served by your LiteLLM proxy.".into(),
            images,
            reply_tokens,
            price,
        };
        listed.extend(self::chat(chat));
    }
    Ok(listed)
}

/// Speech models that answer only raw PCM, which NodeCraft can't place without its sample rate.
const PCM_ONLY: [&str; 2] = ["heygen/", "google/gemini"];

/// OpenRouter's text-to-speech models (`GET /api/v1/models?output_modalities=speech`), run
/// through its speech API as MP3. Priced per character when the listing says so (its `prompt`
/// rate), free when it lists no rate, else by what the run reports.
pub(crate) fn openrouter_speech(body: &[u8]) -> Result<Vec<ModelDescriptor>, Error> {
    let mut models = Vec::new();
    for entry in entries(body)? {
        let Some(remote) = entry.get("id").and_then(Value::as_str) else { continue };
        if !strings(entry.get("architecture").and_then(|a| a.get("output_modalities"))).contains(&"speech") || PCM_ONLY.iter().any(|p| remote.starts_with(p)) {
            continue;
        }
        let Some(id) = catalog_id("openrouter", remote) else { continue };
        // A price NodeCraft can't quote or read back is never offered: speech replies carry no
        // charge, so a model priced any way but per character (or free) would go unrecorded.
        let Some(pricing) = entry.get("pricing").and_then(Value::as_object) else { continue };
        let paid: Vec<(&String, &Value)> = pricing.iter().filter(|(_, value)| !price::is_zero(value)).collect();
        let (pricing, cost) = match paid.as_slice() {
            [] if !pricing.is_empty() => (json!({"type": "free"}), "Free on OpenRouter.".to_owned()),
            [(name, per_char)] if name.as_str() == "prompt" => match price::per_thousand_tokens(per_char) {
                // The same arithmetic: a rate per character is this price per 1000 characters.
                Some(per_k) => (
                    json!({"type": "per_unit", "unit": "thousand_characters", "price": price_text(per_k)}),
                    format!("{} per million characters.", price::per_million_label(per_k)),
                ),
                None => continue,
            },
            _ => continue,
        };
        let mut voices: Vec<String> = Vec::new();
        for voice in entry.get("supported_voices").and_then(Value::as_array).into_iter().flatten().take(128) {
            if let Some(voice) = voice.as_str()
                && !voice.is_empty()
                && voice.len() <= 128
                && voice.bytes().all(|b| b.is_ascii_alphanumeric() || b"._:-x".contains(&b))
                && !voices.iter().any(|v| v == voice)
            {
                voices.push(voice.to_owned());
            }
        }
        let params: Vec<Value> = voices.first().map_or_else(Vec::new, |first| {
            let options: Vec<Value> = voices.iter().map(|v| json!({"value": v, "label": v})).collect();
            vec![json!({"name": "voice", "label": "Voice", "control": {"type": "choice", "options": options}, "default": {"type": "text", "value": first}})]
        });
        let name = entry.get("name").and_then(Value::as_str).unwrap_or(remote);
        let about = summary(entry.get("description").and_then(Value::as_str)).unwrap_or_else(|| "Speech listed by OpenRouter.".into());
        let (Some(label), Some(description)) = (shown(name, MAX_LABEL), shown(&format!("{about} {cost}"), MAX_DESCRIPTION)) else { continue };
        models.extend(descriptor(json!({
            "id": id, "provider": nodecraft_providers::openrouter_media::SPEECH_ID, "provider_model": remote,
            "label": label, "description": description, "latency_s": 10, "quality": LISTED_TIER, "pricing": pricing,
            "modes": [{"id": "text-to-speech", "output": "audio", "inputs": [{"role": "prompt", "modality": "text", "min": 1, "max": 1, "inline": true}]}],
            "params": {"params": params},
        })));
    }
    Ok(models)
}

/// Whole seconds a video model lists, ascending, bounded.
fn durations(value: Option<&Value>) -> Vec<u64> {
    let mut list: Vec<u64> =
        value.and_then(Value::as_array).into_iter().flatten().take(64).filter_map(Value::as_u64).filter(|n| (1..=600).contains(n)).collect();
    list.sort_unstable();
    list.dedup();
    list
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// OpenRouter's video models (`GET /api/v1/videos/models`), run through its video API. A model
/// priced only by the second (`duration_seconds`) gets that quote; one priced by resolution,
/// audio or tokens is priced by what OpenRouter reports for the run.
pub(crate) fn openrouter_video(body: &[u8]) -> Result<Vec<ModelDescriptor>, Error> {
    let mut models = Vec::new();
    for entry in entries(body)? {
        let Some(remote) = entry.get("id").and_then(Value::as_str) else { continue };
        let Some(id) = catalog_id("openrouter", remote) else { continue };
        let listed = |name: &str| {
            let mut values: Vec<String> = Vec::new();
            for value in entry.get(name).and_then(Value::as_array).into_iter().flatten().take(32) {
                if let Some(text) = value.as_str()
                    && !text.is_empty()
                    && text.len() <= 16
                    && text.bytes().all(|b| b.is_ascii_alphanumeric() || b".:x".contains(&b))
                    && !values.iter().any(|v| v == text)
                {
                    values.push(text.to_owned());
                }
            }
            values
        };
        let mut params = Vec::new();
        let seconds = durations(entry.get("supported_durations"));
        if let (Some(first), Some(last)) = (seconds.first(), seconds.last()) {
            // Listed values are steps from the shortest; an uneven list may let a gap through,
            // which OpenRouter refuses with the values it takes (and doesn't bill).
            let step =
                seconds.windows(2).fold(0, |g, pair| gcd(g, pair.get(1).copied().unwrap_or(0).saturating_sub(pair.first().copied().unwrap_or(0)))).max(1);
            let default = seconds.iter().copied().find(|s| *s >= 5).unwrap_or(*first);
            params.push(json!({"name": "duration", "label": "Duration (s)",
                "description": format!("Lengths this model makes: {} seconds.", seconds.iter().map(u64::to_string).collect::<Vec<_>>().join(", ")),
                "control": {"type": "unsigned", "min": first.to_string(), "max": last.to_string(), "step": step.to_string()},
                "default": {"type": "unsigned", "value": default.to_string()}}));
        }
        params.extend(choice_param("resolution", "Resolution", &listed("supported_resolutions"), "720p"));
        params.extend(choice_param("aspect_ratio", "Aspect ratio", &listed("supported_aspect_ratios"), "16:9"));
        if entry.get("generate_audio").and_then(Value::as_bool) == Some(true) {
            params.push(json!({"name": "generate_audio", "label": "With sound", "control": {"type": "toggle"}, "default": {"type": "bool", "value": true}}));
        }
        if entry.get("seed").and_then(Value::as_bool) == Some(true) {
            params
                .push(json!({"name": "seed", "label": "Fixed seed (optional)", "control": {"type": "unsigned", "min": "0", "max": "2147483647", "step": "1"}}));
        }
        let frames = strings(entry.get("supported_frame_images"));
        let prompt = json!({"role": "prompt", "modality": "text", "min": 1, "max": 1, "inline": true});
        let mut modes = vec![json!({"id": "text-to-video", "output": "video", "inputs": [prompt]})];
        if frames.contains(&"first_frame") {
            modes
                .push(json!({"id": "image-to-video", "output": "video", "inputs": [prompt, {"role": "first_frame", "modality": "image", "min": 1, "max": 1}]}));
            if frames.contains(&"last_frame") {
                modes.push(json!({"id": "first-last-frame", "output": "video",
                    "inputs": [prompt, {"role": "first_frame", "modality": "image", "min": 1, "max": 1}, {"role": "last_frame", "modality": "image", "min": 1, "max": 1}]}));
            }
        }
        let skus: Vec<(&String, &Value)> = entry.get("pricing_skus").and_then(Value::as_object).map(|o| o.iter().collect()).unwrap_or_default();
        let per_second = |value: &Value| -> Option<nodecraft_catalog::Price> {
            // A per-second USD price, written as decimal text; through the per-1000 helper and back.
            let per_k = price::per_thousand_tokens(value)?;
            Some(nodecraft_catalog::Price(nodecraft_types::Money::new(nodecraft_types::Currency::USD, per_k.0.micros.div_ceil(1000))))
        };
        // Every listed price is a USD rate per second (by resolution, audio or input kind): the
        // highest is an upper bound for any run. Other units (tokens, cents, images) aren't.
        let per_second_sku = |name: &str| {
            let rest = name.strip_prefix("text_to_video_").or_else(|| name.strip_prefix("image_to_video_")).unwrap_or(name);
            rest == "duration_seconds" || rest.starts_with("duration_seconds_")
        };
        let bound = (!seconds.is_empty() && !skus.is_empty() && skus.iter().all(|(name, _)| per_second_sku(name)))
            .then(|| skus.iter().map(|(_, value)| per_second(value)).collect::<Option<Vec<_>>>())
            .flatten()
            .and_then(|prices| prices.into_iter().max_by_key(|p| p.0.micros))
            .filter(|p| p.0.micros > 0);
        let single = skus.len() == 1;
        let (pricing, cost) = match bound {
            Some(price) => (
                json!({"type": "per_unit", "unit": "second", "count": "duration", "price": price_text(price)}),
                if single {
                    format!("{} per second.", price_text(price))
                } else {
                    format!("Quoted at its highest rate, {} per second; settings with a lower rate are charged less.", price_text(price))
                },
            ),
            None => {
                let lowest = skus
                    .iter()
                    .filter(|(name, _)| name.starts_with("duration_seconds"))
                    .filter_map(|(_, value)| per_second(value))
                    .map(|p| p.0.micros)
                    .filter(|m| *m > 0)
                    .min();
                let hint = lowest.map_or_else(
                    || "Priced by OpenRouter per video.".to_owned(),
                    |m| format!("From {} per second.", price_text(nodecraft_catalog::Price(nodecraft_types::Money::new(nodecraft_types::Currency::USD, m)))),
                );
                (json!({"type": "by_usage", "currency": "USD"}), format!("{hint} The charge is known once the video is made."))
            }
        };
        let name = entry.get("name").and_then(Value::as_str).unwrap_or(remote);
        let about = summary(entry.get("description").and_then(Value::as_str)).unwrap_or_else(|| "Video listed by OpenRouter.".into());
        let (Some(label), Some(description)) = (shown(name, MAX_LABEL), shown(&format!("{about} {cost}"), MAX_DESCRIPTION)) else { continue };
        models.extend(descriptor(json!({
            "id": id, "provider": nodecraft_providers::openrouter_media::VIDEO_ID, "provider_model": remote,
            "label": label, "description": description, "latency_s": 180, "quality": LISTED_TIER, "pricing": pricing,
            "modes": modes, "params": {"params": params},
        })));
    }
    Ok(models)
}
