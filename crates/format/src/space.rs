//! Disk-space budgeting for saves and opens (#367): which files an operation writes, where, and
//! how many bytes each takes, checked against the free space the platform reports before any of
//! them is written.
//!
//! The check is advisory. Other programs use and free space between the probe and the writes,
//! so a real write can still run out of room; those errors are handled where they happen and
//! leave the previous file intact. What the check adds is an early, specific refusal for the
//! common case (a save that plainly can't fit), before a large archive is half-written.
//!
//! The inventory counts the new extents an operation creates while the old ones still exist:
//! the previous destination and recovery checkpoint are kept until their replacements are
//! published, so their space is never credited. It does not count history-only assets spilled
//! before a save, imports already staged, or an older-format file rewritten while opening; those
//! are bounded by the session's own disk quota rather than predicted here.
use nodecraft_types::{Error, ErrorKind};

/// The volume an extent lands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Volume {
    /// The volume holding the project file's folder.
    Destination,
    /// The volume holding the recovery journal and the private asset workspace beside it.
    Recovery,
}

/// One file an operation writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extent {
    /// A save's sealed archive, prepared in the private workspace before anything is replaced.
    PreparedArchive,
    /// The new project file, written beside the destination and renamed over it when complete.
    DestinationStage,
    /// An open's private copy of the project file, so the file can change or go afterwards.
    FrozenBase,
    /// The recovery checkpoint that replaces the journal once the operation succeeds.
    Checkpoint,
}
impl Extent {
    pub const fn name(self) -> &'static str {
        match self {
            Self::PreparedArchive => "prepared archive",
            Self::DestinationStage => "new project file",
            Self::FrozenBase => "private copy of the project",
            Self::Checkpoint => "recovery checkpoint",
        }
    }
}

/// One extent's bytes on its volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceItem {
    pub extent: Extent,
    pub volume: Volume,
    pub bytes: u64,
}

/// What the platform reports about the volumes an operation writes to. `None` means unknown
/// (the browser, an in-memory host, or a probe that failed), and an unknown volume is not checked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpaceProbe {
    pub destination_free: Option<u64>,
    pub recovery_free: Option<u64>,
    /// The destination and recovery folders are on one volume, so their needs add up.
    pub shared: bool,
    /// The size of the file already at the location, if there is one (what an open will copy).
    pub existing_bytes: Option<u64>,
}

/// Filesystems allocate whole blocks and keep metadata; each extent is charged this much more
/// than its bytes so a save that would fit only to the last byte isn't promised to.
pub const EXTENT_SLACK: u64 = 64 * 1024;
/// Header a native checkpoint adds in front of its archive.
const CHECKPOINT_HEADER: u64 = 80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Save,
    Open,
}

