//! Training a Soul ID (#680): `POST /v1/custom-references` with the wired images, then
//! `GET /v1/custom-references/{id}` until Higgsfield finishes it. The result is a style descriptor
//! naming the identity, which stays on the account.
//!
//! Training is paid and Higgsfield documents no idempotency key for it, so a POST is never sent
//! twice for one job. Each name carries a tag made from the job's key, and every submit first lists
//! the account's references for it: a Soul ID already carrying the tag is the job's, whether this
//! process sent it or one before a restart did. A POST this process sent that the list doesn't
//! show yet is not sent again; the run fails saying so.
use super::*;
use crate::JobInput;
use serde_json::{Value, json};

/// Most images Higgsfield trains one Soul ID from.
pub(super) const MAX_IMAGES: usize = 100;
/// Characters of a person's name for the Soul ID: Higgsfield takes 100, and the tag takes the rest.
pub(super) const NAME_CHARS: usize = 84;
/// Hex digits of the job's key the tag carries.
const TAG_HEX: usize = 10;

/// A training job, checked: the name sent (with its tag), the tag alone, and the images.
pub(super) struct Dataset<'a> {
    pub name: String,
    pub tag: String,
    pub images: Vec<request::Image<'a>>,
}

/// The tag a job's Soul ID name ends with: ` [nc-` and the first hex digits of its key's hash.
pub(super) fn tag(idempotency_key: &str) -> String {
    let hex = ContentHash::of(idempotency_key.as_bytes()).to_hex();
    format!(" [nc-{}]", hex.get(..TAG_HEX).unwrap_or(&hex))
}

/// A Soul ID name without the tag NodeCraft added, for showing to a person.
pub(super) fn shown(name: &str) -> &str {
    match name.rsplit_once(" [nc-") {
        Some((head, rest)) if rest.len() == TAG_HEX + 1 && rest.ends_with(']') && rest.bytes().take(TAG_HEX).all(|b| b.is_ascii_hexdigit()) => head,
        _ => name,
    }
}

pub(super) fn prepare(job: &ProviderJob, l: Limits) -> Result<Dataset<'_>, ProviderError> {
    if job.model != SOUL_TRAINER {
        return Err(invalid("choose a Higgsfield model this version supports"));
    }
    if job.inputs.len() > MAX_IMAGES || job.params.len() > 1 {
        return Err(invalid(format!("a Higgsfield Soul ID is trained from a name and 1 to {MAX_IMAGES} images")));
    }
    let mut name = None;
    for (param, value) in &job.params {
        match param.as_str() {
            "name" => {
                let text = value.as_str().map(str::trim).ok_or_else(|| invalid("a Soul ID's name is text"))?;
                if text.is_empty() || text.chars().any(char::is_control) || text.chars().take(NAME_CHARS + 1).count() > NAME_CHARS {
                    return Err(invalid(format!("a Soul ID's name is one line of 1 to {NAME_CHARS} characters")));
                }
                name = Some(text);
            }
            _ => return Err(invalid("unknown Higgsfield Soul ID training option")),
        }
    }
    let name = name.ok_or_else(|| invalid("name the Soul ID before training it"))?;
    let mut images = Vec::new();
    images.try_reserve_exact(job.inputs.len()).map_err(|_| malformed("Higgsfield bounded allocation failed"))?;
    for input in &job.inputs {
        let JobInput::Media { role, mime, bytes } = input else { return Err(invalid("a Soul ID is trained from images only")) };
        if role != "images" {
            return Err(invalid("a Soul ID is trained from images only"));
        }
        let mime = request::mime(mime).ok_or_else(|| invalid("Higgsfield takes PNG, JPEG or WebP images"))?;
        if bytes.is_empty() || bytes.len() > l.input_bytes {
            return Err(invalid(format!("a Higgsfield image must be under {} MiB", l.input_bytes.div_ceil(MIB))));
        }
        if nodecraft_media::sniff(bytes).map(|s| s.mime) != Some(mime) {
            return Err(invalid("a Higgsfield image's bytes don't match its type"));
        }
        images.push(request::Image { mime, bytes });
    }
    if images.is_empty() {
        return Err(invalid(format!("a Higgsfield Soul ID is trained from 1 to {MAX_IMAGES} images")));
    }
    let tag = tag(&job.idempotency_key);
    Ok(Dataset { name: format!("{name}{tag}"), tag, images })
}

/// The handle a training is polled by.
fn handle(id: String) -> Submission {
    Submission::Queued(JobHandle { remote_id: id, data: json!({"v": 1, "model": SOUL_TRAINER, "count": 1}) })
}

