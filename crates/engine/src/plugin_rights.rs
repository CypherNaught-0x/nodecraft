//! What a plugin package asks for (#174): the capabilities its manifest names and, for a
//! provider operations package, the origins its templates reach. This is what a person sees
//! before granting, what a grant must match exactly, and what an update's permission delta
//! compares. The store records a grant; reading the templates is the broker's (layer 3).
//!
//! A provider template also says things of its own service the host can't check: what each model
//! costs, and whether the service deduplicates a resent job by its key. Those are shown as the
//! publisher's claims ([`Claim`]), next to what is granted but never part of it.
use nodecraft_plugin::manifest::{Capability, Optional, RegistrationKind, Runtime};
use nodecraft_plugin_store::{Package, Rights};
use nodecraft_providers::broker::Template;
use nodecraft_types::{Error, ErrorKind};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// A package's asks, read from its manifest and templates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Asks {
    pub required: BTreeSet<Capability>,
    /// Known optional capabilities; unknown ones are never granted.
    pub optional: BTreeSet<Capability>,
    /// Every origin its provider templates name: their API and output origins.
    pub origins: BTreeSet<String>,
    /// The origins among those that receive a key: the API of each template that takes one. The
    /// others are output origins, which results are downloaded from without it. Shown apart at consent; a grant
    /// still covers [`Self::origins`] as a whole.
    pub key_origins: BTreeSet<String>,
    /// What its provider templates say of their services, one per provider.
    pub claims: Vec<Claim>,
}

/// What one provider template says of its service, which the host can't check (#174).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    /// The provider's label.
    pub provider: String,
    /// Each model's label and the price the template declares for one job of it.
    pub prices: Vec<(String, nodecraft_types::Money)>,
    /// The header the template says its service deduplicates resent jobs by. The host sends the
    /// key in it but never sends a job again on this claim.
    pub idempotency_header: Option<&'static str>,
}

impl Claim {
    fn to_json(&self) -> Value {
        let prices: Vec<Value> = self.prices.iter().map(|(model, price)| json!({"model": model, "price": price})).collect();
        json!({"provider": self.provider, "prices": prices, "idempotency_header": self.idempotency_header})
    }
}

impl Asks {
    /// Everything it may be granted.
    fn all(&self) -> BTreeSet<Capability> {
        self.required.union(&self.optional).copied().collect()
    }

    pub fn to_json(&self) -> Value {
        let names = |c: &BTreeSet<Capability>| c.iter().map(|c| c.as_str()).collect::<Vec<_>>();
        let claims: Vec<Value> = self.claims.iter().map(Claim::to_json).collect();
        json!({"required": names(&self.required), "optional": names(&self.optional), "origins": self.origins, "key_origins": self.key_origins, "claims": claims})
    }

    /// The rights a grant of `chosen` capabilities is: every origin when network is among them.
    /// `chosen` must hold every required capability and only ones this package asks for; the
    /// store checks the same again against the manifest it grants.
    pub fn rights(&self, chosen: BTreeSet<Capability>) -> Result<Rights, Error> {
        if let Some(c) = self.required.iter().find(|c| !chosen.contains(c)) {
            return Err(Error::new(ErrorKind::InvalidInput, format!("this package requires the {} capability", c.as_str())));
        }
        if let Some(c) = chosen.iter().find(|c| !self.all().contains(c)) {
            return Err(Error::new(ErrorKind::InvalidInput, format!("this package doesn't ask for the {} capability", c.as_str())));
        }
        let origins = if chosen.contains(&Capability::Network) { self.origins.clone() } else { BTreeSet::new() };
        Ok(Rights { capabilities: chosen, origins })
    }
}

/// What `package` asks for. A provider template that breaks a rule makes the package
/// ungrantable: what it reaches can't be shown, so it can't be agreed to.
pub fn asks(package: &Package) -> Result<Asks, Error> {
    let caps = &package.manifest.capabilities;
    let optional = caps
        .optional
        .iter()
        .filter_map(|c| match c {
            Optional::Known(c) => Some(*c),
            Optional::Unknown(_) => None,
        })
        .collect();
    let mut asks = Asks { required: caps.required.clone(), optional, origins: BTreeSet::new(), key_origins: BTreeSet::new(), claims: Vec::new() };
    if matches!(package.manifest.runtime, Runtime::ProviderOperations { .. }) {
        for r in package.manifest.registrations.iter().filter(|r| r.kind == RegistrationKind::Provider) {
            let refuse = |why: String| Error::new(ErrorKind::InvalidInput, format!("its provider {} can't be used: {why}", r.id));
            let bytes = package.files.get(&r.descriptor).ok_or_else(|| refuse("its template is missing".into()))?;
            let template = Template::decode(bytes).map_err(|e| refuse(e.to_string()))?;
            asks.origins.extend(template.origins().map(|o| o.as_str().to_owned()));
            if template.credential.is_some() {
                asks.key_origins.insert(template.origin.as_str().to_owned());
            }
            asks.claims.push(Claim {
                provider: template.label.clone(),
                prices: template.models.iter().map(|m| (m.label.clone(), m.price)).collect(),
                idempotency_header: template.idempotency.map(|h| h.name()),
            });
        }
        if asks.origins.len() > nodecraft_plugin_store::grants::MAX_ORIGINS {
            return Err(Error::new(ErrorKind::InvalidInput, "its providers reach more than 8 origins, more than one grant covers"));
        }
    }
    Ok(asks)
}

/// What an update asks for that the installed version didn't, and what it no longer does. A
/// grant never carries over an update, so this is what the next yes would cover beyond the last.
pub fn delta(old: &Asks, new: &Asks) -> Value {
    let caps = |a: &Asks, b: &Asks| a.all().difference(&b.all()).map(|c| c.as_str()).collect::<Vec<_>>();
    let origins = |a: &Asks, b: &Asks| a.origins.difference(&b.origins).cloned().collect::<Vec<_>>();
    json!({
        "added_capabilities": caps(new, old), "removed_capabilities": caps(old, new),
        "added_origins": origins(new, old), "removed_origins": origins(old, new),
    })
}

/// `asks` as JSON, or the reason the package can't be granted.
pub fn asks_json(package: &Package) -> Value {
    match asks(package) {
        Ok(a) => a.to_json(),
        Err(e) => json!({"error": e.message}),
    }
}