/// The extents one save or open writes, for [`SpaceInventory::check`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceInventory {
    operation: Operation,
    items: Vec<SpaceItem>,
}
impl SpaceInventory {
    /// A native save of an archive of `archive_bytes`: the sealed archive in the workspace, the
    /// staged destination, then the checkpoint. All three exist at once before the old files go.
    pub fn save(archive_bytes: u64) -> Result<Self, Error> {
        let checkpoint = archive_bytes.checked_add(CHECKPOINT_HEADER).ok_or_else(overflow)?;
        Ok(Self {
            operation: Operation::Save,
            items: vec![
                SpaceItem { extent: Extent::PreparedArchive, volume: Volume::Recovery, bytes: archive_bytes },
                SpaceItem { extent: Extent::DestinationStage, volume: Volume::Destination, bytes: archive_bytes },
                SpaceItem { extent: Extent::Checkpoint, volume: Volume::Recovery, bytes: checkpoint },
            ],
        })
    }
    /// A native open of a file of `file_bytes`: its private frozen copy, then the checkpoint.
    pub fn open(file_bytes: u64) -> Result<Self, Error> {
        let checkpoint = file_bytes.checked_add(CHECKPOINT_HEADER).ok_or_else(overflow)?;
        Ok(Self {
            operation: Operation::Open,
            items: vec![
                SpaceItem { extent: Extent::FrozenBase, volume: Volume::Recovery, bytes: file_bytes },
                SpaceItem { extent: Extent::Checkpoint, volume: Volume::Recovery, bytes: checkpoint },
            ],
        })
    }
    pub fn items(&self) -> &[SpaceItem] {
        &self.items
    }
    /// Bytes needed on `volume`, slack included.
    pub fn needed(&self, volume: Volume) -> Result<u64, Error> {
        self.items
            .iter()
            .filter(|item| item.volume == volume)
            .try_fold(0u64, |total, item| total.checked_add(item.bytes).and_then(|n| n.checked_add(EXTENT_SLACK)).ok_or_else(overflow))
    }
    /// Refuses, before anything is written, when a known volume has less free space than the
    /// operation needs on it. A volume reporting no free space at all is treated as unknown:
    /// some network and virtual filesystems report zero, and a disk that really is full fails
    /// the first write with its own error anyway.
    pub fn check(&self, probe: &SpaceProbe) -> Result<(), Error> {
        let known = |free: Option<u64>| free.filter(|free| *free > 0);
        let destination = self.needed(Volume::Destination)?;
        let recovery = self.needed(Volume::Recovery)?;
        if probe.shared {
            let free = match (known(probe.destination_free), known(probe.recovery_free)) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let needed = destination.checked_add(recovery).ok_or_else(overflow)?;
            return match free {
                Some(free) if free < needed => Err(self.short(Where::Both, needed, free)),
                _ => Ok(()),
            };
        }
        if destination > 0
            && let Some(free) = known(probe.destination_free)
            && free < destination
        {
            return Err(self.short(Where::Destination, destination, free));
        }
        if recovery > 0
            && let Some(free) = known(probe.recovery_free)
            && free < recovery
        {
            return Err(self.short(Where::Recovery, recovery, free));
        }
        Ok(())
    }
    fn short(&self, at: Where, needed: u64, free: u64) -> Error {
        let what = match self.operation {
            Operation::Save => "save",
            Operation::Open => "open this project",
        };
        let drive = match at {
            Where::Both => "the drive holding the project and NodeCraft's recovery folder",
            Where::Destination => "the drive you are saving to",
            Where::Recovery => "the drive holding NodeCraft's recovery folder",
        };
        let fix = match (self.operation, at) {
            (Operation::Save, Where::Destination) => "Free some space there or use Save As to choose another drive.",
            (Operation::Save, _) => "Free some space there, then save again.",
            (Operation::Open, _) => "Free some space there, then open the project again.",
        };
        let unchanged = match self.operation {
            Operation::Save => "Nothing was written; the file is unchanged.",
            Operation::Open => "Nothing was written; the current project is unchanged.",
        };
        // Sizes, never paths: the message may be shown, logged or sent to an agent.
        Error::new(
            ErrorKind::Busy,
            format!("Not enough free disk space to {what}: it needs about {} on {drive}, and {} is free. {fix} {unchanged}", size(needed), size(free)),
        )
    }
}
#[derive(Clone, Copy)]
enum Where {
    Both,
    Destination,
    Recovery,
}
fn overflow() -> Error {
    Error::new(ErrorKind::Overflow, "disk space estimate overflow")
}
/// A byte count for people: one decimal place, rounded up, so "needs" never understates.
pub fn size(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [("TB", 1_000_000_000_000), ("GB", 1_000_000_000), ("MB", 1_000_000), ("KB", 1_000)];
    for (unit, scale) in UNITS {
        if bytes >= scale {
            let tenths = (u128::from(bytes) * 10).div_ceil(u128::from(scale));
            return format!("{}.{} {unit}", tenths / 10, tenths % 10);
        }
    }
    format!("{bytes} bytes")
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1_000_000_000;

    #[test]
    fn a_save_counts_every_extent_that_exists_before_the_old_files_go() {
        let save = SpaceInventory::save(GB).unwrap();
        assert_eq!(save.needed(Volume::Destination).unwrap(), GB + EXTENT_SLACK);
        assert_eq!(save.needed(Volume::Recovery).unwrap(), 2 * GB + 80 + 2 * EXTENT_SLACK);
        let open = SpaceInventory::open(GB).unwrap();
        assert_eq!(open.needed(Volume::Destination).unwrap(), 0);
        assert_eq!(open.needed(Volume::Recovery).unwrap(), 2 * GB + 80 + 2 * EXTENT_SLACK);
    }

    #[test]
    fn one_shared_volume_must_hold_the_sum() {
        let save = SpaceInventory::save(GB).unwrap();
        let total = 3 * GB + 80 + 3 * EXTENT_SLACK;
        let probe = |free| SpaceProbe { destination_free: Some(free), recovery_free: Some(free), shared: true, existing_bytes: None };
        assert!(save.check(&probe(total)).is_ok());
        let error = save.check(&probe(total - 1)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Busy);
        assert!(error.message.contains("needs about 3.1 GB"), "{}", error.message);
        assert!(error.message.contains("the file is unchanged"), "{}", error.message);
        // Separate volumes are each checked for their own share only.
        let split =
            SpaceProbe { destination_free: Some(GB + EXTENT_SLACK), recovery_free: Some(2 * GB + 80 + 2 * EXTENT_SLACK), shared: false, existing_bytes: None };
        assert!(save.check(&split).is_ok());
    }

    #[test]
    fn a_short_destination_suggests_another_drive_and_names_no_path() {
        let save = SpaceInventory::save(10 * GB).unwrap();
        let probe = SpaceProbe { destination_free: Some(GB), recovery_free: None, shared: false, existing_bytes: None };
        let error = save.check(&probe).unwrap_err();
        assert!(error.message.contains("Save As"), "{}", error.message);
        assert!(error.message.contains("1.0 GB is free"), "{}", error.message);
        assert!(!error.message.contains('/') && !error.message.contains('\\'));
        let short_recovery = SpaceProbe { destination_free: None, recovery_free: Some(GB), shared: false, existing_bytes: None };
        assert!(save.check(&short_recovery).unwrap_err().message.contains("recovery folder"));
        let open = SpaceInventory::open(10 * GB).unwrap();
        let error = open.check(&short_recovery).unwrap_err();
        assert!(error.message.contains("open this project") && error.message.contains("current project is unchanged"), "{}", error.message);
    }

    #[test]
    fn unknown_or_zero_reports_are_not_refusals() {
        let save = SpaceInventory::save(GB).unwrap();
        assert!(save.check(&SpaceProbe::default()).is_ok());
        let zero = SpaceProbe { destination_free: Some(0), recovery_free: Some(0), shared: true, existing_bytes: None };
        assert!(save.check(&zero).is_ok());
        let one_known = SpaceProbe { destination_free: Some(0), recovery_free: Some(GB), shared: true, existing_bytes: None };
        assert!(save.check(&one_known).is_err());
    }

    #[test]
    fn hostile_sizes_overflow_into_errors_and_sizes_round_up() {
        assert!(SpaceInventory::save(u64::MAX).is_err());
        assert!(SpaceInventory::save(u64::MAX - 80).unwrap().needed(Volume::Recovery).is_err());
        assert!(SpaceInventory::open(u64::MAX - 80).unwrap().check(&SpaceProbe { recovery_free: Some(1), ..Default::default() }).is_err());
        assert_eq!(size(999), "999 bytes");
        assert_eq!(size(1_000), "1.0 KB");
        assert_eq!(size(1_000_001), "1.1 MB");
        assert_eq!(size(u64::MAX), "18446744.1 TB");
    }
}