/// One page of the account's references, read only for ids and names: the list's shape is
/// undocumented, so an entry missing either is passed over here, never guessed at.
fn named_page(body: &[u8], page_size: usize) -> Result<(Vec<(String, String)>, usize), ProviderError> {
    let v: Value = serde_json::from_slice(body).map_err(|_| malformed("Higgsfield reply isn't the expected JSON"))?;
    let items = match &v {
        Value::Array(items) => items,
        _ => v.get("items").and_then(Value::as_array).ok_or_else(|| malformed("Higgsfield's Soul ID list isn't a list NodeCraft reads"))?,
    };
    if items.len() > page_size {
        return Err(malformed("Higgsfield's Soul ID list page is longer than asked for"));
    }
    let named = items
        .iter()
        .filter_map(|item| {
            let id = item.get("id").and_then(Value::as_str).filter(|id| request::uuid(id))?.to_ascii_lowercase();
            let name = item.get("name").and_then(Value::as_str).filter(|n| n.len() <= 512)?;
            Some((id, name.to_owned()))
        })
        .collect();
    Ok((named, items.len()))
}

/// What a creation answered: the new Soul ID's id. Higgsfield may tidy the name it echoes, so
/// only the tag is compared. The training was sent and may be billed, so a reply that doesn't
/// read is a server fault: the engine tries the same key again, which lists the account for the
/// tag instead of sending another training.
fn created(body: &[u8], tag: &str) -> Result<String, ProviderError> {
    let unread = |why: &str| ProviderError::new(ErrorKind::Server, format!("{why}; NodeCraft looks for the Soul ID on the account rather than training again"));
    let v: Value = serde_json::from_slice(body).map_err(|_| unread("Higgsfield's reply to the training isn't the expected JSON"))?;
    let id = v.get("id").and_then(Value::as_str).filter(|id| request::uuid(id)).ok_or_else(|| unread("Higgsfield accepted the Soul ID but gave no id"))?;
    if v.get("name").and_then(Value::as_str).is_some_and(|n| !n.trim_end().ends_with(tag.trim_start())) {
        return Err(unread("Higgsfield answered for a Soul ID with another name"));
    }
    Ok(id.to_ascii_lowercase())
}

/// Where a training stands, from `GET /v1/custom-references/{id}`.
pub(super) enum Trained {
    Progress(JobProgress),
    Done,
}

fn trained(body: &[u8], id: &str, leaks: impl Fn(&str) -> bool) -> Result<Trained, ProviderError> {
    let v: Value = serde_json::from_slice(body).map_err(|_| malformed("Higgsfield reply isn't the expected JSON"))?;
    if v.get("id").and_then(Value::as_str).map(str::to_ascii_lowercase).as_deref() != Some(id) {
        return Err(malformed("Higgsfield answered for a different Soul ID"));
    }
    if v.get("model_version").and_then(Value::as_str).is_some_and(|version| version != "v2") {
        return Err(malformed("Higgsfield trained this Soul ID for another SOUL version"));
    }
    let state = v.get("status").and_then(Value::as_str).filter(|s| s.len() <= 32).ok_or_else(|| malformed("Higgsfield's Soul ID reply has no status"))?;
    let progress = match state {
        "not_ready" | "queued" => JobProgress::Queued { position: None },
        "in_progress" => JobProgress::Running { fraction: None, message: Some("training".into()), partial: None },
        "completed" => return Ok(Trained::Done),
        "failed" => {
            let reason = v.get("fail_reason").and_then(Value::as_str).filter(|r| !r.trim().is_empty()).map(|r| response::said(r, &leaks));
            JobProgress::Failed(ProviderError::new(
                ErrorKind::JobFailed,
                match reason {
                    Some(why) => format!("Higgsfield couldn't train this Soul ID: {why}"),
                    None => "Higgsfield couldn't train this Soul ID".into(),
                },
            ))
        }
        // An undocumented state is neither taken for ready nor for failed: polling goes on.
        other => JobProgress::Running { fraction: None, message: Some(format!("Higgsfield reports {}", response::said(other, &leaks))), partial: None },
    };
    Ok(Trained::Progress(progress))
}

/// A creation refused before anything was trained: a client error Higgsfield explains. A 503 or
/// a lock may come after the identity was made, so those are left to the tag lookup.
fn refused(r: &HttpResponse) -> bool {
    matches!(r.status, 400 | 401 | 403 | 422) && response::refused_submit(r)
}

