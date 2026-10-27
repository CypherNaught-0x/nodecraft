//! Disk-space budgeting for saves and opens (#367): which files an operation writes, where, and
//! how many bytes each takes, checked against the free space the platform reports before any of
//! them is written.
//!
//! The check is advisory. Other programs use and free space between the probe and the writes,
//! so a real write can still run out of room; those errors are handled where they happen and
//! leave the previous file intact. What the check adds is an early, specific refusal for the
//! common case (a save that plainly can't fit), before a large archive is half-written.
//!
//! The inventory counts the new extents an operation creates while the old ones still exist,
//! phase by phase. The previous recovery checkpoint stays until its replacement is published, so
//! its space is never credited. The file a save replaces is credited only after the atomic
//! rename that publishes the new one, which is when its space comes back: a save over an
//! existing file peaks at the larger of "prepared archive + staged file" and "prepared archive +
//! staged file - old file + checkpoint". It does not count history-only assets spilled before a
//! save, imports already staged, the pack that compacting many small cached assets writes before
//! an archive is sealed, or an older-format file rewritten while opening; those are bounded by
//! the session's own disk quota rather than predicted here.
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
    /// Bytes of the file a save replaces, freed when the new one is renamed over it.
    replaced: Option<u64>,
}
impl SpaceInventory {
    /// A native save of an archive of `archive_bytes`: the sealed archive in the workspace, the
    /// staged destination, then the checkpoint. `replaced` is the size of the file already at the
    /// destination, if any: publishing the new file frees it before the checkpoint is written.
    pub fn save(archive_bytes: u64, replaced: Option<u64>) -> Result<Self, Error> {
        let checkpoint = archive_bytes.checked_add(CHECKPOINT_HEADER).ok_or_else(overflow)?;
        Ok(Self {
            operation: Operation::Save,
            items: vec![
                SpaceItem { extent: Extent::PreparedArchive, volume: Volume::Recovery, bytes: archive_bytes },
                SpaceItem { extent: Extent::DestinationStage, volume: Volume::Destination, bytes: archive_bytes },
                SpaceItem { extent: Extent::Checkpoint, volume: Volume::Recovery, bytes: checkpoint },
            ],
            replaced,
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
            replaced: None,
        })
    }
    pub fn items(&self) -> &[SpaceItem] {
        &self.items
    }
    /// Bytes needed on `volume` at its peak, slack included. On separate volumes each one peaks
    /// with everything it holds: freeing the replaced file only shrinks the destination's share
    /// after its peak.
    pub fn needed(&self, volume: Volume) -> Result<u64, Error> {
        self.sum(|item| item.volume == volume)
    }
    fn sum(&self, counted: impl Fn(&SpaceItem) -> bool) -> Result<u64, Error> {
        self.items
            .iter()
            .filter(|item| counted(item))
            .try_fold(0u64, |total, item| total.checked_add(item.bytes).and_then(|n| n.checked_add(EXTENT_SLACK)).ok_or_else(overflow))
    }
    /// The peak when every extent shares one volume. Before publication the prepared archive and
    /// the staged file coexist with the old file; after it the old file is gone and the
    /// checkpoint is written beside the other two.
    pub fn needed_on_one_volume(&self) -> Result<u64, Error> {
        let all = self.sum(|_| true)?;
        let Some(replaced) = self.replaced else { return Ok(all) };
        let before = self.sum(|item| item.extent != Extent::Checkpoint)?;
        Ok(before.max(all.saturating_sub(replaced)))
    }
    /// Refuses, before anything is written, when a known volume has less free space than the
    /// operation needs on it. A volume reporting no free space at all is treated as unknown:
    /// some network and virtual filesystems report zero, and a disk that really is full fails
    /// the first write with its own error anyway.
    pub fn check(&self, probe: &SpaceProbe) -> Result<(), Error> {
        let known = |free: Option<u64>| free.filter(|free| *free > 0);
        let destination = self.needed(Volume::Destination)?;
        let recovery = self.needed(Volume::Recovery)?;
        // Sharing matters only when both volumes are written: an open writes only recovery.
        if probe.shared && destination > 0 && recovery > 0 {
            let free = match (known(probe.destination_free), known(probe.recovery_free)) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let needed = self.needed_on_one_volume()?;
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
            Operation::Save if self.replaced.is_some() => "Nothing was written; the file is unchanged.",
            Operation::Save => "Nothing was written.",
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
        let save = SpaceInventory::save(GB, None).unwrap();
        assert_eq!(save.needed(Volume::Destination).unwrap(), GB + EXTENT_SLACK);
        assert_eq!(save.needed(Volume::Recovery).unwrap(), 2 * GB + 80 + 2 * EXTENT_SLACK);
        let open = SpaceInventory::open(GB).unwrap();
        assert_eq!(open.needed(Volume::Destination).unwrap(), 0);
        assert_eq!(open.needed(Volume::Recovery).unwrap(), 2 * GB + 80 + 2 * EXTENT_SLACK);
    }

    #[test]
    fn one_shared_volume_must_hold_the_peak_of_each_phase() {
        let probe = |free| SpaceProbe { destination_free: Some(free), recovery_free: Some(free), shared: true, existing_bytes: None };
        // A new file: nothing is freed, so the archive, the file and the checkpoint all coexist.
        let new = SpaceInventory::save(GB, None).unwrap();
        let total = 3 * GB + 80 + 3 * EXTENT_SLACK;
        assert_eq!(new.needed_on_one_volume().unwrap(), total);
        assert!(new.check(&probe(total)).is_ok());
        let error = new.check(&probe(total - 1)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Busy);
        assert!(error.message.contains("needs about 3.1 GB"), "{}", error.message);
        // No file was there, so none is claimed unchanged.
        assert!(error.message.ends_with("Nothing was written."), "{}", error.message);
        // Re-saving over a file: its space comes back at the rename, before the checkpoint.
        let resave = SpaceInventory::save(GB, Some(GB)).unwrap();
        let peak = 2 * GB + 80 + 3 * EXTENT_SLACK;
        assert_eq!(resave.needed_on_one_volume().unwrap(), peak);
        assert!(resave.check(&probe(peak)).is_ok());
        assert!(resave.check(&probe(peak - 1)).unwrap_err().message.contains("the file is unchanged"));
        // Before publication the old file still exists: a tiny old file credits almost nothing.
        let small_old = SpaceInventory::save(GB, Some(1)).unwrap();
        assert_eq!(small_old.needed_on_one_volume().unwrap(), total - 1);
        // A replaced file larger than the new one can't make the pre-publication phase smaller.
        let shrink = SpaceInventory::save(GB, Some(10 * GB)).unwrap();
        assert_eq!(shrink.needed_on_one_volume().unwrap(), 2 * GB + 2 * EXTENT_SLACK);
        // Separate volumes are each checked for their own share only.
        let split =
            SpaceProbe { destination_free: Some(GB + EXTENT_SLACK), recovery_free: Some(2 * GB + 80 + 2 * EXTENT_SLACK), shared: false, existing_bytes: None };
        assert!(new.check(&split).is_ok());
    }

    #[test]
    fn a_ten_gigabyte_resave_fits_in_twenty_five_free() {
        // Regression: charging all three extents (30 GB) refused a save that fits (~20 GB peak).
        let resave = SpaceInventory::save(10 * GB, Some(10 * GB)).unwrap();
        let probe = SpaceProbe { destination_free: Some(25 * GB), recovery_free: Some(25 * GB), shared: true, existing_bytes: Some(10 * GB) };
        assert!(resave.check(&probe).is_ok());
    }

    #[test]
    fn an_open_is_never_refused_for_the_source_drive() {
        // An open writes only to the recovery volume, even when the probe thinks they're shared.
        let open = SpaceInventory::open(GB).unwrap();
        let probe = SpaceProbe { destination_free: Some(1), recovery_free: Some(3 * GB), shared: true, existing_bytes: Some(GB) };
        assert!(open.check(&probe).is_ok());
        let short = SpaceProbe { recovery_free: Some(GB), ..probe };
        assert!(open.check(&short).is_err());
    }

    #[test]
    fn a_short_destination_suggests_another_drive_and_names_no_path() {
        let save = SpaceInventory::save(10 * GB, None).unwrap();
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
        let save = SpaceInventory::save(GB, None).unwrap();
        assert!(save.check(&SpaceProbe::default()).is_ok());
        let zero = SpaceProbe { destination_free: Some(0), recovery_free: Some(0), shared: true, existing_bytes: None };
        assert!(save.check(&zero).is_ok());
        let one_known = SpaceProbe { destination_free: Some(0), recovery_free: Some(GB), shared: true, existing_bytes: None };
        assert!(save.check(&one_known).is_err());
    }

    #[test]
    fn hostile_sizes_overflow_into_errors_and_sizes_round_up() {
        assert!(SpaceInventory::save(u64::MAX, None).is_err());
        assert!(SpaceInventory::save(u64::MAX - 80, Some(u64::MAX)).unwrap().needed(Volume::Recovery).is_err());
        assert!(SpaceInventory::open(u64::MAX - 80).unwrap().check(&SpaceProbe { recovery_free: Some(1), ..Default::default() }).is_err());
        assert_eq!(size(999), "999 bytes");
        assert_eq!(size(1_000), "1.0 KB");
        assert_eq!(size(1_000_001), "1.1 MB");
        assert_eq!(size(u64::MAX), "18446744.1 TB");
    }
}
