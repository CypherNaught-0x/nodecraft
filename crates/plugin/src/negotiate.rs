//! Version negotiation: protocol major equal and minor ranges overlapping (the highest common
//! minor wins); descriptor contract major equal and the package's minor no newer than the
//! host's; every required feature known to the host, unknown optional ones dropped.
use crate::error::{Code, Result, err};
use crate::manifest::{ContractVersion, Features, ProtocolRange};
use std::collections::BTreeSet;

/// What this host speaks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostVersions {
    pub protocol: ProtocolRange,
    pub descriptor: ContractVersion,
    pub features: BTreeSet<String>,
}

impl HostVersions {
    /// Protocol 1.0 and descriptor contract 1.0, no optional features.
    pub fn current() -> Self {
        Self { protocol: ProtocolRange { major: 1, min_minor: 0, max_minor: 0 }, descriptor: ContractVersion { major: 1, minor: 0 }, features: BTreeSet::new() }
    }
}

/// What host and package agreed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agreed {
    pub protocol_minor: u32,
    pub features: BTreeSet<String>,
}

pub fn negotiate(host: &HostVersions, protocol: ProtocolRange, descriptor: ContractVersion, features: &Features) -> Result<Agreed> {
    if protocol.major != host.protocol.major {
        return err(Code::Incompatible, "the package speaks another protocol major");
    }
    let low = protocol.min_minor.max(host.protocol.min_minor);
    let high = protocol.max_minor.min(host.protocol.max_minor);
    if low > high {
        return err(Code::Incompatible, "no protocol minor both sides speak");
    }
    if descriptor.major != host.descriptor.major || descriptor.minor > host.descriptor.minor {
        return err(Code::Incompatible, "descriptors were written for a contract this host doesn't read");
    }
    if features.required.iter().any(|f| !host.features.contains(f)) {
        return err(Code::UnsupportedFeature, "the package requires a feature this host doesn't have");
    }
    let mut agreed: BTreeSet<String> = features.required.clone();
    agreed.extend(features.optional.iter().filter(|f| host.features.contains(*f)).cloned());
    Ok(Agreed { protocol_minor: high, features: agreed })
}