impl Higgsfield {
    fn posted(&self) -> std::sync::MutexGuard<'_, VecDeque<String>> {
        self.posted.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// Trains a Soul ID, or finds the one this job already asked for.
    pub(super) async fn train(&self, job: &ProviderJob, context: Option<&crate::SubmitContext>) -> Result<Submission, ProviderError> {
        let dataset = prepare(job, self.limits)?;
        if let Some(id) = self.tagged(&dataset.tag).await? {
            return Ok(handle(id));
        }
        if self.posted().contains(&job.idempotency_key) {
            return Err(ProviderError::new(
                ErrorKind::JobFailed,
                "Higgsfield's reply to this Soul ID's training was lost and its list doesn't show the Soul ID yet. It may have been billed, so it wasn't sent again. Refresh option lists in a few minutes and look for it in SOUL 2's Soul ID picker, or on Higgsfield, before training again: a new run trains and bills a new one",
            ));
        }
        let mut images = Vec::new();
        images.try_reserve_exact(dataset.images.len()).map_err(|_| malformed("Higgsfield bounded allocation failed"))?;
        for image in &dataset.images {
            images.push(json!({"type": "image_url", "image_url": self.upload(image.mime, image.bytes).await?}));
        }
        let body = json!({"name": dataset.name, "model_version": "v2", "input_images": images});
        let mut create = HttpRequest::post_json(format!("{API}/v1/custom-references"), &body)?.timeout_ms(60_000);
        create.max_response_bytes = self.limits.response_bytes;
        let create = self.auth(create)?;
        {
            // Checked again with the mark, in one lock: an attempt under this key that sent
            // meanwhile is never followed by a second.
            let mut posted = self.posted();
            if posted.contains(&job.idempotency_key) {
                return Err(ProviderError::new(ErrorKind::JobFailed, "this Soul ID's training is already being sent under this run"));
            }
            if posted.len() >= STAGED {
                posted.pop_front();
            }
            posted.push_back(job.idempotency_key.clone());
        }
        if let Some(context) = context {
            context.paid();
        }
        let reply = self.http.send_no_redirect(create).await.map_err(|e| transport(e, true))?;
        if reply.body.len() <= self.limits.response_bytes && refused(&reply) {
            // Refused before it was accepted: nothing was trained, so a later attempt may send it.
            self.posted().retain(|key| key != &job.idempotency_key);
            if let Some(context) = context {
                context.refused();
            }
        }
        response::check(&reply, self.limits, |s| self.leaks(s))?;
        Ok(handle(created(&reply.body, &dataset.tag)?))
    }
    /// The account's Soul ID whose name ends with `tag`. Free. Refused rather than guessed when
    /// the list can't be read in full or two carry it.
    async fn tagged(&self, tag: &str) -> Result<Option<String>, ProviderError> {
        let mut found: Option<String> = None;
        for page in 1..=SOUL_PAGES.saturating_add(1) {
            let list = HttpRequest::get(format!("{API}/v1/custom-references/list?page={page}&page_size={SOUL_PAGE}")).timeout_ms(30_000);
            let reply = self.send(list, false, None).await?;
            let (named, entries) = named_page(&reply.body, SOUL_PAGE)?;
            if entries == 0 {
                return Ok(found);
            }
            if page > SOUL_PAGES {
                break;
            }
            for (id, name) in named {
                if !name.trim_end().ends_with(tag.trim_end()) {
                    continue;
                }
                match &found {
                    Some(other) if *other != id => {
                        return Err(malformed("two Higgsfield Soul IDs carry this run's tag; delete one on Higgsfield and run again"));
                    }
                    _ => found = Some(id),
                }
            }
        }
        Err(malformed(
            "this Higgsfield account has more Soul IDs than NodeCraft lists, so it can't tell whether this one was already trained. Nothing was sent",
        ))
    }
    /// Where training `id` stands; once done, the descriptor naming it.
    pub(super) async fn training(&self, id: &str) -> Result<JobProgress, ProviderError> {
        if !request::uuid(id) {
            return Err(malformed("Higgsfield Soul ID refused"));
        }
        let status = HttpRequest::get(format!("{API}/v1/custom-references/{id}")).timeout_ms(30_000);
        let reply = self.send(status, false, None).await?;
        match trained(&reply.body, id, |s| self.leaks(s))? {
            Trained::Progress(progress) => Ok(progress),
            Trained::Done => {
                let style = nodecraft_media::style::Style {
                    provider: "higgsfield".into(),
                    trainer: SOUL_TRAINER.into(),
                    family: SOUL_FAMILY.into(),
                    trigger: String::new(),
                    weights: format!("{API}/v1/custom-references/{id}"),
                };
                let bytes = nodecraft_media::style::encode(&style).map_err(|_| malformed("Higgsfield Soul ID refused"))?;
                Ok(JobProgress::Done(Completed::new(vec![Output::Bytes { mime: nodecraft_media::style::MIME.into(), bytes: Arc::from(bytes) }])))
            }
        }
    }
}
